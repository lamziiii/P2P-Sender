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

mod chats;
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
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use iroh::endpoint::{presets, Connection, QuicTransportConfig, RecvStream, SendStream, VarInt};
use iroh::address_lookup::{PkarrPublisher, PkarrResolver};
use iroh::{Endpoint, PublicKey, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey, Watcher};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot, watch, Notify};

use crate::store::{write_atomic, JsonStore};

pub const ALPN: &[u8] = b"p2pshare/3";

const TID_BYTES: usize = 16;
const DATA_HEADER: usize = TID_BYTES + 8;
const CHUNK_SIZE: u64 = 1 << 20;
const DISK_BUFFER: usize = 8 << 20;
/// Buffer of an accepted file between its streams: thousands of accepted
/// files may wait their turn, they must not each hold `DISK_BUFFER`.
const IDLE_BUFFER: usize = 64 << 10;
/// Receiving streams holding a `DISK_BUFFER` at once; others use the idle one.
const MAX_LARGE_BUFFERS: usize = 8;
const MAX_FRAME: usize = 256 * 1024;

const DIAL_TIMEOUT: Duration = Duration::from_secs(15);
const ONLINE_TIMEOUT: Duration = Duration::from_secs(15);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// A peer that went offline is dialed again after that long; also how
/// often unacknowledged chats are sent again, and groups that differ from
/// a connected member's are compared again.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(15);
/// Then less and less often while it stays offline, the last delay
/// repeating (`ECONOMY_BACKOFF` after it in economy mode). A friend that
/// comes back dials us anyway: this is for when it cannot.
const BACKOFF: [Duration; 6] = [
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(2 * 60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(10 * 60),
];
const ECONOMY_BACKOFF: [Duration; 2] = [Duration::from_secs(20 * 60), Duration::from_secs(30 * 60)];
// A peer that just started is not published yet: retry a few times quickly.
const FAST_RETRY_DELAY: Duration = Duration::from_secs(1);
const FAST_RETRIES: u8 = 10;
/// Dialing everyone after a network change, at most that often (the
/// system may report several in a row).
const RECONNECT_ALL_MIN: Duration = Duration::from_secs(10);
/// Groups that differ from a connected member's, compared again in economy mode.
const ECONOMY_GROUPS_SYNC: Duration = Duration::from_secs(60);
/// How often the progress of running transfers is sent.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);
/// Changed stores and group logs are written that long after the change.
const SAVE_DELAY: Duration = Duration::from_millis(500);
/// Everything periodic also runs that often, in case a schedule was missed.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Shortest sleep of the timer task, should something stay due.
const MIN_SLEEP: Duration = Duration::from_millis(50);
const WAKE_GAP: Duration = Duration::from_secs(10);
const DUPLICATE_WINDOW: Duration = Duration::from_secs(10);
const CHAT_RETRY: Duration = Duration::from_secs(10);
const MAX_TEXT: usize = 5000;
const RATE_LIMIT: usize = 30;
const RATE_WINDOW: Duration = Duration::from_secs(10);
/// Control messages of any kind a peer may send per `RATE_WINDOW` (a fast
/// folder brings an offer, or an accept and a complete, per file).
const CONTROL_RATE_LIMIT: usize = 20_000;
/// Offers from one friend waiting for our answer.
const MAX_PENDING_OFFERS: usize = 100;
/// Offers we send to one friend per `RATE_WINDOW`, below its `RATE_LIMIT`.
const OFFER_BUDGET: usize = 20;
/// Margin before using the budget again: the receiver's window starts when
/// the offers reach it.
const OFFER_SLACK: Duration = Duration::from_millis(500);
/// An unanswered offer is sent again after that long: it may have been lost,
/// or dropped by the receiver's limits. A receiver that knows it ignores it.
/// Receiver side, an accepted transfer still paused that long after the
/// sender came back is asked about.
const REOFFER_DELAY: Duration = Duration::from_secs(30);
/// A data stream cut while the connection stays up is restarted after that
/// long, at most `MAX_STALLS` times in a row without progress.
const STALL_RETRY: Duration = Duration::from_secs(2);
const MAX_STALLS: u8 = 5;
/// Receiver side: a stream cut and not replaced within that long is resumed.
const CUT_RESYNC: Duration = Duration::from_secs(5);
/// Finished transfers kept, the oldest are forgotten first.
const MAX_FINISHED: usize = 1000;
const RENAME_ATTEMPTS: u32 = 5;
const RENAME_RETRY_DELAY: Duration = Duration::from_millis(500);
/// Bytes waiting to be sent to a peer that does not read them.
const MAX_QUEUED: usize = 64 << 20;
/// Messages kept per conversation.
const MAX_HISTORY: usize = 10_000;
const MESSAGES_SAVE_INTERVAL: Duration = Duration::from_secs(5);
/// Finished transfers hidden from the list are forgotten after that long.
const FORGET_FINISHED: Duration = Duration::from_secs(60 * 60);
/// Finished transfers leave the list on their own after a day.
const HIDE_FINISHED: Duration = Duration::from_secs(24 * 60 * 60);
/// Longest preview of a chat's last message in the friend list.
const PREVIEW_CHARS: usize = 120;
const MAX_FRIEND_NAME: usize = 40;
/// Longest quote of the message a reply answers.
const REPLY_CHARS: usize = 120;
/// The reactions the UI offers; one per person and message.
pub const REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "😮", "😢", "🙏"];
/// "Typing" sent at most that often to a friend.
const TYPING_INTERVAL: Duration = Duration::from_secs(3);
/// And taken from one at most that many times per `RATE_WINDOW`.
const TYPING_RATE_LIMIT: usize = 10;
/// Levels of a folder sent, from its own name to the file's.
const MAX_FOLDER_DEPTH: usize = 32;
const MAX_FOLDER_FILES: usize = 10_000;
/// Offers of a folder the receiver accepted, sent per `RATE_WINDOW`; and
/// taken by the receiver, a bit more. Versions before 3.4 take 300: they
/// get `FOLDER_OFFER_BUDGET`; later ones say in their `folder_accept`
/// ("rate") that they take `FAST_FOLDER_BUDGET`, which small files need.
const FOLDER_OFFER_BUDGET: usize = 200;
const FAST_FOLDER_BUDGET: usize = 5000;
const FOLDER_RATE_LIMIT: usize = 6000;
/// Files accepted without asking (folder accepted, friend trusted) that
/// may be open at once; the others wait for their turn. A small file counts
/// for `1 / SMALL_PER_SLOT`: its time goes into creating and saving it
/// more than into its bytes, so more of them at once go faster.
const MAX_AUTO_ACTIVE: usize = 8;
const SMALL_PER_SLOT: usize = 4;
/// Below that (what is left to send), a file is small: it is not worth a
/// local connection either (see `lan.rs`), whose setup would cost more
/// than it saves.
const SMALL_FILE: u64 = 1 << 20;
/// With a speed limit, bytes are counted in pieces of that size.
const THROTTLE_SLICE: usize = 64 << 10;
/// Lowest speed limit, in bytes per second.
const MIN_SPEED_LIMIT: u64 = 16 << 10;

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
    /// The name we gave them; empty to use their nickname.
    #[serde(default)]
    pub name: String,
    /// The nickname they chose, sent when they connect.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub nickname: String,
    /// Their offers are accepted without asking, except programs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_accept: bool,
    /// Our reactions they did not acknowledge yet: message id → emoji, or
    /// "" for a reaction taken back. Kept here to survive a restart.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    unsent_reactions: HashMap<String, String>,
}

impl Friend {
    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else if !self.nickname.is_empty() {
            self.nickname.clone()
        } else {
            format!("Ami {}", &self.id[..4.min(self.id.len())])
        }
    }
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

#[derive(Clone, PartialEq, Serialize)]
pub struct RelayStatusView {
    pub url: String,
    pub custom: bool,
    pub connected: bool,
    pub error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendView {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub legacy: bool,
    /// The last message of the chat, for the friend list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_text: Option<String>,
    pub last_ts: u64,
    pub last_mine: bool,
    /// Their offers are accepted without asking, except programs.
    pub auto_accept: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    Pending,
    Delivered,
    /// Seen by the friend (read receipts on both sides).
    Read,
}

/// The message a reply answers, quoted.
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct ChatReply {
    pub id: String,
    pub from: String,
    /// Its first `REPLY_CHARS` characters (an image: its name).
    pub text: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub from: String,
    pub text: String,
    pub ts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<MessageStatus>,
    /// An image sent or offered in this chat (its id is the transfer's).
    /// Only kept here: peers see the transfer, not this entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<ChatFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<ChatReply>,
    /// One emoji per person: their id → emoji (see `REACTIONS`).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub reactions: HashMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ChatFile {
    pub name: String,
    pub size: u64,
    /// Ours: the source. Theirs: where it was saved, once received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// How the transfer ended, once it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Status>,
}

pub type Messages = HashMap<String, Vec<ChatMessage>>;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
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
    /// While it runs, how its bytes go: "lan" (as `lan`), "direct" (QUIC
    /// straight to the peer) or "relay" (QUIC through a relay).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<&'static str>,
    /// A group's shared file (served or downloaded): no manual pause.
    pub group: bool,
    /// Paused by hand: "me" (here) or "peer" (the other side).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused_by: Option<&'static str>,
    /// Incoming: accepted without asking (folder accepted, trusted friend),
    /// waiting for its turn while still `pending`.
    pub queued: bool,
    /// Part of a folder: its path there, starting with the folder's name
    /// ("Photos/2024/a.jpg"), with the names as saved here when incoming.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<FolderView>,
}

/// A folder sent as one file transfer per file.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderView {
    /// Shared by its files' transfers.
    pub id: String,
    /// As named by the sender.
    pub name: String,
    /// Files and bytes of the whole folder, as the sender counted them.
    pub files: u64,
    pub size: u64,
    /// Incoming: where it is saved, once a file was accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

/// What a sent folder's offers carry; shared by its files.
struct FolderMeta {
    id: String,
    name: String,
    files: u64,
    size: u64,
}

/// A folder we receive from a friend.
#[derive(Default)]
struct FolderIn {
    /// The user's answer for the whole folder.
    decision: Option<bool>,
    /// The sender was told to offer the rest quickly (`folder_accept`).
    fast: bool,
    /// Where it is saved, picked when the first file is accepted.
    root: Option<PathBuf>,
    /// Offers taken so far.
    files: usize,
    /// The user was notified of it.
    notified: bool,
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
    /// Our messages the friend has now seen.
    MessagesRead {
        friend_id: String,
        ids: Vec<String>,
    },
    /// The reactions to a message changed.
    Reactions {
        friend_id: String,
        id: String,
        reactions: HashMap<String, String>,
    },
    /// The friend is typing to us.
    Typing {
        friend_id: String,
    },
    Transfer(TransferView),
    /// A friend's name changed (their nickname came).
    FriendsChanged,
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
        /// It mentions us or everyone (a muted group only notifies what
        /// mentions us by name).
        mention: bool,
    },
    GroupInvite {
        gid: String,
        group: String,
        from: String,
    },
    /// Our home relay changed (picked, connected, lost…): what
    /// `Node::relay_status` now returns.
    RelayStatus(Option<RelayStatusView>),
}

pub struct Options {
    pub secret: SecretKey,
    pub friends: JsonStore<Vec<Friend>>,
    /// Where chats live, one file per friend; `<messages_dir>.json`, the
    /// single file of earlier versions, is moved there on first start.
    pub messages_dir: PathBuf,
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
    /// Receiver side: data streams started so far.
    streams: AtomicU64,
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

/// A speed limit shared by all the transfers of one direction: a token
/// bucket that may go into debt, each stream then waiting its share.
struct Throttle {
    /// Bytes per second, 0 for no limit.
    rate: AtomicU64,
    /// Tokens (bytes, negative in debt) and when they were counted.
    bucket: Mutex<(f64, Instant)>,
}

impl Throttle {
    fn new() -> Self {
        Throttle {
            rate: AtomicU64::new(0),
            bucket: Mutex::new((0.0, Instant::now())),
        }
    }

    fn set(&self, rate: u64) {
        self.rate.store(rate, Ordering::Relaxed);
    }

    fn limited(&self) -> bool {
        self.rate.load(Ordering::Relaxed) > 0
    }

    /// Tokens after counting those that came since last time.
    fn refill(&self, rate: u64) -> std::sync::MutexGuard<'_, (f64, Instant)> {
        let mut b = self.bucket.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        // A quarter of a second of bytes at most, saved while idle.
        let burst = (rate as f64 / 4.0).max(THROTTLE_SLICE as f64);
        b.0 = (b.0 + now.duration_since(b.1).as_secs_f64() * rate as f64).min(burst);
        b.1 = now;
        b
    }

