//! LocalSend compatibility (protocol v2.2, github.com/localsend/protocol):
//! devices running the LocalSend app on the same network see this computer
//! and can send it files, which go through the usual consent prompt and
//! transfer list.
//!
//!   discovery: JSON announcements on UDP multicast 224.0.0.167:53317. We
//!              announce ourselves, and answer the announcements of others
//!              with an HTTPS `register` (or a UDP announcement if that fails).
//!   transfer:  HTTPS server with a self-signed certificate whose SHA-256 is
//!              our fingerprint. `prepare-upload` lists the files and waits
//!              for the user's answer, then the sender makes one `upload`
//!              per accepted file. One session at a time. A text message
//!              comes as a single text file whose content is its preview:
//!              it goes to the device's chat, and nothing is uploaded.
//!   sending:   the same protocol the other way (see `send.rs`), to the
//!              devices discovered: files, or text for the chat.
//!
//! Peers present their own certificate (mutual TLS): any certificate is
//! accepted, it only tells devices apart. Older LocalSend versions present
//! none, so it is optional. When we send, the device's certificate must
//! match the fingerprint it announced.

mod send;

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{CONTENT_TYPE, HOST};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, Notify};
use tokio::task::{AbortHandle, JoinSet};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::p2p::{ExternalFile, Node, OutgoingFile, Status};
use crate::store::write_atomic;

pub const PORT: u16 = 53317;
const MULTICAST: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
const VERSION: &str = "2.2";
const API: &str = "/api/localsend/v2";

/// `prepare-upload` may carry previews (the text of a shared message).
const MAX_JSON: usize = 1 << 20;
const MAX_FILES: usize = 1000;
const MAX_ALIAS: usize = 40;
const MAX_TEXT: usize = 100_000;
/// How long a sender waits for the user's answer before we give up.
const DECISION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Requests that reach the user (files, messages) per device and minute.
const PROMPTS_PER_MINUTE: usize = 10;
/// Devices kept in the list.
const MAX_DEVICES: usize = 256;
/// Announcements processed per source address and `INTERFACE_CHECK`.
const ANNOUNCES_PER_IP: usize = 20;
/// Registrations with announcing devices at the same time.
const MAX_REGISTERS: usize = 8;
/// Open HTTPS connections, in all and per device.
const MAX_CONNECTIONS: usize = 64;
const MAX_CONNECTIONS_PER_IP: usize = 8;
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
/// The list of devices is sent to the window at most that often.
const DEVICES_REFRESH: Duration = Duration::from_secs(1);
/// A session without any progress for that long is abandoned.
const IDLE_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const SESSION_CHECK: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const REGISTER_TIMEOUT: Duration = Duration::from_secs(5);
/// Peers repeat their announcement a few times: answer each device once.
const REGISTER_INTERVAL: Duration = Duration::from_secs(5);
const INTERFACE_CHECK: Duration = Duration::from_secs(10);
/// Devices not heard from for that long are no longer offered.
const DEVICE_TTL: Duration = Duration::from_secs(30 * 60);
/// An announcement is repeated, as datagrams get lost and devices that just
/// joined the network may not be listening yet.
const ANNOUNCE_DELAYS: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(500),
    Duration::from_millis(2000),
];
/// The port may still be held for a moment by a service just stopped.
const BIND_RETRIES: u32 = 10;
const BIND_RETRY_DELAY: Duration = Duration::from_millis(200);

// ─── Identity ────────────────────────────────────────────────────────────────

/// Our TLS certificate, kept across restarts so that devices recognize us.
#[derive(Clone)]
pub struct Identity {
    cert: CertificateDer<'static>,
    /// PKCS#8 DER.
    key: Vec<u8>,
    fingerprint: String,
}

#[derive(Serialize, Deserialize)]
struct StoredIdentity {
    cert: String,
    key: String,
}

impl Identity {
    pub fn load_or_create(path: &Path) -> io::Result<Identity> {
        let bytes = match std::fs::read(path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            // Locked or unreadable for now: do not replace it with a new one,
            // devices would no longer recognize us.
            Err(e) => return Err(e),
        };
        let stored = bytes.as_ref().and_then(|b| {
            let s = serde_json::from_slice::<StoredIdentity>(b).ok()?;
            Some((hex::decode(s.cert).ok()?, hex::decode(s.key).ok()?))
        });
        if bytes.is_some() && stored.is_none() {
            eprintln!("localsend: identité illisible, conservée en .bad et remplacée");
            let _ = std::fs::rename(path, path.with_extension("json.bad"));
        }
        let (cert, key) = match stored {
            Some(pair) => pair,
            None => {
                let key = rcgen::KeyPair::generate().map_err(io::Error::other)?;
                let mut params = rcgen::CertificateParams::default();
                params.distinguished_name = rcgen::DistinguishedName::new();
                params
                    .distinguished_name
                    .push(rcgen::DnType::CommonName, "LocalSend User");
                let cert = params.self_signed(&key).map_err(io::Error::other)?;
                let (cert, key) = (cert.der().to_vec(), key.serialize_der());
                let stored = StoredIdentity {
                    cert: hex::encode(&cert),
                    key: hex::encode(&key),
                };
                write_atomic(path, &serde_json::to_vec(&stored)?)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
                }
                (cert, key)
            }
        };
        Ok(Identity {
            fingerprint: fingerprint(&cert),
            cert: CertificateDer::from(cert),
            key,
        })
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    fn key(&self) -> PrivateKeyDer<'static> {
        PrivatePkcs8KeyDer::from(self.key.clone()).into()
    }
}

