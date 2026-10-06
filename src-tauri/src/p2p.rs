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

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use iroh::endpoint::{presets, Connection, QuicTransportConfig, RecvStream, SendStream, VarInt};
use iroh::{Endpoint, PublicKey, SecretKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, watch};

use crate::store::{write_atomic, JsonStore};

pub const ALPN: &[u8] = b"p2pshare/3";

const TID_BYTES: usize = 16;
const DATA_HEADER: usize = TID_BYTES + 8;
const CHUNK_SIZE: u64 = 1 << 20;
const DISK_BUFFER: usize = 8 << 20;
const MAX_FRAME: usize = 256 * 1024;

const DIAL_TIMEOUT: Duration = Duration::from_secs(30);
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
    FileOffer(TransferView),
    TransferComplete(TransferView),
}

pub struct Options {
    pub secret: SecretKey,
    pub friends: JsonStore<Vec<Friend>>,
    pub messages: JsonStore<Messages>,
    pub download_dir: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    pub on_event: Arc<dyn Fn(Event) + Send + Sync>,
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
    // outgoing
    src_path: Option<PathBuf>,
    conn_id: Option<u64>,
    // incoming
    part_path: Option<PathBuf>,
    saved_path: Option<PathBuf>,
    resyncing: bool,
    /// Cleared from the list by the user; kept to answer re-offers.
    hidden: bool,
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
        }
    }
}

struct Conn {
    id: u64,
    conn: Connection,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    since: Instant,
    outgoing: bool,
}