    /// Wait until `n` more bytes are within the limit.
    async fn take(&self, n: usize) {
        let rate = self.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return;
        }
        self.refill(rate).0 -= n as f64;
        loop {
            let rate = self.rate.load(Ordering::Relaxed);
            if rate == 0 {
                return;
            }
            let tokens = self.refill(rate).0;
            if tokens >= 0.0 {
                return;
            }
            // In short steps: the limit may change or go meanwhile.
            let wait = Duration::from_secs_f64(-tokens / rate as f64);
            tokio::time::sleep(wait.min(Duration::from_millis(250))).await;
        }
    }

    /// Largest piece to read or write at once.
    fn piece(&self, default: usize) -> usize {
        if self.limited() {
            THROTTLE_SLICE.min(default)
        } else {
            default
        }
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
    /// Outgoing: when it was last offered. Incoming: when the sender was
    /// last asked about it.
    last_sent: Option<Instant>,
    /// Outgoing: streams cut in a row on a live connection, and the bytes
    /// sent when the last one was.
    stalls: u8,
    stall_bytes: u64,
    /// The cancel or decline the peer was offline for, sent when it is back.
    stop_unsent: Option<&'static str>,
    /// Shown in the friend's chat (an image), updated when it ends.
    in_chat: bool,
    /// Paused by hand here, or by the peer: nothing moves until resumed.
    held: bool,
    peer_held: bool,
    /// Part of a folder: its path there (see `TransferView::rel_path`).
    folder: Option<Arc<FolderMeta>>,
    rel_path: Option<String>,
    /// Incoming in a folder: where the folder is saved, and the file.
    folder_root: Option<PathBuf>,
    dest_dir: Option<PathBuf>,
    /// Incoming: accepted without asking; `queued` until its turn comes
    /// (see `MAX_AUTO_ACTIVE`), `accepting` while its file is created.
    auto: bool,
    queued: bool,
    accepting: bool,
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
            // Needs the connection: see `State::transfer_view`.
            route: None,
            group: self.repo.is_some() || self.serve,
            paused_by: if self.held {
                Some("me")
            } else if self.peer_held {
                Some("peer")
            } else {
                None
            },
            queued: self.queued && self.status == Status::Pending,
            rel_path: self.rel_path.clone(),
            folder: self.folder.as_ref().map(|f| FolderView {
                id: f.id.clone(),
                name: f.name.clone(),
                files: f.files,
                size: f.size,
                root: self
                    .folder_root
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
            }),
        }
    }

    /// Paused or resumed by hand on either side.
    fn held(&self) -> bool {
        self.held || self.peer_held
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

/// A store that knows it changed since the timer task last saved it, so
/// that a change wakes that task (see `State::schedule_saves`).
struct Saved<T> {
    store: JsonStore<T>,
    changed: bool,
}

impl<T> Deref for Saved<T> {
    type Target = JsonStore<T>;
    fn deref(&self) -> &JsonStore<T> {
        &self.store
    }
}

impl<T> DerefMut for Saved<T> {
    fn deref_mut(&mut self) -> &mut JsonStore<T> {
        &mut self.store
    }
}

impl<T: Serialize + DeserializeOwned> Saved<T> {
    fn new(store: JsonStore<T>) -> Self {
        Saved {
            store,
            changed: false,
        }
    }

    fn mark_dirty(&mut self) {
        self.store.mark_dirty();
        self.changed = true;
    }

    fn take_dirty(&mut self) -> Option<(PathBuf, Vec<u8>)> {
        self.changed = false;
        self.store.take_dirty()
    }

    /// On failure the timer task tries again.
    fn save_now(&mut self) -> io::Result<()> {
        let result = self.store.save_now();
        self.changed = result.is_err();
        result
    }
}

/// When to dial an offline peer again.
struct Backoff {
    /// Dials in a row that failed, beyond the quick ones.
    step: u8,
    next: Instant,
}

fn backoff_delay(step: u8, economy: bool) -> Duration {
    let step = step as usize;
    match BACKOFF.get(step) {
        Some(delay) => *delay,
        None if economy => ECONOMY_BACKOFF[(step - BACKOFF.len()).min(ECONOMY_BACKOFF.len() - 1)],
        None => BACKOFF[BACKOFF.len() - 1],
    }
}

struct State {
    me: String,
    secret: SecretKey,
    nickname: String,
    friends: Saved<Vec<Friend>>,
    chats: chats::Chats,
    groups: HashMap<String, groups::GroupState>,
    gmeta: Saved<groups::GroupsMeta>,
    groups_dir: PathBuf,
    conns: HashMap<String, Conn>,
    dialing: HashSet<String>,
    fast_retries: HashMap<String, u8>,
    /// Offline peers we may talk to: when to dial them next.
    backoff: HashMap<String, Backoff>,
    /// When every offline peer was last dialed at once.
    reconnected_all: Option<Instant>,
    transfers: HashMap<String, Transfer>,
    /// Chats sent and not acknowledged yet: to whom, and when.
    chat_last_sent: HashMap<String, (String, Instant)>,
    /// The last entry of each author a peer sent us per group, over its
    /// current connection (see `groups_settled`).
    group_seen: HashMap<(String, String), crate::group::Have>,
    /// When the changed stores and group logs are due to be written; the
    /// chats, each rewritten whole when it changed, at most every
    /// `MESSAGES_SAVE_INTERVAL`.
    save_at: Option<Instant>,
    messages_save_at: Option<Instant>,
    last_messages_save: Instant,
    /// Work was added that the timer task must schedule: `Node::with`
    /// wakes it.
    wake_timers: bool,
    rate: HashMap<String, VecDeque<Instant>>,
    /// What peers told us of their local-network address, and when the
    /// connection to one last failed.
    lan_peers: HashMap<String, lan::LanPeer>,
    lan_down: HashMap<String, Instant>,
    /// What we tell peers of ours, once we have an address.
    lan_advert: Option<Value>,
    /// Read receipts and "typing", both ways (see `Node::set_read_receipts`).
    read_receipts: bool,
    /// The last message of a friend's we saw, still to tell them.
    read_unsent: HashMap<String, String>,
    /// And the last we told them.
    read_sent: HashMap<String, String>,
    /// When we last told a friend we are typing.
    typing_sent: HashMap<String, Instant>,
    /// Folders we receive, by (friend, folder id).
    folders_in: HashMap<(String, String), FolderIn>,
    /// Folders we send that the receiver answered for, by folder id: the
    /// offers it takes per `RATE_WINDOW` when accepted, `None` if declined.
    folders_out: HashMap<String, Option<usize>>,
    events: Vec<Event>,
    next_id: u64,
}

impl State {
    fn is_friend(&self, id: &str) -> bool {
        self.friends.data.iter().any(|f| f.id == id)
    }

    /// Everyone we may dial: friends, and the members of our groups.
    fn allowed_peers(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.friends.data.iter().map(|f| f.id.clone()).collect();
        let others: Vec<String> = self
            .group_peers()
            .into_iter()
            .filter(|p| !self.is_friend(p))
            .collect();
        ids.extend(others);
        ids.retain(|id| is_valid_id(id));
        ids
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Schedule the writing of what changed since the last save.
    fn schedule_saves(&mut self) {
        let now = Instant::now();
        if self.save_at.is_none()
            && (self.friends.changed
                || self.gmeta.changed
                || self.groups.values().any(|g| !g.unsaved.is_empty()))
        {
            self.save_at = Some(now + SAVE_DELAY);
            self.wake_timers = true;
        }
        if self.messages_save_at.is_none() && self.chats.changed {
            // A chat's whole history is rewritten each time: not on every message.
            self.messages_save_at = Some((self.last_messages_save + MESSAGES_SAVE_INTERVAL).max(now));
            self.wake_timers = true;
        }
    }

    /// When `limit` events per `RATE_WINDOW` for `key` allow one more, if
    /// they do not now.
    fn limit_frees_at(&self, key: &str, limit: usize) -> Option<Instant> {
        let bucket = self.rate.get(key)?;
        (bucket.len() >= limit)
            .then(|| bucket.front().map(|t| *t + RATE_WINDOW))
            .flatten()
    }

    /// The limit an offer of ours counts against: the files of a folder the
    /// receiver accepted go faster, it takes them without asking.
    fn offer_budget(&self, t: &Transfer) -> (String, usize) {
        let accepted = t
            .folder
            .as_ref()
            .and_then(|f| self.folders_out.get(&f.id).copied().flatten());
        if let Some(budget) = accepted {
            (format!("offer_folder:{}", t.friend_id), budget)
        } else {
            (format!("offer_out:{}", t.friend_id), OFFER_BUDGET)
        }
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
                streams: AtomicU64::new(0),
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
            last_sent: None,
            stalls: 0,
            stall_bytes: 0,
            stop_unsent: None,
            in_chat: false,
            held: false,
            peer_held: false,
            folder: None,
            rel_path: None,
            folder_root: None,
            dest_dir: None,
            auto: false,
            queued: false,
            accepting: false,
            speed: 0.0,
            sample_bytes: 0,
            sample_time: Instant::now(),
        };
        self.transfers.insert(id.clone(), t);
        if self.transfers.len() > MAX_FINISHED {
            // The timer task forgets the oldest finished ones.
            self.wake_timers = true;
        }
        id
    }

    fn send(&self, friend_id: &str, msg: Value) {
        if let Some(c) = self.conns.get(friend_id) {
            c.push(frame(&msg));
        }
    }

    /// Forget finished transfers nobody looks at any more, and the oldest
    /// beyond `MAX_FINISHED` (a peer offering and cancelling in a loop).
    fn forget_finished(&mut self) {
        for t in self.transfers.values_mut() {
            if t.ended.is_some_and(|e| e.elapsed() > HIDE_FINISHED) {
                t.hidden = true;
            }
        }
        self.transfers.retain(|_, t| {
            let old = t.ended.is_some_and(|e| e.elapsed() > FORGET_FINISHED);
            !(old && (t.hidden || t.serve) && t.stop_unsent.is_none())
        });
        // Called on every timer pass while many transfers are listed (a large
        // folder): only the finished ones are gathered, when too many.
        let finished = self.transfers.values().filter(|t| t.ended.is_some()).count();
        if finished > MAX_FINISHED {
            let mut ended: Vec<(Instant, String)> = self
                .transfers
                .values()
                .filter_map(|t| t.ended.map(|e| (e, t.id.clone())))
                .collect();
            ended.sort();
            for (_, id) in &ended[..ended.len() - MAX_FINISHED] {
                self.transfers.remove(id);
            }
        }
        // Folders whose files are all forgotten.
        let folders: HashSet<&str> = self
            .transfers
            .values()
            .filter_map(|t| t.folder.as_ref().map(|f| f.id.as_str()))
            .collect();
        self.folders_in.retain(|(_, id), _| folders.contains(id.as_str()));
        self.folders_out.retain(|id, _| folders.contains(id.as_str()));
    }

    /// Tell the peer a transfer is over (`file_cancel`, `file_reject`), now
    /// or as soon as it is back: it would otherwise wait for it forever.
    fn send_stop(&mut self, tid: &str, kind: &'static str) {
        let Some(t) = self.transfers.get_mut(tid) else {
            return;
        };
        match self.conns.get(&t.friend_id) {
            Some(c) => {
                c.push(frame(&json!({ "type": kind, "tid": tid })));
                t.stop_unsent = None;
            }
            // Only friends reconnect (external peers have no connection).
            None if t.peer_name.is_none() && t.repo.is_none() && !t.serve => {
                t.stop_unsent = Some(kind);
            }
            None => {}
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
        let view = self.transfer_view(&self.transfers[id]);
        self.events.push(Event::Transfer(view));
    }

    /// `Transfer::view`, with the way its bytes go while it runs.
    fn transfer_view(&self, t: &Transfer) -> TransferView {
        let mut view = t.view();
        view.route = if view.lan {
            Some("lan")
        } else if t.status.is_active() {
            self.conns.get(&t.friend_id).and_then(|c| {
                let paths = c.conn.paths();
                let selected = paths.iter().find(|p| p.is_selected())?;
                Some(if selected.is_relay() { "relay" } else { "direct" })
            })
        } else {
            None
        };
        view
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
        // Running now, or waiting for something the timer task does
        // (an offer to send again, a member to ask…).
        self.wake_timers = true;
        if status.is_final() && t.in_chat {
            let friend_id = t.friend_id.clone();
            let saved = (t.dir == Dir::In)
                .then(|| t.saved_path.as_ref().map(|p| p.to_string_lossy().into_owned()))
                .flatten();
            self.end_chat_file(&friend_id, id, status, saved);
        }
        self.emit_transfer(id);
    }

    /// Show an image sent or offered in the friend's chat.
    fn add_chat_file(&mut self, friend_id: &str, tid: &str, from: String, path: Option<String>) {
        let Some(t) = self.transfers.get_mut(tid) else {
            return;
        };
        t.in_chat = true;
        let entry = ChatMessage {
            id: tid.to_string(),
            from,
            text: String::new(),
            ts: now_ms(),
            status: None,
            file: Some(ChatFile {
                name: t.file_name.clone(),
                size: t.file_size,
                path,
                state: None,
            }),
            ..Default::default()
        };
        self.chats.push(friend_id, entry.clone());
        self.events.push(Event::Message {
            friend_id: friend_id.to_string(),
            message: entry,
        });
    }

    fn end_chat_file(&mut self, friend_id: &str, tid: &str, status: Status, saved: Option<String>) {
        let Some(file) = self
            .chats
            .list_mut(friend_id)
            .iter_mut()
            .find(|m| m.id == tid)
            .and_then(|m| m.file.as_mut())
        else {
            return;
        };
        file.state = Some(status);
        if saved.is_some() {
            file.path = saved;
        }
        self.chats.mark_dirty(friend_id);
    }

    /// Tell a friend our nickname (shown when we have no name there).
    fn send_profile(&self, friend_id: &str) {
        if !self.nickname.is_empty() && self.is_friend(friend_id) {
            self.send(friend_id, json!({ "type": "profile", "nick": self.nickname }));
        }
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
        // Not in memory: no chat to send (see `Chats`).
        let list = self.chats.loaded(friend_id).map_or(&[][..], |l| &l[..]);
        let now = Instant::now();
        for m in list
            .iter()
            .filter(|m| m.status == Some(MessageStatus::Pending))
        {
            let recent = self
                .chat_last_sent
                .get(&m.id)
                .is_some_and(|(_, t)| now.duration_since(*t) < CHAT_RETRY);
            if !force && recent {
                continue;
            }
            self.chat_last_sent
                .insert(m.id.clone(), (friend_id.to_string(), now));
            self.wake_timers = true;
            let mut msg = json!({ "type": "chat", "id": m.id, "text": m.text, "ts": m.ts });
            if let Some(reply) = &m.reply {
                // Older versions ignore it and show the text alone.
                msg["reply"] = json!(reply);
            }
            c.push(frame(&msg));
        }
        // Reactions after the chats: one may be to a message just sent.
        let Some(f) = self.friends.data.iter().find(|f| f.id == friend_id) else {
            return;
        };
        for (id, emoji) in &f.unsent_reactions {
            let key = format!("react:{id}");
            let recent = self
                .chat_last_sent
                .get(&key)
                .is_some_and(|(_, t)| now.duration_since(*t) < CHAT_RETRY);
            if !force && recent {
                continue;
            }
            self.chat_last_sent.insert(key, (friend_id.to_string(), now));
            self.wake_timers = true;
            c.push(frame(&reaction_msg(id, emoji)));
        }
    }

    /// Tell the friend the last of its messages we saw, if not done yet.
    fn send_read(&mut self, friend_id: &str) {
        if !self.read_receipts || !self.conns.contains_key(friend_id) {
            return;
        }
        if let Some(id) = self.read_unsent.remove(friend_id) {
            self.send(friend_id, json!({ "type": "chat_read", "upTo": id }));
            self.read_sent.insert(friend_id.to_string(), id);
        }
    }

    /// Whether `tid` is not in a folder, or no other file of its folder
    /// is still on its way.
    fn folder_done(&self, tid: &str) -> bool {
        let Some(t) = self.transfers.get(tid) else {
            return false;
        };
        let Some(folder) = &t.folder else {
            return true;
        };
        // Some may not be offered yet.
        let key = (t.friend_id.clone(), folder.id.clone());
        if self
            .folders_in
            .get(&key)
            .is_some_and(|f| (f.files as u64) < folder.files)
        {
            return false;
        }
        !self.transfers.values().any(|o| {
            o.id != t.id
                && o.dir == t.dir
                && o.friend_id == t.friend_id
                && !o.status.is_final()
                && o.folder.as_ref().is_some_and(|f| f.id == folder.id)
        })
    }

    /// Tell the peer of the transfers we paused, now that it is connected.
    fn send_holds(&self, peer: &str) {
        for t in self.transfers.values() {
            if t.friend_id == peer && t.held && !t.status.is_final() {
                self.send(peer, json!({ "type": "file_pause", "tid": t.id }));
            }
        }
    }
}

/// `chat_react`: our reaction to a message, `emoji` "" for none.
fn reaction_msg(id: &str, emoji: &str) -> Value {
    let emoji = (!emoji.is_empty()).then_some(emoji);
    json!({ "type": "chat_react", "id": id, "emoji": emoji })
}

/// One of `REACTIONS`, as written there.
fn valid_reaction(emoji: &str) -> Option<&'static str> {
    // The heart also comes without its emoji variation selector.
    let emoji = if emoji == "\u{2764}" { "❤️" } else { emoji };
    REACTIONS.iter().find(|r| **r == emoji).copied()
}

/// The quote of `m` in a reply to it.
fn reply_of(m: &ChatMessage) -> ChatReply {
    let text = match &m.file {
        Some(file) => file.name.clone(),
        None => m.text.clone(),
    };
    ChatReply {
        id: m.id.clone(),
        from: m.from.clone(),
        text: text.chars().take(REPLY_CHARS).collect(),
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

/// Create the directory `name` in `dir`, or "name (2)", "name (3)"… if
/// taken (creating it claims the name).
fn reserve_dir(dir: &Path, name: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for i in 1..10_000 {
        let candidate = if i == 1 {
            dir.join(name)
        } else {
            dir.join(format!("{name} ({i})"))
        };
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

/// A file to send.
struct SourceFile {
    path: PathBuf,
    size: u64,
    mtime: Option<SystemTime>,
    /// In a folder: its path there, from the folder's name, with `/`.
    rel_path: Option<String>,
}

/// The name of folder `root` and its files, in order. Links are not
/// followed, empty folders give nothing.
fn walk_folder(root: &Path) -> Result<(String, Vec<SourceFile>), &'static str> {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .ok_or("not_found")?;
    if !std::fs::metadata(root).is_ok_and(|m| m.is_dir()) {
        return Err("not_found");
    }
    let mut files = Vec::new();
    // (directory, its path from the folder's name, its depth)
    let mut dirs = vec![(root.to_path_buf(), name.clone(), 1usize)];
    while let Some((dir, rel, depth)) = dirs.pop() {
        let Ok(list) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in list.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let entry_rel = format!("{rel}/{}", entry.file_name().to_string_lossy());
            if kind.is_dir() {
                if depth + 1 >= MAX_FOLDER_DEPTH {
                    return Err("too_deep");
                }
                dirs.push((entry.path(), entry_rel, depth + 1));
            } else if kind.is_file() {
                if files.len() >= MAX_FOLDER_FILES {
                    return Err("too_many");
                }
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                files.push(SourceFile {
                    path: entry.path(),
                    size: meta.len(),
                    mtime: meta.modified().ok(),
                    rel_path: Some(entry_rel),
                });
            }
        }
    }
    if files.is_empty() {
        return Err("empty");
    }
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok((name, files))
}

/// The folder an offer's file is part of, and its path there with each
/// name made safe (the last one `name`, the file's); `None` for a file on
/// its own, or a path that could leave the folder: it is then saved with
/// the others.
fn parse_folder(msg: &Value, name: &str) -> Option<(FolderMeta, Vec<String>)> {
    let folder = msg.get("folder")?;
    let id = folder.get("id")?.as_str().filter(|id| is_tid(id))?;
    let folder_name = folder.get("name")?.as_str()?;
    let mut parts = folder_path(msg.get("path")?.as_str()?, folder_name)?;
    *parts.last_mut()? = name.to_string();
    let meta = FolderMeta {
        id: id.to_string(),
        name: parts[0].clone(),
        files: folder.get("files").and_then(Value::as_u64).unwrap_or(0),
        size: folder.get("size").and_then(Value::as_u64).unwrap_or(0),
    };
    Some((meta, parts))
}

/// The names of a path in a folder, made safe ("Photos/2024/a.jpg"):
/// relative, starting with the folder's name, at most `MAX_FOLDER_DEPTH`
/// deep, without "." or "..", drive letters or backslashes.
fn folder_path(path: &str, folder: &str) -> Option<Vec<String>> {
    if path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return None;
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 || parts.len() > MAX_FOLDER_DEPTH || parts[0] != folder {
        return None;
    }
    let drive = |p: &str| {
        let b = p.as_bytes();
        b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
    };
    parts
        .into_iter()
        .map(|p| {
            let unsafe_name = p.is_empty() || p == "." || p == ".." || drive(p);
            (!unsafe_name).then(|| sanitize_file_name(p))
        })
        .collect()
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
/// restart) in the download folder and the shared-files folders, with
/// their folders (links are not followed).
fn remove_stale_parts(download_dir: &Path) {
    // (folder, how deep its folders are looked through)
    let mut dirs = vec![
        (download_dir.to_path_buf(), 0),
        (download_dir.join(groups::REPO_FOLDER), MAX_FOLDER_DEPTH),
    ];
    while let Some((dir, depth)) = dirs.pop() {
        let Ok(list) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in list.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && depth > 0 {
                dirs.push((entry.path(), depth - 1));
            } else if kind.is_file() && entry.file_name().to_str().is_some_and(is_part_file) {
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
    /// Permits for a receiving stream's `DISK_BUFFER`.
    large_buffers: Arc<tokio::sync::Semaphore>,
    /// The computer is going to sleep: no dialing until `resume`.
    suspended: AtomicBool,
    /// Screen locked or on battery: see `set_economy`.
    economy: AtomicBool,
    /// Wakes the timer task early: work was added (see `Node::with`).
    timers: Notify,
    /// Wakes the task that tells peers our local addresses.
    lan_refresh: Notify,
    /// Speed limits of what we send and receive (see `set_speed_limit`).
    up: Arc<Throttle>,
    down: Arc<Throttle>,
    /// Held while picking where a received folder is saved.
    folder_lock: Mutex<()>,
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
            friends: Saved::new(opts.friends),
            chats: chats::Chats::open(opts.messages_dir),
            groups: group_states,
            gmeta: Saved::new(gmeta),
            groups_dir: opts.groups_dir,
            conns: HashMap::new(),
            dialing: HashSet::new(),
            fast_retries: HashMap::new(),
            backoff: HashMap::new(),
            reconnected_all: None,
            transfers: HashMap::new(),
            chat_last_sent: HashMap::new(),
            group_seen: HashMap::new(),
            save_at: None,
            messages_save_at: None,
            last_messages_save: Instant::now(),
            wake_timers: false,
            rate: HashMap::new(),
            lan_peers: HashMap::new(),
            lan_down: HashMap::new(),
            lan_advert: None,
            read_receipts: true,
            read_unsent: HashMap::new(),
            read_sent: HashMap::new(),
            typing_sent: HashMap::new(),
            folders_in: HashMap::new(),
            folders_out: HashMap::new(),
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
            large_buffers: Arc::new(tokio::sync::Semaphore::new(MAX_LARGE_BUFFERS)),
            suspended: AtomicBool::new(false),
            economy: AtomicBool::new(false),
            timers: Notify::new(),
            lan_refresh: Notify::new(),
            up: Arc::new(Throttle::new()),
            down: Arc::new(Throttle::new()),
            folder_lock: Mutex::new(()),
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
        node.spawn(node.clone().watch_relay());
        node.with(|s| node.reconnect_all(s, false));
        // Dials made before we reach our relay often fail: once online, give
        // every peer a fresh round of quick retries.
        let n = node.clone();
        node.spawn(async move {
            let _ = tokio::time::timeout(ONLINE_TIMEOUT, n.0.endpoint.online()).await;
            n.with(|s| n.reconnect_all(s, false));
        });
        Ok(node)
    }

    /// Close every connection cleanly so friends see us offline right away.
    pub async fn shutdown(&self) {
        self.save_now();
        // A folder's unfinished files are in it, where the next start does
        // not look for them (see `remove_stale_parts`).
        let parts: Vec<PathBuf> = self.with(|s| {
            s.transfers
                .values()
                .filter(|t| t.dir == Dir::In && t.folder.is_some() && !t.status.is_final())
                .filter_map(|t| t.part_path.clone())
                .collect()
        });
        for part in parts {
            let _ = std::fs::remove_file(part);
        }
        self.0.endpoint.close().await;
    }

    pub fn save_now(&self) {
        self.with(|s| {
            let _ = s.friends.save_now();
            s.chats.save_now();
            let _ = s.gmeta.save_now();
            for (path, lines) in s.take_group_writes() {
                let _ = groups::append_lines(&path, &lines);
            }
            s.index_group_logs();
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

    /// Tell the UI each change of our home relay. Once it reconnects after
    /// being lost, the network is back: offline peers are dialed right away.
    async fn watch_relay(self) {
        let mut watcher = self.0.endpoint.home_relay_status();
        let mut last = None;
        let mut was_connected = false;
        let mut lost = false;
        loop {
            let status = self.relay_status();
            let connected = status.as_ref().is_some_and(|r| r.connected);
            if was_connected && !connected {
                lost = true;
            } else if connected && lost {
                lost = false;
                self.with(|s| self.reconnect_all(s, false));
            }
            was_connected = connected;
            if status != last {
                last = status.clone();
                (self.0.on_event)(Event::RelayStatus(status));
            }
            if watcher.updated().await.is_err() || self.0.endpoint.is_closed() {
                return;
            }
        }
    }

    fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(&self, f: F) {
        self.0.rt.spawn(f);
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let (result, events, wake) = {
            let mut s = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let result = f(&mut s);
            s.schedule_saves();
            let wake = std::mem::take(&mut s.wake_timers);
            (result, std::mem::take(&mut s.events), wake)
        };
        if wake {
            self.0.timers.notify_one();
        }
        for e in events {
            (self.0.on_event)(e);
        }
        result
    }

    /// Everything that happens on its own: progress of running transfers,
    /// offers, chats and group syncs sent again, offline peers dialed,
    /// changes saved… The task sleeps until the next of them is due, or
    /// until `Node::with` wakes it because work was added. With nothing
    /// going on, only the dial schedule of offline peers and the sweep every
    /// `SWEEP_INTERVAL` (which runs everything) wake it.
    async fn timers(self) {
        let mut last_wall = SystemTime::now();
        let mut last_mono = Instant::now();
        let mut sweep_at = Instant::now() + SWEEP_INTERVAL;
        let mut progress_at: Option<Instant> = None;
        let mut groups_at: Option<Instant> = None;
        loop {
            if self.0.endpoint.is_closed() {
                return;
            }
            let now = Instant::now();
            // The wall clock moving on much more than ours means the
            // computer was asleep (on systems where `resume` is not called):
            // detect the new network and dial offline peers again right away.
            let wall = SystemTime::now();
            let asleep = wall
                .duration_since(last_wall)
                .is_ok_and(|d| d > now.duration_since(last_mono) + WAKE_GAP);
            (last_wall, last_mono) = (wall, now);
            if asleep {
                self.wake();
            }
            let sweep = now >= sweep_at;
            if sweep {
                sweep_at = now + SWEEP_INTERVAL;
            }
            let economy = self.0.economy.load(Ordering::Relaxed);

            let (stores, chats, logs, next) = self.with(|s| {
                let active: Vec<String> = s
                    .transfers
                    .values()
                    .filter(|t| t.status.is_active() && !t.hidden)
                    .map(|t| t.id.clone())
                    .collect();
                if active.is_empty() {
                    progress_at = None;
                } else if progress_at.is_none_or(|at| at <= now) {
                    // The first progress comes one interval after the start.
                    if progress_at.is_some() {
                        for id in active {
                            s.emit_transfer(&id);
                        }
                    }
                    progress_at = Some(now + PROGRESS_INTERVAL);
                }
                self.offer_queued(s);
                self.probe_paused(s);
                self.accept_queued(s);
                self.retry_downloads(s, false);
                self.retry_chats(s, now, sweep);
                self.dial_due(s, now, sweep);
                // Groups that differ from a connected member's are compared
                // again, in case what was sent got lost.
                let interval = if economy {
                    ECONOMY_GROUPS_SYNC
                } else {
                    RECONNECT_INTERVAL
                };
                if sweep {
                    self.groups_tick(s);
                    groups_at = None;
                } else if s.groups_settled() {
                    groups_at = None;
                } else if groups_at.is_none() {
                    groups_at = Some(now + interval);
                } else if groups_at.is_some_and(|at| at <= now) {
                    self.groups_tick(s);
                    groups_at = Some(now + interval);
                }
                if sweep || s.transfers.len() > MAX_FINISHED {
                    s.forget_finished();
                }
                if sweep {
                    s.chats.unload_idle(now);
                }

                let mut stores = [None, None];
                let mut logs = Vec::new();
                let mut chats = Vec::new();
                if s.save_at.is_some_and(|at| at <= now) {
                    s.save_at = None;
                    stores[0] = s.friends.take_dirty();
                    stores[1] = s.gmeta.take_dirty();
                    logs = s.take_group_writes();
                }
                if s.messages_save_at.is_some_and(|at| at <= now) {
                    s.messages_save_at = None;
                    s.last_messages_save = now;
                    chats = s.chats.take_dirty();
                }
                s.schedule_saves();
                let next = self.next_due(s, now, [progress_at, groups_at, Some(sweep_at)]);
                // Whatever was added meanwhile is in `next` already.
                s.wake_timers = false;
                (stores, chats, logs, next)
            });

            let mut failed = [false; 2];
            for (i, (path, json)) in stores
                .into_iter()
                .enumerate()
                .filter_map(|(i, store)| Some((i, store?)))
            {
                let written = tokio::task::spawn_blocking(move || write_atomic(&path, &json)).await;
                failed[i] = !matches!(written, Ok(Ok(())));
            }
            // Only chats that changed are written.
            let mut failed_chats = Vec::new();
            for (id, path, json) in chats {
                let written = tokio::task::spawn_blocking(move || write_atomic(&path, &json)).await;
                if !matches!(written, Ok(Ok(()))) {
                    failed_chats.push(id);
                }
            }
            if failed.contains(&true) || !failed_chats.is_empty() {
                // Taking the snapshot cleared the dirty flag: retry later.
                self.with(|s| {
                    for id in &failed_chats {
                        s.chats.mark_dirty(id);
                    }
                    if failed[0] {
                        s.friends.mark_dirty();
                    }
                    if failed[1] {
                        s.gmeta.mark_dirty();
                    }
                });
            }
            if !logs.is_empty() {
                let _ = tokio::task::spawn_blocking(move || {
                    for (path, lines) in logs {
                        let _ = groups::append_lines(&path, &lines);
                    }
                })
                .await;
                // What was appended no longer needs to stay in memory.
                self.with(|s| s.index_group_logs());
            }

            let next = next.max(Instant::now() + MIN_SLEEP);
            tokio::select! {
                _ = tokio::time::sleep_until(next.into()) => {}
                _ = self.0.timers.notified() => {}
            }
        }
    }

    /// The soonest time something the timer task does is due, given
    /// `others` it keeps itself.
    fn next_due(&self, s: &State, now: Instant, others: [Option<Instant>; 3]) -> Instant {
        let mut next = others
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(now + SWEEP_INTERVAL);
        let mut at = |t: Instant| next = next.min(t);
        if let Some(t) = s.save_at {
            at(t);
        }
        if let Some(t) = s.messages_save_at {
            at(t);
        }
        for t in s.transfers.values() {
            match (t.dir, t.status) {
                // See `offer_queued`.
                (Dir::Out, Status::WaitingConsent | Status::Paused)
                    if !t.serve
                        && !t.held()
                        && t.peer_name.is_none()
                        && s.conns.contains_key(&t.friend_id) =>
                {
                    match t.last_sent.map(|l| l + REOFFER_DELAY).filter(|due| *due > now) {
                        Some(due) => at(due),
                        // Due already: waits for the offer budget.
                        None => {
                            let (key, budget) = s.offer_budget(t);
                            at(s.limit_frees_at(&key, budget)
                                .map_or(now + RATE_WINDOW, |free| free + OFFER_SLACK))
                        }
                    }
                }
                // See `probe_paused`.
                (Dir::In, Status::Paused)
                    if t.repo.is_none()
                        && t.peer_name.is_none()
                        && t.part_path.is_some()
                        && !t.held() =>
                {
                    if let Some(c) = s.conns.get(&t.friend_id) {
                        let after = t.last_sent.map_or(c.since, |l| l.max(c.since));
                        at(after + REOFFER_DELAY);
                    }
                }
                // See `retry_downloads`.
                (Dir::In, Status::Paused) => {
                    if let Some(due) = t.repo.as_ref().and_then(|r| r.next_try) {
                        at(due);
                    }
                }
                _ => {}
            }
        }
        for (friend_id, sent) in s.chat_last_sent.values() {
            if s.conns.contains_key(friend_id) {
                at(*sent + RECONNECT_INTERVAL);
            }
        }
        if !self.0.suspended.load(Ordering::Relaxed) {
            for (id, b) in &s.backoff {
                if !s.conns.contains_key(id) && !s.dialing.contains(id) {
                    at(b.next);
                }
            }
        }
        next
    }

    /// Send again the chats not acknowledged for a while (the receiver drops
    /// them when flooded); on a `sweep`, every pending one.
    fn retry_chats(&self, s: &mut State, now: Instant, sweep: bool) {
        let due: HashSet<String> = if sweep {
            s.conns.keys().filter(|id| s.is_friend(id)).cloned().collect()
        } else {
            s.chat_last_sent
                .values()
                .filter(|(f, at)| now >= *at + RECONNECT_INTERVAL && s.conns.contains_key(f))
                .map(|(f, _)| f.clone())
                .collect()
        };
        for friend_id in &due {
            s.flush_outbox(friend_id, false);
        }
        // Still old: no longer pending (acknowledged, or out of the history).
        s.chat_last_sent
            .retain(|_, (f, at)| !(due.contains(f.as_str()) && now.duration_since(*at) >= CHAT_RETRY));
    }

    /// Dial the offline peers whose turn it is; on a `sweep`, also any we
    /// may talk to that has no schedule.
    fn dial_due(&self, s: &mut State, now: Instant, sweep: bool) {
        if self.0.suspended.load(Ordering::Relaxed) {
            return;
        }
        let mut due: Vec<String> = Vec::new();
        let (conns, dialing) = (&s.conns, &s.dialing);
        s.backoff.retain(|id, b| {
            if conns.contains_key(id) {
                return false;
            }
            if b.next <= now && !dialing.contains(id) {
                due.push(id.clone());
            }
            true
        });
        if sweep {
            let unscheduled: Vec<String> = s
                .allowed_peers()
                .into_iter()
                .filter(|id| {
                    !s.conns.contains_key(id) && !s.dialing.contains(id) && !s.backoff.contains_key(id)
                })
                .collect();
            due.extend(unscheduled);
        }
        for id in due {
            if s.is_allowed(&id) && is_valid_id(&id) {
                self.dial_now(s, &id);
            } else {
                s.backoff.remove(&id);
            }
        }
    }

    /// Call after the computer wakes up (same as `resume`).
    pub fn wake(&self) {
        let node = self.clone();
        self.spawn(async move {
            node.0.endpoint.network_change().await;
            node.with(|s| node.reconnect_all(s, false));
        });
        self.refresh_lan();
    }

    /// Dial every offline peer now with a fresh round of quick retries (the
    /// dial schedule starts over), and send again what waits for connected
    /// ones. With `debounce`, a second call within `RECONNECT_ALL_MIN`
    /// dials them once that time is up instead.
    fn reconnect_all(&self, s: &mut State, debounce: bool) {
        let now = Instant::now();
        s.fast_retries.clear();
        let offline: Vec<String> = s
            .allowed_peers()
            .into_iter()
            .filter(|id| !s.conns.contains_key(id))
            .collect();
        let recent = s
            .reconnected_all
            .filter(|at| now.duration_since(*at) < RECONNECT_ALL_MIN);
        if let (true, Some(last)) = (debounce, recent) {
            let next = last + RECONNECT_ALL_MIN;
            for id in offline {
                let b = s.backoff.entry(id).or_insert(Backoff { step: 0, next });
                b.step = 0;
                b.next = b.next.min(next);
            }
            s.wake_timers = true;
            return;
        }
        s.reconnected_all = Some(now);
        s.backoff.clear();
        for id in offline {
            self.dial_now(s, &id);
        }
        let online: Vec<String> = s.conns.keys().filter(|id| s.is_friend(id)).cloned().collect();
        for id in online {
            s.flush_outbox(&id, false);
        }
        self.offer_queued(s);
        self.probe_paused(s);
        self.groups_tick(s);
    }

    /// Call right before the computer sleeps: saves everything now, closes
    /// the connections so that peers see us offline at once, and stops
    /// dialing until `resume`.
    pub fn suspend(&self) {
        self.0.suspended.store(true, Ordering::Relaxed);
        self.save_now();
        self.with(|s| {
            for c in s.conns.values() {
                c.conn.close(VarInt::from_u32(CLOSE_NORMAL), b"sleep");
            }
        });
    }

    /// Call after the computer wakes up: detects the new network, dials
    /// every peer right away (the dial schedule starts over) and sends what
    /// waited: chats, offers, transfers to resume, group syncs.
    pub fn resume(&self) {
        self.0.suspended.store(false, Ordering::Relaxed);
        self.wake();
    }

    /// Call when the system reports a network change (interface up or down,
    /// new address): iroh looks for new paths, offline peers are dialed
    /// again right away and peers learn our new local addresses.
    pub fn network_changed(&self) {
        let node = self.clone();
        self.spawn(async move {
            node.0.endpoint.network_change().await;
            node.with(|s| node.reconnect_all(s, true));
        });
        self.refresh_lan();
    }

    /// Screen locked or on battery: offline peers are dialed less often
    /// (every 30 minutes at most) and background work is spaced out. We stay
    /// reachable: messages, offers and transfers go on as usual. Turning it
    /// off catches up at once, as `resume` does without the network check.
    pub fn set_economy(&self, on: bool) {
        let was = self.0.economy.swap(on, Ordering::Relaxed);
        if was && !on {
            self.with(|s| self.reconnect_all(s, false));
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
                .map(Friend::display_name)
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
        // No name: theirs is used, once they send it.
        Ok(self.with(|s| {
            match s.friends.data.iter_mut().find(|f| f.id == id) {
                Some(f) if !name.is_empty() => f.name = name,
                Some(_) => {}
                None => s.friends.data.push(Friend {
                    id: id.clone(),
                    name,
                    nickname: String::new(),
                    auto_accept: false,
                    unsent_reactions: HashMap::new(),
                }),
            }
            // Stays dirty on failure: the timer retries.
            let _ = s.friends.save_now();
            s.fast_retries.remove(&id);
            self.dial(s, &id);
            friend_views(s)
        }))
    }

    /// The name we give a friend; empty to use their nickname.
    pub fn rename_friend(&self, id: &str, name: &str) -> Vec<FriendView> {
        let name: String = name.trim().chars().take(MAX_FRIEND_NAME).collect();
        self.with(|s| {
            if let Some(f) = s.friends.data.iter_mut().find(|f| f.id == id) {
                f.name = name;
                // Stays dirty on failure: the timer retries.
                let _ = s.friends.save_now();
            }
            friend_views(s)
        })
    }

    /// Accept the friend's offers without asking (programs still ask).
    pub fn set_friend_auto_accept(&self, id: &str, on: bool) -> Vec<FriendView> {
        self.with(|s| {
            if let Some(f) = s.friends.data.iter_mut().find(|f| f.id == id) {
                f.auto_accept = on;
                // Stays dirty on failure: the timer retries.
                let _ = s.friends.save_now();
            }
            friend_views(s)
        })
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

    /// Dial a peer we have something for right away; if it stays offline,
    /// its dial schedule starts over.
    fn dial(&self, s: &mut State, friend_id: &str) {
        if let Some(b) = s.backoff.get_mut(friend_id) {
            b.step = 0;
        }
        self.dial_now(s, friend_id);
    }

    /// Dial a peer; when that fails, schedule the next try (see `BACKOFF`).
    fn dial_now(&self, s: &mut State, friend_id: &str) {
        if self.0.suspended.load(Ordering::Relaxed)
            || !is_valid_id(friend_id)
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
            let economy = node.0.economy.load(Ordering::Relaxed);
            node.with(|s| {
                s.dialing.remove(&friend_id);
                if s.conns.contains_key(&friend_id) || !s.is_allowed(&friend_id) {
                    s.backoff.remove(&friend_id);
                    return;
                }
                let tries = s.fast_retries.entry(friend_id.clone()).or_insert(0);
                *tries += 1;
                let fast = *tries <= FAST_RETRIES;
                let now = Instant::now();
                let b = s
                    .backoff
                    .entry(friend_id.clone())
                    .or_insert(Backoff { step: 0, next: now });
                if fast {
                    b.next = now + FAST_RETRY_DELAY;
                } else {
                    b.next = now + backoff_delay(b.step, economy);
                    b.step = b.step.saturating_add(1);
                }
                s.wake_timers = true;
            });
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
        s.backoff.remove(&friend_id);
        s.group_seen.retain(|(_, peer), _| *peer != friend_id);
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
        s.send_profile(&friend_id);
        s.flush_outbox(&friend_id, true);
        s.send_read(&friend_id);
        // A peer still holding a transfer says so again (it may have
        // restarted since, and forgotten it).
        for t in s.transfers.values_mut().filter(|t| t.friend_id == friend_id) {
            t.peer_held = false;
        }
        s.send_holds(&friend_id);
        let unsent: Vec<(String, &'static str)> = s
            .transfers
            .values()
            .filter(|t| t.friend_id == friend_id)
            .filter_map(|t| Some((t.id.clone(), t.stop_unsent?)))
            .collect();
        for (tid, kind) in unsent {
            s.send_stop(&tid, kind);
        }
        let to_offer: Vec<(String, Status)> = s
            .transfers
            .values()
            .filter(|t| {
                t.dir == Dir::Out
                    && !t.serve
                    && t.friend_id == friend_id
                    && !t.status.is_final()
                    && t.status != Status::Sending
            })
            .map(|t| (t.id.clone(), t.status))
            .collect();
        for (tid, status) in to_offer {
            if matches!(status, Status::Connecting | Status::WaitingConsent) {
                // Not answered yet: offered within the budget, below.
                s.transfers.get_mut(&tid).unwrap().last_sent = None;
                s.set_status(&tid, Status::WaitingConsent, None);
            } else {
                // Accepted already: the receiver knows it, no limit applies.
                self.offer(s, &tid);
            }
        }
        self.offer_queued(s);
        self.groups_on_connect(s, &friend_id);
    }

    fn on_close(&self, s: &mut State, friend_id: &str, conn_id: u64) {
        if s.conns.get(friend_id).is_some_and(|c| c.id == conn_id) {
            s.conns.remove(friend_id);
            s.events.push(Event::FriendStatus {
                friend_id: friend_id.to_string(),
                online: false,
            });
            if s.is_allowed(friend_id) {
                // It dials us when it comes back; this is in case it cannot.
                let next = Instant::now() + RECONNECT_INTERVAL;
                s.backoff
                    .insert(friend_id.to_string(), Backoff { step: 0, next });
                s.wake_timers = true;
            }
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
            Some("chat_react") if friend => self.on_chat_react(s, friend_id, msg),
            Some("chat_react_ack") if friend => self.on_chat_react_ack(s, friend_id, msg),
            Some("chat_read") if friend => self.on_chat_read(s, friend_id, msg),
            Some("typing") if friend => self.on_typing(s, friend_id),
            Some("profile") if friend => self.on_profile(s, friend_id, msg),
            Some("file_offer") if friend => self.on_offer(s, friend_id, msg),
            Some("file_accept") => self.on_accept(s, friend_id, conn_id, msg),
            Some("file_reject") => self.on_peer_stop(s, friend_id, tid, Status::Declined),
            Some("file_cancel") => self.on_peer_stop(s, friend_id, tid, Status::Cancelled),
            Some("file_complete") => self.on_complete(s, friend_id, tid),
            Some("file_pause") => self.on_peer_hold(s, friend_id, tid, true),
            Some("file_resume") => self.on_peer_hold(s, friend_id, tid, false),
            Some("folder_accept") if friend => self.on_folder_answer(s, friend_id, msg, true),
            Some("folder_reject") if friend => self.on_folder_answer(s, friend_id, msg, false),
            Some("group_invite") if friend => self.on_group_invite(s, friend_id, msg),
            Some("group_decline") => self.on_group_decline(s, friend_id, msg),
            Some("group_have") => {
                self.on_group_have(s, friend_id, msg);
                // Maybe in sync with this member now, or not any more.
                s.wake_timers = true;
            }
            Some("group_entries") => {
                s.note_group_entries(friend_id, msg);
                self.on_group_entries(s, friend_id, msg);
                s.wake_timers = true;
            }
            Some("blob_req") => self.on_blob_req(s, friend_id, conn_id, msg),
            Some("blob_missing") => self.on_blob_missing(s, friend_id, msg),
            Some("lan") => self.on_lan(s, friend_id, msg),
            _ => {}
        }
    }

    // ─── Chat ────────────────────────────────────────────────────────────────

    pub fn messages(&self, friend_id: &str) -> Vec<ChatMessage> {
        self.with(|s| s.chats.list(friend_id).clone())
    }

    /// Send a message, as a reply to the message `reply_to` of this chat if
    /// given (left out if it is not there any more).
    pub fn send_message(
        &self,
        friend_id: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<ChatMessage, &'static str> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty");
        }
        if js_len(text) > MAX_TEXT {
            return Err("too_long");
        }
        let me = self.id();
        Ok(self.with(|s| {
            let reply = reply_to.and_then(|id| {
                s.chats
                    .list(friend_id)
                    .iter()
                    .rev()
                    .find(|m| m.id == id)
                    .map(reply_of)
            });
            let entry = ChatMessage {
                id: uuid::Uuid::new_v4().to_string(),
                from: me,
                text: text.to_string(),
                ts: now_ms(),
                status: Some(MessageStatus::Pending),
                reply,
                ..Default::default()
            };
            s.chats.push(friend_id, entry.clone());
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
        let known = s.chats.list(friend_id).iter().any(|m| m.id == id);
        if !known {
            // No ack when flooded: the sender keeps the message and retries later.
            if s.is_rate_limited(friend_id) {
                return;
            }
            let me = s.me.clone();
            let reply = msg.get("reply").and_then(|r| {
                let id = r.get("id")?.as_str().filter(|id| id.len() <= 64)?;
                // Quoting one of us two only.
                let from = r
                    .get("from")?
                    .as_str()
                    .filter(|f| *f == friend_id || *f == me)?;
                let text = strip_invisible(r.get("text")?.as_str()?);
                Some(ChatReply {
                    id: id.to_string(),
                    from: from.to_string(),
                    text: text.chars().take(REPLY_CHARS).collect(),
                })
            });
            let entry = ChatMessage {
                id: id.to_string(),
                from: friend_id.to_string(),
                text: text.to_string(),
                ts: now_ms(),
                reply,
                ..Default::default()
            };
            s.chats.push(friend_id, entry.clone());
            s.events.push(Event::Message {
                friend_id: friend_id.to_string(),
                message: entry,
            });
        }
        s.send(friend_id, json!({ "type": "chat_ack", "id": id }));
    }

    /// React to a message of the chat with one of `REACTIONS`, or take our
    /// reaction back with `None`; the same emoji again takes it back too.
    /// Returns the message's reactions (author's id → emoji).
    pub fn react(
        &self,
        friend_id: &str,
        id: &str,
        emoji: Option<&str>,
    ) -> Result<HashMap<String, String>, &'static str> {
        let emoji = match emoji {
            Some(e) => Some(valid_reaction(e).ok_or("invalid_emoji")?),
            None => None,
        };
        self.with(|s| {
            if !s.is_friend(friend_id) {
                return Err("not_friend");
            }
            let me = s.me.clone();
            let m = s
                .chats
                .list_mut(friend_id)
                .iter_mut()
                .rev()
                .find(|m| m.id == id)
                .ok_or("not_found")?;
            let emoji = emoji.filter(|e| m.reactions.get(&me).map(String::as_str) != Some(*e));
            match emoji {
                Some(e) => m.reactions.insert(me, e.to_string()),
                None => m.reactions.remove(&me),
            };
            let reactions = m.reactions.clone();
            s.chats.mark_dirty(friend_id);
            if let Some(f) = s.friends.data.iter_mut().find(|f| f.id == friend_id) {
                f.unsent_reactions
                    .insert(id.to_string(), emoji.unwrap_or("").to_string());
                s.friends.mark_dirty();
            }
            if s.conns.contains_key(friend_id) {
                let key = format!("react:{id}");
                s.chat_last_sent
                    .insert(key, (friend_id.to_string(), Instant::now()));
                s.wake_timers = true;
                s.send(friend_id, reaction_msg(id, emoji.unwrap_or("")));
            } else {
                self.dial(s, friend_id);
            }
            s.events.push(Event::Reactions {
                friend_id: friend_id.to_string(),
                id: id.to_string(),
                reactions: reactions.clone(),
            });
            Ok(reactions)
        })
    }

    fn on_chat_react(&self, s: &mut State, friend_id: &str, msg: &Value) {
        let Some(id) = msg
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| id.len() <= 64)
        else {
            return;
        };
        let emoji = match msg.get("emoji") {
            None | Some(Value::Null) => None,
            Some(e) => match e.as_str().and_then(valid_reaction) {
                Some(e) => Some(e),
                None => return,
            },
        };
        // No ack when flooded: the sender tries again later.
        if s.over_limit(&format!("react:{friend_id}"), RATE_LIMIT) {
            return;
        }
        // Unknown here (beyond our history): acknowledged all the same.
        let found = s
            .chats
            .list_mut(friend_id)
            .iter_mut()
            .rev()
            .find(|m| m.id == id);
        if let Some(m) = found {
            let before = m.reactions.get(friend_id).cloned();
            if before.as_deref() != emoji {
                match emoji {
                    Some(e) => m.reactions.insert(friend_id.to_string(), e.to_string()),
                    None => m.reactions.remove(friend_id),
                };
                let reactions = m.reactions.clone();
                s.chats.mark_dirty(friend_id);
                s.events.push(Event::Reactions {
                    friend_id: friend_id.to_string(),
                    id: id.to_string(),
                    reactions,
                });
            }
        }
        s.send(
            friend_id,
            json!({ "type": "chat_react_ack", "id": id, "emoji": emoji }),
        );
    }

    fn on_chat_react_ack(&self, s: &mut State, friend_id: &str, msg: &Value) {
        let Some(id) = msg.get("id").and_then(Value::as_str) else {
            return;
        };
        let emoji = msg.get("emoji").and_then(Value::as_str).unwrap_or("");
        let Some(f) = s.friends.data.iter_mut().find(|f| f.id == friend_id) else {
            return;
        };
        // Changed again since: that one is still to acknowledge.
        if f.unsent_reactions.get(id).is_some_and(|e| e == emoji) {
            f.unsent_reactions.remove(id);
            s.friends.mark_dirty();
            s.chat_last_sent.remove(&format!("react:{id}"));
        }
    }

    /// Read receipts and "typing", sent and shown, both on or both off.
    pub fn set_read_receipts(&self, on: bool) {
        self.with(|s| {
            s.read_receipts = on;
            if !on {
                s.read_unsent.clear();
            }
        });
    }

    /// The user sees the chat (window focused, chat open): the friend's
    /// messages so far are read, which it learns if read receipts are on.
    pub fn mark_chat_read(&self, friend_id: &str) {
        self.with(|s| {
            if !s.read_receipts || !s.is_friend(friend_id) {
                return;
            }
            let last = s
                .chats
                .list(friend_id)
                .iter()
                .rev()
                .find(|m| m.from == friend_id)
                .map(|m| m.id.clone());
            let Some(last) = last else { return };
            let known = |m: &HashMap<String, String>| m.get(friend_id) == Some(&last);
            if known(&s.read_unsent) || known(&s.read_sent) {
                return;
            }
            s.read_unsent.insert(friend_id.to_string(), last);
            s.send_read(friend_id);
        });
    }

    /// The friend saw our messages up to `upTo`.
    fn on_chat_read(&self, s: &mut State, friend_id: &str, msg: &Value) {
        if !s.read_receipts {
            return;
        }
        let Some(up_to) = msg.get("upTo").and_then(Value::as_str) else {
            return;
        };
        let me = s.me.clone();
        let list = s.chats.list_mut(friend_id);
        let Some(end) = list.iter().rposition(|m| m.id == up_to && m.from == me) else {
            return;
        };
        let mut ids = Vec::new();
        // The newest first: those before were read already, once one was.
        for m in list[..=end].iter_mut().rev() {
            if m.from != me || m.status.is_none() {
                continue;
            }
            if m.status == Some(MessageStatus::Read) {
                break;
            }
            m.status = Some(MessageStatus::Read);
            ids.push(m.id.clone());
        }
        if ids.is_empty() {
            return;
        }
        for id in &ids {
            s.chat_last_sent.remove(id);
        }
        s.chats.mark_dirty(friend_id);
        s.events.push(Event::MessagesRead {
            friend_id: friend_id.to_string(),
            ids,
        });
    }

    /// The user is typing to the friend (the UI calls it every few seconds
    /// while they type): it is told, at most every `TYPING_INTERVAL`.
    pub fn chat_typing(&self, friend_id: &str) {
        self.with(|s| {
            if !s.read_receipts || !s.is_friend(friend_id) || !s.conns.contains_key(friend_id) {
                return;
            }
            let now = Instant::now();
            let recent = s
                .typing_sent
                .get(friend_id)
                .is_some_and(|t| now.duration_since(*t) < TYPING_INTERVAL);
            if recent {
                return;
            }
            s.typing_sent.insert(friend_id.to_string(), now);
            s.send(friend_id, json!({ "type": "typing" }));
        });
    }

    fn on_typing(&self, s: &mut State, friend_id: &str) {
        if s.read_receipts && !s.over_limit(&format!("typing:{friend_id}"), TYPING_RATE_LIMIT) {
            s.events.push(Event::Typing {
                friend_id: friend_id.to_string(),
            });
        }
    }

    fn on_profile(&self, s: &mut State, friend_id: &str, msg: &Value) {
        let Some(nick) = msg.get("nick").and_then(Value::as_str) else {
            return;
        };
        let nick: String = nick
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(crate::group::MAX_NICK)
            .collect();
        let Some(f) = s.friends.data.iter_mut().find(|f| f.id == friend_id) else {
            return;
        };
        if f.nickname == nick {
            return;
        }
        f.nickname = nick;
        s.friends.mark_dirty();
        s.events.push(Event::FriendsChanged);
    }

    /// Where the image of a chat entry is on disk: ours, or theirs once received.
    pub fn chat_file_path(&self, friend_id: &str, id: &str) -> Option<PathBuf> {
        self.with(|s| {
            let m = s.chats.list(friend_id).iter().find(|m| m.id == id)?;
            let file = m.file.as_ref()?;
            if let Some(path) = &file.path {
                // Theirs only once complete: the path may be set while saving.
                let ours = m.from == s.me;
                if ours || file.state == Some(Status::Completed) {
                    return Some(PathBuf::from(path));
                }
            }
            let t = s.transfers.get(id)?;
            (t.status == Status::Completed)
                .then(|| t.saved_path.clone())
                .flatten()
        })
    }

    fn on_chat_ack(&self, s: &mut State, friend_id: &str, msg: &Value) {
        let Some(id) = msg.get("id").and_then(Value::as_str) else {
            return;
        };
        let me = s.me.clone();
        // Not in memory: none of ours is pending there.
        let Some(list) = s.chats.loaded_mut(friend_id) else {
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
        s.chats.mark_dirty(friend_id);
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
            list.into_iter().map(|t| s.transfer_view(t)).collect()
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

    /// A received file that may be opened with a click: never one that runs code.
    pub fn openable_path(&self, id: &str) -> Option<PathBuf> {
        self.with(|s| {
            let t = s.transfers.get(id)?;
            let ok = t.dir == Dir::In
                && t.status == Status::Completed
                && !is_executable(&t.file_name);
            ok.then(|| t.saved_path.clone()).flatten()
        })
    }

    // ── Sender side ──

    pub fn send_files(&self, friend_id: &str, paths: &[PathBuf]) -> usize {
        let files: Vec<SourceFile> = paths
            .iter()
            .filter_map(|p| {
                std::fs::metadata(p)
                    .ok()
                    .filter(|m| m.is_file())
                    .map(|m| SourceFile {
                        path: p.clone(),
                        size: m.len(),
                        mtime: m.modified().ok(),
                        rel_path: None,
                    })
            })
            .collect();
        self.queue_files(friend_id, &files, None)
    }

    /// Send a folder and everything in it, one transfer per file, keeping
    /// the tree (empty folders and links are left out). Reads the whole
    /// tree: call it off the async threads. Returns how many files are sent;
    /// `Err` is "not_found", "empty", "too_many" (over 10,000 files) or
    /// "too_deep" (over 32 levels).
    pub fn send_folder(&self, friend_id: &str, dir: &Path) -> Result<usize, &'static str> {
        let (name, files) = walk_folder(dir)?;
        let folder = FolderMeta {
            id: hex::encode(rand::random::<[u8; TID_BYTES]>()),
            name,
            files: files.len() as u64,
            size: files.iter().map(|f| f.size).sum(),
        };
        Ok(self.queue_files(friend_id, &files, Some(Arc::new(folder))))
    }

    fn queue_files(
        &self,
        friend_id: &str,
        files: &[SourceFile],
        folder: Option<Arc<FolderMeta>>,
    ) -> usize {
        self.with(|s| {
            for file in files {
                let tid = hex::encode(rand::random::<[u8; TID_BYTES]>());
                let name = file
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                s.add_transfer(
                    tid.clone(),
                    Dir::Out,
                    friend_id,
                    name,
                    file.size,
                    Status::Connecting,
                );
                let t = s.transfers.get_mut(&tid).unwrap();
                t.src_path = Some(file.path.clone());
                t.src_mtime = file.mtime;
                t.folder = folder.clone();
                t.rel_path = file.rel_path.clone();
                let path = &file.path;
                // A folder's pictures are not shown in the chat one by one.
                if is_image(&t.file_name) && t.folder.is_none() {
                    let me = s.me.clone();
                    let src = path.to_string_lossy().into_owned();
                    s.add_chat_file(friend_id, &tid, me, Some(src));
                }
                if s.conns.contains_key(friend_id) {
                    // Offered right away, or once the budget allows.
                    s.set_status(&tid, Status::WaitingConsent, None);
                } else {
                    s.emit_transfer(&tid);
                    self.dial(s, friend_id);
                }
            }
            self.offer_queued(s);
        });
        files.len()
    }

    fn offer(&self, s: &mut State, tid: &str) {
        // Paused by hand: offered again once resumed.
        let Some(t) = s.transfers.get_mut(tid).filter(|t| !t.held()) else {
            return;
        };
        t.last_sent = Some(Instant::now());
        let mut msg =
            json!({ "type": "file_offer", "tid": t.id, "name": t.file_name, "size": t.file_size });
        if let (Some(folder), Some(rel)) = (&t.folder, &t.rel_path) {
            // Older versions ignore them and save the file with the others.
            msg["path"] = json!(rel);
            msg["folder"] = json!({
                "id": folder.id,
                "name": folder.name,
                "files": folder.files,
                "size": folder.size,
            });
        }
        let status = t.status;
        let friend_id = t.friend_id.clone();
        s.send(&friend_id, msg);
        if status == Status::Connecting {
            s.set_status(tid, Status::WaitingConsent, None);
        } else {
            s.emit_transfer(tid);
        }
    }

    /// Send the offers not answered yet to connected friends: new ones
    /// first, then those unanswered for `REOFFER_DELAY`, at most
    /// `OFFER_BUDGET` per friend and `RATE_WINDOW` so that the receiver's
    /// rate limit does not drop them.
    fn offer_queued(&self, s: &mut State) {
        let now = Instant::now();
        let mut due: Vec<(Option<Instant>, u64, String, usize, String)> = s
            .transfers
            .values()
            .filter(|t| {
                t.dir == Dir::Out
                    && !t.serve
                    && !t.held()
                    && t.peer_name.is_none()
                    && matches!(t.status, Status::WaitingConsent | Status::Paused)
                    && t
                        .last_sent
                        .is_none_or(|at| now.duration_since(at) >= REOFFER_DELAY)
                    && s.conns.contains_key(&t.friend_id)
            })
            .map(|t| {
                let (key, budget) = s.offer_budget(t);
                (t.last_sent, t.seq, key, budget, t.id.clone())
            })
            .collect();
        if due.is_empty() {
            return;
        }
        due.sort();
        for (_, _, key, budget, tid) in due {
            // The receiver counts them from when they arrive, a bit later.
            let waiting = s
                .limit_frees_at(&key, budget)
                .is_some_and(|at| at + OFFER_SLACK > now);
            if !waiting && !s.over_limit(&key, budget) {
                self.offer(s, &tid);
            }
        }
    }

    fn on_accept(&self, s: &mut State, friend_id: &str, conn_id: u64, msg: &Value) {
        let Some(tid) = msg.get("tid").and_then(Value::as_str) else {
            return;
        };
        let Some(t) = s.transfers.get_mut(tid) else {
            // Cancelled long ago or before a restart: a receiver still waiting
            // for it (see `probe_paused`) can give up.
            if is_tid(tid) {
                s.send(friend_id, json!({ "type": "file_cancel", "tid": tid }));
            }
            return;
        };
        if t.dir != Dir::Out || t.friend_id != friend_id {
            return;
        }
        // The receiver asking for the bytes is not holding it any more.
        t.peer_held = false;
        if t.held && !t.status.is_final() {
            s.send(friend_id, json!({ "type": "file_pause", "tid": tid }));
            s.emit_transfer(tid);
            return;
        }
        if t.status.is_final() {
            let stopped = matches!(t.status, Status::Cancelled | Status::Error) && !t.serve;
            if stopped {
                s.send(friend_id, json!({ "type": "file_cancel", "tid": tid }));
            }
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
            throttle: self.0.up.clone(),
        };
        // Opening a local connection costs more than it saves on a small file.
        if self.0.lan.is_some() && job.size - offset >= SMALL_FILE {
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
                // Connection lost (on_close re-offers on the next one), or
                // only the stream.
                Ok(Streamed::Stopped) => self.on_stall(s, &tid, session),
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

    /// The data stream stopped although its session goes on. If the
    /// connection is still up nothing else would restart it: offer again
    /// shortly, which makes the receiver answer with the offset it has.
    fn on_stall(&self, s: &mut State, tid: &str, session: u64) {
        // A shared file is never offered: its downloader asks again itself.
        let Some(t) = s.transfers.get_mut(tid).filter(|t| !t.serve) else {
            return;
        };
        let live = s
            .conns
            .get(&t.friend_id)
            .is_some_and(|c| Some(c.id) == t.conn_id && c.conn.close_reason().is_none());
        if !live {
            return;
        }
        let bytes = t.bytes();
        t.stalls = if bytes > t.stall_bytes { 1 } else { t.stalls + 1 };
        t.stall_bytes = bytes;
        if t.stalls > MAX_STALLS {
            s.send_stop(tid, "file_cancel");
            self.stop_transfer(s, tid, Status::Error, Some("Transfert interrompu".into()));
            return;
        }
        // Offered again below, not by `offer_queued`.
        t.last_sent = Some(Instant::now());
        s.set_status(tid, Status::Paused, None);
        let node = self.clone();
        let tid = tid.to_string();
        self.spawn(async move {
            tokio::time::sleep(STALL_RETRY).await;
            node.with(|s| {
                let waiting = s.transfers.get(&tid).is_some_and(|t| {
                    t.status == Status::Paused
                        && t.shared.session() == session
                        && s.conns.contains_key(&t.friend_id)
                });
                if waiting {
                    node.offer(s, &tid);
                }
            });
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
                // Paused by hand here: it waits until we resume.
                Status::Receiving | Status::Paused if known.held => Some("file_pause"),
                Status::Receiving | Status::Paused => {
                    known.resyncing = false;
                    known.peer_held = false;
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
        let name = sanitize_file_name(name);
        let folder = parse_folder(msg, &name);
        let key = folder
            .as_ref()
            .map(|(f, _)| (friend_id.to_string(), f.id.clone()));
        let known = key.as_ref().and_then(|k| s.folders_in.get(k));
        let decision = known.and_then(|f| f.decision);
        let fast = known.is_some_and(|f| f.fast);
        if decision == Some(false) || known.is_some_and(|f| f.files >= MAX_FOLDER_FILES) {
            s.send(friend_id, json!({ "type": "file_reject", "tid": tid }));
            return;
        }
        // A flood of offers: ignored (a real sender offers again when it
        // reconnects). A folder accepted already comes faster.
        let limited = if fast {
            s.over_limit(&format!("folder_offer:{friend_id}"), FOLDER_RATE_LIMIT)
        } else {
            let pending = s
                .transfers
                .values()
                .filter(|t| t.friend_id == friend_id && t.status == Status::Pending && !t.auto)
                .count();
            pending >= MAX_PENDING_OFFERS || s.is_rate_limited(&format!("offer:{friend_id}"))
        };
        if limited {
            return;
        }
        // A trusted friend's files come without asking, except programs.
        let trusted = !is_executable(&name)
            && s.friends
                .data
                .iter()
                .any(|f| f.id == friend_id && f.auto_accept);
        let auto = decision == Some(true) || trusted;
        let id = s.add_transfer(
            tid.to_string(),
            Dir::In,
            friend_id,
            name,
            size,
            Status::Pending,
        );
        let t = s.transfers.get_mut(&id).unwrap();
        t.auto = auto;
        t.queued = auto;
        let notify = !auto;
        if let (Some((meta, parts)), Some(key)) = (folder, key) {
            t.rel_path = Some(parts.join("/"));
            let (files, folder_id) = (meta.files, meta.id.clone());
            t.folder = Some(Arc::new(meta));
            let entry = s.folders_in.entry(key).or_default();
            entry.files += 1;
            // One notification for the whole folder.
            let notify = notify && !entry.notified;
            entry.notified |= notify;
            if auto && !entry.fast {
                // It may offer the rest without waiting.
                entry.fast = true;
                s.send(
                    friend_id,
                    json!({ "type": "folder_accept", "folder": folder_id, "rate": FAST_FOLDER_BUDGET }),
                );
            }
            s.emit_transfer(&id);
            if notify {
                let view = s.transfers[&id].view();
                let files = usize::try_from(files).unwrap_or(usize::MAX);
                s.events.push(Event::FileOffer(view, files));
            }
        } else {
            if is_image(&s.transfers[&id].file_name) {
                s.add_chat_file(friend_id, &id, friend_id.to_string(), None);
            }
            s.emit_transfer(&id);
            if notify {
                let view = s.transfers[&id].view();
                s.events.push(Event::FileOffer(view, 1));
            }
        }
        if auto {
            // See `accept_queued`.
            s.wake_timers = true;
        }
    }

    /// Answer for a whole folder (its files offered so far, and those still
    /// to come): accepted, they are saved in turn, programs included.
    /// Returns how many files waiting for an answer it answered.
    pub fn respond_to_folder(&self, friend_id: &str, folder_id: &str, accept: bool) -> usize {
        self.with(|s| {
            let key = (friend_id.to_string(), folder_id.to_string());
            let Some(f) = s.folders_in.get_mut(&key) else {
                return 0;
            };
            f.decision = Some(accept);
            f.fast |= accept;
            let kind = if accept {
                "folder_accept"
            } else {
                "folder_reject"
            };
            let mut answer = json!({ "type": kind, "folder": folder_id });
            if accept {
                answer["rate"] = json!(FAST_FOLDER_BUDGET);
            }
            s.send(friend_id, answer);
            let pending: Vec<String> = s
                .transfers
                .values()
                .filter(|t| {
                    t.dir == Dir::In
                        && t.friend_id == friend_id
                        && t.status == Status::Pending
                        && t.folder.as_ref().is_some_and(|f| f.id == folder_id)
                })
                .map(|t| t.id.clone())
                .collect();
            for tid in &pending {
                if accept {
                    let t = s.transfers.get_mut(tid).unwrap();
                    t.auto = true;
                    t.queued = true;
                    s.emit_transfer(tid);
                } else {
                    s.send(friend_id, json!({ "type": "file_reject", "tid": tid }));
                    s.set_status(tid, Status::Declined, None);
                }
            }
            s.wake_timers = true;
            pending.len()
        })
    }

    /// Open the files accepted without asking as others finish, at most
    /// `MAX_AUTO_ACTIVE` at once (each holds its file open), more if small.
    fn accept_queued(&self, s: &mut State) {
        let slots = |size: u64| if size < SMALL_FILE { 1 } else { SMALL_PER_SLOT };
        let capacity = MAX_AUTO_ACTIVE * SMALL_PER_SLOT;
        let mut active = 0;
        let mut queued = Vec::new();
        for t in s.transfers.values().filter(|t| t.dir == Dir::In && t.auto) {
            if t.status == Status::Pending && t.queued && !t.accepting {
                queued.push((t.seq, &t.id, t.file_size));
            } else if t.accepting
                || (!t.held()
                    && matches!(
                        t.status,
                        Status::Receiving | Status::Paused | Status::Finishing
                    ))
            {
                active += slots(t.file_size);
            }
        }
        if queued.is_empty() || active >= capacity {
            return;
        }
        // A large folder queues thousands: only the next few are sorted out.
        let free = capacity - active;
        if queued.len() > free {
            queued.select_nth_unstable(free - 1);
            queued.truncate(free);
        }
        queued.sort_unstable();
        // In their order: the next one waits for room rather than be passed.
        let mut room = free;
        let next: Vec<String> = queued
            .into_iter()
            .take_while(|(_, _, size)| {
                let fits = slots(*size) <= room;
                room = room.saturating_sub(slots(*size));
                fits
            })
            .map(|(_, id, _)| id.clone())
            .collect();
        for tid in next {
            s.transfers.get_mut(&tid).unwrap().accepting = true;
            let node = self.clone();
            // It creates the file: not under the lock.
            self.0
                .rt
                .spawn_blocking(move || node.respond_to_offer(&tid, true));
        }
    }

    /// The receiver answered for a folder of ours.
    fn on_folder_answer(&self, s: &mut State, friend_id: &str, msg: &Value, accepted: bool) {
        let Some(id) = msg.get("folder").and_then(Value::as_str) else {
            return;
        };
        let ours: Vec<String> = s
            .transfers
            .values()
            .filter(|t| {
                t.dir == Dir::Out
                    && t.friend_id == friend_id
                    && t.folder.as_ref().is_some_and(|f| f.id == id)
            })
            .map(|t| t.id.clone())
            .collect();
        if ours.is_empty() {
            return;
        }
        // Older receivers do not say how many offers they take.
        let budget = msg
            .get("rate")
            .and_then(Value::as_u64)
            .map_or(FOLDER_OFFER_BUDGET, |r| {
                (r as usize).clamp(FOLDER_OFFER_BUDGET, FAST_FOLDER_BUDGET)
            });
        s.folders_out
            .insert(id.to_string(), accepted.then_some(budget));
        for tid in ours {
            let t = s.transfers.get_mut(&tid).unwrap();
            if !matches!(t.status, Status::Connecting | Status::WaitingConsent) {
                continue;
            }
            if accepted {
                // Some may have been dropped by its limits: all again, now.
                t.last_sent = None;
            } else {
                s.set_status(&tid, Status::Declined, None);
            }
        }
        if accepted {
            self.offer_queued(s);
        }
    }

    /// Where a file of a received folder goes: the folder's root, picked
    /// when its first file is accepted ("Photos", or "Photos (2)" if taken),
    /// and the file's directory there.
    fn folder_dest(
        &self,
        dir: &Path,
        key: &(String, String),
        rel: &str,
    ) -> io::Result<(PathBuf, PathBuf)> {
        let parts: Vec<&str> = rel.split('/').collect();
        let _picking = self.0.folder_lock.lock().unwrap_or_else(|e| e.into_inner());
        let known = self.with(|s| s.folders_in.get(key).and_then(|f| f.root.clone()));
        let root = match known {
            Some(root) => root,
            None => {
                let root = reserve_dir(dir, parts[0])?;
                self.with(|s| {
                    if let Some(f) = s.folders_in.get_mut(key) {
                        f.root = Some(root.clone());
                    }
                });
                root
            }
        };
        let subdirs = &parts[1..parts.len().saturating_sub(1).max(1)];
        let dest = subdirs.iter().fold(root.clone(), |p, c| p.join(c));
        Ok((root, dest))
    }

    /// Accept or decline an offer. Also called for the files accepted
    /// without asking (see `accept_queued`), off the async threads: it
    /// creates the file.
    pub fn respond_to_offer(&self, id: &str, accept: bool) {
        let dir = (self.0.download_dir)();
        let found = self.with(|s| {
            let t = s.transfers.get_mut(id)?;
            if t.dir != Dir::In || t.status != Status::Pending {
                t.accepting = false;
                return None;
            }
            t.queued = false;
            if !accept {
                t.accepting = false;
                let friend_id = t.friend_id.clone();
                s.send(&friend_id, json!({ "type": "file_reject", "tid": id }));
                s.set_status(id, Status::Declined, None);
                return None;
            }
            let folder = t.folder.as_ref().map(|f| {
                let key = (t.friend_id.clone(), f.id.clone());
                (key, t.rel_path.clone().unwrap_or_default())
            });
            Some((t.file_name.clone(), folder))
        });
        let Some((name, folder)) = found else { return };
        // Disk access outside the lock: the folder may be slow (network, cloud).
        let place = match &folder {
            Some((key, rel)) => self.folder_dest(&dir, key, rel).map(Some),
            None => Ok(None),
        };
        let opened = place.and_then(|place| {
            // A folder's file is written where it goes: renamed from
            // another directory, many small files at once take much longer.
            let part_dir = place.as_ref().map_or(dir.as_path(), |(_, dest)| dest.as_path());
            create_part(part_dir, &name).map(|(part, file)| (part, file, place))
        });
        self.with(|s| {
            let Some(t) = s.transfers.get_mut(id) else {
                if let Ok((part, _, _)) = &opened {
                    let _ = std::fs::remove_file(part);
                }
                return;
            };
            t.accepting = false;
            if t.status != Status::Pending {
                // Cancelled meanwhile.
                if let Ok((part, _, _)) = &opened {
                    let _ = std::fs::remove_file(part);
                }
                return;
            }
            let friend_id = t.friend_id.clone();
            let Ok((part, file, place)) = opened else {
                s.send(&friend_id, json!({ "type": "file_cancel", "tid": id }));
                s.set_status(
                    id,
                    Status::Error,
                    Some("Dossier de téléchargement inaccessible".into()),
                );
                return;
            };
            if let Some((root, dest)) = place {
                t.folder_root = Some(root);
                t.dest_dir = Some(dest);
            }
            // The large buffer only comes with the data (see `large_buffer`).
            let writer = BufWriter::with_capacity(IDLE_BUFFER, tokio::fs::File::from_std(file));
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
            // Paused: accepted while the sender was away, which answered
            // `probe_paused` by starting.
            let waiting = t.status == Status::Receiving
                || (t.status == Status::Paused && t.repo.is_none());
            let ok = t.dir == Dir::In && t.friend_id == friend_id && waiting;
            ok.then(|| (t.shared.clone(), t.shared.session(), t.file_size))
        });
        let Some((shared, session, size)) = found else {
            recv.stop();
            return;
        };

        // Wait for any previous stream of this transfer to release the file.
        let mut slot = shared.writer.lock().await;
        if slot.is_none() {
            return;
        }
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
        shared.streams.fetch_add(1, Ordering::Relaxed);
        self.with(|s| {
            if let Some(t) = s.transfers.get_mut(&tid) {
                t.resyncing = false;
                t.shared.lan.store(via_lan, Ordering::Relaxed);
                if t.status == Status::Paused {
                    s.set_status(&tid, Status::Receiving, None);
                }
            }
        });
        let large = match self.large_buffer(&mut slot).await {
            Ok(permit) => permit,
            Err(_) => {
                drop(slot);
                return self.fail(&tid, "Écriture sur le disque impossible");
            }
        };
        let Some(writer) = slot.as_mut() else { return };

        let mut cancel = shared.session.subscribe();
        let mut pos = offset;
        let mut cut = false;
        let throttle = self.0.down.clone();
        let failure: Option<&str> = loop {
            // Under a speed limit, read less at once, and only our share.
            let chunk = tokio::select! {
                c = async {
                    let c = recv.chunk(throttle.piece(CHUNK_SIZE as usize)).await;
                    if let Ok(Some(data)) = &c {
                        throttle.take(data.len()).await;
                    }
                    c
                } => c,
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

        // Back to the small buffer, which writes out the large one.
        let flushed = match large {
            Some(_) => rebuffer(&mut slot, IDLE_BUFFER).await,
            None => writer.flush().await,
        };
        drop(large);
        if failure.is_none() && shared.session() == session && flushed.is_err() {
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
            // A QUIC stream cut: the sender restarts it itself when it
            // stopped it or lost the connection. If no other stream comes
            // while the connection stays up, ask to resume.
            None if cut && shared.session() == session => {
                drop(slot);
                let streams = shared.streams.load(Ordering::Relaxed);
                tokio::time::sleep(CUT_RESYNC).await;
                if shared.session() == session && shared.streams.load(Ordering::Relaxed) == streams
                {
                    self.with(|s| {
                        let connected = s.conns.contains_key(&friend_id);
                        if connected {
                            self.resync(s, &tid);
                        }
                    });
                }
            }
            None => {}
        }
    }

    /// Give a receiving stream the large disk buffer, unless
    /// `MAX_LARGE_BUFFERS` streams already have one (it then keeps the idle
    /// one). Hold the permit until `rebuffer(slot, IDLE_BUFFER)` once the
    /// stream ends.
    async fn large_buffer(
        &self,
        slot: &mut Option<Writer>,
    ) -> io::Result<Option<tokio::sync::OwnedSemaphorePermit>> {
        let Ok(permit) = self.0.large_buffers.clone().try_acquire_owned() else {
            return Ok(None);
        };
        rebuffer(slot, DISK_BUFFER).await?;
        Ok(Some(permit))
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

    /// Accepted files still paused while their sender has been back for a
    /// while: it did not re-offer them, so it cancelled them while we were
    /// away or restarted since. Ask; it either resumes or answers `file_cancel`.
    fn probe_paused(&self, s: &mut State) {
        let now = Instant::now();
        let paused: Vec<(String, String, u64)> = s
            .transfers
            .values()
            .filter(|t| {
                t.dir == Dir::In
                    && t.status == Status::Paused
                    && t.repo.is_none()
                    && t.peer_name.is_none()
                    && t.part_path.is_some()
                    && !t.held()
                    && t.last_sent.is_none_or(|at| now.duration_since(at) >= REOFFER_DELAY)
                    && s
                        .conns
                        .get(&t.friend_id)
                        .is_some_and(|c| c.since.elapsed() >= REOFFER_DELAY)
            })
            .map(|t| (t.id.clone(), t.friend_id.clone(), t.bytes()))
            .collect();
        for (tid, friend_id, offset) in paused {
            s.transfers.get_mut(&tid).unwrap().last_sent = Some(now);
            s.send(
                &friend_id,
                json!({ "type": "file_accept", "tid": tid, "offset": offset }),
            );
        }
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
        // In a folder: in its own directory there.
        let dest = t.dest_dir.clone();
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
            let dir = dest.unwrap_or_else(|| part.parent().unwrap_or(Path::new(".")).to_path_buf());
            let (part_moved, name_moved) = (part.clone(), name.clone());
            let saved = tokio::task::spawn_blocking(move || {
                std::fs::create_dir_all(&dir)?;
                let target = reserve_path(&dir, &name_moved)?;
                // An antivirus or sync client often holds a new file a moment.
                let mut attempt = 1;
                while let Err(e) = std::fs::rename(&part_moved, &target) {
                    if attempt >= RENAME_ATTEMPTS {
                        let _ = std::fs::remove_file(&target);
                        return Err(e);
                    }
                    attempt += 1;
                    std::thread::sleep(RENAME_RETRY_DELAY);
                }
                mark_downloaded(&target);
                Ok(target)
            })
            .await
            .ok()
            .and_then(Result::ok);
            let renamed = saved.is_some();
            let target = saved.unwrap_or_default();
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
                    } else if s.folder_done(&tid) {
                        // A folder's: once, for its last file.
                        let view = s.transfers[&tid].view();
                        s.events.push(Event::TransferComplete(view));
                    }
                } else {
                    // Every byte is there: keep them under the temporary name
                    // rather than lose the whole download.
                    let kept = part
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    t.part_path = None;
                    t.saved_path = Some(part.clone());
                    s.send(&friend_id, json!({ "type": "file_cancel", "tid": tid }));
                    node.stop_transfer(
                        s,
                        &tid,
                        Status::Error,
                        Some(format!("Impossible d’enregistrer le fichier (gardé sous « {kept} »)")),
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

    /// Pause a transfer by hand (either side, once accepted): it stays
    /// paused, across reconnections too, until `resume_transfer`; the peer
    /// sees it paused. False if it cannot be paused.
    pub fn pause_transfer(&self, id: &str) -> bool {
        self.with(|s| {
            let Some(t) = s.transfers.get_mut(id) else {
                return false;
            };
            let accepted = matches!(
                t.status,
                Status::Sending | Status::Receiving | Status::Paused
            );
            if t.held || !accepted || t.serve || t.repo.is_some() || t.peer_name.is_some() {
                return false;
            }
            t.held = true;
            // Stops the running stream; what came so far is kept.
            t.shared.next_session();
            if t.dir == Dir::Out {
                t.conn_id = None;
            }
            let friend_id = t.friend_id.clone();
            s.send(&friend_id, json!({ "type": "file_pause", "tid": id }));
            if s.transfers[id].status == Status::Paused {
                s.emit_transfer(id);
            } else {
                s.set_status(id, Status::Paused, None);
            }
            true
        })
    }

    /// Resume a transfer paused with `pause_transfer`, from where it was
    /// (unless the peer holds it too). False if it was not paused here.
    pub fn resume_transfer(&self, id: &str) -> bool {
        self.with(|s| {
            let Some(t) = s.transfers.get_mut(id) else {
                return false;
            };
            if !t.held || t.status.is_final() {
                return false;
            }
            t.held = false;
            let (friend_id, dir, peer_held) = (t.friend_id.clone(), t.dir, t.peer_held);
            let offset = t.bytes();
            if s.conns.contains_key(&friend_id) {
                s.send(&friend_id, json!({ "type": "file_resume", "tid": id }));
                if !peer_held {
                    match dir {
                        // It answers with the offset it has.
                        Dir::Out => self.offer(s, id),
                        Dir::In => {
                            s.transfers.get_mut(id).unwrap().last_sent = Some(Instant::now());
                            s.send(
                                &friend_id,
                                json!({ "type": "file_accept", "tid": id, "offset": offset }),
                            );
                        }
                    }
                }
            } else {
                // Goes on when it is back (re-offer, or `probe_paused`).
                self.dial(s, &friend_id);
            }
            s.emit_transfer(id);
            true
        })
    }

    /// The peer paused (`hold`) or resumed a transfer by hand.
    fn on_peer_hold(&self, s: &mut State, friend_id: &str, tid: Option<&str>, hold: bool) {
        let Some(t) = tid.and_then(|tid| s.transfers.get_mut(tid)) else {
            return;
        };
        if t.friend_id != friend_id || t.status.is_final() || t.serve || t.repo.is_some() {
            return;
        }
        let id = t.id.clone();
        if !hold {
            // What restarts it follows: an offer, or an accept.
            if t.peer_held {
                t.peer_held = false;
                s.emit_transfer(&id);
            }
            return;
        }
        if !matches!(t.status, Status::Sending | Status::Receiving | Status::Paused) {
            return;
        }
        t.peer_held = true;
        t.shared.next_session();
        if t.dir == Dir::Out {
            t.conn_id = None;
        }
        if t.status == Status::Paused {
            s.emit_transfer(&id);
        } else {
            s.set_status(&id, Status::Paused, None);
        }
    }

    /// Limit the speed of everything sent, and of everything received, to
    /// `bytes_per_sec` each (at least 16 Kio/s); `None` for no limit.
    pub fn set_speed_limit(&self, bytes_per_sec: Option<u64>) {
        let rate = bytes_per_sec.filter(|r| *r > 0).map_or(0, |r| r.max(MIN_SPEED_LIMIT));
        self.0.up.set(rate);
        self.0.down.set(rate);
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
            s.send_stop(id, "file_reject");
            s.set_status(id, Status::Declined, None);
            return;
        }
        s.send_stop(id, "file_cancel");
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

/// Pictures shown in the chat, with a preview.
pub fn is_image(name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    matches!(
        ext.as_deref(),
        Some("jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp")
    )
}

fn friend_views(s: &State) -> Vec<FriendView> {
    s.friends
        .data
        .iter()
        .map(|f| {
            let last = s.chats.last(&f.id);
            FriendView {
                id: f.id.clone(),
                name: f.display_name(),
                online: s.conns.contains_key(&f.id),
                legacy: !is_valid_id(&f.id),
                last_text: last.map(|m| match &m.file {
                    Some(_) => "Image".to_string(),
                    None => m.text.chars().take(PREVIEW_CHARS).collect(),
                }),
                last_ts: last.map_or(0, |m| m.ts),
                last_mine: last.is_some_and(|m| m.from == s.me),
                auto_accept: f.auto_accept,
            }
        })
        .collect()
}

/// Give the writer a buffer of `capacity` bytes, after writing out what the
/// current one holds; kept as it is if that fails.
async fn rebuffer(slot: &mut Option<Writer>, capacity: usize) -> io::Result<()> {
    let Some(writer) = slot.as_mut() else {
        return Ok(());
    };
    writer.flush().await?;
    if let Some(writer) = slot.take() {
        *slot = Some(BufWriter::with_capacity(capacity, writer.into_inner()));
    }
    Ok(())
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
    /// The speed limit of what we send.
    throttle: Arc<Throttle>,
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
        throttle,
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
        // In small pieces under a speed limit, each waiting for its turn.
        let step = throttle.piece(data.len());
        let mut rest = data;
        while !rest.is_empty() {
            let piece = rest.split_to(step.min(rest.len()));
            let len = piece.len();
            let written = tokio::select! {
                ok = async {
                    throttle.take(len).await;
                    send.write(piece).await
                } => Some(ok),
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
            pos += len as u64;
            // A replaced session must not overwrite the new session's offset.
            if shared.session() != session {
                send.abort();
                return Ok(Streamed::Stopped);
            }
            shared.bytes.store(pos, Ordering::Relaxed);
        }
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
    fn folder_paths_stay_inside() {
        let ok = |p: &str| folder_path(p, "Photos");
        assert_eq!(ok("Photos/2024/a.jpg").unwrap(), ["Photos", "2024", "a.jpg"]);
        assert_eq!(ok("Photos/con/a<b>.txt").unwrap(), ["Photos", "_con", "a_b_.txt"]);
        for bad in [
            "Photos",
            "Photos/../a.jpg",
            "Photos/./a.jpg",
            "Photos//a.jpg",
            "/Photos/a.jpg",
            "Photos\\a.jpg",
            "Photos/C:/a.jpg",
            "Autre/a.jpg",
        ] {
            assert!(ok(bad).is_none(), "{bad}");
        }
        assert!(folder_path("C:/a.jpg", "C:").is_none());
        let deep = format!("Photos/{}a.jpg", "d/".repeat(MAX_FOLDER_DEPTH - 2));
        assert!(ok(&deep).is_some());
        let deeper = format!("Photos/{}a.jpg", "d/".repeat(MAX_FOLDER_DEPTH - 1));
        assert!(ok(&deeper).is_none());
    }

    #[test]
    fn walks_and_reserves_folders() {
        let dir = std::env::temp_dir().join(format!("p2p-folder-{}", rand::random::<u32>()));
        let root = dir.join("Photos");
        std::fs::create_dir_all(root.join("2024/vide")).unwrap();
        std::fs::write(root.join("a.txt"), b"a").unwrap();
        std::fs::write(root.join("2024/b.txt"), b"bb").unwrap();
        let (name, files) = walk_folder(&root).unwrap();
        assert_eq!(name, "Photos");
        let rels: Vec<&str> = files.iter().filter_map(|f| f.rel_path.as_deref()).collect();
        assert_eq!(rels, ["Photos/2024/b.txt", "Photos/a.txt"]);
        assert_eq!(walk_folder(&root.join("2024/vide")).err(), Some("empty"));
        let out = dir.join("out");
        assert_eq!(reserve_dir(&out, "Photos").unwrap(), out.join("Photos"));
        assert_eq!(reserve_dir(&out, "Photos").unwrap(), out.join("Photos (2)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_offered_reactions() {
        assert_eq!(valid_reaction("👍"), Some("👍"));
        assert_eq!(valid_reaction("\u{2764}"), Some("❤️"));
        assert_eq!(valid_reaction("🤡"), None);
        assert_eq!(valid_reaction(""), None);
    }

    #[tokio::test]
    async fn throttles_to_the_limit() {
        let throttle = Throttle::new();
        throttle.set(1 << 20);
        let start = Instant::now();
        // The first quarter second's worth may go at once.
        for _ in 0..20 {
            throttle.take(64 << 10).await;
        }
        let secs = start.elapsed().as_secs_f64();
        assert!((0.9..2.0).contains(&secs), "{secs}");
    }

    #[test]
    fn plain_http_relay_only_on_this_computer() {
        assert!(CustomRelay::parse("http://relay.example.com", "").is_err());
        assert!(CustomRelay::parse("http://localhost:3340", "").is_ok());
        assert!(CustomRelay::parse("relay.example.com", "").is_ok());
    }

    #[test]
    fn dials_offline_peers_less_and_less_often() {
        let secs = |step, economy| backoff_delay(step, economy).as_secs();
        let plain: Vec<u64> = (0..8).map(|i| secs(i, false)).collect();
        assert_eq!(plain, [15, 30, 60, 120, 300, 600, 600, 600]);
        let economy: Vec<u64> = (5..10).map(|i| secs(i, true)).collect();
        assert_eq!(economy, [600, 1200, 1800, 1800, 1800]);
        assert_eq!(secs(u8::MAX, true), 1800);
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