/// LocalSend's device fingerprint: SHA-256 of the DER certificate, uppercase hex.
pub(crate) fn fingerprint(cert: &[u8]) -> String {
    hex::encode_upper(ring::digest::digest(&ring::digest::SHA256, cert))
}

// ─── TLS ─────────────────────────────────────────────────────────────────────

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Accepts any certificate, self-signed as they all are, as long as the peer
/// proves it holds its key during the handshake. As a client, the server may
/// also have to match the fingerprint we expect.
#[derive(Debug)]
struct PeerCert {
    provider: Arc<CryptoProvider>,
    pin: Option<String>,
}

impl PeerCert {
    fn tls12(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn tls13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ServerCertVerifier for PeerCert {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match &self.pin {
            Some(pin) if !fingerprint(cert).eq_ignore_ascii_case(pin) => {
                Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::ApplicationVerificationFailure,
                ))
            }
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls12(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

impl ClientCertVerifier for PeerCert {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        false
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls12(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

pub(crate) fn tls_acceptor(id: &Identity) -> Result<TlsAcceptor, rustls::Error> {
    let verifier = Arc::new(PeerCert {
        provider: provider(),
        pin: None,
    });
    let mut server = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![id.cert.clone()], id.key())?;
    server.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(server)))
}

/// `pin`: the fingerprint the server must have, if we know it.
pub(crate) fn tls_connector(id: &Identity, pin: Option<&str>) -> Result<TlsConnector, rustls::Error> {
    let verifier = Arc::new(PeerCert {
        provider: provider(),
        pin: pin.map(String::from),
    });
    let mut client = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![id.cert.clone()], id.key())?;
    client.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsConnector::from(Arc::new(client)))
}

// ─── Protocol messages ───────────────────────────────────────────────────────

/// What a device says about itself (announcement, register, prepare-upload).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceInfo {
    #[serde(default)]
    alias: String,
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    device_model: Option<String>,
    #[serde(default)]
    device_type: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    announce: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileInfo {
    file_name: String,
    size: u64,
    #[serde(default)]
    file_type: String,
    #[serde(default)]
    preview: Option<String>,
}

#[derive(Deserialize)]
struct PrepareUpload {
    info: DeviceInfo,
    files: HashMap<String, FileInfo>,
}

fn device_model() -> &'static str {
    if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    }
}

/// The text of a message, which LocalSend sends as a lone text file.
fn message_text(request: &PrepareUpload) -> Option<String> {
    let mut files = request.files.values();
    let (file, None) = (files.next()?, files.next()) else {
        return None;
    };
    let text = file
        .preview
        .as_ref()
        .filter(|_| file.file_type.starts_with("text/"))?;
    Some(text.chars().take(MAX_TEXT).collect())
}

fn clean_alias(alias: &str) -> String {
    let alias: String = crate::p2p::strip_invisible(alias)
        .chars()
        .take(MAX_ALIAS)
        .collect();
    match alias.trim() {
        "" => "LocalSend".into(),
        a => a.to_string(),
    }
}

// ─── Service ─────────────────────────────────────────────────────────────────

/// Who sent a text message.
pub struct TextSender {
    /// The device's fingerprint, as in the list of devices.
    pub fingerprint: String,
    pub alias: String,
    /// Its certificate proved the fingerprint (older versions only claim it).
    pub verified: bool,
}

/// How the service tells the app what happens.
pub struct Hooks {
    /// A text message arrived.
    pub on_text: Arc<dyn Fn(TextSender, String) + Send + Sync>,
    /// The list of devices changed.
    pub on_devices: Arc<dyn Fn() + Send + Sync>,
}

struct Inner {
    node: Node,
    hooks: Hooks,
    identity: Identity,
    fingerprint: String,
    alias: Mutex<String>,
    port: u16,
    rt: Handle,
    /// Unpinned, for discovery.
    connector: TlsConnector,
    session: Mutex<Option<Session>>,
    /// Devices that made themselves known, by fingerprint, with when each
    /// was last heard from.
    devices: Mutex<HashMap<String, (Device, Instant)>>,
    /// Asks the discovery task to announce us again.
    scan: mpsc::UnboundedSender<()>,
    /// Sending sessions in progress.
    sends: Mutex<Vec<AbortHandle>>,
    /// When each address last asked for the user's attention.
    prompts: Mutex<HashMap<IpAddr, Vec<Instant>>>,
    /// Open HTTPS connections per address.
    connections: Mutex<HashMap<IpAddr, usize>>,
    /// The list of devices changed since the window last got it.
    devices_dirty: std::sync::atomic::AtomicBool,
    devices_sent: Mutex<Instant>,
}

