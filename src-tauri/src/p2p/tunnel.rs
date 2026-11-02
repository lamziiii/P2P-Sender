//! Our own relay as a way to iroh's public relays, on networks that block
//! them. With our relay set, iroh reaches every relay through this local
//! proxy (`Endpoint::proxy_url`, HTTP CONNECT on 127.0.0.1). Each relay is
//! reached directly when it answers that way on this network (checked over
//! HTTPS, once per relay and network); otherwise, and when our relay
//! answers, through a WebSocket to its `/tunnel` (see P2P-Sender-relay),
//! which passes the bytes on. iroh's TLS with the relay goes inside: the
//! relay still checks who we are, and ours sees nothing.

use n0_future::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_websockets::{Message, WebSocketStream};

use super::*;

const HEAD_MAX: usize = 8 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long we trust that a relay answers directly on this network, or not.
const DIRECT_KNOWN: Duration = Duration::from_secs(10 * 60);
const BUFFER: usize = 64 * 1024;

trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

pub(super) struct Tunnel {
    relay: CustomRelay,
    /// By relay host: whether it answers directly, and since when we know.
    direct: Mutex<HashMap<String, (bool, Instant)>>,
    /// Tests only: as on a network that blocks iroh's relays.
    block_public: bool,
}

impl Tunnel {
    /// Listening on 127.0.0.1: the proxy URL to give iroh.
    pub(super) async fn start(relay: CustomRelay, rt: &Handle) -> io::Result<(Arc<Tunnel>, url::Url)> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let proxy = url::Url::parse(&format!("http://127.0.0.1:{port}")).map_err(io::Error::other)?;
        let tunnel = Arc::new(Tunnel {
            relay,
            direct: Mutex::new(HashMap::new()),
            block_public: std::env::var_os("P2PSHARE_BLOCK_PUBLIC").is_some(),
        });
        rt.spawn(tunnel.clone().serve(listener));
        Ok((tunnel, proxy))
    }

    /// Another network: what answers directly is to be checked again.
    pub(super) fn network_changed(&self) {
        self.direct.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    async fn serve(self: Arc<Self>, listener: TcpListener) {
        loop {
            match listener.accept().await {
                Ok((client, _)) => {
                    let _ = client.set_nodelay(true);
                    tokio::spawn(self.clone().handle(client));
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    async fn handle(self: Arc<Self>, mut client: TcpStream) {
        let head = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client)).await {
            Ok(Some(head)) => head,
            _ => return,
        };
        let (host, port) = match head {
            Head::Connect { host, port } => (host, port),
            // Plain HTTP to our relay (an http:// relay's probes): passed on
            // as iroh would have sent it.
            Head::Http { host, port, request } if self.is_our_relay(&host, port) => {
                if let Ok(Ok(mut upstream)) =
                    tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port))).await
                {
                    if upstream.write_all(&request).await.is_ok() {
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                    }
                }
                return;
            }
            // Anything else, e.g. iroh's captive portal check on another
            // relay: it just fails, which changes nothing.
            Head::Http { .. } => {
                let _ = client.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n").await;
                return;
            }
        };
        let ours = self.is_our_relay(&host, port);
        if ours || self.direct_ok(&host, port).await {
            if let Ok(Ok(mut upstream)) =
                tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port))).await
            {
                let _ = upstream.set_nodelay(true);
                if established(&mut client).await {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                }
                return;
            }
            if ours {
                let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n").await;
                return;
            }
            self.set_direct(&host, false);
        }
        match self.through_relay(&host, port).await {
            Some(ws) => {
                if established(&mut client).await {
                    pipe(ws, client).await;
                }
            }
            None => {
                let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n").await;
            }
        }
    }

    fn relay_url(&self) -> url::Url {
        url::Url::from(self.relay.url.clone())
    }

    fn is_our_relay(&self, host: &str, port: u16) -> bool {
        let ours = self.relay_url();
        ours.host_str().is_some_and(|h| same_host(h, host)) && ours.port_or_known_default() == Some(port)
    }

    async fn direct_ok(&self, host: &str, port: u16) -> bool {
        if self.block_public {
            return false;
        }
        let known = self
            .direct
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(host)
            .filter(|(_, at)| at.elapsed() < DIRECT_KNOWN)
            .map(|(ok, _)| *ok);
        if let Some(ok) = known {
            return ok;
        }
        let Ok(url) = url::Url::parse(&format!("https://{host}:{port}/")) else {
            return false;
        };
        let ok = relay_answers(&url).await;
        self.set_direct(host, ok);
        ok
    }

    fn set_direct(&self, host: &str, ok: bool) {
        self.direct
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(host.to_string(), (ok, Instant::now()));
    }

    /// A WebSocket to `/tunnel` of our relay, to `host:port`.
    async fn through_relay(&self, host: &str, port: u16) -> Option<WebSocketStream<Box<dyn Io>>> {
        let ours = self.relay_url();
        let relay_host = ours.host_str()?.to_string();
        let relay_port = ours.port_or_known_default()?;
        let connect = async {
            let tcp = TcpStream::connect((relay_host.as_str(), relay_port)).await.ok()?;
            let _ = tcp.set_nodelay(true);
            let stream: Box<dyn Io> = if ours.scheme() == "https" {
                let name = rustls::pki_types::ServerName::try_from(relay_host.clone()).ok()?;
                Box::new(tls_connector().connect(name, tcp).await.ok()?)
            } else {
                Box::new(tcp)
            };
            let scheme = if ours.scheme() == "https" { "wss" } else { "ws" };
            let uri: hyper::Uri = format!("{scheme}://{relay_host}:{relay_port}/tunnel?to={host}:{port}")
                .parse()
                .ok()?;
            let mut builder = tokio_websockets::ClientBuilder::from_uri(uri);
            if let Some(token) = &self.relay.token {
                let value = hyper::header::HeaderValue::from_str(&format!("Bearer {token}")).ok()?;
                builder = builder.add_header(hyper::header::AUTHORIZATION, value).ok()?;
            }
            let (ws, _) = builder.connect_on(stream).await.ok()?;
            Some(ws)
        };
        tokio::time::timeout(CONNECT_TIMEOUT, connect).await.ok().flatten()
    }
}

