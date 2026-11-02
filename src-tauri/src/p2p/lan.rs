//! Fast path on the local network: when a peer is on the same network, the
//! bytes of a file go over a direct TCP + TLS connection (the stack and kind
//! of identity LocalSend uses) instead of QUIC, which is much faster there.
//! Offers, answers and resuming still go over the QUIC control stream.
//!
//!   advertisement: each side tells its peers, over the authenticated control
//!                  stream, the fingerprint of its TLS certificate, its port
//!                  and its private IPv4 addresses (`lan` message).
//!   data:          the sender connects to an address of the peer on one of
//!                  our networks, pins the certificate to the fingerprint it
//!                  advertised and presents its own; then the same stream as
//!                  over QUIC: [tid:16][offset:u64 BE] and the raw bytes.
//!
//! The sender only tries for files that are not small (`SMALL_FILE`), and
//! when iroh reaches the peer directly on one of our networks: then the address iroh uses is where the peer really is (two
//! homes both numbered 192.168.1.x are reached over the internet, or a
//! relay, and are not tried). Advertised addresses are only used when that
//! direct path is over IPv6, and only a few of them.
//!
//! The receiver only takes a stream whose client certificate is the one the
//! transfer's peer advertised. When the connection fails, the sender goes
//! back to QUIC for that peer for a while; the receiver asks to resume from
//! what it has, as for any interrupted stream.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use iroh::endpoint::PathList;
use iroh::TransportAddr;
use netdev::ipnet::{Ipv4Net, Ipv6Net};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_rustls::{TlsAcceptor, TlsStream};

use super::*;
use crate::localsend::{self, Identity};

const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
/// A whole connection attempt (TCP + TLS). A failed one always takes that
/// long, so its timing says nothing of what is at the address.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);
const TLS_TIMEOUT: Duration = Duration::from_secs(5);
/// A write the receiver does not take for that long ends the stream (it may
/// be busy writing to a slow disk for a while).
pub(super) const IO_TIMEOUT: Duration = Duration::from_secs(60);
/// No bytes for that long ends the stream. The sender may be stuck reading a
/// slow file (a cloud placeholder being downloaded), so it is long: a peer
/// that is gone is noticed sooner by the TCP keepalive.
const READ_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const KEEPALIVE_TIME: Duration = Duration::from_secs(10);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
const KEEPALIVE_RETRIES: u32 = 4;
/// A stream cut short waits that long before saying so: when the peer
/// stopped on purpose (cancel, new session), its message over the control
/// stream ends the session meanwhile, and the cut is not taken for a failure.
const END_GRACE: Duration = Duration::from_millis(1500);
/// How long a peer whose connection failed is reached over QUIC only.
const DOWN_FOR: Duration = Duration::from_secs(2 * 60);
/// The same when it fails again right after (a firewall, most likely).
const DOWN_AGAIN_FOR: Duration = Duration::from_secs(10 * 60);
/// A peer advertising new addresses is tried again, at the soonest, then.
const RETRY_ON_CHANGE: Duration = Duration::from_secs(60);
/// Our addresses are looked at again on each network change the system
/// reports (`Node::network_changed`), and that often in case it does not.
const ADDRESS_CHECK: Duration = Duration::from_secs(60);
const ECONOMY_ADDRESS_CHECK: Duration = Duration::from_secs(5 * 60);
const MAX_ADDRESSES: usize = 16;
/// Addresses tried at once.
const MAX_TRIED: usize = 4;
/// Connections not authenticated yet, in all and from one address.
const MAX_HANDSHAKES: usize = 32;
/// (Above `MAX_AUTO_ACTIVE`: a folder's files may all start at once.)
const MAX_HANDSHAKES_PER_IP: usize = 16;
const READ_CHUNK: usize = 256 * 1024;

pub(super) type Stream = TlsStream<TcpStream>;

pub(super) struct Lan {
    identity: Identity,
    acceptor: TlsAcceptor,
    port: u16,
}

/// What a peer advertised.
#[derive(Clone)]
pub(super) struct LanPeer {
    fingerprint: String,
    port: u16,
    ips: Vec<Ipv4Addr>,
    /// The connection it came over: a peer advertises again on each one.
    conn: Option<u64>,
}

