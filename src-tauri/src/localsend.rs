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
use std::sync::atomic::{AtomicBool, Ordering};
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
/// Devices kept in the list, and among them at most that many whose
/// fingerprint is only claimed: those never push out the proven ones.
const MAX_DEVICES: usize = 256;
const MAX_UNVERIFIED: usize = 128;
/// Fingerprints proven by a certificate that we keep in mind.
const MAX_VERIFIED: usize = 4096;
/// Last addresses of the devices we exchanged with, kept past `DEVICE_TTL`.
const MAX_KNOWN: usize = 256;
/// Longest fingerprint taken from a device (LocalSend's are hex or UUIDs).
const MAX_FINGERPRINT: usize = 128;
/// Announcements processed per source address and `INTERFACE_CHECK`.
const ANNOUNCES_PER_IP: usize = 20;
/// Registrations with announcing devices at the same time, and per minute.
const MAX_REGISTERS: usize = 8;
const REGISTERS_PER_MINUTE: usize = 60;
/// Open HTTPS connections, in all and per device.
const MAX_CONNECTIONS: usize = 128;
const MAX_CONNECTIONS_PER_IP: usize = 8;
/// For the headers of a request (also between requests on a connection),
/// and for the whole JSON body of one.
const HEADER_TIMEOUT: Duration = Duration::from_secs(15);
const BODY_TIMEOUT: Duration = Duration::from_secs(15);
/// How long we look for a device no longer listed before sending to it.
const FIND_TIMEOUT: Duration = Duration::from_secs(4);
const FIND_CHECK: Duration = Duration::from_millis(200);
/// The list of devices is sent to the window at most that often.
const DEVICES_REFRESH: Duration = Duration::from_secs(1);
/// A session without any progress for that long is abandoned.
const IDLE_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const SESSION_CHECK: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const REGISTER_TIMEOUT: Duration = Duration::from_secs(5);
/// Peers repeat their announcement a few times: answer each device once.
const REGISTER_INTERVAL: Duration = Duration::from_secs(5);
/// How often our interfaces are looked at again, unless the system reports
/// network changes (`LocalSend::network_changed`).
const INTERFACE_CHECK: Duration = Duration::from_secs(10);
/// When it does, in case a change went unreported.
const INTERFACE_FALLBACK: Duration = Duration::from_secs(60);
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
    /// Ours only (other apps ignore it): see `P2pHint`.
    #[serde(default)]
    p2pshare: Option<P2pHint>,
}

/// The P2P ID and QUIC port of a P2P Share device, so that friends on the
/// same network connect even when relays are unreachable.
#[derive(Deserialize)]
struct P2pHint {
    id: String,
    port: u16,
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
    } else if cfg!(target_os = "android") {
        "Android"
    } else {
        "Linux"
    }
}

/// How LocalSend shows us: a phone or a computer.
const DEVICE_TYPE: &str = if cfg!(mobile) { "mobile" } else { "desktop" };

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

/// The key of a device whose fingerprint is only claimed, the same in the
/// list of devices and for its requests, so that both meet in one chat.
fn claimed_key(fingerprint: &str) -> Option<String> {
    let fingerprint = fingerprint.trim();
    let valid = !fingerprint.is_empty()
        && fingerprint.len() <= MAX_FINGERPRINT
        && fingerprint.bytes().all(|b| b.is_ascii_graphic());
    valid.then(|| fingerprint.to_ascii_uppercase())
}

/// Calls `done` once: with the outcome, or with an error if the task carrying
/// it is dropped first (the service stopped).
struct Done<F: FnOnce(Result<(), String>)>(Option<F>);

impl<F: FnOnce(Result<(), String>)> Done<F> {
    fn call(mut self, result: Result<(), String>) {
        if let Some(done) = self.0.take() {
            done(result);
        }
    }
}

