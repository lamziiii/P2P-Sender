//! P2P core: friends, chat and file transfers over iroh (QUIC).
//!
//! One authenticated QUIC connection per friend; the friend's ID is its
//! Ed25519 public key, checked during the TLS handshake.
//!
//!   control: one bidirectional stream, opened by the dialer, carrying
//!            length-prefixed JSON messages ([u32 BE len][JSON]).
//!   data:    one unidirectional stream per file (re)start:
//!            [tid:16][offset:u64 BE] followed by the raw file bytes.
//!
//! The receiver accepts a stream only if it starts exactly at the number of
//! bytes it already has, and otherwise asks the sender to restart at that
//! offset, so reconnections resume where they stopped instead of starting over.
//!
//! All state lives in one `State` behind a mutex that is never held across an
//! `.await`; events produced while it is locked are dispatched after unlocking.
//!
//! Groups (see `groups.rs`) add connections between co-members who are not
//! friends, and a signed log per group synced over the same control stream.
//! Files can also come from outside the P2P network (see `external.rs`).
//! With a peer on the same local network, the data streams go over a direct
//! TCP + TLS connection instead, which is faster there (see `lan.rs`).

mod external;
mod groups;
mod lan;

pub use external::{ExternalFile, OutgoingFile};

pub use groups::{
    GroupDetail, GroupFileView, GroupMessageView, GroupSummary, GroupsOverview, InviteView,
    MemberView,
};

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use iroh::endpoint::{presets, Connection, QuicTransportConfig, RecvStream, SendStream, VarInt};
use iroh::address_lookup::{PkarrPublisher, PkarrResolver};
use iroh::{Endpoint, PublicKey, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot, watch};

use crate::store::{write_atomic, JsonStore};

pub const ALPN: &[u8] = b"p2pshare/3";

const TID_BYTES: usize = 16;
const DATA_HEADER: usize = TID_BYTES + 8;
const CHUNK_SIZE: u64 = 1 << 20;
const DISK_BUFFER: usize = 8 << 20;
const MAX_FRAME: usize = 256 * 1024;

const DIAL_TIMEOUT: Duration = Duration::from_secs(15);
const ONLINE_TIMEOUT: Duration = Duration::from_secs(15);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(15);
// A peer that just started is not published yet: retry a few times quickly.
const FAST_RETRY_DELAY: Duration = Duration::from_secs(1);
const FAST_RETRIES: u8 = 10;
const TICK: Duration = Duration::from_millis(500);
const WAKE_GAP: Duration = Duration::from_secs(10);
const DUPLICATE_WINDOW: Duration = Duration::from_secs(10);
const CHAT_RETRY: Duration = Duration::from_secs(10);
const MAX_TEXT: usize = 5000;
const RATE_LIMIT: usize = 30;
const RATE_WINDOW: Duration = Duration::from_secs(10);
/// Control messages of any kind a peer may send per `RATE_WINDOW`.
const CONTROL_RATE_LIMIT: usize = 1000;
/// Offers from one friend waiting for our answer.
const MAX_PENDING_OFFERS: usize = 100;
/// Bytes waiting to be sent to a peer that does not read them.
const MAX_QUEUED: usize = 64 << 20;
/// Messages kept per conversation.
const MAX_HISTORY: usize = 10_000;
const MESSAGES_SAVE_INTERVAL: Duration = Duration::from_secs(5);
/// Finished transfers hidden from the list are forgotten after that long.
const FORGET_FINISHED: Duration = Duration::from_secs(60 * 60);

const CLOSE_NORMAL: u32 = 0;
const CLOSE_DUPLICATE: u32 = 1;
const CLOSE_REJECTED: u32 = 2;

pub fn is_valid_id(id: &str) -> bool {
    id.len() == 64
        && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && PublicKey::from_str(id).is_ok()
}