impl LanPeer {
    fn same_place(&self, other: &LanPeer) -> bool {
        self.fingerprint == other.fingerprint && self.port == other.port && self.ips == other.ips
    }
}

/// Where to reach a peer for one stream.
pub(super) struct LanTarget {
    peer: LanPeer,
    /// Remote addresses of the direct paths iroh has to the peer.
    direct: Vec<IpAddr>,
}

impl Lan {
    pub(super) async fn bind(identity: Identity) -> io::Result<(Lan, TcpListener)> {
        let acceptor = localsend::tls_acceptor(&identity).map_err(io::Error::other)?;
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
        let port = listener.local_addr()?.port();
        Ok((
            Lan {
                identity,
                acceptor,
                port,
            },
            listener,
        ))
    }
}

/// The networks of our interfaces.
struct Nets {
    /// With whether the interface looks physical (not a VPN, VM or container).
    v4: Vec<(Ipv4Net, bool)>,
    v6: Vec<(Ipv6Net, bool)>,
}

impl Nets {
    fn ours() -> Nets {
        let mut nets = Nets {
            v4: Vec::new(),
            v6: Vec::new(),
        };
        for i in netdev::get_interfaces() {
            if !i.is_up() || i.is_loopback() {
                continue;
            }
            let physical = i.is_physical() && !i.is_tun();
            nets.v4.extend(i.ipv4.iter().map(|n| (*n, physical)));
            nets.v6.extend(i.ipv6.iter().map(|n| (*n, physical)));
        }
        nets
    }

    /// 0 on the network of a physical interface, 1 on another of ours,
    /// `None` elsewhere.
    fn rank(&self, ip: &Ipv4Addr) -> Option<u8> {
        self.v4
            .iter()
            .filter(|(n, _)| n.contains(ip))
            .map(|(_, physical)| u8::from(!physical))
            .min()
    }

    fn has_v6(&self, ip: &std::net::Ipv6Addr) -> bool {
        self.v6.iter().any(|(n, _)| !n.addr().is_loopback() && n.contains(ip))
    }

    /// On the network of a physical interface: the local network, not a
    /// VPN (Tailscale…), VM or container network.
    fn near(&self, ip: IpAddr) -> bool {
        match canonical(ip) {
            IpAddr::V4(v4) => self.rank(&v4) == Some(0),
            IpAddr::V6(v6) => self
                .v6
                .iter()
                .any(|(n, physical)| *physical && !n.addr().is_loopback() && n.contains(&v6)),
        }
    }
}

/// Our private addresses, which peers on the same network can reach; those
/// of physical interfaces first, so that the cap drops virtual ones.
fn private_ipv4s() -> Vec<Ipv4Addr> {
    let mut ranked: Vec<(u8, Ipv4Addr)> = Nets::ours()
        .v4
        .iter()
        .map(|(n, physical)| (u8::from(!physical), n.addr()))
        .filter(|(_, ip)| ip.is_private())
        .collect();
    ranked.sort();
    let mut seen = HashSet::new();
    ranked
        .into_iter()
        .map(|(_, ip)| ip)
        .filter(|ip| seen.insert(*ip))
        .take(MAX_ADDRESSES)
        .collect()
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// How a connection goes, from its selected path: "relay", "lan" (direct,
/// to an address on the network of one of our physical interfaces) or
/// "direct" (over the internet, or a VPN).
pub(super) fn path_route(paths: &PathList) -> Option<&'static str> {
    let selected = paths.iter().find(|p| p.is_selected())?;
    let TransportAddr::Ip(addr) = selected.remote_addr() else {
        return Some("relay");
    };
    Some(if Nets::ours().near(addr.ip()) { "lan" } else { "direct" })
}