impl<F: FnOnce(Result<(), String>)> Drop for Done<F> {
    fn drop(&mut self) {
        if let Some(done) = self.0.take() {
            done(Err("LocalSend désactivé".into()));
        }
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
    /// Fingerprints a certificate proved, with when last (none: told by the
    /// app, kept for good). Kept past the list of devices: a device claiming
    /// one of them over plain HTTP is refused, even once the real one left
    /// the list.
    verified: Mutex<HashMap<String, Option<Instant>>>,
    /// Where the devices we exchanged with were last, by fingerprint, to
    /// find them again once they left the list.
    known: Mutex<HashMap<String, (Device, Instant)>>,
    /// The networks of our interfaces, and when we last looked.
    nets: Mutex<(Instant, Vec<netdev::ipnet::Ipv4Net>)>,
    /// For the discovery task.
    signals: mpsc::UnboundedSender<Signal>,
    /// The system reports network changes: interfaces are looked at only
    /// every `INTERFACE_FALLBACK` otherwise.
    network_watched: AtomicBool,
    /// Sending sessions in progress.
    sends: Mutex<Vec<AbortHandle>>,
    /// When each address last asked for the user's attention.
    prompts: Mutex<HashMap<IpAddr, Vec<Instant>>>,
    /// Open HTTPS connections per address.
    connections: Mutex<HashMap<IpAddr, usize>>,
    /// The list of devices changed since the window last got it.
    devices_dirty: AtomicBool,
    devices_sent: Mutex<Instant>,
}

/// What the discovery task is asked.
enum Signal {
    /// Forget who was answered, and announce us.
    Scan,
    /// Our interfaces may have changed.
    NetworkChanged,
    /// Back from sleep: look at our interfaces, and announce us.
    Resumed,
    /// Economy on or off (see `LocalSend::set_economy`).
    Economy(bool),
    /// The window is due the list of devices at that time.
    DevicesDue(Instant),
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
    /// The P2P ID it announces, when it runs P2P Share: a friend's is not
    /// listed, being in the friends already (a false claim only hides it).
    p2p_id: Option<String>,
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
    /// See `Device::p2p_id`.
    #[serde(skip)]
    pub p2p_id: Option<String>,
    /// That ID is a friend's (set by the app): the list of devices leaves
    /// it out, being in the friends already.
    pub friend: bool,
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
        let (signals, signals_rx) = mpsc::unbounded_channel();
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
            verified: Mutex::new(HashMap::new()),
            known: Mutex::new(HashMap::new()),
            nets: Mutex::new((Instant::now(), local_nets())),
            signals,
            network_watched: AtomicBool::new(false),
            sends: Mutex::new(Vec::new()),
            prompts: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            devices_dirty: AtomicBool::new(false),
            devices_sent: Mutex::new(Instant::now() - DEVICES_REFRESH),
        });
        let mut tasks = vec![tokio::spawn(inner.clone().serve(listener, acceptor)).abort_handle()];
        let discovery_error = match multicast_socket() {
            Ok(socket) => {
                tasks.push(tokio::spawn(inner.clone().discovery(socket, signals_rx)).abort_handle());
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
                p2p_id: d.p2p_id.clone(),
                friend: false,
            })
            .collect();
        list.sort_by_key(|d| d.alias.to_lowercase());
        list
    }

    /// Forget the devices and ask those still around to answer again.
    pub fn scan(&self) {
        self.inner.devices().clear();
        (self.inner.hooks.on_devices)();
        let _ = self.inner.signals.send(Signal::Scan);
    }

    /// The system reports network changes, and `network_changed` is called
    /// on each: interfaces are then only looked at now and then otherwise.
    pub fn set_network_watched(&self, watched: bool) {
        self.inner.network_watched.store(watched, Ordering::Relaxed);
    }

    /// An interface or address changed: join the networks that appeared, and
    /// announce us there.
    pub fn network_changed(&self) {
        let _ = self.inner.signals.send(Signal::NetworkChanged);
    }

    /// Back from sleep: announce us again.
    pub fn resumed(&self) {
        let _ = self.inner.signals.send(Signal::Resumed);
    }

    /// While on (session locked, on battery), we no longer announce
    /// ourselves nor look at our interfaces on a timer, but devices still
    /// get answers, and their files and texts come as usual. Turned off, it
    /// catches up and announces us.
    pub fn set_economy(&self, on: bool) {
        let _ = self.inner.signals.send(Signal::Economy(on));
    }

    /// Fingerprints the app knows as proven by a certificate (its chats with
    /// verified devices): a device claiming one of them without it is
    /// refused, as for those proven since the service started.
    pub fn remember_verified<'a>(&self, fingerprints: impl IntoIterator<Item = &'a str>) {
        let mut verified = self.inner.verified();
        for fingerprint in fingerprints {
            verified.insert(fingerprint.to_ascii_uppercase(), None);
        }
    }

    /// Send a text message to a device; `done` gets the outcome, also when
    /// the service stops first.
    pub fn send_text(
        &self,
        fingerprint: &str,
        text: String,
        done: impl FnOnce(Result<(), String>) + Send + 'static,
    ) -> Result<(), &'static str> {
        const NOT_FOUND: &str = "Appareil introuvable : est-il allumé, avec LocalSend ouvert ?";
        let target = self.inner.target(fingerprint).ok_or(NOT_FOUND)?;
        let inner = self.inner.clone();
        let done = Done(Some(done));
        let task = self.inner.rt.spawn(async move {
            let result = match inner.resolve(target).await {
                Some(device) => {
                    inner.note_known(&device);
                    inner.send_text(device, text).await
                }
                None => Err(NOT_FOUND.into()),
            };
            done.call(result);
        });
        let mut sends = self.inner.sends.lock().unwrap_or_else(|e| e.into_inner());
        sends.retain(|t| !t.is_finished());
        sends.push(task.abort_handle());
        Ok(())
    }

    /// Send files to a device; returns how many will be offered to it.
    pub fn send_files(&self, fingerprint: &str, paths: &[PathBuf]) -> Result<usize, &'static str> {
        let target = self
            .inner
            .target(fingerprint)
            .ok_or("Appareil introuvable : relancez la recherche")?;
        let device = &target.0;
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
            .spawn(self.inner.clone().send_session(target, outgoing));
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

