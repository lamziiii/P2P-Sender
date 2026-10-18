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
//! The receiver only takes a stream whose client certificate is the one the
//! transfer's peer advertised. When the connection fails, the sender goes
//! back to QUIC for that peer for a while; the receiver asks to resume from
//! what it has, as for any interrupted stream.

use std::net::{Ipv4Addr, SocketAddr};

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_rustls::{TlsAcceptor, TlsStream};

use super::*;
use crate::localsend::{self, Identity};

const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
const TLS_TIMEOUT: Duration = Duration::from_secs(5);
/// A write or read that makes no progress for that long ends the stream.
pub(super) const IO_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a peer whose connection failed is reached over QUIC only.
const DOWN_FOR: Duration = Duration::from_secs(2 * 60);
const ADDRESS_CHECK: Duration = Duration::from_secs(10);
const MAX_ADDRESSES: usize = 16;
const MAX_CONNECTIONS: usize = 32;
const READ_CHUNK: usize = 256 * 1024;

pub(super) type Stream = TlsStream<TcpStream>;

pub(super) struct Lan {
    identity: Identity,
    acceptor: TlsAcceptor,
    port: u16,
}

/// What a peer advertised.
#[derive(Clone, PartialEq)]
pub(super) struct LanPeer {
    fingerprint: String,
    port: u16,
    ips: Vec<Ipv4Addr>,
}

/// Where to reach a peer for one stream.
pub(super) struct LanTarget {
    peer: LanPeer,
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

/// Our private addresses, which peers on the same network can reach.
fn private_ipv4s() -> Vec<Ipv4Addr> {
    let mut ips: Vec<Ipv4Addr> = localsend::local_ipv4s()
        .into_iter()
        .filter(Ipv4Addr::is_private)
        .collect();
    ips.sort();
    ips.truncate(MAX_ADDRESSES);
    ips
}

/// Data received over a QUIC stream or a LAN connection.
pub(super) enum Source {
    Quic(RecvStream),
    Lan(Box<Stream>),
}

impl Source {
    /// The next bytes, `None` at the end of the stream.
    pub(super) async fn chunk(&mut self) -> io::Result<Option<Bytes>> {
        match self {
            Source::Quic(recv) => recv
                .read_chunk(CHUNK_SIZE as usize)
                .await
                .map_err(io::Error::other),
            Source::Lan(stream) => {
                let mut buf = bytes::BytesMut::with_capacity(READ_CHUNK);
                match tokio::time::timeout(IO_TIMEOUT, stream.read_buf(&mut buf)).await {
                    Err(_) => Err(io::ErrorKind::TimedOut.into()),
                    Ok(Err(e)) => Err(e),
                    Ok(Ok(0)) => Ok(None),
                    Ok(Ok(_)) => Ok(Some(buf.freeze())),
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
                matches!(tokio::time::timeout(IO_TIMEOUT, write).await, Ok(Ok(())))
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
        if self
            .lan_down
            .get(peer)
            .is_some_and(|t| t.elapsed() < DOWN_FOR)
        {
            return None;
        }
        self.lan_down.remove(peer);
        let peer = self.lan_peers.get(peer)?.clone();
        Some(LanTarget { peer })
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

    /// Keep peers told of our addresses as networks come and go.
    async fn lan_addresses(self) {
        let Some(lan) = &self.0.lan else { return };
        let (fingerprint, port) = (lan.identity.fingerprint().to_string(), lan.port);
        let mut check = tokio::time::interval(ADDRESS_CHECK);
        loop {
            check.tick().await;
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
                };
                // Somewhere else now: worth trying again.
                if s.lan_peers.get(peer) != Some(&new) {
                    s.lan_down.remove(peer);
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
        let peer = target.peer;
        let connector = localsend::tls_connector(&lan.identity, Some(&peer.fingerprint)).ok()?;
        // Only addresses of our own networks: the peer is next to us.
        let nets = tokio::task::spawn_blocking(localsend::local_nets)
            .await
            .ok()?;
        let candidates: Vec<Ipv4Addr> = peer
            .ips
            .iter()
            .copied()
            .filter(|ip| nets.iter().any(|n| n.contains(ip)))
            .collect();
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
                let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
                    .await
                    .ok()?
                    .ok()?;
                let _ = tcp.set_nodelay(true);
                let name = rustls::pki_types::ServerName::IpAddress(addr.ip().into());
                let tls = tokio::time::timeout(TLS_TIMEOUT, connector.connect(name, tcp))
                    .await
                    .ok()?
                    .ok()?;
                Some(TlsStream::Client(tls))
            });
        }
        while let Some(result) = attempts.join_next().await {
            if let Ok(Some(stream)) = result {
                return Some(stream);
            }
        }
        None
    }

    /// The connection to `peer` failed: QUIC only for a while.
    pub(super) fn lan_failed(&self, s: &mut State, peer: &str) {
        s.lan_down.insert(peer.to_string(), Instant::now());
    }

    async fn lan_accept_loop(self, listener: TcpListener) {
        let Some(lan) = &self.0.lan else { return };
        let acceptor = lan.acceptor.clone();
        let mut conns = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((tcp, _)) = accepted else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    };
                    if self.0.endpoint.is_closed() {
                        return;
                    }
                    if conns.len() >= MAX_CONNECTIONS {
                        continue;
                    }
                    conns.spawn(self.clone().lan_connection(tcp, acceptor.clone()));
                }
                Some(_) = conns.join_next(), if !conns.is_empty() => {}
            }
        }
    }

    async fn lan_connection(self, tcp: TcpStream, acceptor: TlsAcceptor) {
        let _ = tcp.set_nodelay(true);
        let Ok(Ok(mut tls)) = tokio::time::timeout(TLS_TIMEOUT, acceptor.accept(tcp)).await else {
            return;
        };
        let Some(cert) = tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|c| c.first())
            .map(|c| localsend::fingerprint(c))
        else {
            return;
        };
        let mut header = [0u8; DATA_HEADER];
        let read = tokio::time::timeout(TLS_TIMEOUT, tls.read_exact(&mut header)).await;
        if !matches!(read, Ok(Ok(_))) {
            return;
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
        });
        let Some(peer) = peer else { return };
        let source = Source::Lan(Box::new(TlsStream::Server(tls)));
        self.receive_data(peer, tid, offset, source).await;
    }
}