/// Where to try a peer, given the addresses it advertised and those of the
/// direct paths iroh has to it.
///
/// A direct IPv4 path on one of our networks proves the peer is there: only
/// that address is tried. A direct IPv6 path on one of our networks says it
/// is next to us: a few of its advertised addresses on our networks are
/// tried, physical interfaces first. Otherwise (relayed, or reached over the
/// internet), nothing.
fn candidates(advertised: &[Ipv4Addr], direct: &[IpAddr], nets: &Nets) -> Vec<Ipv4Addr> {
    let mut proven: Vec<Ipv4Addr> = direct
        .iter()
        .filter_map(|ip| match canonical(*ip) {
            IpAddr::V4(v4) if nets.rank(&v4).is_some() => Some(v4),
            _ => None,
        })
        .collect();
    if !proven.is_empty() {
        proven.sort();
        proven.dedup();
        proven.truncate(MAX_TRIED);
        return proven;
    }
    let near = direct
        .iter()
        .any(|ip| matches!(canonical(*ip), IpAddr::V6(v6) if nets.has_v6(&v6)));
    if !near {
        return Vec::new();
    }
    let mut ranked: Vec<(u8, Ipv4Addr)> = advertised
        .iter()
        .filter_map(|ip| nets.rank(ip).map(|r| (r, *ip)))
        .collect();
    ranked.sort();
    ranked.dedup();
    ranked
        .into_iter()
        .map(|(_, ip)| ip)
        .take(MAX_TRIED)
        .collect()
}

/// No delay, and a keepalive so that a peer that is gone is noticed while
/// the stream is idle.
fn tune(tcp: &TcpStream) {
    let _ = tcp.set_nodelay(true);
    let keepalive = socket2::TcpKeepalive::new().with_time(KEEPALIVE_TIME);
    #[cfg(any(
        windows,
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    let keepalive = keepalive
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_RETRIES);
    let _ = socket2::SockRef::from(tcp).set_tcp_keepalive(&keepalive);
}

/// Data received over a QUIC stream or a LAN connection.
pub(super) enum Source {
    Quic(RecvStream),
    Lan(Box<Stream>),
}

impl Source {
    /// The next bytes, at most `max`; `None` at the end of the stream.
    pub(super) async fn chunk(&mut self, max: usize) -> io::Result<Option<Bytes>> {
        match self {
            Source::Quic(recv) => recv
                .read_chunk(max.min(CHUNK_SIZE as usize))
                .await
                .map_err(io::Error::other),
            Source::Lan(stream) => {
                let mut buf = bytes::BytesMut::with_capacity(max.min(READ_CHUNK));
                match tokio::time::timeout(READ_TIMEOUT, stream.read_buf(&mut buf)).await {
                    Err(_) => Err(io::ErrorKind::TimedOut.into()),
                    Ok(Ok(n)) if n > 0 => Ok(Some(buf.freeze())),
                    // Cut short (the end of a whole file is never read: the
                    // receiver stops at its size). A sender that stopped on
                    // purpose ends our session meanwhile.
                    ended => {
                        tokio::time::sleep(END_GRACE).await;
                        match ended {
                            Ok(Err(e)) => Err(e),
                            _ => Ok(None),
                        }
                    }
                }
            }
        }
    }

    /// Refuse the rest of the stream.
    pub(super) fn stop(&mut self) {
        if let Source::Quic(recv) = self {
            let _ = recv.stop(VarInt::from_u32(0));
        }
    }

    pub(super) fn is_lan(&self) -> bool {
        matches!(self, Source::Lan(_))
    }
}

/// Data sent over a QUIC stream or a LAN connection.
pub(super) enum Sink {
    Quic(SendStream),
    Lan(Box<Stream>),
}

impl Sink {
    pub(super) async fn write(&mut self, data: Bytes) -> bool {
        match self {
            Sink::Quic(send) => send.write_chunk(data).await.is_ok(),
            Sink::Lan(stream) => {
                let write = stream.write_all(&data);
                match tokio::time::timeout(IO_TIMEOUT, write).await {
                    Ok(Ok(())) => true,
                    Err(_) => false,
                    // Closed by the receiver: when on purpose (cancel), its
                    // message over the control stream ends our session
                    // meanwhile, and the stream is not taken for broken.
                    Ok(Err(_)) => {
                        tokio::time::sleep(END_GRACE).await;
                        false
                    }
                }
            }
        }
    }

    /// Stop midway: the receiver keeps what it got.
    pub(super) fn abort(self) {
        if let Sink::Quic(mut send) = self {
            let _ = send.reset(VarInt::from_u32(0));
        }
    }