struct State {
    me: String,
    friends: JsonStore<Vec<Friend>>,
    messages: JsonStore<Messages>,
    conns: HashMap<String, Conn>,
    dialing: HashSet<String>,
    fast_retries: HashMap<String, u8>,
    transfers: HashMap<String, Transfer>,
    chat_last_sent: HashMap<String, Instant>,
    rate: HashMap<String, VecDeque<Instant>>,
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
            }),
            src_path: None,
            conn_id: None,
            part_path: None,
            saved_path: None,
            resyncing: false,
            hidden: false,
            speed: 0.0,
            sample_bytes: 0,
            sample_time: Instant::now(),
        };
        self.transfers.insert(id.clone(), t);
        id
    }

    fn send(&self, friend_id: &str, msg: Value) {
        if let Some(c) = self.conns.get(friend_id) {
            let _ = c.tx.send(frame(&msg));
        }
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
        let now = Instant::now();
        let bucket = self.rate.entry(key.to_string()).or_default();
        while bucket
            .front()
            .is_some_and(|t| now.duration_since(*t) >= RATE_WINDOW)
        {
            bucket.pop_front();
        }
        let limited = bucket.len() >= RATE_LIMIT;
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
            let _ = c.tx.send(frame(
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

pub fn sanitize_file_name(name: &str) -> String {
    let unified = name.replace('\\', "/");
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

    let stem = n.split('.').next().unwrap_or("").to_ascii_lowercase();
    let reserved = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit());
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

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let ext = extension(name);
    let base = &name[..name.len() - ext.len()];
    let mut candidate = dir.join(name);
    let mut i = 1;
    while candidate.exists() {
        candidate = dir.join(format!("{base} ({i}){ext}"));
        i += 1;
    }
    candidate
}

// ─── Node ────────────────────────────────────────────────────────────────────

struct Inner {
    state: Mutex<State>,
    endpoint: Endpoint,
    rt: Handle,
    download_dir: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    on_event: Arc<dyn Fn(Event) + Send + Sync>,
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
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(opts.secret)
            .alpns(vec![ALPN.to_vec()])
            .transport_config(transport)
            .bind()
            .await
            .map_err(io::Error::other)?;

        let state = State {
            me: endpoint.id().to_string(),
            friends: opts.friends,
            messages: opts.messages,
            conns: HashMap::new(),
            dialing: HashSet::new(),
            fast_retries: HashMap::new(),
            transfers: HashMap::new(),
            chat_last_sent: HashMap::new(),
            rate: HashMap::new(),
            events: Vec::new(),
            next_id: 0,
        };
        let node = Node(Arc::new(Inner {
            state: Mutex::new(state),
            endpoint,
            rt: Handle::current(),
            download_dir: opts.download_dir,
            on_event: opts.on_event,
        }));

        node.spawn(node.clone().accept_loop());
        node.spawn(node.clone().timers());
        node.with(|s| node.reconnect_tick(s));
        Ok(node)
    }

    /// Close every connection cleanly so friends see us offline right away.
    pub async fn shutdown(&self) {
        self.save_now();
        self.0.endpoint.close().await;
    }

    pub fn save_now(&self) {
        self.with(|s| {
            s.friends.save_now();
            s.messages.save_now();
        });
    }

    pub fn id(&self) -> String {
        self.0.endpoint.id().to_string()
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

            let (messages, friends) = self.with(|s| {
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
                }
                (s.messages.take_dirty(), s.friends.take_dirty())
            });
            for (path, json) in [messages, friends].into_iter().flatten() {
                let _ = tokio::task::spawn_blocking(move || write_atomic(&path, &json)).await;
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
        let ids: Vec<String> = s.friends.data.iter().map(|f| f.id.clone()).collect();
        for id in ids {
            if s.conns.contains_key(&id) {
                s.flush_outbox(&id, false);
            } else {
                self.dial(s, &id);
            }
        }
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
            s.friends.save_now();
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
            if let Some(c) = s.conns.get(id) {
                c.conn.close(VarInt::from_u32(CLOSE_REJECTED), b"removed");
            }
            s.friends.data.retain(|f| f.id != id);
            s.friends.save_now();
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
                if s.conns.contains_key(&friend_id) || !s.is_friend(&friend_id) {
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
                // Only friends may connect.
                let friend_id = conn.remote_id().to_string();
                if !node.with(|s| s.is_friend(&friend_id)) {
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
        if !s.is_friend(&friend_id) {
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
        self.spawn(write_control(send, rx));
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

        s.flush_outbox(&friend_id, true);
        let to_offer: Vec<String> = s
            .transfers
            .values()
            .filter(|t| {
                t.dir == Dir::Out
                    && t.friend_id == friend_id
                    && !t.status.is_final()
                    && t.status != Status::Sending
            })
            .map(|t| t.id.clone())
            .collect();
        for tid in to_offer {
            self.offer(s, &tid);
        }
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
    }

    async fn accept_streams(self, friend_id: String, conn: Connection) {
        while let Ok(recv) = conn.accept_uni().await {
            self.spawn(self.clone().receive_stream(friend_id.clone(), recv));
        }
    }

    fn on_control(&self, s: &mut State, friend_id: &str, conn_id: u64, msg: &Value) {
        let tid = msg.get("tid").and_then(Value::as_str).filter(|t| is_tid(t));
        match msg.get("type").and_then(Value::as_str) {
            Some("chat") => self.on_chat(s, friend_id, msg),
            Some("chat_ack") => self.on_chat_ack(s, friend_id, msg),
            Some("file_offer") => self.on_offer(s, friend_id, msg),
            Some("file_accept") => self.on_accept(s, friend_id, conn_id, msg),
            Some("file_reject") => self.on_peer_stop(s, friend_id, tid, Status::Declined),
            Some("file_cancel") => self.on_peer_stop(s, friend_id, tid, Status::Cancelled),
            Some("file_complete") => self.on_complete(s, friend_id, tid),
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
            s.messages
                .data
                .entry(friend_id.to_string())
                .or_default()
                .push(entry.clone());
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
            s.messages
                .data
                .entry(friend_id.to_string())
                .or_default()
                .push(entry.clone());
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
        let files: Vec<(PathBuf, u64)> = paths
            .iter()
            .filter_map(|p| {
                std::fs::metadata(p)
                    .ok()
                    .filter(|m| m.is_file())
                    .map(|m| (p.clone(), m.len()))
            })
            .collect();
        self.with(|s| {
            for (path, size) in &files {
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
                s.transfers.get_mut(&tid).unwrap().src_path = Some(path.clone());
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
        t.shared.bytes.store(offset, Ordering::Relaxed);
        t.conn_id = Some(conn_id);
        let session = t.shared.next_session();
        let job = Job {
            tid: tid.to_string(),
            shared: t.shared.clone(),
            session,
            conn,
            path: t.src_path.clone().unwrap_or_default(),
            start: offset,
            size: t.file_size,
        };
        s.set_status(tid, Status::Sending, None);
        self.spawn(self.clone().pump(job));
    }

    async fn pump(self, job: Job) {
        let (tid, session) = (job.tid.clone(), job.session);
        let result = stream_file(job).await;
        self.with(|s| {
            let Some(t) = s.transfers.get(&tid) else {
                return;
            };
            if t.shared.session() != session || t.status.is_final() {
                return;
            }
            match result {
                // Everything is on the wire: wait for the receiver's file_complete.
                Ok(true) => s.set_status(&tid, Status::Finishing, None),
                // Connection lost: on_close re-offers on the next connection.
                Ok(false) => {}
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
        s.events.push(Event::FileOffer(view));
    }

    pub fn respond_to_offer(&self, id: &str, accept: bool) {
        let dir = (self.0.download_dir)();
        self.with(|s| {
            let Some(t) = s.transfers.get_mut(id) else {
                return;
            };
            if t.dir != Dir::In || t.status != Status::Pending {
                return;
            }
            let friend_id = t.friend_id.clone();
            if !accept {
                s.send(&friend_id, json!({ "type": "file_reject", "tid": id }));
                s.set_status(id, Status::Declined, None);
                return;
            }
            let part = dir.join(format!("{}.{}.part", t.file_name, &t.id[..8]));
            let opened = std::fs::create_dir_all(&dir).and_then(|_| File::create(&part));
            let Ok(file) = opened else {
                s.send(&friend_id, json!({ "type": "file_cancel", "tid": id }));
                s.set_status(
                    id,
                    Status::Error,
                    Some("Dossier de téléchargement inaccessible".into()),
                );
                return;
            };
            let writer = BufWriter::with_capacity(DISK_BUFFER, tokio::fs::File::from_std(file));
            if let Ok(mut slot) = t.shared.writer.try_lock() {
                *slot = Some(writer);
            }
            t.part_path = Some(part);
            t.shared.bytes.store(0, Ordering::Relaxed);
            if t.file_size == 0 {
                self.begin_finalize(s, id);
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
        if recv.read_exact(&mut header).await.is_err() {
            return;
        }
        let tid = hex::encode(&header[..TID_BYTES]);
        let offset = u64::from_be_bytes(header[TID_BYTES..].try_into().unwrap());
        let found = self.with(|s| {
            let t = s.transfers.get(&tid)?;
            let ok = t.dir == Dir::In && t.friend_id == friend_id && t.status == Status::Receiving;
            ok.then(|| (t.shared.clone(), t.shared.session(), t.file_size))
        });
        let Some((shared, session, size)) = found else {
            let _ = recv.stop(VarInt::from_u32(0));
            return;
        };

        // Wait for any previous stream of this transfer to release the file.
        let mut slot = shared.writer.lock().await;
        let Some(writer) = slot.as_mut() else { return };
        if shared.session() != session {
            return;
        }
        if offset != shared.bytes.load(Ordering::Relaxed) {
            // Stale or out-of-sync stream: ask once to resume from what we really have.
            let _ = recv.stop(VarInt::from_u32(0));
            drop(slot);
            self.with(|s| self.resync(s, &tid));
            return;
        }
        self.with(|s| {
            if let Some(t) = s.transfers.get_mut(&tid) {
                t.resyncing = false;
            }
        });

        let mut cancel = shared.session.subscribe();
        let mut pos = offset;
        let failure: Option<&str> = loop {
            let chunk = tokio::select! {
                c = recv.read_chunk(CHUNK_SIZE as usize) => c,
                _ = session_changed(&mut cancel, session) => break None,
            };
            let data = match chunk {
                Ok(Some(data)) => data,
                // Sender restarted or connection lost: keep what we have.
                Ok(None) | Err(_) => break None,
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
        drop(slot);
        match failure {
            Some(err) => self.fail(&tid, err),
            None if pos == size && shared.session() == session => {
                self.with(|s| self.begin_finalize(s, &tid))
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
        let shared = t.shared.clone();
        let part = t.part_path.clone().unwrap_or_default();
        let name = t.file_name.clone();
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
            let target = unique_path(part.parent().unwrap_or(Path::new(".")), &name);
            let renamed = tokio::fs::rename(&part, &target).await.is_ok();
            node.with(|s| {
                let Some(t) = s.transfers.get_mut(&tid) else {
                    return;
                };
                if t.status != Status::Finishing {
                    return;
                }
                let friend_id = t.friend_id.clone();
                if renamed {
                    t.saved_path = Some(target);
                    s.set_status(&tid, Status::Completed, None);
                    s.send(&friend_id, json!({ "type": "file_complete", "tid": tid }));
                    let view = s.transfers[&tid].view();
                    s.events.push(Event::TransferComplete(view));
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

async fn write_control(mut send: SendStream, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(frame) = rx.recv().await {
        if send.write_all(&frame).await.is_err() {
            return;
        }
    }
}

/// One sending session of a file, from `start` to the end.
struct Job {
    tid: String,
    shared: Arc<Shared>,
    session: u64,
    conn: Connection,
    path: PathBuf,
    start: u64,
    size: u64,
}

/// `Ok(false)` means the connection went away or the session was replaced;
/// `Err` is a local read failure.
async fn stream_file(job: Job) -> Result<bool, &'static str> {
    let Job {
        tid,
        shared,
        session,
        conn,
        path,
        start,
        size,
    } = job;
    let mut cancel = shared.session.subscribe();

    // Read ahead on a blocking thread while the network sends.
    let (tx, mut chunks) = mpsc::channel::<io::Result<Bytes>>(4);
    tokio::task::spawn_blocking(move || read_file(path, start, size, tx));

    let Ok(mut send) = conn.open_uni().await else {
        return Ok(false);
    };
    let mut header = [0u8; DATA_HEADER];
    hex::decode_to_slice(&tid, &mut header[..TID_BYTES]).map_err(|_| "Identifiant invalide")?;
    header[TID_BYTES..].copy_from_slice(&start.to_be_bytes());
    if send.write_all(&header).await.is_err() {
        return Ok(false);
    }

    let mut pos = start;
    while pos < size {
        let next = tokio::select! {
            c = chunks.recv() => Some(c),
            _ = session_changed(&mut cancel, session) => None,
        };
        let data = match next {
            None => {
                let _ = send.reset(VarInt::from_u32(0));
                return Ok(false);
            }
            Some(Some(Ok(data))) if !data.is_empty() => data,
            Some(Some(Err(_))) => return Err("Lecture du fichier impossible"),
            Some(_) => return Err("Fichier source modifié pendant l’envoi"),
        };
        let len = data.len() as u64;
        let written = tokio::select! {
            r = send.write_chunk(data) => Some(r.is_ok()),
            _ = session_changed(&mut cancel, session) => None,
        };
        match written {
            Some(true) => {}
            Some(false) => return Ok(false),
            None => {
                let _ = send.reset(VarInt::from_u32(0));
                return Ok(false);
            }
        }
        pos += len;
        // A replaced session must not overwrite the new session's offset.
        if shared.session() != session {
            let _ = send.reset(VarInt::from_u32(0));
            return Ok(false);
        }
        shared.bytes.store(pos, Ordering::Relaxed);
    }
    let _ = send.finish();
    Ok(true)
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
        assert_eq!(sanitize_file_name("trail. . "), "trail");
        assert_eq!(sanitize_file_name(".."), "fichier");
        assert_eq!(sanitize_file_name(""), "fichier");
        let long = format!("{}.extension", "a".repeat(300));
        let s = sanitize_file_name(&long);
        assert_eq!(s.chars().count(), 180);
        assert!(s.ends_with(".extension"));
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