/// A LocalSend device on the network.
#[derive(Clone, PartialEq)]
struct Device {
    fingerprint: String,
    alias: String,
    model: Option<String>,
    kind: Option<String>,
    ip: IpAddr,
    port: u16,
    https: bool,
    /// Its certificate proved the fingerprint (otherwise it is only claimed).
    verified: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub fingerprint: String,
    pub alias: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,
    /// mobile, desktop, web, headless or server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_type: Option<String>,
    /// Files go over HTTPS to the device whose certificate matches; without
    /// it (older devices), anyone on the network could read them.
    pub secure: bool,
}

/// The upload session in progress (LocalSend allows one at a time).
struct Session {
    id: String,
    ip: IpAddr,
    /// Set until the user answered, for the sender to call the request off.
    pending: Option<Arc<Notify>>,
    /// Accepted files, by LocalSend file ID.
    files: HashMap<String, SessionFile>,
    /// Every transfer of the session, accepted or not.
    tids: Vec<String>,
}

struct SessionFile {
    token: String,
    tid: String,
    started: bool,
}

/// The running service; dropping it stops it, and the transfers it carries.
pub struct LocalSend {
    inner: Arc<Inner>,
    tasks: Vec<AbortHandle>,
    /// Why discovery is unavailable, if it is.
    discovery_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusView {
    pub port: u16,
    pub alias: String,
    pub fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl LocalSend {
    pub async fn start(
        node: Node,
        id: &Identity,
        alias: &str,
        hooks: Hooks,
    ) -> io::Result<LocalSend> {
        let acceptor = tls_acceptor(id).map_err(io::Error::other)?;
        let connector = tls_connector(id, None).map_err(io::Error::other)?;
        let listener = bind_listener().await?;
        let port = listener.local_addr()?.port();
        let (scan, scans) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            node,
            hooks,
            identity: id.clone(),
            fingerprint: id.fingerprint.clone(),
            alias: Mutex::new(clean_alias(alias)),
            port,
            rt: Handle::current(),
            connector,
            session: Mutex::new(None),
            devices: Mutex::new(HashMap::new()),
            scan,
            sends: Mutex::new(Vec::new()),
            prompts: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            devices_dirty: std::sync::atomic::AtomicBool::new(false),
            devices_sent: Mutex::new(Instant::now() - DEVICES_REFRESH),
        });
        let mut tasks = vec![tokio::spawn(inner.clone().serve(listener, acceptor)).abort_handle()];
        let discovery_error = match multicast_socket() {
            Ok(socket) => {
                tasks.push(tokio::spawn(inner.clone().discovery(socket, scans)).abort_handle());
                None
            }
            Err(e) => Some(format!("Découverte automatique indisponible : {e}")),
        };
        Ok(LocalSend {
            inner,
            tasks,
            discovery_error,
        })
    }

    pub fn status(&self) -> StatusView {
        StatusView {
            port: self.inner.port,
            alias: self.inner.alias().clone(),
            fingerprint: self.inner.fingerprint.clone(),
            error: self.discovery_error.clone(),
        }
    }

    pub fn set_alias(&self, alias: &str) {
        *self.inner.alias() = clean_alias(alias);
    }

    /// The devices we can send to, by name.
    pub fn devices(&self) -> Vec<DeviceView> {
        let mut list: Vec<DeviceView> = self
            .inner
            .devices()
            .values()
            .filter(|(_, seen)| seen.elapsed() < DEVICE_TTL)
            .map(|(d, _)| DeviceView {
                fingerprint: d.fingerprint.clone(),
                alias: d.alias.clone(),
                device_model: d.model.clone(),
                device_type: d.kind.clone(),
                secure: d.https,
            })
            .collect();
        list.sort_by_key(|d| d.alias.to_lowercase());
        list
    }

    /// Forget the devices and ask those still around to answer again.
    pub fn scan(&self) {
        self.inner.devices().clear();
        (self.inner.hooks.on_devices)();
        let _ = self.inner.scan.send(());
    }

    /// Send a text message to a device; `done` gets the outcome.
    pub fn send_text(
        &self,
        fingerprint: &str,
        text: String,
        done: impl FnOnce(Result<(), String>) + Send + 'static,
    ) -> Result<(), &'static str> {
        let device = self
            .inner
            .devices()
            .get(fingerprint)
            .map(|(d, _)| d.clone())
            .ok_or("Appareil introuvable : est-il allumé, avec LocalSend ouvert ?")?;
        let inner = self.inner.clone();
        let task = self.inner.rt.spawn(async move {
            done(inner.send_text(device, text).await);
        });
        let mut sends = self.inner.sends.lock().unwrap_or_else(|e| e.into_inner());
        sends.retain(|t| !t.is_finished());
        sends.push(task.abort_handle());
        Ok(())
    }

    /// Send files to a device; returns how many will be offered to it.
    pub fn send_files(&self, fingerprint: &str, paths: &[PathBuf]) -> Result<usize, &'static str> {
        let device = self
            .inner
            .devices()
            .get(fingerprint)
            .map(|(d, _)| d.clone())
            .ok_or("Appareil introuvable : relancez la recherche")?;
        let files: Vec<(PathBuf, u64)> = paths
            .iter()
            .filter_map(|p| {
                std::fs::metadata(p)
                    .ok()
                    .filter(|m| m.is_file())
                    .map(|m| (p.clone(), m.len()))
            })
            .collect();
        if files.is_empty() {
            return Ok(0);
        }
        let peer_id = format!("localsend:{}", device.fingerprint);
        let outgoing = self
            .inner
            .node
            .external_send(&peer_id, &device.alias, &files);
        let task = self
            .inner
            .rt
            .spawn(self.inner.clone().send_session(device, outgoing));
        let mut sends = self.inner.sends.lock().unwrap_or_else(|e| e.into_inner());
        sends.retain(|t| !t.is_finished());
        sends.push(task.abort_handle());
        Ok(files.len())
    }
}