    /// Everything is written: let it reach the receiver.
    pub(super) async fn finish(self) -> bool {
        match self {
            Sink::Quic(mut send) => send.finish().is_ok(),
            Sink::Lan(mut stream) => {
                let done = async {
                    stream.flush().await?;
                    stream.shutdown().await
                };
                matches!(tokio::time::timeout(IO_TIMEOUT, done).await, Ok(Ok(())))
            }
        }
    }

    pub(super) fn is_lan(&self) -> bool {
        matches!(self, Sink::Lan(_))
    }
}

impl State {
    /// Where to reach `peer` over the local network, if we may.
    pub(super) fn lan_target(&mut self, peer: &str) -> Option<LanTarget> {
        // `lan_down` holds until when the peer is reached over QUIC only.
        if self
            .lan_down
            .get(peer)
            .is_some_and(|until| Instant::now() < *until)
        {
            return None;
        }
        let conn = self.conns.get(peer)?;
        let lan = self.lan_peers.get(peer)?;
        if lan.conn != Some(conn.id) {
            // Advertised over an earlier connection and not since.
            self.lan_peers.remove(peer);
            return None;
        }
        // Over a relay only, the peer is not next to us.
        let direct: Vec<IpAddr> = conn
            .conn
            .paths()
            .iter()
            .filter_map(|p| match p.remote_addr() {
                TransportAddr::Ip(addr) => Some(canonical(addr.ip())),
                _ => None,
            })
            .collect();
        if direct.is_empty() {
            return None;
        }
        Some(LanTarget {
            peer: lan.clone(),
            direct,
        })
    }

    pub(super) fn send_lan_advert(&self, peer: &str) {
        if let Some(advert) = &self.lan_advert {
            self.send(peer, advert.clone());
        }
    }
}

impl Node {
    pub(super) fn start_lan(&self, listener: TcpListener) {
        self.spawn(self.clone().lan_accept_loop(listener));
        self.spawn(self.clone().lan_addresses());
    }

    /// Look at our addresses again now (the network changed).
    pub(super) fn refresh_lan(&self) {
        if self.0.lan.is_some() {
            self.0.lan_refresh.notify_one();
        }
    }

    /// Keep peers told of our addresses as networks come and go.
    async fn lan_addresses(self) {
        let Some(lan) = &self.0.lan else { return };
        let (fingerprint, port) = (lan.identity.fingerprint().to_string(), lan.port);
        loop {
            if self.0.endpoint.is_closed() {
                return;
            }
            let ips = tokio::task::spawn_blocking(private_ipv4s)
                .await
                .unwrap_or_default();
            let advert = (!ips.is_empty())
                .then(|| json!({ "type": "lan", "fp": fingerprint, "port": port, "ips": ips }));
            self.with(|s| {
                if s.lan_advert == advert {
                    return;
                }
                // No address any more: an empty list tells peers to stop.
                let sent = advert.clone().unwrap_or_else(
                    || json!({ "type": "lan", "fp": fingerprint, "port": port, "ips": [] }),
                );
                s.lan_advert = advert;
                for c in s.conns.values() {
                    c.push(frame(&sent));
                }
            });
            let check = if self.0.economy.load(Ordering::Relaxed) {
                ECONOMY_ADDRESS_CHECK
            } else {
                ADDRESS_CHECK
            };
            tokio::select! {
                _ = tokio::time::sleep(check) => {}
                _ = self.0.lan_refresh.notified() => {}
            }
        }
    }