/// The networks our interfaces are on (as `local_nets`), and our addresses
/// on those that can reach LocalSend devices, from a single look.
fn local_networks() -> (Vec<netdev::ipnet::Ipv4Net>, HashSet<Ipv4Addr>) {
    let interfaces: Vec<_> = netdev::get_interfaces()
        .into_iter()
        .filter(|i| i.is_up())
        .collect();
    let ips = interfaces
        .iter()
        .filter(|i| !i.is_loopback())
        .flat_map(|i| i.ipv4_addrs())
        .filter(|ip| !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified())
        .collect();
    let nets = interfaces.into_iter().flat_map(|i| i.ipv4).collect();
    (nets, ips)
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

/// Room in the list for one more device, proven by its certificate or not:
/// the oldest of those only claimed go first, and they never push out a
/// proven one. False if there is none.
fn make_room(devices: &mut HashMap<String, (Device, Instant)>, verified: bool) -> bool {
    let unverified = devices.values().filter(|(d, _)| !d.verified).count();
    let full = devices.len() >= MAX_DEVICES;
    if !full && (verified || unverified < MAX_UNVERIFIED) {
        return true;
    }
    let oldest = |proven: bool| {
        devices
            .iter()
            .filter(|(_, (d, _))| d.verified == proven)
            .min_by_key(|(_, (_, seen))| *seen)
            .map(|(k, _)| k.clone())
    };
    let victim = match oldest(false) {
        Some(k) => Some(k),
        None if verified => oldest(true),
        None => None,
    };
    match victim {
        Some(k) => {
            devices.remove(&k);
            true
        }
        None => false,
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

/// A JSON body, which a client sending it slowly does not hold for long.
async fn read_json<T: serde::de::DeserializeOwned>(body: Incoming) -> Option<T> {
    let bytes = tokio::time::timeout(BODY_TIMEOUT, Limited::new(body, MAX_JSON).collect())
        .await
        .ok()?
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

    fn verified(&self) -> MutexGuard<'_, HashMap<String, Option<Instant>>> {
        self.verified.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn known(&self) -> MutexGuard<'_, HashMap<String, (Device, Instant)>> {
        self.known.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether a device at `ip` is on the local network (our interfaces
    /// looked at again every `INTERFACE_CHECK`).
    fn is_local(&self, ip: IpAddr) -> bool {
        if on_local_network(ip, &[]) {
            return true;
        }
        let mut nets = self.nets.lock().unwrap_or_else(|e| e.into_inner());
        if nets.0.elapsed() >= INTERFACE_CHECK {
            *nets = (Instant::now(), local_nets());
        }
        on_local_network(ip, &nets.1)
    }

    /// A certificate proved `fingerprint`.
    fn note_verified(&self, fingerprint: &str) {
        let mut verified = self.verified();
        match verified.get_mut(fingerprint) {
            Some(Some(seen)) => *seen = Instant::now(),
            Some(None) => {}
            None => {
                if verified.len() >= MAX_VERIFIED {
                    let oldest = verified
                        .iter()
                        .filter_map(|(k, seen)| Some((k, (*seen)?)))
                        .min_by_key(|(_, seen)| *seen)
                        .map(|(k, _)| k.clone());
                    if let Some(oldest) = oldest {
                        verified.remove(&oldest);
                    }
                }
                verified.insert(fingerprint.to_string(), Some(Instant::now()));
            }
        }
    }

    fn was_verified(&self, fingerprint: &str) -> bool {
        self.verified().contains_key(fingerprint)
    }

    /// Keep where `device` is, to find it again once it left the list.
    fn note_known(&self, device: &Device) {
        let mut known = self.known();
        if !known.contains_key(&device.fingerprint) && known.len() >= MAX_KNOWN {
            let oldest = known
                .iter()
                .min_by_key(|(_, (_, seen))| *seen)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                known.remove(&oldest);
            }
        }
        known.insert(device.fingerprint.clone(), (device.clone(), Instant::now()));
    }

    /// The device to send to, and whether it is listed: otherwise it was
    /// last seen at that address, and may have left or moved.
    fn target(&self, fingerprint: &str) -> Option<(Device, bool)> {
        let listed = self
            .devices()
            .get(fingerprint)
            .map(|(d, seen)| (d.clone(), seen.elapsed() < DEVICE_TTL));
        match listed {
            Some((device, true)) => Some((device, true)),
            stale => {
                let known = || self.known().get(fingerprint).map(|(d, _)| d.clone());
                stale.map(|(d, _)| d).or_else(known).map(|d| (d, false))
            }
        }
    }

    /// Find a device no longer listed: ask it at its last address (its
    /// certificate must match, if it had one), else on the whole network.
    async fn resolve(&self, (device, listed): (Device, bool)) -> Option<Device> {
        if listed {
            return Some(device);
        }
        let start = Instant::now();
        let fingerprint = device.fingerprint.clone();
        let found = || {
            self.devices()
                .get(&fingerprint)
                .filter(|(_, seen)| *seen >= start)
                .map(|(d, _)| d.clone())
        };
        // Never in clear to a device that proved its fingerprint.
        if device.https || !self.was_verified(&fingerprint) {
            let pin = device.https.then(|| fingerprint.clone());
            self.register_with(device.ip, device.port, device.https, pin)
                .await;
            if let Some(device) = found() {
                return Some(device);
            }
        }
        let _ = self.signals.send(Signal::Scan);
        let deadline = Instant::now() + FIND_TIMEOUT;
        while Instant::now() < deadline {
            tokio::time::sleep(FIND_CHECK).await;
            if let Some(device) = found() {
                return Some(device);
            }
        }
        None
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
        if let Some(hint) = &info.p2pshare {
            self.node.nearby_peer(&hint.id, SocketAddr::new(ip, hint.port));
        }
        let verified = cert.is_some();
        let fingerprint = match cert {
            // A claim the certificate contradicts is not trusted.
            Some(cert) if !info.fingerprint.eq_ignore_ascii_case(&cert) => return,
            Some(cert) => cert,
            None => match claimed_key(&info.fingerprint) {
                Some(key) => key,
                None => return,
            },
        };
        if fingerprint.eq_ignore_ascii_case(&self.fingerprint) {
            return;
        }
        if verified {
            self.note_verified(&fingerprint);
        } else if !https && self.was_verified(&fingerprint) {
            // A fingerprint once proven is only taken again over HTTPS,
            // where the certificate must match it when we send.
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
            p2p_id: info
                .p2pshare
                .as_ref()
                .map(|h| h.id.to_ascii_lowercase())
                .filter(|id| crate::p2p::is_valid_id(id)),
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
            } else if !make_room(&mut devices, verified) {
                return;
            }
            devices.insert(fingerprint.clone(), (device.clone(), Instant::now()));
            known != Some(device.clone())
        };
        // The address kept for a device we exchanged with follows it, but a
        // mere claim does not replace a proven one.
        {
            let mut known = self.known();
            if let Some((kept, seen)) = known.get_mut(&fingerprint) {
                if verified || (!kept.verified && (device.https || !kept.https)) {
                    *kept = device;
                    *seen = Instant::now();
                }
            }
        }
        if changed {
            self.devices_changed();
        }
    }

    /// Tell the window, at most every `DEVICES_REFRESH` (the discovery task
    /// sends what is left over once the delay is over).
    fn devices_changed(&self) {
        let mut sent = self.devices_sent.lock().unwrap_or_else(|e| e.into_inner());
        if sent.elapsed() >= DEVICES_REFRESH {
            *sent = Instant::now();
            self.devices_dirty.store(false, Ordering::Relaxed);
            drop(sent);
            (self.hooks.on_devices)();
        } else if !self.devices_dirty.swap(true, Ordering::Relaxed) {
            let _ = self.signals.send(Signal::DevicesDue(*sent + DEVICES_REFRESH));
        }
    }

    fn check_period(&self) -> Duration {
        if self.network_watched.load(Ordering::Relaxed) {
            INTERFACE_FALLBACK
        } else {
            INTERFACE_CHECK
        }
    }

    /// Look at our interfaces again, and join the multicast group on the new
    /// ones: true if there are.
    async fn check_interfaces(
        &self,
        socket: &UdpSocket,
        joined: &mut HashSet<Ipv4Addr>,
        nets: &mut Vec<netdev::ipnet::Ipv4Net>,
    ) -> bool {
        let Ok((found, ips)) = tokio::task::spawn_blocking(local_networks).await else {
            return false;
        };
        *nets = found;
        *self.nets.lock().unwrap_or_else(|e| e.into_inner()) = (Instant::now(), nets.clone());
        self.devices().retain(|_, (_, seen)| seen.elapsed() < DEVICE_TTL);
        let new: Vec<_> = ips.difference(joined).copied().collect();
        for ip in &new {
            let _ = socket.join_multicast_v4(MULTICAST, *ip);
        }
        *joined = ips;
        !new.is_empty()
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
        let mut info = json!({
            "alias": *self.alias(),
            "version": VERSION,
            "deviceModel": device_model(),
            "deviceType": DEVICE_TYPE,
            "fingerprint": self.fingerprint,
            "download": false,
        });
        if let Some((id, port)) = self.node.nearby_hint() {
            info["p2pshare"] = json!({ "id": id, "port": port });
        }
        info
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

    async fn discovery(
        self: Arc<Self>,
        socket: UdpSocket,
        mut signals: mpsc::UnboundedReceiver<Signal>,
    ) {
        let mut joined = HashSet::new();
        // Right away, then every `check_period` (not in economy).
        let mut next_check = Instant::now();
        let mut economy = false;
        let mut burst: Vec<Instant> = Vec::new();
        let mut answered: HashMap<IpAddr, Instant> = HashMap::new();
        // Announcements per address since then (`INTERFACE_CHECK` at most).
        let mut heard: (Instant, HashMap<IpAddr, usize>) = (Instant::now(), HashMap::new());
        let mut registered: Vec<Instant> = Vec::new();
        let mut nets = Vec::new();
        let mut registers = JoinSet::new();
        let mut devices_due: Option<Instant> = None;
        let mut buf = vec![0u8; 65536];
        let announce_burst = || {
            let now = Instant::now();
            ANNOUNCE_DELAYS.iter().map(|d| now + *d).collect::<Vec<_>>()
        };
        loop {
            let next = burst.first().copied();
            tokio::select! {
                _ = tokio::time::sleep_until(devices_due.unwrap_or_else(Instant::now).into()), if devices_due.is_some() => {
                    devices_due = None;
                    if self.devices_dirty.load(Ordering::Relaxed) {
                        self.devices_changed();
                    }
                }
                _ = tokio::time::sleep_until(next_check.into()), if !economy => {
                    if self.check_interfaces(&socket, &mut joined, &mut nets).await {
                        burst = announce_burst();
                    }
                    answered.retain(|_, t| t.elapsed() < REGISTER_INTERVAL);
                    next_check = Instant::now() + self.check_period();
                }
                Some(signal) = signals.recv() => match signal {
                    Signal::Scan => {
                        burst = announce_burst();
                        answered.clear();
                    }
                    Signal::NetworkChanged | Signal::Resumed => {
                        let resumed = matches!(signal, Signal::Resumed);
                        let new = self.check_interfaces(&socket, &mut joined, &mut nets).await;
                        if (new || resumed) && !economy {
                            burst = announce_burst();
                        }
                        next_check = Instant::now() + self.check_period();
                    }
                    Signal::Economy(on) => {
                        if economy && !on {
                            // Catch up on what changed meanwhile.
                            self.check_interfaces(&socket, &mut joined, &mut nets).await;
                            next_check = Instant::now() + self.check_period();
                            burst = announce_burst();
                        }
                        economy = on;
                    }
                    Signal::DevicesDue(at) => {
                        devices_due = Some(devices_due.map_or(at, |due| due.min(at)));
                    }
                },
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
                    if heard.0.elapsed() >= INTERFACE_CHECK {
                        heard = (Instant::now(), HashMap::new());
                        answered.retain(|_, t| t.elapsed() < REGISTER_INTERVAL);
                    }
                    // Only devices of the local network, each a few times.
                    let count = heard.1.entry(ip).or_default();
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
                    // Source addresses can be forged: not many in all either.
                    registered.retain(|t| t.elapsed() < Duration::from_secs(60));
                    if registered.len() >= REGISTERS_PER_MINUTE {
                        continue;
                    }
                    registered.push(Instant::now());
                    answered.insert(ip, Instant::now());
                    let inner = self.clone();
                    registers.spawn(async move { inner.register_with(ip, port, https, None).await });
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

    /// Introduce ourselves to a device that announced itself, or that we
    /// look for (`pin`: the fingerprint its certificate must have).
    async fn register_with(&self, ip: IpAddr, port: u16, https: bool, pin: Option<String>) -> bool {
        let peer = send::Peer {
            ip,
            port,
            https,
            pin,
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
                    // Only devices of the local network.
                    let ip = canonical(addr.ip());
                    if !self.is_local(ip) {
                        continue;
                    }
                    let Some(slot) = self.clone().connection_slot(ip, conns.len()) else {
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
        // Keyed like the list of devices, to land in the same chat.
        let Some(key) = cert
            .clone()
            .or_else(|| claimed_key(&request.info.fingerprint))
        else {
            return reply(StatusCode::BAD_REQUEST);
        };
        let text = message_text(&request);
        // Busy: the request does not reach the user, so it does not count.
        if text.is_none() && self.session().is_some() {
            return reply(StatusCode::CONFLICT);
        }
        // Anyone on the network can ask: not over and over.
        if !self.may_prompt(ip) {
            return reply(StatusCode::TOO_MANY_REQUESTS);
        }
        if let Some(text) = text {
            // Its address, to answer in the chat once it left the list.
            let listed = self
                .devices()
                .get(&key)
                .filter(|(d, _)| d.ip == ip && (d.verified || cert.is_none()))
                .map(|(d, _)| d.clone());
            if let Some(device) = listed {
                self.note_known(&device);
            }
            let sender = TextSender {
                verified: cert.is_some(),
                fingerprint: key,
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
            Some(_) => format!("localsend:{key}"),
            None => format!("localsend-unverified:{key}"),
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

        // The files left unanswered in time are withdrawn; those the user
        // accepted meanwhile still come.
        let deadline = tokio::time::Instant::now() + DECISION_TIMEOUT;
        let decisions = async {
            let mut accepted = Vec::with_capacity(offers.len());
            for (tid, answer) in offers {
                accepted.push(match tokio::time::timeout_at(deadline, answer).await {
                    Ok(answer) => answer.unwrap_or(false),
                    Err(_) => {
                        self.node
                            .external_stop(&tid, Status::Cancelled, Some("Sans réponse à temps"));
                        false
                    }
                });
            }
            accepted
        };
        let accepted = tokio::select! {
            accepted = decisions => accepted,
            _ = cancelled.notified() => return reply(StatusCode::FORBIDDEN),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn device(fingerprint: &str, verified: bool) -> Device {
        Device {
            fingerprint: fingerprint.into(),
            alias: "x".into(),
            model: None,
            kind: None,
            ip: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2)),
            port: PORT,
            https: verified,
            verified,
            p2p_id: None,
        }
    }

    #[test]
    fn claimed_keys_match_the_list() {
        let uuid = "3f2a8c1e-7b4d-4e9a-b2c6-0d1e5f6a7b8c-and-some-more";
        assert_eq!(claimed_key(uuid), Some(uuid.to_ascii_uppercase()));
        assert_eq!(claimed_key(" ab "), Some("AB".into()));
        assert_eq!(claimed_key(""), None);
        assert_eq!(claimed_key("a b"), None);
        assert_eq!(claimed_key(&"a".repeat(MAX_FINGERPRINT + 1)), None);
    }

    #[test]
    fn claimed_devices_never_push_out_proven_ones() {
        let mut devices = HashMap::new();
        for i in 0..MAX_DEVICES - MAX_UNVERIFIED {
            let fp = format!("V{i}");
            devices.insert(fp.clone(), (device(&fp, true), Instant::now()));
        }
        for i in 0..MAX_UNVERIFIED * 3 {
            let fp = format!("U{i}");
            if make_room(&mut devices, false) {
                devices.insert(fp.clone(), (device(&fp, false), Instant::now()));
            }
        }
        let proven = devices.values().filter(|(d, _)| d.verified).count();
        assert_eq!(proven, MAX_DEVICES - MAX_UNVERIFIED);
        assert!(devices.len() <= MAX_DEVICES);
        // A proven device still gets in, in place of a claimed one.
        assert!(make_room(&mut devices, true));
        assert_eq!(devices.len(), MAX_DEVICES - 1);
    }
}