fn is_tid(tid: &str) -> bool {
    tid.len() == TID_BYTES * 2 && tid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Length as JavaScript counts it, so limits match the UI's `maxLength`.
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

// ─── Public types ────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
pub struct Friend {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// A self-hosted relay (see the P2P-Sender-relay project), used instead of
/// iroh's public relays on networks that block them.
#[derive(Clone)]
pub struct CustomRelay {
    pub url: RelayUrl,
    pub token: Option<String>,
}

impl CustomRelay {
    /// Accepts `relay.example.com` or a full `https://…` URL.
    pub fn parse(url: &str, token: &str) -> Result<Self, String> {
        let url = url.trim();
        let full = if url.contains("://") {
            url.to_string()
        } else {
            format!("https://{url}")
        };
        let parsed = url::Url::parse(&full).map_err(|_| "Adresse invalide".to_string())?;
        // Plain http would send the token in the clear: only for a relay on
        // this computer.
        let local = match parsed.host() {
            Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        let scheme_ok = parsed.scheme() == "https" || (parsed.scheme() == "http" && local);
        if !scheme_ok || parsed.host_str().is_none() {
            return Err("L'adresse doit commencer par https://".into());
        }
        if parsed.path() != "/" {
            return Err("Le relais doit être à la racine du domaine (sans chemin)".into());
        }
        let token = token.trim();
        Ok(CustomRelay {
            url: RelayUrl::from(parsed),
            token: (!token.is_empty()).then(|| token.to_string()),
        })
    }

    fn pkarr_url(&self) -> url::Url {
        let mut url = url::Url::from(self.url.clone());
        url.set_path("/pkarr");
        url
    }
}

#[derive(Clone, Serialize)]
pub struct RelayStatusView {
    pub url: String,
    pub custom: bool,
    pub connected: bool,
    pub error: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct FriendView {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub legacy: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    Pending,
    Delivered,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub from: String,
    pub text: String,
    pub ts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<MessageStatus>,
}

pub type Messages = HashMap<String, Vec<ChatMessage>>;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Connecting,
    WaitingConsent,
    Pending,
    Sending,
    Receiving,
    Paused,
    Finishing,
    Completed,
    Declined,
    Cancelled,
    Error,
}

impl Status {
    pub fn is_final(self) -> bool {
        matches!(
            self,
            Status::Completed | Status::Declined | Status::Cancelled | Status::Error
        )
    }
    pub fn is_active(self) -> bool {
        matches!(self, Status::Sending | Status::Receiving)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    In,
    Out,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferView {
    pub id: String,
    pub dir: Dir,
    pub friend_id: String,
    pub file_name: String,
    pub file_size: u64,
    pub bytes: u64,
    pub progress: u32,
    pub speed: u64,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
    /// Sender's name for transfers from outside the P2P network.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_name: Option<String>,
    /// The file runs code when opened (program, script, shortcut…).
    pub executable: bool,
    /// Its bytes go over the local network (see `lan.rs`).
    pub lan: bool,
}

pub enum Event {
    FriendStatus {
        friend_id: String,
        online: bool,
    },
    Message {
        friend_id: String,
        message: ChatMessage,
    },
    MessageStatus {
        friend_id: String,
        id: String,
    },
    Transfer(TransferView),
    /// An offer, and how many files came with it (one notification for all).
    FileOffer(TransferView, usize),
    TransferComplete(TransferView),
    GroupUpdate {
        gid: String,
    },
    GroupMessage {
        gid: String,
        group: String,
        author: String,
        text: String,
    },
    GroupInvite {
        gid: String,
        group: String,
        from: String,
    },
}

pub struct Options {
    pub secret: SecretKey,
    pub friends: JsonStore<Vec<Friend>>,
    pub messages: JsonStore<Messages>,
    pub download_dir: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    pub on_event: Arc<dyn Fn(Event) + Send + Sync>,
    /// Where group logs, group settings and shared-file records live.
    pub groups_dir: PathBuf,
    /// Our display name in groups.
    pub nickname: String,
    /// Replaces iroh's public relays when set.
    pub relay: Option<CustomRelay>,
    /// Our certificate for the local-network fast path, which is off without.
    pub lan: Option<crate::localsend::Identity>,
}

// ─── Internal state ──────────────────────────────────────────────────────────

type Writer = BufWriter<tokio::fs::File>;

/// Shared with the tasks moving a transfer's bytes.
struct Shared {
    bytes: AtomicU64,
    /// Bumped whenever the running stream must stop (restart, cancel…).
    session: watch::Sender<u64>,
    /// Receiver side: the open `.part` file, owned by one stream at a time.
    writer: tokio::sync::Mutex<Option<Writer>>,
    /// The running stream goes over the local network.
    lan: AtomicBool,
}

impl Shared {
    fn session(&self) -> u64 {
        *self.session.borrow()
    }
    fn next_session(&self) -> u64 {
        self.session.send_modify(|s| *s += 1);
        self.session()
    }
}

struct Transfer {
    id: String,
    seq: u64,
    dir: Dir,
    friend_id: String,
    file_name: String,
    file_size: u64,
    status: Status,
    error: Option<String>,
    shared: Arc<Shared>,
    /// When it reached a final status.
    ended: Option<Instant>,
    // outgoing
    src_path: Option<PathBuf>,
    /// Modification time of the source when offered: a resume must not mix
    /// two versions of the file.
    src_mtime: Option<SystemTime>,
    conn_id: Option<u64>,
    // incoming
    part_path: Option<PathBuf>,
    saved_path: Option<PathBuf>,
    resyncing: bool,
    /// Cleared from the list by the user; kept to answer re-offers.
    hidden: bool,
    /// Outgoing: serves a shared file a member asked for (no consent, no re-offer).
    serve: bool,
    /// Incoming: downloads a shared file of a group.
    repo: Option<groups::Repo>,
    /// Incoming from outside the P2P network: the sender's name, and who
    /// waits for the user's answer to the offer.
    peer_name: Option<String>,
    decision: Option<oneshot::Sender<bool>>,
    // speed sampling
    speed: f64,
    sample_bytes: u64,
    sample_time: Instant,
}

impl Transfer {
    fn bytes(&self) -> u64 {
        self.shared.bytes.load(Ordering::Relaxed)
    }

    fn view(&self) -> TransferView {
        let bytes = self.bytes();
        let progress = if self.status == Status::Completed {
            100
        } else if self.file_size > 0 {
            ((bytes as u128 * 100) / self.file_size as u128) as u32
        } else {
            0
        };
        TransferView {
            id: self.id.clone(),
            dir: self.dir,
            friend_id: self.friend_id.clone(),
            file_name: self.file_name.clone(),
            file_size: self.file_size,
            bytes,
            progress,
            speed: self.speed.round() as u64,
            status: self.status,
            error: self.error.clone(),
            saved_path: self
                .saved_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            peer_name: self.peer_name.clone(),
            executable: self.dir == Dir::In && is_executable(&self.file_name),
            lan: self.status.is_active() && self.shared.lan.load(Ordering::Relaxed),
        }
    }
}

struct Conn {
    id: u64,
    conn: Connection,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    /// Bytes in `tx` not written yet.
    queued: Arc<AtomicU64>,
    since: Instant,
    outgoing: bool,
}

impl Conn {
    /// Queue a frame; a peer that lets too much pile up is cut off.
    fn push(&self, data: Vec<u8>) {
        let len = data.len() as u64;
        if self.queued.fetch_add(len, Ordering::Relaxed) + len > MAX_QUEUED as u64 {
            self.conn
                .close(VarInt::from_u32(CLOSE_REJECTED), b"not reading");
            return;
        }
        let _ = self.tx.send(data);
    }
}

struct State {
    me: String,
    secret: SecretKey,
    nickname: String,
    friends: JsonStore<Vec<Friend>>,
    messages: JsonStore<Messages>,
    groups: HashMap<String, groups::GroupState>,
    gmeta: JsonStore<groups::GroupsMeta>,
    groups_dir: PathBuf,
    conns: HashMap<String, Conn>,
    dialing: HashSet<String>,
    fast_retries: HashMap<String, u8>,
    transfers: HashMap<String, Transfer>,
    chat_last_sent: HashMap<String, Instant>,
    rate: HashMap<String, VecDeque<Instant>>,
    /// What peers told us of their local-network address, and when the
    /// connection to one last failed.
    lan_peers: HashMap<String, lan::LanPeer>,
    lan_down: HashMap<String, Instant>,
    /// What we tell peers of ours, once we have an address.
    lan_advert: Option<Value>,
    events: Vec<Event>,
    next_id: u64,
}

impl State {
    fn is_friend(&self, id: &str) -> bool {
        self.friends.data.iter().any(|f| f.id == id)
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn add_transfer(
        &mut self,
        id: String,
        dir: Dir,
        friend_id: &str,
        file_name: String,
        file_size: u64,
        status: Status,
    ) -> String {
        let seq = self.next_id();
        let t = Transfer {
            id: id.clone(),
            seq,
            dir,
            friend_id: friend_id.to_string(),
            file_name,
            file_size,
            status,
            error: None,
            shared: Arc::new(Shared {
                bytes: AtomicU64::new(0),
                session: watch::Sender::new(0),
                writer: tokio::sync::Mutex::new(None),
                lan: AtomicBool::new(false),
            }),
            ended: None,
            src_path: None,
            src_mtime: None,
            conn_id: None,
            part_path: None,
            saved_path: None,
            resyncing: false,
            hidden: false,
            serve: false,
            repo: None,
            peer_name: None,
            decision: None,
            speed: 0.0,
            sample_bytes: 0,
            sample_time: Instant::now(),
        };
        self.transfers.insert(id.clone(), t);
        id
    }

    fn send(&self, friend_id: &str, msg: Value) {
        if let Some(c) = self.conns.get(friend_id) {
            c.push(frame(&msg));
        }
    }

    /// Forget finished transfers nobody looks at any more.
    fn forget_finished(&mut self) {
        self.transfers.retain(|_, t| {
            let old = t.ended.is_some_and(|e| e.elapsed() > FORGET_FINISHED);
            !(old && (t.hidden || t.serve))
        });
    }

    fn emit_transfer(&mut self, id: &str) {
        let Some(t) = self.transfers.get_mut(id).filter(|t| !t.hidden) else {
            return;
        };
        let now = Instant::now();
        let bytes = t.bytes();
        let dt = now.duration_since(t.sample_time).as_secs_f64();
        if !t.status.is_active() {
            t.speed = 0.0;
            t.sample_bytes = bytes;
            t.sample_time = now;
        } else if dt >= 0.4 {
            let instant = bytes.saturating_sub(t.sample_bytes) as f64 / dt;
            t.speed = if t.speed > 0.0 {
                t.speed * 0.5 + instant * 0.5
            } else {
                instant
            };
            t.sample_bytes = bytes;
            t.sample_time = now;
        }
        let view = t.view();
        self.events.push(Event::Transfer(view));
    }

    fn set_status(&mut self, id: &str, status: Status, error: Option<String>) {
        let Some(t) = self.transfers.get_mut(id) else {
            return;
        };
        if t.status == status && error.is_none() {
            return;
        }
        let was_active = t.status.is_active();
        t.status = status;
        if status.is_final() {
            // Dropping it tells an external sender the file is not coming.
            t.decision = None;
            t.ended = Some(Instant::now());
        }
        if error.is_some() {
            t.error = error;
        }
        if !was_active {
            t.speed = 0.0;
            t.sample_bytes = t.bytes();
            t.sample_time = Instant::now();
        }
        self.emit_transfer(id);
    }

    fn is_rate_limited(&mut self, key: &str) -> bool {
        self.over_limit(key, RATE_LIMIT)
    }

    /// Counts one more event for `key`; true once more than `limit` happened
    /// within `RATE_WINDOW` (the extra ones are not counted).
    fn over_limit(&mut self, key: &str, limit: usize) -> bool {
        let now = Instant::now();
        let bucket = self.rate.entry(key.to_string()).or_default();
        while bucket
            .front()
            .is_some_and(|t| now.duration_since(*t) >= RATE_WINDOW)
        {
            bucket.pop_front();
        }
        let limited = bucket.len() >= limit;
        if !limited {
            bucket.push_back(now);
        }
        limited
    }

    /// (Re)send every message the friend has not acknowledged yet.
    fn flush_outbox(&mut self, friend_id: &str, force: bool) {
        let Some(c) = self.conns.get(friend_id) else {
            return;
        };
        let Some(list) = self.messages.data.get(friend_id) else {
            return;
        };
        let now = Instant::now();
        for m in list
            .iter()
            .filter(|m| m.status == Some(MessageStatus::Pending))
        {
            let recent = self
                .chat_last_sent
                .get(&m.id)
                .is_some_and(|t| now.duration_since(*t) < CHAT_RETRY);
            if !force && recent {
                continue;
            }
            self.chat_last_sent.insert(m.id.clone(), now);
            c.push(frame(
                &json!({ "type": "chat", "id": m.id, "text": m.text, "ts": m.ts }),
            ));
        }
    }
}

fn frame(msg: &Value) -> Vec<u8> {
    let json = serde_json::to_vec(msg).unwrap_or_default();
    let mut out = Vec::with_capacity(4 + json.len());
    out.extend_from_slice(&(json.len() as u32).to_be_bytes());
    out.extend_from_slice(&json);
    out
}

async fn read_frame(recv: &mut RecvStream) -> Option<Value> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.ok()?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return None;
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await.ok()?;
    // Malformed JSON is skipped, not fatal.
    Some(serde_json::from_slice(&buf).unwrap_or(Value::Null))
}

/// Resolves once the transfer's session differs from `session`.
async fn session_changed(rx: &mut watch::Receiver<u64>, session: u64) {
    loop {
        if *rx.borrow_and_update() != session {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

// ─── File names ──────────────────────────────────────────────────────────────

/// Remove control characters and the invisible ones that change how text
/// reads (e.g. U+202E turns "invoice\u{202E}fdp.exe" into "invoiceexe.pdf").
pub fn strip_invisible(s: &str) -> String {
    s.chars()
        .filter(|c| {
            let c = *c as u32;
            !(c < 0x20
                || (0x7f..=0x9f).contains(&c)
                || c == 0xad
                || c == 0x061c
                || c == 0x180e
                || (0x200b..=0x200f).contains(&c)
                || (0x202a..=0x202e).contains(&c)
                || (0x2060..=0x206f).contains(&c)
                || c == 0xfeff
                || (0xfff9..=0xfffb).contains(&c))
        })
        .collect()
}

/// Whether opening the file may run code.
pub fn is_executable(name: &str) -> bool {
    const RISKY: &[&str] = &[
        "exe", "com", "scr", "pif", "bat", "cmd", "msi", "msp", "msix", "appx", "appinstaller",
        "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "hta", "cpl", "lnk", "url", "scf",
        "reg", "jar", "dll", "sys", "inf", "chm", "iso", "img", "vhd", "vhdx", "application",
        "settingcontent-ms", "library-ms", "docm", "xlsm", "pptm", "dotm", "xlam", "ppam", "sh",
        "command", "app", "pkg", "dmg", "deb", "rpm", "appimage", "run", "desktop",
    ];
    let ext = extension(name).trim_start_matches('.').to_lowercase();
    RISKY.contains(&ext.as_str())
}

pub fn sanitize_file_name(name: &str) -> String {
    let unified = strip_invisible(name).replace('\\', "/");
    let base = unified
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    let replaced: String = base
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*".contains(c) || (c as u32) < 0x20 {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut n = replaced.trim_end_matches(['.', ' ']).trim().to_string();

    // Windows device names, also with an extension or trailing spaces.
    let stem = n.split('.').next().unwrap_or("").trim_end().to_lowercase();
    let port = |prefix: &str| {
        stem.strip_prefix(prefix).is_some_and(|rest| {
            let mut c = rest.chars();
            matches!((c.next(), c.next()), (Some(d), None) if d.is_ascii_digit() || "¹²³".contains(d))
        })
    };
    let reserved = matches!(
        stem.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$" | "clock$"
    ) || port("com")
        || port("lpt");
    if reserved {
        n = format!("_{n}");
    }
    if n.is_empty() || n == "." || n == ".." {
        n = "fichier".into();
    }
    if n.chars().count() > 180 {
        let ext: String = extension(&n).chars().take(20).collect();
        let keep = 180 - ext.chars().count();
        n = n.chars().take(keep).collect::<String>() + &ext;
    }
    n
}

/// Extension including the dot, as Node's `path.extname`.
fn extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i..],
        _ => "",
    }
}

/// Create an empty file named `name` in `dir`, or "name (1)", "name (2)"…
/// if taken. Creating it claims the name: two downloads finishing together
/// can never pick the same one.
fn reserve_path(dir: &Path, name: &str) -> io::Result<PathBuf> {
    let ext = extension(name);
    let base = &name[..name.len() - ext.len()];
    for i in 0..10_000 {
        let candidate = if i == 0 {
            dir.join(name)
        } else {
            dir.join(format!("{base} ({i}){ext}"))
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

/// A new, empty `.part` file for receiving `name` in `dir`.
fn create_part(dir: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    std::fs::create_dir_all(dir)?;
    loop {
        let suffix = hex::encode(rand::random::<[u8; 4]>());
        let path = dir.join(format!("{name}.{suffix}.part"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Whether `name` is one of our `.part` files: "<name>.<8 hex>.part".
fn is_part_file(name: &str) -> bool {
    let Some(rest) = name.strip_suffix(".part") else {
        return false;
    };
    rest.len() > 9
        && rest.is_char_boundary(rest.len() - 9)
        && rest[rest.len() - 9..].starts_with('.')
        && rest[rest.len() - 8..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Remove `.part` files left by a previous run (transfers do not survive a
/// restart) in the download folder and the shared-files folders.
fn remove_stale_parts(download_dir: &Path) {
    let mut dirs = vec![download_dir.to_path_buf()];
    if let Ok(list) = std::fs::read_dir(download_dir.join(groups::REPO_FOLDER)) {
        dirs.extend(list.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    for dir in dirs {
        let Ok(list) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in list.flatten() {
            let name = entry.file_name();
            let is_file = entry.file_type().is_ok_and(|t| t.is_file());
            if is_file && name.to_str().is_some_and(is_part_file) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Mark a received file as coming from another computer, so the system
/// warns before running it (SmartScreen, Office Protected View, Gatekeeper).
fn mark_downloaded(path: &Path) {
    #[cfg(windows)]
    {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let _ = std::fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n");
    }
    #[cfg(target_os = "macos")]
    {
        let value = format!("0081;{:x};P2P Share;", now_ms() / 1000);
        let _ = std::process::Command::new("xattr")
            .args(["-w", "com.apple.quarantine", &value])
            .arg(path)
            .status();
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = path;
}

// ─── Node ────────────────────────────────────────────────────────────────────

struct Inner {
    state: Mutex<State>,
    endpoint: Endpoint,
    rt: Handle,
    download_dir: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    on_event: Arc<dyn Fn(Event) + Send + Sync>,
    custom_relay: bool,
    lan: Option<lan::Lan>,
}

#[derive(Clone)]
pub struct Node(Arc<Inner>);

impl Node {
    pub async fn start(opts: Options) -> io::Result<Node> {
        let transport = QuicTransportConfig::builder()
            // Large flow-control windows keep the pipe full on fast/long links.
            .stream_receive_window(VarInt::from_u32(32 << 20))
            .send_window(64 << 20)
            .build();
        let secret = opts.secret.clone();
        let mut builder = Endpoint::builder(presets::N0)
            .secret_key(opts.secret)
            .alpns(vec![ALPN.to_vec()])
            .transport_config(transport);
        let custom_relay = opts.relay.is_some();
        if let Some(relay) = &opts.relay {
            // Our relay becomes our only home relay, so friends always reach
            // us through it; we still publish to and look up in iroh's public
            // directory (kept by the preset) as well as the relay's own.
            let mut config = RelayConfig::new(relay.url.clone(), None);
            if let Some(token) = &relay.token {
                config = config.with_auth_token(token.clone());
            }
            builder = builder
                .relay_mode(RelayMode::Custom(RelayMap::from(config)))
                .address_lookup(PkarrPublisher::builder(relay.pkarr_url()))
                .address_lookup(PkarrResolver::builder(relay.pkarr_url()));
        }
        let endpoint = builder
            .bind()
            .await
            .map_err(io::Error::other)?;

        // Without it, everything goes over QUIC as before.
        let (lan, lan_listener) = match opts.lan {
            Some(identity) => match lan::Lan::bind(identity).await {
                Ok((lan, listener)) => (Some(lan), Some(listener)),
                Err(e) => {
                    eprintln!("réseau local indisponible : {e}");
                    (None, None)
                }
            },
            None => (None, None),
        };

        let _ = std::fs::create_dir_all(&opts.groups_dir);
        let (gmeta, group_states) = groups::load(&opts.groups_dir);
        let state = State {
            me: endpoint.id().to_string(),
            secret,
            nickname: opts.nickname,
            friends: opts.friends,
            messages: opts.messages,
            groups: group_states,
            gmeta,
            groups_dir: opts.groups_dir,
            conns: HashMap::new(),
            dialing: HashSet::new(),
            fast_retries: HashMap::new(),
            transfers: HashMap::new(),
            chat_last_sent: HashMap::new(),
            rate: HashMap::new(),
            lan_peers: HashMap::new(),
            lan_down: HashMap::new(),
            lan_advert: None,
            events: Vec::new(),
            next_id: 0,
        };
        let node = Node(Arc::new(Inner {
            state: Mutex::new(state),
            endpoint,
            rt: Handle::current(),
            download_dir: opts.download_dir,
            on_event: opts.on_event,
            custom_relay,
            lan,
        }));
        if let Some(listener) = lan_listener {
            node.start_lan(listener);
        }

        let download_dir = (node.0.download_dir)();
        node.spawn(async move {
            let _ = tokio::task::spawn_blocking(move || remove_stale_parts(&download_dir)).await;
        });
        node.spawn(node.clone().accept_loop());
        node.spawn(node.clone().timers());
        node.with(|s| node.reconnect_tick(s));
        // Dials made before we reach our relay often fail: once online, give
        // every peer a fresh round of quick retries.
        let n = node.clone();
        node.spawn(async move {
            let _ = tokio::time::timeout(ONLINE_TIMEOUT, n.0.endpoint.online()).await;
            n.with(|s| {
                s.fast_retries.clear();
                n.reconnect_tick(s);
            });
        });
        Ok(node)
    }

    /// Close every connection cleanly so friends see us offline right away.
    pub async fn shutdown(&self) {
        self.save_now();
        self.0.endpoint.close().await;
    }

    pub fn save_now(&self) {
        self.with(|s| {
            let _ = s.friends.save_now();
            let _ = s.messages.save_now();
            let _ = s.gmeta.save_now();
            for (path, lines) in s.take_group_writes() {
                let _ = groups::append_lines(&path, &lines);
            }
        });
    }

    pub fn id(&self) -> String {
        self.0.endpoint.id().to_string()
    }

    /// Our home relay, `None` until one is picked.
    pub fn relay_status(&self) -> Option<RelayStatusView> {
        let status = self.0.endpoint.home_relay_status().get();
        let home = status.first()?;
        let error = if home.is_connected() {
            None
        } else if let Some(reason) = home.auth_denied_reason() {
            Some(format!("Refusé par le relais : {reason}"))
        } else {
            home.last_error().map(|e| e.to_string())
        };
        Some(RelayStatusView {
            url: home.url().to_string(),
            custom: self.0.custom_relay,
            connected: home.is_connected(),
            error,
        })
    }

    fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(&self, f: F) {
        self.0.rt.spawn(f);
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let (result, events) = {
            let mut s = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let result = f(&mut s);
            (result, std::mem::take(&mut s.events))
        };
        for e in events {
            (self.0.on_event)(e);
        }
        result
    }

    async fn timers(self) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_reconnect = Instant::now();
        let mut last_messages_save = Instant::now();
        let mut last_wall = SystemTime::now();
        loop {
            tick.tick().await;
            if self.0.endpoint.is_closed() {
                return;
            }
            // A jump of the wall clock means the computer was asleep: detect
            // the new network and dial offline friends again right away.
            let wall = SystemTime::now();
            let woke = wall.duration_since(last_wall).is_ok_and(|d| d > WAKE_GAP);
            last_wall = wall;
            if woke {
                self.wake();
            }

            let (stores, logs) = self.with(|s| {
                let active: Vec<String> = s
                    .transfers
                    .values()
                    .filter(|t| t.status.is_active())
                    .map(|t| t.id.clone())
                    .collect();
                for id in active {
                    s.emit_transfer(&id);
                }
                if last_reconnect.elapsed() >= RECONNECT_INTERVAL {
                    last_reconnect = Instant::now();
                    self.reconnect_tick(s);
                    s.forget_finished();
                }
                // The whole history is rewritten each time: not on every message.
                let messages = if last_messages_save.elapsed() >= MESSAGES_SAVE_INTERVAL {
                    last_messages_save = Instant::now();
                    s.messages.take_dirty()
                } else {
                    None
                };
                (
                    [
                        messages,
                        s.friends.take_dirty(),
                        s.gmeta.take_dirty(),
                    ],
                    s.take_group_writes(),
                )
            });
            for (path, json) in stores.into_iter().flatten() {
                let _ = tokio::task::spawn_blocking(move || write_atomic(&path, &json)).await;
            }
            if !logs.is_empty() {
                let _ = tokio::task::spawn_blocking(move || {
                    for (path, lines) in logs {
                        let _ = groups::append_lines(&path, &lines);
                    }
                })
                .await;
            }
        }
    }

    /// Call after the computer wakes up.
    pub fn wake(&self) {
        let node = self.clone();
        self.spawn(async move {
            node.0.endpoint.network_change().await;
            node.with(|s| {
                s.fast_retries.clear();
                node.reconnect_tick(s);
            });
        });
    }

    fn reconnect_tick(&self, s: &mut State) {
        let mut ids: Vec<String> = s.friends.data.iter().map(|f| f.id.clone()).collect();
        let others: Vec<String> = s
            .group_peers()
            .into_iter()
            .filter(|p| !s.is_friend(p))
            .collect();
        ids.extend(others);
        for id in ids {
            if !s.conns.contains_key(&id) {
                self.dial(s, &id);
            } else if s.is_friend(&id) {
                s.flush_outbox(&id, false);
            }
        }
        self.groups_tick(s);
    }

    // ─── Friends ─────────────────────────────────────────────────────────────

    pub fn friends(&self) -> Vec<FriendView> {
        self.with(|s| friend_views(s))
    }

    pub fn friend_name(&self, id: &str) -> Option<String> {
        self.with(|s| {
            s.friends
                .data
                .iter()
                .find(|f| f.id == id)
                .map(|f| f.name.clone())
        })
    }

    pub fn add_friend(&self, id: &str, name: &str) -> Result<Vec<FriendView>, &'static str> {
        let id = id.trim().to_lowercase();
        let name = name.trim().to_string();
        if !is_valid_id(&id) {
            return Err("invalid_id");
        }
        if id == self.id() {
            return Err("self");
        }
        if name.is_empty() {
            return Err("no_name");
        }
        Ok(self.with(|s| {
            match s.friends.data.iter_mut().find(|f| f.id == id) {
                Some(f) => f.name = name,
                None => s.friends.data.push(Friend {
                    id: id.clone(),
                    name,
                }),
            }
            // Stays dirty on failure: the timer retries.
            let _ = s.friends.save_now();
            s.fast_retries.remove(&id);
            self.dial(s, &id);
            friend_views(s)
        }))
    }

    pub fn remove_friend(&self, id: &str) -> Vec<FriendView> {
        self.with(|s| {
            let running: Vec<String> = s
                .transfers
                .values()
                .filter(|t| t.friend_id == id && !t.status.is_final())
                .map(|t| t.id.clone())
                .collect();
            for tid in running {
                self.cancel(s, &tid);
            }
            s.friends.data.retain(|f| f.id != id);
            // Stays dirty on failure: the timer retries.
            let _ = s.friends.save_now();
            // Still a co-member of one of our groups: keep the connection.
            if !s.is_allowed(id) {
                if let Some(c) = s.conns.get(id) {
                    c.conn.close(VarInt::from_u32(CLOSE_REJECTED), b"removed");
                }
            }
            friend_views(s)
        })
    }

    fn dial(&self, s: &mut State, friend_id: &str) {
        if !is_valid_id(friend_id)
            || s.conns.contains_key(friend_id)
            || !s.dialing.insert(friend_id.to_string())
        {
            return;
        }
        let Ok(key) = PublicKey::from_str(friend_id) else {
            return;
        };
        let node = self.clone();
        let friend_id = friend_id.to_string();
        self.spawn(async move {
            if let Ok(Ok(conn)) =
                tokio::time::timeout(DIAL_TIMEOUT, node.0.endpoint.connect(key, ALPN)).await
            {
                node.clone().setup(conn, true).await;
            }
            let retry = node.with(|s| {
                s.dialing.remove(&friend_id);
                if s.conns.contains_key(&friend_id) || !s.is_allowed(&friend_id) {
                    return false;
                }
                let tries = s.fast_retries.entry(friend_id.clone()).or_insert(0);
                *tries += 1;
                *tries <= FAST_RETRIES
            });
            if retry {
                tokio::time::sleep(FAST_RETRY_DELAY).await;
                node.with(|s| node.dial(s, &friend_id));
            }
        });
    }

    // ─── Connections ─────────────────────────────────────────────────────────

    async fn accept_loop(self) {
        while let Some(incoming) = self.0.endpoint.accept().await {
            let node = self.clone();
            self.spawn(async move {
                let Ok(Ok(conn)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, incoming).await else {
                    return;
                };
                // Only friends and members of our groups may connect.
                let friend_id = conn.remote_id().to_string();
                if !node.with(|s| s.is_allowed(&friend_id)) {
                    conn.close(VarInt::from_u32(CLOSE_REJECTED), b"not a friend");
                    return;
                }
                node.setup(conn, false).await;
            });
        }
    }

    /// Open (dialer) or accept (listener) the control stream, then register.
    async fn setup(self, conn: Connection, outgoing: bool) {
        let streams = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            if outgoing {
                let (mut send, recv) = conn.open_bi().await.ok()?;
                // A stream only becomes visible to the peer once data is sent.
                send.write_all(&frame(&json!({ "type": "hello" })))
                    .await
                    .ok()?;
                Some((send, recv))
            } else {
                let (send, mut recv) = conn.accept_bi().await.ok()?;
                read_frame(&mut recv).await?;
                Some((send, recv))
            }
        })
        .await;
        match streams {
            Ok(Some((send, recv))) => self.with(|s| self.register(s, conn, send, recv, outgoing)),
            _ => conn.close(VarInt::from_u32(CLOSE_NORMAL), b"handshake"),
        }
    }

    fn register(
        &self,
        s: &mut State,
        conn: Connection,
        send: SendStream,
        recv: RecvStream,
        outgoing: bool,
    ) {
        let friend_id = conn.remote_id().to_string();
        if !s.is_allowed(&friend_id) {
            conn.close(VarInt::from_u32(CLOSE_REJECTED), b"not a friend");
            return;
        }
        // Both sides may dial each other at the same time. Within a short
        // window, both keep the connection dialed by the smaller ID; after
        // that the newest connection replaces a possibly dead one.
        let mut was_online = false;
        if let Some(old) = s.conns.get(&friend_id) {
            let dialer = |out: bool| {
                if out {
                    s.me.as_str()
                } else {
                    friend_id.as_str()
                }
            };
            let keep_old = old.conn.close_reason().is_none()
                && old.since.elapsed() < DUPLICATE_WINDOW
                && dialer(old.outgoing) < dialer(outgoing);
            if keep_old {
                conn.close(VarInt::from_u32(CLOSE_DUPLICATE), b"duplicate");
                return;
            }
            if let Some(old) = s.conns.remove(&friend_id) {
                old.conn
                    .close(VarInt::from_u32(CLOSE_DUPLICATE), b"duplicate");
                was_online = true;
            }
        }

        let id = s.next_id();
        let (tx, rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicU64::new(0));
        self.spawn(write_control(send, rx, queued.clone()));
        self.spawn(self.clone().read_control(friend_id.clone(), id, recv));
        self.spawn(self.clone().accept_streams(friend_id.clone(), conn.clone()));
        let node = self.clone();
        let watched = conn.clone();
        let fid = friend_id.clone();
        self.spawn(async move {
            watched.closed().await;
            node.with(|s| node.on_close(s, &fid, id));
        });

        s.fast_retries.remove(&friend_id);
        s.conns.insert(
            friend_id.clone(),
            Conn {
                id,
                conn,
                tx,
                queued,
                since: Instant::now(),
                outgoing,
            },
        );
        if !was_online {
            s.events.push(Event::FriendStatus {
                friend_id: friend_id.clone(),
                online: true,
            });
        }

        s.send_lan_advert(&friend_id);
        s.flush_outbox(&friend_id, true);
        let to_offer: Vec<String> = s
            .transfers
            .values()
            .filter(|t| {
                t.dir == Dir::Out
                    && !t.serve
                    && t.friend_id == friend_id
                    && !t.status.is_final()
                    && t.status != Status::Sending
            })
            .map(|t| t.id.clone())
            .collect();
        for tid in to_offer {
            self.offer(s, &tid);
        }
        self.groups_on_connect(s, &friend_id);
    }

    fn on_close(&self, s: &mut State, friend_id: &str, conn_id: u64) {
        if s.conns.get(friend_id).is_some_and(|c| c.id == conn_id) {
            s.conns.remove(friend_id);
            s.events.push(Event::FriendStatus {
                friend_id: friend_id.to_string(),
                online: false,
            });
        }
        let live = s.conns.contains_key(friend_id);
        let affected: Vec<String> = s
            .transfers
            .values()
            .filter(|t| t.friend_id == friend_id && !t.status.is_final())
            .map(|t| t.id.clone())
            .collect();
        for tid in affected {
            let t = &s.transfers[&tid];
            let (dir, status, bound) = (t.dir, t.status, t.conn_id == Some(conn_id));
            if t.serve {
                // The downloader asks again (here or elsewhere) when it can.
                if bound || !live {
                    s.transfers.get_mut(&tid).unwrap().shared.next_session();
                    s.set_status(&tid, Status::Cancelled, None);
                }
                continue;
            }
            if t.repo.is_some() {
                if !live {
                    self.next_source(s, &tid, friend_id);
                }
                continue;
            }
            if dir == Dir::Out && bound {
                let t = s.transfers.get_mut(&tid).unwrap();
                t.shared.next_session();
                t.conn_id = None;
                if status == Status::Sending {
                    s.set_status(&tid, Status::Paused, None);
                }
                if live {
                    self.offer(s, &tid);
                }
            } else if !live && dir == Dir::Out && status == Status::WaitingConsent {
                s.set_status(&tid, Status::Connecting, None);
            } else if !live && dir == Dir::In && status == Status::Receiving {
                s.set_status(&tid, Status::Paused, None);
            }
        }
    }

    async fn read_control(self, friend_id: String, conn_id: u64, mut recv: RecvStream) {
        while let Some(msg) = read_frame(&mut recv).await {
            self.with(|s| self.on_control(s, &friend_id, conn_id, &msg));
        }
        // Nothing is read from this connection any more (oversized frame,
        // stream closed…): close it rather than keep a peer that looks online.
        self.with(|s| {
            if let Some(c) = s.conns.get(&friend_id).filter(|c| c.id == conn_id) {
                c.conn
                    .close(VarInt::from_u32(CLOSE_NORMAL), b"control stream closed");
            }
        });
    }

    async fn accept_streams(self, friend_id: String, conn: Connection) {
        while let Ok(recv) = conn.accept_uni().await {
            self.spawn(self.clone().receive_stream(friend_id.clone(), recv));
        }
    }

    fn on_control(&self, s: &mut State, friend_id: &str, conn_id: u64, msg: &Value) {
        // Whatever is dropped here is sent again later (chats, offers, group
        // syncs are all retried).
        if s.over_limit(&format!("control:{friend_id}"), CONTROL_RATE_LIMIT) {
            return;
        }
        let tid = msg.get("tid").and_then(Value::as_str).filter(|t| is_tid(t));
        // Group co-members who are not friends get no private chat or offers.
        let friend = s.is_friend(friend_id);
        match msg.get("type").and_then(Value::as_str) {
            Some("chat") if friend => self.on_chat(s, friend_id, msg),
            Some("chat_ack") if friend => self.on_chat_ack(s, friend_id, msg),
            Some("file_offer") if friend => self.on_offer(s, friend_id, msg),
            Some("file_accept") => self.on_accept(s, friend_id, conn_id, msg),
            Some("file_reject") => self.on_peer_stop(s, friend_id, tid, Status::Declined),
            Some("file_cancel") => self.on_peer_stop(s, friend_id, tid, Status::Cancelled),
            Some("file_complete") => self.on_complete(s, friend_id, tid),
            Some("group_invite") if friend => self.on_group_invite(s, friend_id, msg),
            Some("group_decline") => self.on_group_decline(s, friend_id, msg),
            Some("group_have") => self.on_group_have(s, friend_id, msg),
            Some("group_entries") => self.on_group_entries(s, friend_id, msg),
            Some("blob_req") => self.on_blob_req(s, friend_id, conn_id, msg),
            Some("blob_missing") => self.on_blob_missing(s, friend_id, msg),
            Some("lan") => self.on_lan(s, friend_id, msg),
            _ => {}
        }
    }

    // ─── Chat ────────────────────────────────────────────────────────────────

    pub fn messages(&self, friend_id: &str) -> Vec<ChatMessage> {
        self.with(|s| s.messages.data.get(friend_id).cloned().unwrap_or_default())
    }

    pub fn send_message(&self, friend_id: &str, text: &str) -> Result<ChatMessage, &'static str> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty");
        }
        if js_len(text) > MAX_TEXT {
            return Err("too_long");
        }
        let me = self.id();
        Ok(self.with(|s| {
            let entry = ChatMessage {
                id: uuid::Uuid::new_v4().to_string(),
                from: me,
                text: text.to_string(),
                ts: now_ms(),
                status: Some(MessageStatus::Pending),
            };
            let list = s.messages.data.entry(friend_id.to_string()).or_default();
            list.push(entry.clone());
            if list.len() > MAX_HISTORY {
                let extra = list.len() - MAX_HISTORY;
                list.drain(..extra);
            }
            s.messages.mark_dirty();
            if s.conns.contains_key(friend_id) {
                s.flush_outbox(friend_id, true);
            } else {
                self.dial(s, friend_id);
            }
            entry
        }))
    }

    fn on_chat(&self, s: &mut State, friend_id: &str, msg: &Value) {
        let Some(id) = msg
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| id.len() <= 64)
        else {
            return;
        };
        let Some(text) = msg.get("text").and_then(Value::as_str) else {
            return;
        };
        if text.is_empty() || js_len(text) > MAX_TEXT {
            return;
        }
        let known = s
            .messages
            .data
            .get(friend_id)
            .is_some_and(|l| l.iter().any(|m| m.id == id));
        if !known {
            // No ack when flooded: the sender keeps the message and retries later.
            if s.is_rate_limited(friend_id) {
                return;
            }
            let entry = ChatMessage {
                id: id.to_string(),
                from: friend_id.to_string(),
                text: text.to_string(),
                ts: now_ms(),
                status: None,
            };
            let list = s.messages.data.entry(friend_id.to_string()).or_default();
            list.push(entry.clone());
            if list.len() > MAX_HISTORY {
                let extra = list.len() - MAX_HISTORY;
                list.drain(..extra);
            }
            s.messages.mark_dirty();
            s.events.push(Event::Message {
                friend_id: friend_id.to_string(),
                message: entry,
            });
        }
        s.send(friend_id, json!({ "type": "chat_ack", "id": id }));
    }

    fn on_chat_ack(&self, s: &mut State, friend_id: &str, msg: &Value) {
        let Some(id) = msg.get("id").and_then(Value::as_str) else {
            return;
        };
        let me = s.me.clone();
        let Some(list) = s.messages.data.get_mut(friend_id) else {
            return;
        };
        let Some(m) = list.iter_mut().find(|m| m.id == id && m.from == me) else {
            return;
        };
        if m.status != Some(MessageStatus::Pending) {
            return;
        }
        m.status = Some(MessageStatus::Delivered);
        s.chat_last_sent.remove(id);
        s.messages.mark_dirty();
        s.events.push(Event::MessageStatus {
            friend_id: friend_id.to_string(),
            id: id.to_string(),
        });
    }

    // ─── File transfers ──────────────────────────────────────────────────────

    pub fn transfers(&self) -> Vec<TransferView> {
        self.with(|s| {
            let mut list: Vec<&Transfer> = s.transfers.values().filter(|t| !t.hidden).collect();
            list.sort_by_key(|t| t.seq);
            list.into_iter().map(Transfer::view).collect()
        })
    }

    /// Remove finished transfers from the list, for good (also after the
    /// window is rebuilt).
    pub fn clear_finished(&self) {
        self.with(|s| {
            for t in s.transfers.values_mut().filter(|t| t.status.is_final()) {
                t.hidden = true;
            }
        });
    }

    pub fn saved_path(&self, id: &str) -> Option<PathBuf> {
        self.with(|s| s.transfers.get(id).and_then(|t| t.saved_path.clone()))
    }

    // ── Sender side ──

    pub fn send_files(&self, friend_id: &str, paths: &[PathBuf]) -> usize {
        let files: Vec<(PathBuf, u64, Option<SystemTime>)> = paths
            .iter()
            .filter_map(|p| {
                std::fs::metadata(p)
                    .ok()
                    .filter(|m| m.is_file())
                    .map(|m| (p.clone(), m.len(), m.modified().ok()))
            })
            .collect();
        self.with(|s| {
            for (path, size, mtime) in &files {
                let tid = hex::encode(rand::random::<[u8; TID_BYTES]>());
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                s.add_transfer(
                    tid.clone(),
                    Dir::Out,
                    friend_id,
                    name,
                    *size,
                    Status::Connecting,
                );
                let t = s.transfers.get_mut(&tid).unwrap();
                t.src_path = Some(path.clone());
                t.src_mtime = *mtime;
                if s.conns.contains_key(friend_id) {
                    self.offer(s, &tid);
                } else {
                    s.emit_transfer(&tid);
                    self.dial(s, friend_id);
                }
            }
        });
        files.len()
    }

    fn offer(&self, s: &mut State, tid: &str) {
        let Some(t) = s.transfers.get(tid) else {
            return;
        };
        s.send(
            &t.friend_id,
            json!({ "type": "file_offer", "tid": t.id, "name": t.file_name, "size": t.file_size }),
        );
        if t.status == Status::Connecting {
            s.set_status(tid, Status::WaitingConsent, None);
        } else {
            s.emit_transfer(tid);
        }
    }

    fn on_accept(&self, s: &mut State, friend_id: &str, conn_id: u64, msg: &Value) {
        let Some(tid) = msg.get("tid").and_then(Value::as_str) else {
            return;
        };
        let Some(t) = s.transfers.get_mut(tid) else {
            return;
        };
        if t.dir != Dir::Out || t.friend_id != friend_id || t.status.is_final() {
            return;
        }
        let Some(offset) = msg
            .get("offset")
            .and_then(Value::as_u64)
            .filter(|o| *o <= t.file_size)
        else {
            return;
        };
        let Some(conn) = s
            .conns
            .get(friend_id)
            .filter(|c| c.id == conn_id)
            .map(|c| c.conn.clone())
        else {
            return;
        };
        if offset > 0 && t.src_mtime.is_some() {
            // Resuming: the receiver's first part must come from the same file.
            let unchanged = t.src_path.as_ref().and_then(|p| std::fs::metadata(p).ok()).is_some_and(
                |m| m.len() == t.file_size && m.modified().ok() == t.src_mtime,
            );
            if !unchanged {
                let tid = tid.to_string();
                s.send(friend_id, json!({ "type": "file_cancel", "tid": tid }));
                self.stop_transfer(
                    s,
                    &tid,
                    Status::Error,
                    Some("Fichier source modifié pendant l’envoi".into()),
                );
                return;
            }
        }
        t.shared.bytes.store(offset, Ordering::Relaxed);
        t.conn_id = Some(conn_id);
        let session = t.shared.next_session();
        let mut job = Job {
            tid: tid.to_string(),
            peer: friend_id.to_string(),
            shared: t.shared.clone(),
            session,
            conn,
            lan: None,
            path: t.src_path.clone().unwrap_or_default(),
            start: offset,
            size: t.file_size,
        };
        if self.0.lan.is_some() {
            job.lan = s.lan_target(friend_id);
        }
        s.set_status(tid, Status::Sending, None);
        self.spawn(self.clone().pump(job));
    }

    async fn pump(self, mut job: Job) {
        let (tid, session) = (job.tid.clone(), job.session);
        let mut sink = None;
        if let Some(target) = job.lan.take() {
            sink = self.lan_connect(target).await.map(|t| lan::Sink::Lan(Box::new(t)));
            if sink.is_none() {
                self.with(|s| self.lan_failed(s, &job.peer));
            }
        }
        let result = stream_file(job, sink).await;
        self.with(|s| {
            let Some(t) = s.transfers.get(&tid) else {
                return;
            };
            if t.shared.session() != session || t.status.is_final() {
                return;
            }
            match result {
                // Everything is on the wire: wait for the receiver's file_complete.
                Ok(Streamed::Done) => s.set_status(&tid, Status::Finishing, None),
                // Connection lost: on_close re-offers on the next connection.
                Ok(Streamed::Stopped) => {}
                // Go on over QUIC from where we are; the receiver asks for
                // another offset if it has less.
                Ok(Streamed::LanFailed) => {
                    let (peer, conn_id, bytes) = (t.friend_id.clone(), t.conn_id, t.bytes());
                    self.lan_failed(s, &peer);
                    if let Some(conn_id) = conn_id {
                        let msg = json!({ "tid": tid, "offset": bytes });
                        self.on_accept(s, &peer, conn_id, &msg);
                    }
                }
                Err(e) => {
                    s.send(
                        &t.friend_id.clone(),
                        json!({ "type": "file_cancel", "tid": tid }),
                    );
                    self.stop_transfer(s, &tid, Status::Error, Some(e.to_string()));
                }
            }
        });
    }

    fn on_complete(&self, s: &mut State, friend_id: &str, tid: Option<&str>) {
        let Some(t) = tid.and_then(|tid| s.transfers.get_mut(tid)) else {
            return;
        };
        if t.dir != Dir::Out || t.friend_id != friend_id || t.status.is_final() {
            return;
        }
        t.shared.next_session();
        t.shared.bytes.store(t.file_size, Ordering::Relaxed);
        let id = t.id.clone();
        s.set_status(&id, Status::Completed, None);
    }

    // ── Receiver side ──

    fn on_offer(&self, s: &mut State, friend_id: &str, msg: &Value) {
        let Some(tid) = msg.get("tid").and_then(Value::as_str).filter(|t| is_tid(t)) else {
            return;
        };
        let Some(name) = msg.get("name").and_then(Value::as_str) else {
            return;
        };
        let Some(size) = msg.get("size").and_then(Value::as_u64) else {
            return;
        };
        if let Some(known) = s.transfers.get_mut(tid) {
            if known.dir != Dir::In || known.friend_id != friend_id {
                return;
            }
            // The sender re-offers after every reconnection: answer with our state.
            let reply = match known.status {
                Status::Receiving | Status::Paused => {
                    known.resyncing = false;
                    let offset = known.bytes();
                    s.send(
                        friend_id,
                        json!({ "type": "file_accept", "tid": tid, "offset": offset }),
                    );
                    s.set_status(tid, Status::Receiving, None);
                    None
                }
                Status::Completed => Some("file_complete"),
                Status::Declined => Some("file_reject"),
                Status::Cancelled | Status::Error => Some("file_cancel"),
                _ => None,
            };
            if let Some(kind) = reply {
                s.send(friend_id, json!({ "type": kind, "tid": tid }));
            }
            return;
        }
        // A flood of offers: ignored (a real sender offers again when it
        // reconnects).
        let pending = s
            .transfers
            .values()
            .filter(|t| t.friend_id == friend_id && t.status == Status::Pending)
            .count();
        if pending >= MAX_PENDING_OFFERS || s.is_rate_limited(&format!("offer:{friend_id}")) {
            return;
        }
        let id = s.add_transfer(
            tid.to_string(),
            Dir::In,
            friend_id,
            sanitize_file_name(name),
            size,
            Status::Pending,
        );
        s.emit_transfer(&id);
        let view = s.transfers[&id].view();
        s.events.push(Event::FileOffer(view, 1));
    }

    pub fn respond_to_offer(&self, id: &str, accept: bool) {
        let dir = (self.0.download_dir)();
        let name = self.with(|s| {
            let t = s.transfers.get(id)?;
            if t.dir != Dir::In || t.status != Status::Pending {
                return None;
            }
            if !accept {
                let friend_id = t.friend_id.clone();
                s.send(&friend_id, json!({ "type": "file_reject", "tid": id }));
                s.set_status(id, Status::Declined, None);
                return None;
            }
            Some(t.file_name.clone())
        });
        let Some(name) = name else { return };
        // Disk access outside the lock: the folder may be slow (network, cloud).
        let opened = create_part(&dir, &name);
        self.with(|s| {
            let Some(t) = s.transfers.get_mut(id) else {
                if let Ok((part, _)) = &opened {
                    let _ = std::fs::remove_file(part);
                }
                return;
            };
            if t.status != Status::Pending {
                // Cancelled meanwhile.
                if let Ok((part, _)) = &opened {
                    let _ = std::fs::remove_file(part);
                }
                return;
            }
            let friend_id = t.friend_id.clone();
            let Ok((part, file)) = opened else {
                s.send(&friend_id, json!({ "type": "file_cancel", "tid": id }));
                s.set_status(
                    id,
                    Status::Error,
                    Some("Dossier de téléchargement inaccessible".into()),
                );
                return;
            };
            let writer = BufWriter::with_capacity(DISK_BUFFER, tokio::fs::File::from_std(file));
            // Nobody else holds the writer of a transfer that was pending.
            let Ok(mut slot) = t.shared.writer.try_lock() else {
                let _ = std::fs::remove_file(&part);
                s.send(&friend_id, json!({ "type": "file_cancel", "tid": id }));
                s.set_status(id, Status::Error, Some("Erreur interne".into()));
                return;
            };
            *slot = Some(writer);
            drop(slot);
            t.part_path = Some(part);
            t.shared.bytes.store(0, Ordering::Relaxed);
            let decision = t.decision.take();
            let external = decision.is_some();
            if let Some(tx) = decision {
                let _ = tx.send(true);
            }
            if t.file_size == 0 {
                self.begin_finalize(s, id);
            } else if external {
                // The sender starts uploading once it has our answer.
                s.set_status(id, Status::Receiving, None);
            } else if s.conns.contains_key(&friend_id) {
                s.send(
                    &friend_id,
                    json!({ "type": "file_accept", "tid": id, "offset": 0 }),
                );
                s.set_status(id, Status::Receiving, None);
            } else {
                // Accepted while the sender is offline: we answer its next re-offer.
                s.set_status(id, Status::Paused, None);
            }
        });
    }

    async fn receive_stream(self, friend_id: String, mut recv: RecvStream) {
        let mut header = [0u8; DATA_HEADER];
        let read = tokio::time::timeout(HANDSHAKE_TIMEOUT, recv.read_exact(&mut header)).await;
        if !matches!(read, Ok(Ok(()))) {
            let _ = recv.stop(VarInt::from_u32(0));
            return;
        }
        let tid = hex::encode(&header[..TID_BYTES]);
        let offset = u64::from_be_bytes(header[TID_BYTES..].try_into().unwrap());
        self.receive_data(friend_id, tid, offset, lan::Source::Quic(recv))
            .await;
    }

    /// Write a data stream for `tid` starting at `offset` into its file.
    async fn receive_data(
        self,
        friend_id: String,
        tid: String,
        offset: u64,
        mut recv: lan::Source,
    ) {
        let found = self.with(|s| {
            let t = s.transfers.get(&tid)?;
            let ok = t.dir == Dir::In && t.friend_id == friend_id && t.status == Status::Receiving;
            ok.then(|| (t.shared.clone(), t.shared.session(), t.file_size))
        });
        let Some((shared, session, size)) = found else {
            recv.stop();
            return;
        };

        // Wait for any previous stream of this transfer to release the file.
        let mut slot = shared.writer.lock().await;
        let Some(writer) = slot.as_mut() else { return };
        if shared.session() != session || offset >= size {
            // Nothing left to receive: the file is complete or being saved.
            recv.stop();
            return;
        }
        if offset != shared.bytes.load(Ordering::Relaxed) {
            // Stale or out-of-sync stream: ask once to resume from what we really have.
            recv.stop();
            drop(slot);
            self.with(|s| self.resync(s, &tid));
            return;
        }
        let via_lan = recv.is_lan();
        self.with(|s| {
            if let Some(t) = s.transfers.get_mut(&tid) {
                t.resyncing = false;
                t.shared.lan.store(via_lan, Ordering::Relaxed);
            }
        });

        let mut cancel = shared.session.subscribe();
        let mut pos = offset;
        let mut cut = false;
        let failure: Option<&str> = loop {
            let chunk = tokio::select! {
                c = recv.chunk() => c,
                _ = session_changed(&mut cancel, session) => break None,
            };
            let data = match chunk {
                Ok(Some(data)) => data,
                // Sender restarted or connection lost: keep what we have.
                Ok(None) | Err(_) => {
                    cut = true;
                    break None;
                }
            };
            if pos + data.len() as u64 > size {
                break Some("Taille reçue incohérente");
            }
            if writer.write_all(&data).await.is_err() {
                break Some("Écriture sur le disque impossible");
            }
            pos += data.len() as u64;
            shared.bytes.store(pos, Ordering::Relaxed);
            if pos == size {
                break None;
            }
        };

        if failure.is_none() && shared.session() == session && writer.flush().await.is_err() {
            return self.fail(&tid, "Écriture sur le disque impossible");
        }
        // Still holding the file: no other stream can start finishing too.
        match failure {
            Some(err) => {
                drop(slot);
                self.fail(&tid, err)
            }
            None if pos == size && shared.session() == session => {
                self.with(|s| self.begin_finalize(s, &tid));
                drop(slot);
            }
            // A local connection cut short while QUIC is still up: nothing
            // else would tell the sender, which may think it is done.
            None if cut && via_lan && shared.session() == session => {
                drop(slot);
                self.with(|s| self.resync(s, &tid));
            }
            None => {}
        }
    }

    fn fail(&self, tid: &str, error: &str) {
        self.with(|s| {
            let Some(t) = s.transfers.get(tid) else {
                return;
            };
            s.send(
                &t.friend_id.clone(),
                json!({ "type": "file_cancel", "tid": tid }),
            );
            self.stop_transfer(s, tid, Status::Error, Some(error.to_string()));
        });
    }

    fn resync(&self, s: &mut State, tid: &str) {
        let Some(t) = s.transfers.get_mut(tid) else {
            return;
        };
        if t.resyncing || t.status != Status::Receiving {
            return;
        }
        t.resyncing = true;
        let msg = json!({ "type": "file_accept", "tid": tid, "offset": t.bytes() });
        let friend_id = t.friend_id.clone();
        s.send(&friend_id, msg);
    }

    fn begin_finalize(&self, s: &mut State, tid: &str) {
        let Some(t) = s.transfers.get(tid) else {
            return;
        };
        if t.status == Status::Finishing || t.status.is_final() {
            return;
        }
        let shared = t.shared.clone();
        let part = t.part_path.clone().unwrap_or_default();
        let name = t.file_name.clone();
        let expected = t.repo.as_ref().map(|r| r.hash.clone());
        s.set_status(tid, Status::Finishing, None);
        let node = self.clone();
        let tid = tid.to_string();
        self.spawn(async move {
            let writer = shared.writer.lock().await.take();
            let mut ok = false;
            if let Some(mut w) = writer {
                ok = w.flush().await.is_ok();
                // Close the file before renaming it (required on Windows).
                drop(w.into_inner().into_std().await);
            }
            let still = node.with(|s| {
                s.transfers
                    .get(&tid)
                    .is_some_and(|t| t.status == Status::Finishing)
            });
            if !still {
                let _ = tokio::fs::remove_file(&part).await;
                return;
            }
            if !ok {
                let _ = tokio::fs::remove_file(&part).await;
                return node.fail(&tid, "Écriture sur le disque impossible");
            }
            if let Some(hash) = expected.clone() {
                if !Node::verify_download(part.clone(), hash).await {
                    // The source sent something else: start over from another.
                    return node.retry_corrupt_download(&tid, part).await;
                }
            }
            let dir = part.parent().unwrap_or(Path::new(".")).to_path_buf();
            let (part_moved, name_moved) = (part.clone(), name.clone());
            let saved = tokio::task::spawn_blocking(move || {
                let target = reserve_path(&dir, &name_moved)?;
                if let Err(e) = std::fs::rename(&part_moved, &target) {
                    let _ = std::fs::remove_file(&target);
                    return Err(e);
                }
                mark_downloaded(&target);
                Ok(target)
            })
            .await
            .ok()
            .and_then(Result::ok);
            let renamed = saved.is_some();
            let target = saved.unwrap_or_default();
            if !renamed {
                let _ = tokio::fs::remove_file(&part).await;
            }
            node.with(|s| {
                let Some(t) = s.transfers.get_mut(&tid) else {
                    return;
                };
                if t.status != Status::Finishing {
                    return;
                }
                let friend_id = t.friend_id.clone();
                if renamed {
                    t.saved_path = Some(target.clone());
                    s.set_status(&tid, Status::Completed, None);
                    s.send(&friend_id, json!({ "type": "file_complete", "tid": tid }));
                    if expected.is_some() {
                        node.record_download(s, &tid, &target);
                    } else {
                        let view = s.transfers[&tid].view();
                        s.events.push(Event::TransferComplete(view));
                    }
                } else {
                    s.send(&friend_id, json!({ "type": "file_cancel", "tid": tid }));
                    node.stop_transfer(
                        s,
                        &tid,
                        Status::Error,
                        Some("Impossible d’enregistrer le fichier".into()),
                    );
                }
            });
        });
    }

    // ── Both sides ──

    fn on_peer_stop(&self, s: &mut State, friend_id: &str, tid: Option<&str>, status: Status) {
        let Some(t) = tid.and_then(|tid| s.transfers.get(tid)) else {
            return;
        };
        if t.friend_id != friend_id || t.status.is_final() {
            return;
        }
        if t.repo.is_some() {
            // A source that stops is not the end: another member may have it.
            let id = t.id.clone();
            self.next_source(s, &id, friend_id);
            return;
        }
        let error = (status == Status::Cancelled).then(|| "Annulé par le contact".to_string());
        let id = t.id.clone();
        self.stop_transfer(s, &id, status, error);
    }

    pub fn cancel_transfer(&self, id: &str) {
        self.with(|s| self.cancel(s, id));
    }

    fn cancel(&self, s: &mut State, id: &str) {
        let Some(t) = s.transfers.get(id) else { return };
        if t.status.is_final() {
            return;
        }
        if t.dir == Dir::In && t.status == Status::Pending {
            let friend_id = t.friend_id.clone();
            s.send(&friend_id, json!({ "type": "file_reject", "tid": id }));
            s.set_status(id, Status::Declined, None);
            return;
        }
        s.send(
            &t.friend_id.clone(),
            json!({ "type": "file_cancel", "tid": id }),
        );
        self.stop_transfer(s, id, Status::Cancelled, None);
    }

    fn stop_transfer(&self, s: &mut State, id: &str, status: Status, error: Option<String>) {
        let Some(t) = s.transfers.get_mut(id) else {
            return;
        };
        if t.status.is_final() {
            return;
        }
        t.shared.next_session();
        t.conn_id = None;
        if let (Dir::In, Some(part)) = (t.dir, t.part_path.clone()) {
            // The finishing task owns the file at that point and removes it itself.
            let finishing = t.status == Status::Finishing;
            let shared = t.shared.clone();
            self.spawn(async move {
                let had_writer = shared.writer.lock().await.take().is_some();
                if had_writer || !finishing {
                    let _ = tokio::fs::remove_file(&part).await;
                }
            });
        }
        s.set_status(id, status, error);
    }
}

fn friend_views(s: &State) -> Vec<FriendView> {
    s.friends
        .data
        .iter()
        .map(|f| FriendView {
            id: f.id.clone(),
            name: f.name.clone(),
            online: s.conns.contains_key(&f.id),
            legacy: !is_valid_id(&f.id),
        })
        .collect()
}

async fn write_control(
    mut send: SendStream,
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
    queued: Arc<AtomicU64>,
) {
    while let Some(frame) = rx.recv().await {
        if send.write_all(&frame).await.is_err() {
            return;
        }
        queued.fetch_sub(frame.len() as u64, Ordering::Relaxed);
    }
}

/// One sending session of a file, from `start` to the end.
struct Job {
    tid: String,
    peer: String,
    shared: Arc<Shared>,
    session: u64,
    conn: Connection,
    /// Where to try the local network first.
    lan: Option<lan::LanTarget>,
    path: PathBuf,
    start: u64,
    size: u64,
}

enum Streamed {
    /// Every byte is on the wire.
    Done,
    /// The connection went away or the session was replaced.
    Stopped,
    /// The local-network connection failed; QUIC may still be up.
    LanFailed,
}

/// Send the file from `job.start` over `lan` if given, QUIC otherwise.
/// `Err` is a local read failure.
async fn stream_file(job: Job, lan: Option<lan::Sink>) -> Result<Streamed, &'static str> {
    let Job {
        tid,
        shared,
        session,
        conn,
        path,
        start,
        size,
        ..
    } = job;
    let mut cancel = shared.session.subscribe();

    // Read ahead on a blocking thread while the network sends.
    let (tx, mut chunks) = mpsc::channel::<io::Result<Bytes>>(4);
    tokio::task::spawn_blocking(move || read_file(path, start, size, tx));

    let mut send = match lan {
        Some(sink) => sink,
        None => match conn.open_uni().await {
            Ok(send) => lan::Sink::Quic(send),
            Err(_) => return Ok(Streamed::Stopped),
        },
    };
    let via_lan = send.is_lan();
    shared.lan.store(via_lan, Ordering::Relaxed);
    let broken = || {
        if via_lan {
            Streamed::LanFailed
        } else {
            Streamed::Stopped
        }
    };
    let mut header = [0u8; DATA_HEADER];
    hex::decode_to_slice(&tid, &mut header[..TID_BYTES]).map_err(|_| "Identifiant invalide")?;
    header[TID_BYTES..].copy_from_slice(&start.to_be_bytes());
    if !send.write(Bytes::copy_from_slice(&header)).await {
        return Ok(broken());
    }

    let mut pos = start;
    while pos < size {
        let next = tokio::select! {
            c = chunks.recv() => Some(c),
            _ = session_changed(&mut cancel, session) => None,
        };
        let data = match next {
            None => {
                send.abort();
                return Ok(Streamed::Stopped);
            }
            Some(Some(Ok(data))) if !data.is_empty() => data,
            Some(Some(Err(_))) => return Err("Lecture du fichier impossible"),
            Some(_) => return Err("Fichier source modifié pendant l’envoi"),
        };
        let len = data.len() as u64;
        let written = tokio::select! {
            ok = send.write(data) => Some(ok),
            _ = session_changed(&mut cancel, session) => None,
        };
        match written {
            Some(true) => {}
            Some(false) => return Ok(broken()),
            None => {
                send.abort();
                return Ok(Streamed::Stopped);
            }
        }
        pos += len;
        // A replaced session must not overwrite the new session's offset.
        if shared.session() != session {
            send.abort();
            return Ok(Streamed::Stopped);
        }
        shared.bytes.store(pos, Ordering::Relaxed);
    }
    if !send.finish().await {
        return Ok(broken());
    }
    Ok(Streamed::Done)
}

fn read_file(path: PathBuf, start: u64, size: u64, tx: mpsc::Sender<io::Result<Bytes>>) {
    let mut file =
        match File::open(&path).and_then(|mut f| f.seek(SeekFrom::Start(start)).map(|_| f)) {
            Ok(f) => f,
            Err(e) => {
                let _ = tx.blocking_send(Err(e));
                return;
            }
        };
    let mut pos = start;
    while pos < size {
        let mut buf = vec![0u8; CHUNK_SIZE.min(size - pos) as usize];
        let item = match file.read(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                pos += n as u64;
                Ok(Bytes::from(buf))
            }
            Err(e) => Err(e),
        };
        let stop = !matches!(&item, Ok(b) if !b.is_empty());
        if tx.blocking_send(item).is_err() || stop {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("C:\\dir\\a<b>.txt"), "a_b_.txt");
        assert_eq!(sanitize_file_name("con.txt"), "_con.txt");
        assert_eq!(sanitize_file_name("COM1"), "_COM1");
        assert_eq!(sanitize_file_name("nul .txt"), "_nul .txt");
        assert_eq!(sanitize_file_name("CONIN$"), "_CONIN$");
        assert_eq!(sanitize_file_name("lpt¹.log"), "_lpt¹.log");
        assert_eq!(sanitize_file_name("computer.txt"), "computer.txt");
        assert_eq!(sanitize_file_name("facture\u{202E}fdp.exe"), "facturefdp.exe");
        assert_eq!(sanitize_file_name("a\u{200B}b\u{FEFF}.txt"), "ab.txt");
        assert_eq!(sanitize_file_name("trail. . "), "trail");
        assert_eq!(sanitize_file_name(".."), "fichier");
        assert_eq!(sanitize_file_name(""), "fichier");
        let long = format!("{}.extension", "a".repeat(300));
        let s = sanitize_file_name(&long);
        assert_eq!(s.chars().count(), 180);
        assert!(s.ends_with(".extension"));
    }

    #[test]
    fn recognizes_risky_and_part_files() {
        assert!(is_executable("setup.EXE") && is_executable("a.lnk"));
        assert!(!is_executable("photo.jpg") && !is_executable("exe"));
        assert!(is_part_file("rapport.pdf.0a1b2c3d.part"));
        assert!(!is_part_file("rapport.part") && !is_part_file("x.0a1b2c3g.part"));
    }

    #[test]
    fn reserves_distinct_names() {
        let dir = std::env::temp_dir().join(format!("p2p-test-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = reserve_path(&dir, "x.txt").unwrap();
        let b = reserve_path(&dir, "x.txt").unwrap();
        assert_eq!(a.file_name().unwrap(), "x.txt");
        assert_eq!(b.file_name().unwrap(), "x (1).txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_http_relay_only_on_this_computer() {
        assert!(CustomRelay::parse("http://relay.example.com", "").is_err());
        assert!(CustomRelay::parse("http://localhost:3340", "").is_ok());
        assert!(CustomRelay::parse("relay.example.com", "").is_ok());
    }

    #[test]
    fn validates_ids() {
        let key = SecretKey::generate().public().to_string();
        assert!(is_valid_id(&key));
        assert!(!is_valid_id(&key.to_uppercase()));
        assert!(!is_valid_id("abc"));
    }

    #[test]
    fn same_identity_as_hyperswarm() {
        // Hyperswarm derives its key pair with libsodium's
        // crypto_sign_seed_keypair: plain Ed25519 from the 32-byte seed.
        // RFC 8032 test vector 1:
        let seed = hex::decode("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
            .unwrap();
        let key = SecretKey::from_bytes(&seed.try_into().unwrap());
        assert_eq!(
            key.public().to_string(),
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
        );
    }
}