/// What a client of the proxy asks.
#[derive(Debug, PartialEq)]
enum Head {
    /// `CONNECT host:port`: a tunnel.
    Connect { host: String, port: u16 },
    /// A plain HTTP request to `http://host:port/…`, rewritten for that
    /// server (`GET /path HTTP/1.1`, the same headers).
    Http { host: String, port: u16, request: Vec<u8> },
}

async fn read_head(client: &mut TcpStream) -> Option<Head> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= HEAD_MAX || client.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    parse_head(&head)
}

fn parse_head(head: &[u8]) -> Option<Head> {
    let text = std::str::from_utf8(head).ok()?;
    let (line, rest) = text.split_once("\r\n")?;
    let mut parts = line.split_whitespace();
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = target.rsplit_once(':')?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        return Some(Head::Connect { host: host.to_string(), port: port.parse().ok()? })
            .filter(|h| !matches!(h, Head::Connect { host, .. } if host.is_empty()));
    }
    let url = url::Url::parse(target).ok().filter(|u| u.scheme() == "http")?;
    let mut path = url.path().to_string();
    if let Some(query) = url.query() {
        path = format!("{path}?{query}");
    }
    Some(Head::Http {
        host: url.host_str()?.trim_start_matches('[').trim_end_matches(']').to_string(),
        port: url.port_or_known_default()?,
        request: format!("{method} {path} {version}\r\n{rest}").into_bytes(),
    })
}

async fn established(client: &mut TcpStream) -> bool {
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .is_ok()
}

fn same_host(a: &str, b: &str) -> bool {
    a.trim_end_matches('.').eq_ignore_ascii_case(b.trim_end_matches('.'))
}

fn tls_connector() -> tokio_rustls::TlsConnector {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(config))
}

/// Bytes both ways until one side ends.
async fn pipe<S: AsyncRead + AsyncWrite + Unpin>(mut ws: WebSocketStream<S>, mut tcp: TcpStream) {
    let mut buf = vec![0u8; BUFFER];
    loop {
        tokio::select! {
            read = tcp.read(&mut buf) => match read {
                Ok(n) if n > 0 => {
                    if ws.send(Message::binary(Bytes::copy_from_slice(&buf[..n]))).await.is_err() {
                        return;
                    }
                }
                _ => break,
            },
            msg = ws.next() => match msg {
                Some(Ok(m)) if m.is_binary() => {
                    let data: Bytes = m.into_payload().into();
                    if tcp.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Some(Ok(m)) if m.is_close() => break,
                // Pings are answered by the WebSocket itself.
                Some(Ok(_)) => {}
                _ => break,
            },
        }
    }
    let _ = ws.close().await;
    let _ = tcp.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_client_asks() {
        assert_eq!(
            parse_head(b"CONNECT euc1-1.relay.n0.iroh.link.:443 HTTP/1.1\r\nHost: x\r\n\r\n"),
            Some(Head::Connect { host: "euc1-1.relay.n0.iroh.link.".into(), port: 443 })
        );
        assert_eq!(
            parse_head(b"GET http://127.0.0.1:3340/ping?x=1 HTTP/1.1\r\nHost: 127.0.0.1:3340\r\n\r\n"),
            Some(Head::Http {
                host: "127.0.0.1".into(),
                port: 3340,
                request: b"GET /ping?x=1 HTTP/1.1\r\nHost: 127.0.0.1:3340\r\n\r\n".to_vec(),
            })
        );
        assert_eq!(parse_head(b"GET /relative HTTP/1.1\r\n\r\n"), None);
        assert_eq!(parse_head(b"CONNECT :443 HTTP/1.1\r\n\r\n"), None);
        assert!(same_host("euc1-1.relay.n0.iroh.link.", "EUC1-1.relay.n0.iroh.link"));
    }
}