impl Drop for LocalSend {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        // Their guards stop the transfers they carry.
        let sends =
            std::mem::take(&mut *self.inner.sends.lock().unwrap_or_else(|e| e.into_inner()));
        for task in sends {
            task.abort();
        }
        if let Some(session) = self.inner.session().take() {
            for tid in &session.tids {
                self.inner
                    .node
                    .external_stop(tid, Status::Cancelled, Some("LocalSend désactivé"));
            }
        }
    }
}

async fn bind_listener() -> io::Result<TcpListener> {
    let mut attempt = 0;
    loop {
        match TcpListener::bind((Ipv4Addr::UNSPECIFIED, PORT)).await {
            Ok(listener) => return Ok(listener),
            Err(_) if attempt < BIND_RETRIES => {
                attempt += 1;
                tokio::time::sleep(BIND_RETRY_DELAY).await;
            }
            // Taken (by the LocalSend app itself?): devices learn the port
            // we really use from our announcements.
            Err(_) => return TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await,
        }
    }
}

fn multicast_socket() -> io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Shares the port with a LocalSend app running on this computer.
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    socket.set_multicast_loop_v4(true)?;
    UdpSocket::from_std(socket.into())
}

/// Our addresses on the networks that can reach LocalSend devices.
pub(crate) fn local_ipv4s() -> HashSet<Ipv4Addr> {
    netdev::get_interfaces()
        .into_iter()
        .filter(|i| i.is_up() && !i.is_loopback())
        .flat_map(|i| i.ipv4_addrs())
        .filter(|ip| !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified())
        .collect()
}

/// The networks our interfaces are on.
pub(crate) fn local_nets() -> Vec<netdev::ipnet::Ipv4Net> {
    netdev::get_interfaces()
        .into_iter()
        .filter(|i| i.is_up())
        .flat_map(|i| i.ipv4)
        .collect()
}

/// Whether a device at `ip` is on the local network: one of ours, or a
/// private address.
fn on_local_network(ip: IpAddr, nets: &[netdev::ipnet::Ipv4Net]) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_link_local()
                || v4.is_loopback()
                || nets.iter().any(|n| n.contains(&v4))
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments()[0];
            v6.is_loopback() || (seg & 0xfe00) == 0xfc00 || (seg & 0xffc0) == 0xfe80
        }
    }
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn reply(status: StatusCode) -> Response<Full<Bytes>> {
    let mut res = Response::new(Full::default());
    *res.status_mut() = status;
    res
}

fn reply_json(body: &Value) -> Response<Full<Bytes>> {
    let mut res = Response::new(Full::new(Bytes::from(body.to_string())));
    res.headers_mut()
        .insert(CONTENT_TYPE, "application/json".parse().unwrap());
    res
}

async fn read_json<T: serde::de::DeserializeOwned>(body: Incoming) -> Option<T> {
    let bytes = Limited::new(body, MAX_JSON)
        .collect()
        .await
        .ok()?
        .to_bytes();
    serde_json::from_slice(&bytes).ok()
}

/// Compare secrets in a time that does not depend on where they differ.
fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn query(req: &Request<Incoming>) -> HashMap<String, String> {
    url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}