    pub(super) fn on_lan(&self, s: &mut State, peer: &str, msg: &Value) {
        let fingerprint = msg
            .get("fp")
            .and_then(Value::as_str)
            .filter(|f| f.len() == 64 && f.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_ascii_uppercase);
        let port = msg
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p >= 1024);
        let ips: Vec<Ipv4Addr> = msg
            .get("ips")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .filter_map(|ip| ip.parse::<Ipv4Addr>().ok())
                    .filter(Ipv4Addr::is_private)
                    .take(MAX_ADDRESSES)
                    .collect()
            })
            .unwrap_or_default();
        match (fingerprint, port) {
            (Some(fingerprint), Some(port)) if !ips.is_empty() => {
                let new = LanPeer {
                    fingerprint,
                    port,
                    ips,
                    conn: s.conns.get(peer).map(|c| c.id),
                };
                // Somewhere else now: worth trying again, but not right away
                // nor more often than that, however often it changes.
                let moved = s.lan_peers.get(peer).is_none_or(|old| !old.same_place(&new));
                if moved {
                    if let Some(until) = s.lan_down.get_mut(peer) {
                        *until = (*until).min(Instant::now() + RETRY_ON_CHANGE);
                    }
                }
                s.lan_peers.insert(peer.to_string(), new);
            }
            _ => {
                s.lan_peers.remove(peer);
            }
        }
    }

    /// A connection to the peer on the local network, if it answers quickly.
    pub(super) async fn lan_connect(&self, target: LanTarget) -> Option<Stream> {
        let lan = self.0.lan.as_ref()?;
        let started = Instant::now();
        let LanTarget { peer, direct } = target;
        let connector = localsend::tls_connector(&lan.identity, Some(&peer.fingerprint)).ok()?;
        let nets = tokio::task::spawn_blocking(Nets::ours).await.ok()?;
        let candidates = candidates(&peer.ips, &direct, &nets);
        if candidates.is_empty() {
            return None;
        }
        // Every address at once; the first that proves the right
        // certificate wins (a virtual network may reuse the same subnet, even
        // our own address on it).
        let mut attempts = JoinSet::new();
        for ip in candidates {
            let connector = connector.clone();
            let addr = SocketAddr::from((ip, peer.port));
            attempts.spawn(async move {
                let attempt = async {
                    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
                        .await
                        .ok()?
                        .ok()?;
                    tune(&tcp);
                    let name = rustls::pki_types::ServerName::IpAddress(addr.ip().into());
                    let tls = connector.connect(name, tcp).await.ok()?;
                    Some(TlsStream::Client(tls))
                };
                tokio::time::timeout(ATTEMPT_TIMEOUT, attempt)
                    .await
                    .ok()
                    .flatten()
            });
        }
        while let Some(result) = attempts.join_next().await {
            if let Ok(Some(stream)) = result {
                return Some(stream);
            }
        }
        // Refused, unanswered or not TLS: all take as long, so the peer, who
        // sees when the data comes over QUIC instead, learns nothing of what
        // is at the addresses it gave.
        tokio::time::sleep_until((started + ATTEMPT_TIMEOUT).into()).await;
        None
    }

    /// The connection to `peer` failed: QUIC only for a while.
    pub(super) fn lan_failed(&self, s: &mut State, peer: &str) {
        let now = Instant::now();
        let previous = s.lan_down.get(peer).copied();
        if previous.is_some_and(|until| now < until) {
            // Several streams failing together: one failure.
            return;
        }
        let again = previous.is_some_and(|until| now < until + DOWN_FOR);
        let pause = if again { DOWN_AGAIN_FOR } else { DOWN_FOR };
        s.lan_down.insert(peer.to_string(), now + pause);
    }

    async fn lan_accept_loop(self, listener: TcpListener) {
        let Some(lan) = &self.0.lan else { return };
        let acceptor = lan.acceptor.clone();
        // Only connections not authenticated yet count: streams of real
        // transfers go on in their own tasks.
        let mut handshakes = JoinSet::new();
        let mut per_ip: HashMap<IpAddr, usize> = HashMap::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((tcp, from)) = accepted else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    };
                    if self.0.endpoint.is_closed() {
                        return;
                    }
                    let ip = canonical(from.ip());
                    let pending = per_ip.get(&ip).copied().unwrap_or(0);
                    if handshakes.len() >= MAX_HANDSHAKES || pending >= MAX_HANDSHAKES_PER_IP {
                        continue;
                    }
                    per_ip.insert(ip, pending + 1);
                    let acceptor = acceptor.clone();
                    let node = self.clone();
                    handshakes.spawn(async move { (ip, node.lan_handshake(tcp, acceptor).await) });
                }
                Some(done) = handshakes.join_next(), if !handshakes.is_empty() => {
                    let Ok((ip, authed)) = done else { continue };
                    if let Some(n) = per_ip.get_mut(&ip) {
                        *n -= 1;
                        if *n == 0 {
                            per_ip.remove(&ip);
                        }
                    }
                    if let Some((peer, tid, offset, tls)) = authed {
                        let source = Source::Lan(Box::new(tls));
                        self.spawn(self.clone().receive_data(peer, tid, offset, source));
                    }
                }
            }
        }
    }

    /// TLS, then the stream header: the peer, transfer and offset of a
    /// stream from the peer that holds the certificate it advertised.
    async fn lan_handshake(
        self,
        tcp: TcpStream,
        acceptor: TlsAcceptor,
    ) -> Option<(String, String, u64, Stream)> {
        tune(&tcp);
        let mut tls = tokio::time::timeout(TLS_TIMEOUT, acceptor.accept(tcp))
            .await
            .ok()?
            .ok()?;
        let cert = tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|c| c.first())
            .map(|c| localsend::fingerprint(c))?;
        let mut header = [0u8; DATA_HEADER];
        let read = tokio::time::timeout(TLS_TIMEOUT, tls.read_exact(&mut header)).await;
        if !matches!(read, Ok(Ok(_))) {
            return None;
        }
        let tid = hex::encode(&header[..TID_BYTES]);
        let offset = u64::from_be_bytes(header[TID_BYTES..].try_into().unwrap());
        // The transfer's peer must be the one holding the certificate it
        // advertised to us.
        let peer = self.with(|s| {
            let t = s.transfers.get(&tid)?;
            let lan = s.lan_peers.get(&t.friend_id)?;
            (lan.fingerprint.eq_ignore_ascii_case(&cert) && s.conns.contains_key(&t.friend_id))
                .then(|| t.friend_id.clone())
        })?;
        Some((peer, tid, offset, TlsStream::Server(tls)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nets() -> Nets {
        Nets {
            v4: vec![
                ("192.168.1.10/24".parse().unwrap(), true),
                ("172.17.0.1/16".parse().unwrap(), false),
            ],
            v6: vec![
                ("fe80::1/64".parse().unwrap(), true),
                ("2001:db8:1:2::10/64".parse().unwrap(), true),
                ("fd7a:115c:a1e0::1/48".parse().unwrap(), false),
            ],
        }
    }

    #[test]
    fn local_network_is_a_physical_interface_one() {
        let nets = nets();
        assert!(nets.near(ip("192.168.1.20")));
        assert!(nets.near(ip("::ffff:192.168.1.20")));
        assert!(nets.near(ip("2001:db8:1:2::30")));
        // A container network, a VPN, the internet.
        assert!(!nets.near(ip("172.17.0.5")));
        assert!(!nets.near(ip("fd7a:115c:a1e0::5")));
        assert!(!nets.near(ip("203.0.113.7")));
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn v4(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn tries_only_the_address_iroh_reaches_on_our_network() {
        let advertised = [v4("10.0.0.5"), v4("192.168.1.20"), v4("172.17.0.9")];
        let direct = [ip("192.168.1.20"), ip("203.0.113.7")];
        assert_eq!(
            candidates(&advertised, &direct, &nets()),
            vec![v4("192.168.1.20")]
        );
        // IPv4-mapped counts too.
        let direct = [ip("::ffff:192.168.1.30")];
        assert_eq!(
            candidates(&advertised, &direct, &nets()),
            vec![v4("192.168.1.30")]
        );
    }

    #[test]
    fn same_numbering_elsewhere_is_not_tried() {
        // Another home also on 192.168.1.x, reached over the internet.
        let advertised = [v4("192.168.1.20")];
        assert!(candidates(&advertised, &[ip("198.51.100.4")], &nets()).is_empty());
        assert!(candidates(&advertised, &[ip("2001:db8:9::4")], &nets()).is_empty());
        assert!(candidates(&advertised, &[], &nets()).is_empty());
    }

    #[test]
    fn next_to_us_over_ipv6_tries_a_few_advertised_physical_first() {
        let mut advertised = vec![v4("172.17.0.9"), v4("10.9.9.9")];
        advertised.extend((20..30).map(|i| Ipv4Addr::new(192, 168, 1, i)));
        let got = candidates(&advertised, &[ip("fe80::2")], &nets());
        assert_eq!(got.len(), MAX_TRIED);
        assert!(got.iter().all(|a| a.octets()[..3] == [192, 168, 1]));
        let got = candidates(&[v4("172.17.0.9")], &[ip("2001:db8:1:2::99")], &nets());
        assert_eq!(got, vec![v4("172.17.0.9")]);
    }
}