impl Inner {
    fn alias(&self) -> MutexGuard<'_, String> {
        self.alias.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn session(&self) -> MutexGuard<'_, Option<Session>> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn devices(&self) -> MutexGuard<'_, HashMap<String, (Device, Instant)>> {
        self.devices.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Note a device that made itself known: `info` is what it says about
    /// itself, `cert` the fingerprint its certificate proves, if we saw it.
    fn remember(
        &self,
        info: &DeviceInfo,
        cert: Option<String>,
        ip: IpAddr,
        port: u16,
        https: bool,
    ) {
        let verified = cert.is_some();
        let fingerprint = match cert {
            // A claim the certificate contradicts is not trusted.
            Some(cert) if !info.fingerprint.eq_ignore_ascii_case(&cert) => return,
            Some(cert) => cert,
            None if https => info.fingerprint.to_ascii_uppercase(),
            None => info.fingerprint.clone(),
        };
        if fingerprint.is_empty()
            || fingerprint.len() > 128
            || fingerprint.eq_ignore_ascii_case(&self.fingerprint)
        {
            return;
        }
        let device = Device {
            fingerprint: fingerprint.clone(),
            alias: clean_alias(&info.alias),
            model: info.device_model.as_deref().map(clean_alias),
            kind: info
                .device_type
                .as_deref()
                .map(|k| clean_alias(k).to_lowercase()),
            ip,
            port,
            https,
            verified,
        };
        let changed = {
            let mut devices = self.devices();
            let known = devices.get(&fingerprint).map(|(d, _)| d.clone());
            if let Some(known) = &known {
                // Anyone can claim a fingerprint in an announcement: it may
                // not move a device proven by its certificate elsewhere, nor
                // turn an HTTPS device into a plain HTTP one.
                let same_place =
                    known.ip == ip && known.port == port && known.https == https;
                let hijack = (known.verified && !verified && !same_place)
                    || (known.https && !https && !verified);
                if hijack {
                    return;
                }
                if known.verified && !verified {
                    devices.insert(fingerprint, (known.clone(), Instant::now()));
                    return;
                }
            } else if devices.len() >= MAX_DEVICES {
                let oldest = devices
                    .iter()
                    .min_by_key(|(_, (_, seen))| *seen)
                    .map(|(k, _)| k.clone());
                if let Some(oldest) = oldest {
                    devices.remove(&oldest);
                }
            }
            devices.insert(fingerprint, (device.clone(), Instant::now()));
            known != Some(device)
        };
        if changed {
            self.devices_changed();
        }
    }

    /// Tell the window, at most every `DEVICES_REFRESH` (the discovery task
    /// sends what is left over).
    fn devices_changed(&self) {
        use std::sync::atomic::Ordering;
        let mut sent = self.devices_sent.lock().unwrap_or_else(|e| e.into_inner());
        if sent.elapsed() >= DEVICES_REFRESH {
            *sent = Instant::now();
            self.devices_dirty.store(false, Ordering::Relaxed);
            drop(sent);
            (self.hooks.on_devices)();
        } else {
            self.devices_dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Whether `ip` may ask the user something again (files or a message).
    fn may_prompt(&self, ip: IpAddr) -> bool {
        let mut prompts = self.prompts.lock().unwrap_or_else(|e| e.into_inner());
        prompts.retain(|_, list| {
            list.retain(|t| t.elapsed() < Duration::from_secs(60));
            !list.is_empty()
        });
        let list = prompts.entry(ip).or_default();
        if list.len() >= PROMPTS_PER_MINUTE {
            return false;
        }
        list.push(Instant::now());
        true
    }

    /// What `info` and `register` answer.
    fn info(&self) -> Value {
        json!({
            "alias": *self.alias(),
            "version": VERSION,
            "deviceModel": device_model(),
            "deviceType": "desktop",
            "fingerprint": self.fingerprint,
            "download": false,
        })
    }

    /// What we tell other devices in announcements and registrations.
    fn advertisement(&self, announce: Option<bool>) -> Value {
        let mut info = self.info();
        info["port"] = json!(self.port);
        info["protocol"] = json!("https");
        if let Some(announce) = announce {
            info["announce"] = json!(announce);
        }
        info
    }

    // ─── Discovery ───────────────────────────────────────────────────────────

    async fn discovery(self: Arc<Self>, socket: UdpSocket, mut scans: mpsc::UnboundedReceiver<()>) {
        let mut joined = HashSet::new();
        let mut check = tokio::time::interval(INTERFACE_CHECK);
        let mut burst: Vec<Instant> = Vec::new();
        let mut answered: HashMap<IpAddr, Instant> = HashMap::new();
        let mut heard: HashMap<IpAddr, usize> = HashMap::new();
        let mut nets = Vec::new();
        let mut registers = JoinSet::new();
        let mut refresh = tokio::time::interval(DEVICES_REFRESH);
        let mut buf = vec![0u8; 65536];
        loop {
            let next = burst.first().copied();
            tokio::select! {
                _ = refresh.tick() => {
                    if self.devices_dirty.load(std::sync::atomic::Ordering::Relaxed) {
                        self.devices_changed();
                    }
                }
                _ = check.tick() => {
                    heard.clear();
                    nets = local_nets();
                    self.devices().retain(|_, (_, seen)| seen.elapsed() < DEVICE_TTL);
                    let ips = local_ipv4s();
                    let new: Vec<_> = ips.difference(&joined).copied().collect();
                    for ip in &new {
                        let _ = socket.join_multicast_v4(MULTICAST, *ip);
                    }
                    joined = ips;
                    if !new.is_empty() {
                        let now = Instant::now();
                        burst = ANNOUNCE_DELAYS.iter().map(|d| now + *d).collect();
                    }
                    answered.retain(|_, t| t.elapsed() < REGISTER_INTERVAL);
                }
                Some(()) = scans.recv() => {
                    let now = Instant::now();
                    burst = ANNOUNCE_DELAYS.iter().map(|d| now + *d).collect();
                    answered.clear();
                }
                _ = tokio::time::sleep_until(next.unwrap_or_else(Instant::now).into()), if next.is_some() => {
                    burst.remove(0);
                    self.announce(&socket, &joined, true).await;
                }
                Some(answered_by_http) = registers.join_next(), if !registers.is_empty() => {
                    // Older versions only listen for answers over UDP.
                    if !matches!(answered_by_http, Ok(true)) {
                        self.announce(&socket, &joined, false).await;
                    }
                }
                received = socket.recv_from(&mut buf) => {
                    // Errors are transient (on Windows, the ICMP error of a
                    // previous send shows up on the next receive).
                    let Ok((len, from)) = received else { continue };
                    let ip = canonical(from.ip());
                    // Only devices of the local network, each a few times.
                    let count = heard.entry(ip).or_default();
                    *count += 1;
                    if !on_local_network(ip, &nets) || *count > ANNOUNCES_PER_IP {
                        continue;
                    }
                    let Ok(peer) = serde_json::from_slice::<DeviceInfo>(&buf[..len]) else {
                        continue;
                    };
                    if peer.fingerprint == self.fingerprint {
                        continue;
                    }
                    let port = peer.port.unwrap_or(PORT);
                    let https = peer.protocol.as_deref() != Some("http");
                    self.remember(&peer, None, ip, port, https);
                    if peer.announce != Some(true) {
                        continue;
                    }
                    // We answer with a request to the address and port it
                    // gives: never to a system port, never many at once.
                    if port < 1024 || registers.len() >= MAX_REGISTERS {
                        continue;
                    }
                    if answered.get(&ip).is_some_and(|t| t.elapsed() < REGISTER_INTERVAL) {
                        continue;
                    }
                    answered.insert(ip, Instant::now());
                    let inner = self.clone();
                    registers.spawn(async move { inner.register_with(ip, port, https).await });
                }
            }
        }
    }

    /// Send our announcement on every network.
    async fn announce(&self, socket: &UdpSocket, ips: &HashSet<Ipv4Addr>, announce: bool) {
        let payload = self.advertisement(Some(announce)).to_string();
        for ip in ips {
            if socket2::SockRef::from(socket)
                .set_multicast_if_v4(ip)
                .is_ok()
            {
                let _ = socket.send_to(payload.as_bytes(), (MULTICAST, PORT)).await;
            }
        }
    }

    /// Introduce ourselves to a device that announced itself.
    async fn register_with(&self, ip: IpAddr, port: u16, https: bool) -> bool {
        let peer = send::Peer {
            ip,
            port,
            https,
            pin: None,
        };
        let body = send::json_body(&self.advertisement(None));
        let path = format!("{API}/register");
        let request = self.request(&peer, &path, "application/json", body);
        let Ok(Ok((status, reply, cert))) = tokio::time::timeout(REGISTER_TIMEOUT, request).await
        else {
            return false;
        };
        if let Ok(info) = serde_json::from_slice::<DeviceInfo>(&reply) {
            self.remember(&info, cert, ip, port, https);
        }
        status.is_success()
    }

    // ─── HTTPS server ────────────────────────────────────────────────────────

    async fn serve(self: Arc<Self>, listener: TcpListener, acceptor: TlsAcceptor) {
        // Owned here so that stopping the service closes every connection.
        let mut conns = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((tcp, addr)) = accepted else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    };
                    let Some(slot) = self.clone().connection_slot(canonical(addr.ip()), conns.len())
                    else {
                        continue;
                    };
                    conns.spawn(self.clone().connection(tcp, addr, acceptor.clone(), slot));
                }
                Some(_) = conns.join_next(), if !conns.is_empty() => {}
            }
        }
    }

    /// Room for one more connection from `ip`, held until the guard drops.
    fn connection_slot(self: Arc<Self>, ip: IpAddr, open: usize) -> Option<ConnectionSlot> {
        if open >= MAX_CONNECTIONS {
            return None;
        }
        let mut connections = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        let count = connections.entry(ip).or_default();
        if *count >= MAX_CONNECTIONS_PER_IP {
            return None;
        }
        *count += 1;
        drop(connections);
        Some(ConnectionSlot { inner: self, ip })
    }

    async fn connection(
        self: Arc<Self>,
        tcp: TcpStream,
        addr: SocketAddr,
        acceptor: TlsAcceptor,
        _slot: ConnectionSlot,
    ) {
        let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await
        else {
            return;
        };
        let cert = tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|c| c.first())
            .map(|c| fingerprint(c));
        let ip = canonical(addr.ip());
        let service = hyper::service::service_fn(move |req| {
            let (inner, cert) = (self.clone(), cert.clone());
            async move { Ok::<_, Infallible>(inner.handle(req, ip, cert).await) }
        });
        // A client that stops sending its request does not hold the
        // connection forever.
        let _ = hyper::server::conn::http1::Builder::new()
            .timer(hyper_util::rt::TokioTimer::new())
            .header_read_timeout(HEADER_TIMEOUT)
            .serve_connection(TokioIo::new(tls), service)
            .await;
    }

    async fn handle(
        self: Arc<Self>,
        req: Request<Incoming>,
        ip: IpAddr,
        cert: Option<String>,
    ) -> Response<Full<Bytes>> {
        let Some(route) = req.uri().path().strip_prefix(API) else {
            return reply(StatusCode::NOT_FOUND);
        };
        match (req.method(), route) {
            (&Method::GET, "/info") => reply_json(&self.info()),
            (&Method::POST, "/register") => {
                if let Some(info) = read_json::<DeviceInfo>(req.into_body()).await {
                    let https = info.protocol.as_deref() != Some("http");
                    let port = info.port.unwrap_or(PORT);
                    self.remember(&info, cert, ip, port, https);
                }
                reply_json(&self.info())
            }
            // We never ask for a PIN: the user accepts each file instead.
            (&Method::POST, "/prepare-upload") => {
                match read_json::<PrepareUpload>(req.into_body()).await {
                    Some(request) => self.prepare_upload(request, ip, cert).await,
                    None => reply(StatusCode::BAD_REQUEST),
                }
            }
            (&Method::POST, "/upload") => {
                let q = query(&req);
                match (q.get("sessionId"), q.get("fileId"), q.get("token")) {
                    (Some(sid), Some(fid), Some(token)) => {
                        let (sid, fid, token) = (sid.clone(), fid.clone(), token.clone());
                        self.upload(&sid, &fid, &token, ip, req.into_body()).await
                    }
                    _ => reply(StatusCode::BAD_REQUEST),
                }
            }
            (&Method::POST, "/cancel") => {
                self.cancel(query(&req).get("sessionId").map(String::as_str), ip);
                reply(StatusCode::OK)
            }
            _ => reply(StatusCode::NOT_FOUND),
        }
    }

    async fn prepare_upload(
        self: &Arc<Self>,
        request: PrepareUpload,
        ip: IpAddr,
        cert: Option<String>,
    ) -> Response<Full<Bytes>> {
        if request.files.is_empty() || request.files.len() > MAX_FILES {
            return reply(StatusCode::BAD_REQUEST);
        }
        // Anyone on the network can ask: not over and over.
        if !self.may_prompt(ip) {
            return reply(StatusCode::TOO_MANY_REQUESTS);
        }
        if let Some(text) = message_text(&request) {
            // Keyed like the list of devices, to land in the same chat.
            let sender = TextSender {
                verified: cert.is_some(),
                fingerprint: cert.clone().unwrap_or_else(|| {
                    clean_alias(&request.info.fingerprint).to_ascii_uppercase()
                }),
                alias: clean_alias(&request.info.alias),
            };
            (self.hooks.on_text)(sender, text);
            // "Finished": no file to transfer.
            return reply(StatusCode::NO_CONTENT);
        }
        let session_id = uuid::Uuid::new_v4().to_string();
        let cancelled = Arc::new(Notify::new());
        {
            let mut slot = self.session();
            if slot.is_some() {
                return reply(StatusCode::CONFLICT);
            }
            *slot = Some(Session {
                id: session_id.clone(),
                ip,
                pending: Some(cancelled.clone()),
                files: HashMap::new(),
                tids: Vec::new(),
            });
        }
        // A sender that hangs up or cancels while the user decides withdraws
        // its offers.
        let mut guard = SessionGuard {
            inner: self.clone(),
            session_id: session_id.clone(),
            armed: true,
        };

        let peer_name = clean_alias(&request.info.alias);
        // The certificate proves who the sender is; the fingerprint claimed
        // by older versions without one proves nothing, and is kept apart.
        let peer_id = match cert {
            Some(cert) => format!("localsend:{cert}"),
            None => format!(
                "localsend-unverified:{}",
                clean_alias(&request.info.fingerprint).to_ascii_uppercase()
            ),
        };
        let (ids, files): (Vec<String>, Vec<ExternalFile>) = request
            .files
            .into_iter()
            .map(|(id, f)| {
                let file = ExternalFile {
                    name: f.file_name,
                    size: f.size,
                };
                (id, file)
            })
            .unzip();
        let offers = self.node.external_offer(&peer_id, &peer_name, &files);
        let tids: Vec<String> = offers.iter().map(|(tid, _)| tid.clone()).collect();
        if let Some(s) = self.session().as_mut().filter(|s| s.id == session_id) {
            s.tids = tids.clone();
        }

        let decisions = async {
            let mut accepted = Vec::with_capacity(offers.len());
            for (_, answer) in offers {
                accepted.push(answer.await.unwrap_or(false));
            }
            accepted
        };
        let accepted = tokio::select! {
            accepted = decisions => accepted,
            _ = cancelled.notified() => return reply(StatusCode::FORBIDDEN),
            _ = tokio::time::sleep(DECISION_TIMEOUT) => {
                for tid in &tids {
                    self.node.external_stop(tid, Status::Cancelled, Some("Sans réponse à temps"));
                }
                return reply(StatusCode::FORBIDDEN);
            }
        };

        let mut tokens = serde_json::Map::new();
        let mut session_files = HashMap::new();
        for ((id, tid), accepted) in ids.into_iter().zip(tids).zip(accepted) {
            if accepted {
                let token = uuid::Uuid::new_v4().to_string();
                tokens.insert(id.clone(), json!(token));
                session_files.insert(
                    id,
                    SessionFile {
                        token,
                        tid,
                        started: false,
                    },
                );
            }
        }
        if session_files.is_empty() {
            return reply(StatusCode::FORBIDDEN);
        }
        {
            let mut slot = self.session();
            let Some(s) = slot.as_mut().filter(|s| s.id == session_id) else {
                return reply(StatusCode::FORBIDDEN);
            };
            s.pending = None;
            s.files = session_files;
        }
        guard.armed = false;
        tokio::spawn(self.clone().watch_session(session_id.clone()));
        reply_json(&json!({ "sessionId": session_id, "files": tokens }))
    }

    async fn upload(
        self: &Arc<Self>,
        session_id: &str,
        file_id: &str,
        token: &str,
        ip: IpAddr,
        mut body: Incoming,
    ) -> Response<Full<Bytes>> {
        let tid = {
            let mut slot = self.session();
            let file = slot
                .as_mut()
                .filter(|s| s.id == session_id && s.ip == ip && s.pending.is_none())
                .and_then(|s| s.files.get_mut(file_id))
                .filter(|f| same_secret(&f.token, token) && !f.started);
            let Some(file) = file else {
                return reply(StatusCode::FORBIDDEN);
            };
            file.started = true;
            file.tid.clone()
        };
        // Fails the transfer if the connection drops (or we stop) midway.
        let mut guard = StopGuard {
            node: self.node.clone(),
            tids: vec![tid.clone()],
            status: Status::Error,
            error: "Transfert interrompu",
            armed: true,
        };
        let (tx, rx) = mpsc::channel::<Bytes>(16);
        let pump = async move {
            // A sender that stops sending shows up as a short file too.
            while let Ok(Some(frame)) = tokio::time::timeout(IDLE_TIMEOUT, body.frame()).await {
                // A broken connection shows up as a short file.
                let Ok(frame) = frame else { break };
                if let Ok(data) = frame.into_data() {
                    if tx.send(data).await.is_err() {
                        break;
                    }
                }
            }
        };
        let (_, result) = tokio::join!(pump, self.node.external_receive(&tid, rx));
        guard.armed = false;
        self.end_session_if_done(session_id);
        match result {
            Ok(()) => reply(StatusCode::OK),
            Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR),
        }
    }

    fn cancel(&self, session_id: Option<&str>, ip: IpAddr) {
        let mut slot = self.session();
        let Some(s) = slot.as_ref() else { return };
        if s.ip != ip || session_id.is_some_and(|id| id != s.id) {
            return;
        }
        if let Some(cancelled) = &s.pending {
            // Before the answer the sender has no session ID yet: the waiting
            // prepare-upload withdraws the offers.
            cancelled.notify_one();
            return;
        }
        let tids = slot.take().map(|s| s.tids).unwrap_or_default();
        drop(slot);
        for tid in &tids {
            self.node
                .external_stop(tid, Status::Cancelled, Some("Annulé par l’expéditeur"));
        }
    }

    /// Free the session slot once all its transfers are over.
    fn end_session_if_done(&self, session_id: &str) -> bool {
        let mut slot = self.session();
        let Some(s) = slot.as_ref().filter(|s| s.id == session_id) else {
            return true;
        };
        if self.node.external_progress(&s.tids).1 {
            *slot = None;
            return true;
        }
        false
    }

    /// Abandon the session when the sender stops making progress.
    async fn watch_session(self: Arc<Self>, session_id: String) {
        let mut last = (0, Instant::now());
        loop {
            tokio::time::sleep(SESSION_CHECK).await;
            if self.end_session_if_done(&session_id) {
                return;
            }
            let tids = match self.session().as_ref() {
                Some(s) if s.id == session_id => s.tids.clone(),
                _ => return,
            };
            let (bytes, _) = self.node.external_progress(&tids);
            if bytes != last.0 {
                last = (bytes, Instant::now());
            } else if last.1.elapsed() >= IDLE_TIMEOUT {
                {
                    let mut slot = self.session();
                    if slot.as_ref().is_some_and(|s| s.id == session_id) {
                        *slot = None;
                    }
                }
                for tid in &tids {
                    self.node.external_stop(
                        tid,
                        Status::Error,
                        Some("L’expéditeur ne répond plus"),
                    );
                }
                return;
            }
        }
    }
}

/// One open connection of an address, counted until dropped.
struct ConnectionSlot {
    inner: Arc<Inner>,
    ip: IpAddr,
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        let mut connections = self
            .inner
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(count) = connections.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                connections.remove(&self.ip);
            }
        }
    }
}

/// Withdraws a session's offers if `prepare-upload` ends without an answer.
struct SessionGuard {
    inner: Arc<Inner>,
    session_id: String,
    armed: bool,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let tids = {
            let mut slot = self.inner.session();
            if !slot.as_ref().is_some_and(|s| s.id == self.session_id) {
                return;
            }
            if !self.armed {
                return;
            }
            slot.take().map(|s| s.tids).unwrap_or_default()
        };
        for tid in &tids {
            self.inner
                .node
                .external_stop(tid, Status::Cancelled, Some("Annulé par l’expéditeur"));
        }
    }
}

/// Stops the transfers a task carries if it ends early.
struct StopGuard {
    node: Node,
    tids: Vec<String>,
    status: Status,
    error: &'static str,
    armed: bool,
}

impl Drop for StopGuard {
    fn drop(&mut self) {
        if self.armed {
            for tid in &self.tids {
                self.node.external_stop(tid, self.status, Some(self.error));
            }
        }
    }
}
