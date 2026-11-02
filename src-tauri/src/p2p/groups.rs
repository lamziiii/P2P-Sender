//! Group networking: invitations, log sync between members, shared files.
//!
//!   group_invite  {gid, name, via}     a friend added us to a group
//!   group_decline {gid}                we refuse: the inviter removes us
//!   group_have    {gid, have, heads, forks, ask}
//!                                      what we have, per author (seq, hash
//!                                      of the last entry), forks known, and
//!                                      whether we want the peer's in return
//!   group_entries {gid, entries}       entries the peer is missing
//!   blob_req      {tid, hash, offset}  send me this shared file from offset
//!   blob_missing  {tid}                I cannot serve it
//!
//! Shared files reuse the transfer engine: the downloader creates an
//! incoming transfer and asks a member who holds the file; the holder answers
//! with a hidden outgoing transfer. The download is checked against the
//! BLAKE3 hash in the catalog before it is kept.

use std::fs::OpenOptions;
use std::io::Write;

use super::*;
use crate::group::{self, Body, Have, Log, Op, SignedEntry, View};

const FRAME_BUDGET: usize = 128 * 1024;
pub(super) const REPO_FOLDER: &str = "P2PShare";
/// Invitations waiting for an answer.
const MAX_INVITES: usize = 50;
/// Invitations refused that we remember (to refuse them again).
const MAX_DECLINED: usize = 200;
/// Entries looked at in one `group_entries`: the rest comes with the next sync.
const MAX_BATCH_ENTRIES: usize = 2048;
/// A shared file nobody can serve is asked for again after this, doubled
/// each round, up to `MAX_RETRY_DELAY`.
const RETRY_DELAY: Duration = Duration::from_secs(15);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(10 * 60);
/// Corrupt copies received before a download gives up.
const MAX_CORRUPT: u32 = 5;
/// Shared files one member may download from us at the same time.
const MAX_SERVES_PER_PEER: usize = 4;
/// Entries dated further in the future are refused until that time comes.
const MAX_CLOCK_SKEW_MS: u64 = 5 * 60 * 1000;
/// Files shared at most from one drop, and how deep folders are walked.
const MAX_DROP_FILES: usize = 1000;
const MAX_DROP_DEPTH: usize = 16;

// ─── Stored state ────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct GroupsMeta {
    #[serde(default)]
    pub groups: Vec<GroupMeta>,
    #[serde(default)]
    pub invites: Vec<Invite>,
    /// Invitations refused, as "author:seq" of the entry that added us.
    #[serde(default)]
    pub declined: Vec<String>,
    /// Shared files we can serve, by hash.
    #[serde(default)]
    pub holdings: HashMap<String, Holding>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GroupMeta {
    pub id: String,
    #[serde(default)]
    pub last_read: u64,
    #[serde(default)]
    pub joined: u64,
    /// We left: kept only until a member has our leave entry.
    #[serde(default)]
    pub left: bool,
    /// Creator of a group whose id does not name it (older groups), as
    /// first seen: see `Log::pinned_creator`.
    #[serde(default)]
    pub creator: Option<String>,
    /// We joined (or rejoined) and do not know yet that we have all our own
    /// earlier entries: see `GroupState::own_synced`.
    #[serde(default)]
    pub pending_sync: bool,
    /// No notifications, except when someone mentions us (local setting).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Invite {
    pub gid: String,
    pub name: String,
    pub from: String,
    pub via: String,
    pub ts: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Holding {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
    /// Groups we shared or downloaded it in: it is served only to their
    /// members, and only while it is listed there.
    #[serde(default)]
    pub groups: Vec<String>,
}

pub(super) struct GroupState {
    pub log: Log,
    pub peer_have: HashMap<String, Have>,
    pub unsaved: Vec<SignedEntry>,
    /// We have every entry of ours the members have, so our next seq is
    /// really the next one. False after joining until a member tells us.
    pub own_synced: bool,
    /// Our entries dropped because members had other versions of them
    /// (`reset_own_history`): posted again once in sync.
    pub orphans: Vec<group::Entry>,
}

impl GroupState {
    fn new(log: Log, own_synced: bool) -> Self {
        Self {
            log,
            peer_have: HashMap::new(),
            unsaved: Vec::new(),
            own_synced,
            orphans: Vec::new(),
        }
    }
}

/// An incoming transfer that downloads a shared file.
pub(super) struct Repo {
    pub gid: String,
    pub hash: String,
    pub tried: HashSet<String>,
    /// Members who sent a copy that did not match the hash: never asked again.
    pub bad: HashSet<String>,
    /// Members who sent part of the current attempt.
    pub sources: HashSet<String>,
    /// Corrupt attempts each member took part in, with others.
    pub strikes: HashMap<String, u32>,
    pub corrupt: u32,
    /// Bytes when the current source was asked.
    pub picked_at: u64,
    /// Rounds without a source, since `progress` bytes.
    pub rounds: u32,
    pub progress: u64,
    pub next_try: Option<Instant>,
}

impl Repo {
    fn new(gid: &str, hash: &str) -> Self {
        Self {
            gid: gid.to_string(),
            hash: hash.to_string(),
            tried: HashSet::new(),
            bad: HashSet::new(),
            sources: HashSet::new(),
            strikes: HashMap::new(),
            corrupt: 0,
            picked_at: 0,
            rounds: 0,
            progress: 0,
            next_try: None,
        }
    }
}

// ─── Views for the UI ────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupSummary {
    pub id: String,
    pub name: String,
    pub members: usize,
    pub online: usize,
    pub unread: usize,
    pub last_message: Option<String>,
    pub last_ts: u64,
    pub syncing: bool,
    pub removed: bool,
    pub muted: bool,
    /// Unread messages that mention us (or everyone).
    pub unread_mentions: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteView {
    pub gid: String,
    pub name: String,
    pub from: String,
    pub from_name: String,
}

#[derive(Serialize)]
pub struct GroupsOverview {
    pub groups: Vec<GroupSummary>,
    pub invites: Vec<InviteView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberView {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub friend: bool,
    pub me: bool,
    /// We may remove them (we created the group or added them).
    pub removable: bool,
    /// What to write after "@" to mention them: their own nickname, else
    /// the start of their id (see `State::handle`).
    pub handle: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupDetail {
    pub id: String,
    pub name: String,
    pub members: Vec<MemberView>,
    pub syncing: bool,
    pub removed: bool,
    pub muted: bool,
    /// Pinned items that still exist, most recently pinned first.
    pub pins: Vec<PinView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinView {
    /// The message's or file's id, as in `GroupMessageView::id` /
    /// `GroupFileView::id`.
    pub target: String,
    /// "message" or "file".
    pub kind: &'static str,
    /// The message's text, or the file's name.
    pub text: String,
    pub author_name: String,
    /// When the message was sent / the file shared.
    pub ts: u64,
    pub pinned_by: String,
    pub pinned_by_name: String,
    pub pinned_at: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupMessageView {
    pub id: String,
    pub author: String,
    pub author_name: String,
    pub text: String,
    pub ts: u64,
    pub mine: bool,
    pub received_by: usize,
    pub total: usize,
    /// It mentions us ("@our nickname") or everyone (@tous, @all); never
    /// for our own messages.
    pub mentions_me: bool,
    pub pinned: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupFileView {
    pub id: String,
    pub name: String,
    /// Folder within what was shared ("Photos/2024"), if a folder was dropped.
    pub dir: Option<String>,
    pub size: u64,
    pub author: String,
    pub author_name: String,
    pub ts: u64,
    pub mine: bool,
    pub local: bool,
    pub transfer_id: Option<String>,
    pub pinned: bool,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn mtime(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn hash_file(path: &Path) -> io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(File::open(path)?)?;
    Ok(hasher.finalize().to_hex().to_string())
}

fn holding_valid(h: &Holding) -> bool {
    std::fs::metadata(&h.path)
        .is_ok_and(|m| m.is_file() && m.len() == h.size && mtime(&m) == h.mtime)
}

fn log_path(dir: &Path, gid: &str) -> PathBuf {
    dir.join(format!("{gid}.jsonl"))
}

/// The log of a group we just created or joined, its texts read back from
/// its file once written there.
fn new_log(dir: &Path, gid: &str) -> Log {
    let mut log = Log::new(gid);
    log.attach(log_path(dir, gid), 0);
    log
}

/// Remember that we can serve `hash` (from `path`) in group `gid`.
fn add_holding(meta: &mut GroupsMeta, hash: String, path: &Path, fs: &std::fs::Metadata, gid: &str) {
    let mut groups = meta
        .holdings
        .remove(&hash)
        .map(|h| h.groups)
        .unwrap_or_default();
    if !groups.iter().any(|g| g == gid) {
        groups.push(gid.to_string());
    }
    meta.holdings.insert(
        hash,
        Holding {
            path: path.to_path_buf(),
            size: fs.len(),
            mtime: mtime(fs),
            groups,
        },
    );
}

/// The files to share for paths dropped on a group: each file as it is (no
/// folder), and every file inside each folder, with its folder relative to
/// the dropped one's parent ("Photos/2024"). Links are not followed.
fn expand_paths(paths: Vec<PathBuf>) -> Vec<(PathBuf, Option<String>)> {
    let mut out = Vec::new();
    for path in paths {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_file() {
            out.push((path, None));
        } else if meta.is_dir() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            walk_dir(&path, &name, 0, &mut out);
        }
        if out.len() >= MAX_DROP_FILES {
            out.truncate(MAX_DROP_FILES);
            break;
        }
    }
    out
}

fn walk_dir(dir: &Path, rel: &str, depth: usize, out: &mut Vec<(PathBuf, Option<String>)>) {
    if depth > MAX_DROP_DEPTH {
        return;
    }
    let Ok(list) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<std::fs::DirEntry> = list.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if out.len() >= MAX_DROP_FILES {
            return;
        }
        let Ok(kind) = e.file_type() else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        if kind.is_file() {
            out.push((e.path(), Some(rel.to_string())));
        } else if kind.is_dir() {
            walk_dir(&e.path(), &format!("{rel}/{name}"), depth + 1, out);
        }
    }
}

/// How long to wait before asking again for a file nobody could serve.
fn retry_delay(rounds: u32) -> Duration {
    RETRY_DELAY
        .saturating_mul(1 << rounds.saturating_sub(1).min(10))
        .min(MAX_RETRY_DELAY)
}

/// One line of a group log on disk, encrypted (`vault::seal_line`).
fn log_line(e: &SignedEntry) -> io::Result<String> {
    let json = serde_json::to_vec(e).map_err(io::Error::other)?;
    Ok(crate::vault::seal_line(&json)? + "\n")
}

/// A whole group log, to write it anew.
fn log_text(entries: &[SignedEntry]) -> io::Result<String> {
    entries.iter().map(log_line).collect()
}

/// What reading a group log found.
#[derive(Default)]
struct LogRead {
    entries: Vec<SignedEntry>,
    /// Where each of `entries` is in the file: offset and length of its line.
    at: Vec<(u64, u32)>,
    /// Lines written before logs were encrypted.
    plain: bool,
    /// Lines that will never open (damaged, or sealed with a lost key).
    damaged: bool,
    /// The last line was cut short (a crash while appending it).
    cut: bool,
    /// Lines that cannot be opened this run (the key is unavailable).
    locked: bool,
}

fn read_log(text: &str) -> LogRead {
    let mut read = LogRead::default();
    let mut lines = text.split_inclusive('\n').peekable();
    let mut offset = 0;
    while let Some(raw) = lines.next() {
        let start = offset;
        offset += raw.len() as u64;
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        if line.trim().is_empty() {
            continue;
        }
        let last = lines.peek().is_none() && !text.ends_with('\n');
        let json = match crate::vault::open_line(line) {
            Ok(json) => json,
            Err(e) if e.kind() != io::ErrorKind::InvalidData => {
                read.locked = true;
                continue;
            }
            Err(_) if last => {
                read.cut = true;
                continue;
            }
            Err(_) => {
                read.damaged = true;
                continue;
            }
        };
        match serde_json::from_slice::<SignedEntry>(&json) {
            Ok(e) => {
                read.plain |= !crate::vault::is_sealed_line(line);
                read.entries.push(e);
                read.at.push((start, line.len() as u32));
            }
            Err(_) if last => read.cut = true,
            Err(_) if crate::vault::is_sealed_line(line) => read.damaged = true,
            // Plain junk: earlier versions skipped it too.
            Err(_) => read.plain = true,
        }
    }
    read
}

/// Write a group log anew, encrypted, with every entry read from it: older plain
/// logs are migrated, a cut last line goes. A log with lines that will never
/// open is first set aside (`<gid>.corrupt-<time>.jsonl`), never written
/// over; nothing is written while the key is unavailable. Returns whether
/// it was written anew.
fn rewrite_log(path: &Path, read: &LogRead) -> bool {
    if read.locked || !(read.plain || read.damaged || read.cut) {
        return false;
    }
    let Ok(text) = log_text(&read.entries) else {
        return false;
    };
    if read.damaged {
        match crate::store::set_aside(path) {
            Ok(aside) => eprintln!("{} damaged, moved to {}", path.display(), aside.display()),
            Err(e) => {
                eprintln!("{} damaged, could not be moved aside: {e}", path.display());
                return false;
            }
        }
    }
    match crate::store::write_atomic(path, text.as_bytes()) {
        // The replaced, plain version must not linger as the backup.
        Ok(()) => {
            crate::store::keep_backup(path);
            true
        }
        Err(e) => {
            eprintln!("{} not encrypted yet: {e}", path.display());
            false
        }
    }
}

/// Load every group log from disk (our own files: signatures were checked
/// when the entries arrived). Only what decisions need stays in memory: the
/// texts of entries are read back from the file when peers need them.
pub(super) fn load(dir: &Path) -> (JsonStore<GroupsMeta>, HashMap<String, GroupState>) {
    let mut meta = JsonStore::load(dir.join("groups.json"), GroupsMeta::default());
    let mut groups = HashMap::new();
    let mut dirty = false;
    for g in &mut meta.data.groups {
        let mut log = Log::new(&g.id);
        log.pinned_creator = g.creator.clone();
        let path = log_path(dir, &g.id);
        // With the end of its last whole line.
        let (read, end) = match std::fs::read_to_string(&path) {
            Ok(text) => {
                // A line cut short by a crash would swallow the next one appended.
                if !text.is_empty() && !text.ends_with('\n') {
                    let _ = append_lines(&path, "\n");
                }
                let end = text.rfind('\n').map_or(0, |i| i as u64 + 1);
                (read_log(&text), end)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (LogRead::default(), 0),
            // Locked or unreadable: never written anew this run.
            Err(_) => (
                LogRead {
                    locked: true,
                    ..Default::default()
                },
                0,
            ),
        };
        let rewritten = rewrite_log(&path, &read);
        let mut lines = Vec::new();
        let entries = read
            .entries
            .into_iter()
            .zip(read.at)
            .filter_map(|(s, (at, len))| {
                let e = group::parse(s, &g.id, false)?;
                lines.push((e.a.clone(), e.n, e.hash.clone(), at, len));
                Some(e)
            })
            .collect();
        log.insert_loaded(entries);
        log.rebuild();
        log.drop_strangers();
        if rewritten {
            log.attach(path.clone(), 0);
            log.index_new();
        } else {
            log.attach(path.clone(), end);
            for (a, n, hash, at, len) in lines {
                log.locate(&a, n, &hash, at, len);
            }
        }
        if g.creator.is_none() && !log.view.creator.is_empty() {
            g.creator = Some(log.view.creator.clone());
            log.pinned_creator = g.creator.clone();
            dirty = true;
        }
        groups.insert(g.id.clone(), GroupState::new(log, !g.pending_sync));
    }
    // Files held before holdings named their groups: the groups listing them now.
    for (hash, h) in meta.data.holdings.iter_mut().filter(|(_, h)| h.groups.is_empty()) {
        h.groups = groups
            .iter()
            .filter(|(_, g)| g.log.view.files.values().any(|f| f.hash == *hash))
            .map(|(gid, _)| gid.clone())
            .collect();
        dirty = true;
    }
    if dirty {
        meta.mark_dirty();
    }
    (meta, groups)
}

impl State {
    fn meta(&self, gid: &str) -> Option<&GroupMeta> {
        self.gmeta.data.groups.iter().find(|g| g.id == gid)
    }

    fn is_left(&self, gid: &str) -> bool {
        self.meta(gid).is_some_and(|m| m.left)
    }

    /// Groups we take part in (not left, and we are a member).
    fn member_of(&self, gid: &str) -> bool {
        !self.is_left(gid)
            && self
                .groups
                .get(gid)
                .is_some_and(|g| g.log.view.is_member(&self.me))
    }

    /// Peers we must be able to talk to for our groups: co-members, plus the
    /// members of a group we left until one of them has our leave entry.
    pub(super) fn group_peers(&self) -> HashSet<String> {
        let mut peers = HashSet::new();
        for (gid, g) in &self.groups {
            if self.member_of(gid) || self.is_left(gid) {
                peers.extend(
                    g.log
                        .view
                        .members
                        .keys()
                        .filter(|m| **m != self.me)
                        .cloned(),
                );
            }
        }
        peers
    }

    pub(super) fn is_allowed(&self, peer: &str) -> bool {
        self.is_friend(peer) || self.group_peers().contains(peer)
    }

    fn display_name(&self, view: &View, id: &str) -> String {
        if id == self.me {
            return if self.nickname.is_empty() {
                "Moi".into()
            } else {
                self.nickname.clone()
            };
        }
        if let Some(f) = self.friends.data.iter().find(|f| f.id == id) {
            return f.display_name();
        }
        // Not a friend: the nickname is theirs to choose, so it always comes
        // with the start of their id and cannot pass for a friend.
        let short = &id[..6.min(id.len())];
        match view.nicks.get(id) {
            Some(n) if !n.is_empty() => format!("{n} ({short})"),
            _ => format!("Membre {short}"),
        }
    }

    /// What members write after "@" to mention `id`: the nickname they
    /// gave the group (the same for every member), else the start of their id.
    fn handle(&self, view: &View, id: &str) -> String {
        let nick = if id == self.me && !self.nickname.is_empty() {
            Some(self.nickname.as_str())
        } else {
            view.nicks.get(id).map(String::as_str)
        };
        match nick.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => n.to_string(),
            None => id[..6.min(id.len())].to_string(),
        }
    }

    /// What mentions us in a group: our nickname (as we set it, and as the
    /// group has it) and the start of our id.
    fn own_handles(&self, view: &View) -> Vec<String> {
        let mut handles = vec![self.me[..6.min(self.me.len())].to_string()];
        handles.extend(view.nicks.get(&self.me).cloned());
        if !self.nickname.is_empty() {
            handles.push(self.nickname.clone());
        }
        handles
    }

    fn mention(&self, view: &View, m: &group::Message) -> group::Mention {
        if m.author == self.me {
            return group::Mention::None;
        }
        group::mention_of(&m.text, &self.own_handles(view))
    }

    fn muted(&self, gid: &str) -> bool {
        self.meta(gid).is_some_and(|m| m.muted)
    }

    /// Joining: not a member yet, or our earlier entries (a rejoin) came
    /// before the one adding us back.
    fn syncing(&self, gid: &str) -> bool {
        self.groups.get(gid).is_some_and(|g| {
            !g.log.view.is_member(&self.me) && (!g.log.has_entry_of(&self.me) || !g.own_synced)
        })
    }

    fn removed(&self, gid: &str) -> bool {
        self.groups.get(gid).is_some_and(|g| {
            !g.log.view.is_member(&self.me) && g.log.has_entry_of(&self.me) && g.own_synced
        })
    }

    /// A member who has not written since being added: not joined yet.
    fn pending_member(&self, gid: &str, member: &str) -> bool {
        self.groups.get(gid).is_some_and(|g| {
            let added = g.log.view.members.get(member);
            let last = g.log.get(member, g.log.last_seq_of(member));
            added.is_some_and(|t| last.is_none_or(|e| e.t < *t))
        })
    }

    /// Whether our next entry would follow every entry of ours the group
    /// has: posting before they are back (after a rejoin) would number new
    /// entries like old ones, and members would refuse them for good.
    fn can_post(&self, gid: &str) -> bool {
        self.groups.get(gid).is_some_and(|g| {
            let ours = g.log.last_seq_of(&self.me);
            g.own_synced
                && g.log.fork_of(&self.me).is_none()
                && g.peer_have
                    .values()
                    .all(|h| h.get(&self.me).copied().unwrap_or(0) <= ours)
        })
    }

    fn send_entries(&self, peer: &str, gid: &str, entries: &[SignedEntry]) {
        let mut batch: Vec<&SignedEntry> = Vec::new();
        let mut size = 0;
        for e in entries {
            // The size once in the frame: quotes in the body get escaped.
            let len = serde_json::to_vec(e).map(|v| v.len()).unwrap_or(0) + 1;
            if size + len > FRAME_BUDGET && !batch.is_empty() {
                self.send(
                    peer,
                    json!({ "type": "group_entries", "gid": gid, "entries": batch }),
                );
                batch.clear();
                size = 0;
            }
            size += len;
            batch.push(e);
        }
        if !batch.is_empty() {
            self.send(
                peer,
                json!({ "type": "group_entries", "gid": gid, "entries": batch }),
            );
        }
    }

    fn send_have(&self, peer: &str, gid: &str) {
        if let Some(g) = self.groups.get(gid) {
            let msg = json!({
                "type": "group_have",
                "gid": gid,
                "have": g.log.have(),
                "heads": g.log.heads(),
                "forks": g.log.forks(),
                "ask": !g.own_synced,
            });
            self.send(peer, msg);
        }
    }

    /// Send `entries` to every connected member except `except`.
    fn broadcast(&self, gid: &str, entries: &[SignedEntry], except: Option<&str>) {
        let Some(g) = self.groups.get(gid) else {
            return;
        };
        for m in g.log.view.members.keys() {
            if *m != self.me && Some(m.as_str()) != except && self.conns.contains_key(m) {
                self.send_entries(m, gid, entries);
            }
        }
    }

    /// Invite a friend to every group they were added to but never joined
    /// (or added back to, after leaving, without writing since).
    fn send_invites(&self, friend: &str) {
        if !self.is_friend(friend) {
            return;
        }
        for (gid, g) in &self.groups {
            let v = &g.log.view;
            if !self.member_of(gid) || !self.pending_member(gid, friend) {
                continue;
            }
            let via = v
                .added_by
                .get(friend)
                .map(|(a, n)| format!("{a}:{n}"))
                .unwrap_or_default();
            self.send(
                friend,
                json!({ "type": "group_invite", "gid": gid, "name": v.name, "via": via }),
            );
        }
    }

    /// Whether every connected member of our groups has all we have, and
    /// we have all it has (as far as it told us), so that `groups_tick` has
    /// nothing to do. Entries of strangers, which we never keep, do not count.
    pub(super) fn groups_settled(&self) -> bool {
        self.groups.iter().all(|(gid, g)| {
            if !(self.member_of(gid) || self.is_left(gid)) {
                return true;
            }
            let ours = g.log.have();
            let members = g.log.view.members.keys();
            members
                .filter(|m| **m != self.me && self.conns.contains_key(*m))
                .all(|m| {
                    let Some(told) = g.peer_have.get(m) else {
                        return false;
                    };
                    // It does not tell us again what it sent us itself.
                    let sent = self.group_seen.get(&(gid.clone(), m.clone()));
                    let theirs = |a: &str| {
                        let told = told.get(a).copied().unwrap_or(0);
                        told.max(sent.and_then(|h| h.get(a)).copied().unwrap_or(0))
                    };
                    ours.iter().all(|(a, n)| theirs(a) >= *n)
                        && told.iter().all(|(a, n)| {
                            ours.get(a).copied().unwrap_or(0) >= *n || !g.log.view.ever.contains(a)
                        })
                })
        })
    }

    /// Remember which entries `peer` sent us in a `group_entries`: it has
    /// them (see `groups_settled`).
    pub(super) fn note_group_entries(&mut self, peer: &str, msg: &Value) {
        let Some(gid) = msg.get("gid").and_then(Value::as_str) else {
            return;
        };
        let Some(g) = self.groups.get(gid) else {
            return;
        };
        let Some(list) = msg.get("entries").and_then(Value::as_array) else {
            return;
        };
        let mut sent: Have = Have::new();
        for item in list.iter().take(MAX_BATCH_ENTRIES) {
            let Some(body) = item.get("body").and_then(Value::as_str) else {
                continue;
            };
            let Ok(body) = serde_json::from_str::<Value>(body) else {
                continue;
            };
            let (Some(a), Some(n)) = (
                body.get("a").and_then(Value::as_str),
                body.get("n").and_then(Value::as_u64),
            ) else {
                continue;
            };
            if g.log.view.ever.contains(a) {
                let top = sent.entry(a.to_string()).or_insert(0);
                *top = (*top).max(n);
            }
        }
        if sent.is_empty() {
            return;
        }
        let seen = self
            .group_seen
            .entry((gid.to_string(), peer.to_string()))
            .or_default();
        for (a, n) in sent {
            let top = seen.entry(a).or_insert(0);
            *top = (*top).max(n);
        }
    }

    /// Group log lines to append to disk, and the meta file if it changed.
    pub(super) fn take_group_writes(&mut self) -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();
        for (gid, g) in &mut self.groups {
            if g.unsaved.is_empty() {
                continue;
            }
            // Without the key, not saved: members send them again next time.
            let lines: String = g
                .unsaved
                .drain(..)
                .filter_map(|e| log_line(&e).ok())
                .collect();
            if !lines.is_empty() {
                out.push((log_path(&self.groups_dir, gid), lines));
            }
        }
        out
    }

    /// Once group log lines are appended: the entries they hold no longer
    /// stay in memory (`Log::index_new`).
    pub(super) fn index_group_logs(&mut self) {
        for g in self.groups.values_mut() {
            g.log.index_new();
        }
    }

    fn close_disallowed(&mut self) {
        let gone: Vec<String> = self
            .conns
            .keys()
            .filter(|p| !self.is_allowed(p))
            .cloned()
            .collect();
        for peer in gone {
            if let Some(c) = self.conns.get(&peer) {
                c.conn
                    .close(VarInt::from_u32(CLOSE_REJECTED), b"not a member");
            }
        }
    }

    fn forget_group(&mut self, gid: &str) {
        self.groups.remove(gid);
        self.gmeta.data.groups.retain(|g| g.id != gid);
        // Saved before the log goes: a crash in between must not leave a
        // group without its log (stuck joining).
        self.gmeta.mark_dirty();
        let _ = self.gmeta.save_now();
        let path = log_path(&self.groups_dir, gid);
        let _ = std::fs::remove_file(&path);
        let mut backup = path.into_os_string();
        backup.push(".bak");
        let _ = std::fs::remove_file(backup);
    }

    fn set_pending_sync(&mut self, gid: &str, pending: bool) {
        if let Some(m) = self.gmeta.data.groups.iter_mut().find(|m| m.id == gid) {
            if m.pending_sync != pending {
                m.pending_sync = pending;
                self.gmeta.mark_dirty();
            }
        }
    }
}

pub(super) fn append_lines(path: &Path, lines: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(lines.as_bytes())?;
    file.sync_data()
}

// ─── Node: groups ────────────────────────────────────────────────────────────

impl Node {
    /// Sign and apply one of our own entries, then send it to the members.
    fn post(&self, s: &mut State, gid: &str, op: Op) -> bool {
        if !s.can_post(gid) {
            return false;
        }
        let me = s.me.clone();
        let Some(g) = s.groups.get_mut(gid) else {
            return false;
        };
        let (n, t, d) = g.log.draft(&me, now_ms());
        // Members before the change too: a removed member must learn it.
        let before: Vec<String> = g.log.view.members.keys().cloned().collect();
        let signed = group::sign(
            &s.secret,
            &Body {
                g: gid.to_string(),
                a: me,
                n,
                t,
                d: Some(d),
                op,
            },
        );
        let Some(entry) = group::parse(signed.clone(), gid, false) else {
            return false;
        };
        // On disk before anyone sees it: after a crash we must not sign
        // something else with the same seq. Never while the key is
        // unavailable: the log could not be read either.
        let Ok(line) = log_line(&signed) else {
            return false;
        };
        if append_lines(&log_path(&s.groups_dir, gid), &line).is_err() {
            return false;
        }
        if g.log.insert(entry) != group::Insert::New {
            return false;
        }
        g.log.refresh();
        g.log.index_new();
        let gone: Vec<String> = before
            .into_iter()
            .filter(|m| !g.log.view.is_member(m))
            .collect();
        s.broadcast(gid, std::slice::from_ref(&signed), None);
        for m in gone
            .iter()
            .filter(|m| **m != s.me && s.conns.contains_key(*m))
        {
            s.send_entries(m, gid, std::slice::from_ref(&signed));
        }
        s.events.push(Event::GroupUpdate {
            gid: gid.to_string(),
        });
        true
    }

    fn post_profile(&self, s: &mut State, gid: &str) {
        let nick = s.nickname.clone();
        let current = s
            .groups
            .get(gid)
            .and_then(|g| g.log.view.nicks.get(&s.me).cloned());
        if !nick.is_empty() && current.as_deref() != Some(nick.as_str()) {
            self.post(s, gid, Op::Profile { nick });
        }
    }

    /// We now have all our own entries: post what had to wait.
    fn after_own_sync(&self, s: &mut State, gid: &str) {
        if !s.member_of(gid) || !s.can_post(gid) {
            return;
        }
        let me = s.me.clone();
        let Some(g) = s.groups.get_mut(gid) else {
            return;
        };
        let orphans = std::mem::take(&mut g.orphans);
        // Only what members do not have already (the same entry, same seq).
        let lost: Vec<Op> = orphans
            .into_iter()
            .filter(|o| g.log.get(&me, o.n).is_none_or(|e| e.hash != o.hash))
            .filter_map(|o| o.op)
            .filter(|op| !matches!(op, Op::Create { .. } | Op::Profile { .. }))
            .collect();
        for op in lost {
            self.post(s, gid, op);
        }
        self.post_profile(s, gid);
    }

    /// Members have other versions of our entries: ours were numbered again
    /// from 1 after a rejoin (by an earlier version, or with our log lost). Theirs are the
    /// ones the group kept, so drop ours, take theirs, and post again what
    /// only ours had once we have them (`after_own_sync`).
    fn reset_own_history(&self, s: &mut State, gid: &str) {
        let me = s.me.clone();
        let path = log_path(&s.groups_dir, gid);
        let Some(g) = s.groups.get_mut(gid) else {
            return;
        };
        let cut = g.log.truncate(&me, 1);
        g.orphans.extend(cut);
        g.own_synced = false;
        g.peer_have.clear();
        g.log.refresh();
        // The texts read back from the file are about to move.
        g.log.hold_texts();
        // Not without the key: lines we could not read would be lost.
        if let Ok(lines) = log_text(&g.log.all_signed()) {
            g.unsaved.clear();
            let _ = crate::store::write_atomic(&path, lines.as_bytes());
        }
        g.log.reindex();
        s.set_pending_sync(gid, true);
        let peers: Vec<String> = s.conns.keys().cloned().collect();
        for peer in peers {
            if s.groups[gid].log.view.is_member(&peer) {
                s.send_have(&peer, gid);
            }
        }
    }

    /// Close connections we no longer need, after a moment so the entries
    /// just queued for them (e.g. their removal) still get out.
    fn close_disallowed_later(&self) {
        let node = self.clone();
        self.spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            node.with(|s| s.close_disallowed());
        });
    }

    pub fn set_nickname(&self, nick: &str) {
        let nick: String = nick.trim().chars().take(group::MAX_NICK).collect();
        self.with(|s| {
            s.nickname = nick;
            let gids: Vec<String> = s
                .groups
                .keys()
                .filter(|g| s.member_of(g))
                .cloned()
                .collect();
            for gid in gids {
                self.post_profile(s, &gid);
            }
            let online: Vec<String> = s.conns.keys().cloned().collect();
            for friend_id in online {
                s.send_profile(&friend_id);
            }
        });
    }

    /// Called when a connection with `peer` is up.
    pub(super) fn groups_on_connect(&self, s: &mut State, peer: &str) {
        let shared: Vec<String> = s
            .groups
            .iter()
            .filter(|(gid, g)| {
                g.log.view.is_member(peer) && (s.member_of(gid) || s.is_left(gid))
                    || (s.syncing(gid) && s.is_friend(peer))
            })
            .map(|(gid, _)| gid.clone())
            .collect();
        for gid in shared {
            s.send_have(peer, &gid);
        }
        s.send_invites(peer);
        self.retry_downloads(s, true);
    }

    /// Refresh what connected members have, in case something sent was
    /// lost: while a group differs from a member's (see `groups_settled`),
    /// on reconnecting everyone and on the timer task's sweep.
    pub(super) fn groups_tick(&self, s: &mut State) {
        let peers: Vec<String> = s.conns.keys().cloned().collect();
        for peer in peers {
            let shared: Vec<String> = s
                .groups
                .iter()
                .filter(|(gid, g)| {
                    g.log.view.is_member(&peer) && (s.member_of(gid) || s.is_left(gid))
                })
                .map(|(gid, _)| gid.clone())
                .collect();
            for gid in shared {
                s.send_have(&peer, &gid);
            }
        }
        self.retry_downloads(s, false);
    }

    pub(super) fn on_group_invite(&self, s: &mut State, peer: &str, msg: &Value) {
        let Some(gid) = msg.get("gid").and_then(Value::as_str).filter(|g| is_tid(g)) else {
            return;
        };
        let name: String = msg
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .chars()
            .take(group::MAX_NAME)
            .collect();
        let via = msg
            .get("via")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if s.gmeta.data.declined.contains(&via) {
            // Our answer did not reach whoever may remove us: once more.
            s.send(peer, json!({ "type": "group_decline", "gid": gid }));
            return;
        }
        if s.groups.contains_key(gid) {
            return;
        }
        if s.gmeta.data.invites.iter().any(|i| i.gid == gid)
            || s.gmeta.data.invites.len() >= MAX_INVITES
        {
            return;
        }
        s.gmeta.data.invites.push(Invite {
            gid: gid.to_string(),
            name: name.clone(),
            from: peer.to_string(),
            via,
            ts: now_ms(),
        });
        s.gmeta.mark_dirty();
        let from = s
            .friends
            .data
            .iter()
            .find(|f| f.id == peer)
            .map(Friend::display_name)
            .unwrap_or_default();
        s.events.push(Event::GroupInvite {
            gid: gid.to_string(),
            group: name,
            from,
        });
    }

    pub(super) fn on_group_decline(&self, s: &mut State, peer: &str, msg: &Value) {
        let Some(gid) = msg.get("gid").and_then(Value::as_str) else {
            return;
        };
        // Only the creator or whoever added them can remove them.
        let pending = s.pending_member(gid, peer)
            && s.groups[gid].log.view.can_remove(&s.me, peer);
        if pending && s.member_of(gid) {
            self.post(
                s,
                gid,
                Op::Remove {
                    member: peer.to_string(),
                },
            );
        }
    }

    pub(super) fn on_group_have(&self, s: &mut State, peer: &str, msg: &Value) {
        let Some(gid) = msg.get("gid").and_then(Value::as_str) else {
            return;
        };
        let field = |name: &str| msg.get(name).cloned().unwrap_or(Value::Null);
        let have: Have = serde_json::from_value(field("have")).unwrap_or_default();
        // Absent from 3.3 peers, which also ignore fork proofs.
        let mut heads: Option<HashMap<String, String>> = serde_json::from_value(field("heads")).ok();
        let forks: Option<HashMap<String, u64>> = serde_json::from_value(field("forks")).ok();
        let ask = msg.get("ask").and_then(Value::as_bool).unwrap_or(false);
        let me = s.me.clone();
        let left = s.is_left(gid);
        let Some(g) = s.groups.get_mut(gid) else {
            return;
        };
        if !g.log.view.is_member(peer) {
            return;
        }
        // The peer has another version of one of our entries.
        let own_head = heads.as_mut().and_then(|h| h.remove(&me));
        let own_fork = own_head.is_some_and(|hash| {
            have.get(&me)
                .and_then(|n| g.log.get(&me, *n))
                .is_some_and(|e| e.hash != hash)
        });
        if own_fork && !left && g.log.fork_of(&me).is_none() {
            return self.reset_own_history(s, gid);
        }
        let acked_before = g
            .peer_have
            .get(peer)
            .and_then(|h| h.get(&me))
            .copied()
            .unwrap_or(0);
        let acked_now = have.get(&me).copied().unwrap_or(0);
        let mut missing = g.log.missing_for(&have);
        if let Some(heads) = &heads {
            missing.extend(g.log.conflicts_with(&have, heads));
        }
        if let Some(forks) = &forks {
            missing.extend(g.log.evidence_for(forks));
        }
        let ours = g.log.have();
        let we_lack = have
            .iter()
            .any(|(a, n)| *n > ours.get(a).copied().unwrap_or(0));
        let leave_delivered = left && acked_now >= g.log.last_seq_of(&me);
        let now_synced = !g.own_synced && acked_now <= g.log.last_seq_of(&me);
        if now_synced {
            g.own_synced = true;
        }
        g.peer_have.insert(peer.to_string(), have);

        s.send_entries(peer, gid, &missing);
        if we_lack || ask {
            s.send_have(peer, gid);
        }
        if now_synced {
            s.set_pending_sync(gid, false);
            self.after_own_sync(s, gid);
        }
        if leave_delivered {
            s.forget_group(gid);
            s.events.push(Event::GroupUpdate {
                gid: gid.to_string(),
            });
        } else if acked_now != acked_before || now_synced {
            // Delivery receipts changed.
            s.events.push(Event::GroupUpdate {
                gid: gid.to_string(),
            });
        }
    }

    pub(super) fn on_group_entries(&self, s: &mut State, peer: &str, msg: &Value) {
        let Some(gid) = msg.get("gid").and_then(Value::as_str).map(str::to_string) else {
            return;
        };
        let Some(list) = msg.get("entries").and_then(Value::as_array) else {
            return;
        };
        if s.is_left(&gid) {
            return;
        }
        let me = s.me.clone();
        let syncing = s.syncing(&gid);
        let from_friend = s.is_friend(peer);
        let Some(g) = s.groups.get(&gid) else {
            return;
        };
        // Members only; while we join, also the friend who invited us.
        if !g.log.view.is_member(peer) && !(syncing && from_friend) {
            return;
        }
        let latest = now_ms().saturating_add(MAX_CLOCK_SKEW_MS);
        // Signatures are checked by `insert_all`, only for what can be kept.
        let entries: Vec<group::Entry> = list
            .iter()
            .take(MAX_BATCH_ENTRIES)
            .filter_map(|item| serde_json::from_value::<SignedEntry>(item.clone()).ok())
            .filter_map(|signed| group::parse(signed, &gid, false))
            .filter(|e| e.t <= latest)
            .collect();
        // Another version of one of our own entries.
        let own_fork = g.log.fork_of(&me).is_none()
            && entries.iter().any(|e| {
                e.a == me
                    && g.log.get(&me, e.n).is_some_and(|o| o.hash != e.hash)
                    && group::verify_entry(e)
            });
        if own_fork {
            self.reset_own_history(s, &gid);
        }
        let Some(g) = s.groups.get_mut(&gid) else {
            return;
        };
        let was_member = g.log.view.is_member(&me);
        let known_msgs: HashSet<String> =
            g.log.view.messages.iter().map(|m| m.id.clone()).collect();
        let inserted = g.log.insert_all(entries, true);
        if inserted.kept.is_empty() {
            return;
        }
        g.log.refresh();
        // Entries of strangers are neither kept nor passed on.
        g.log.drop_strangers();
        let ever = &g.log.view.ever;
        let new: Vec<SignedEntry> = inserted
            .kept
            .into_iter()
            .filter(|e| group::parse(e.clone(), &gid, false).is_some_and(|e| ever.contains(&e.a)))
            .collect();
        if new.is_empty() {
            return;
        }
        // Whoever holds other versions learns of the fork, and where it starts.
        let evidence: Vec<SignedEntry> = inserted
            .forked
            .iter()
            .flat_map(|a| g.log.fork_evidence(a))
            .collect();
        g.unsaved.extend(new.iter().cloned());
        let creator = g.log.view.creator.clone();
        let is_member = g.log.view.is_member(&me);
        let fresh: Vec<group::Message> = g
            .log
            .view
            .messages
            .iter()
            .filter(|m| m.author != me && !known_msgs.contains(&m.id))
            .cloned()
            .collect();

        if !creator.is_empty() {
            if let Some(m) = s.gmeta.data.groups.iter_mut().find(|m| m.id == gid) {
                if m.creator.is_none() {
                    m.creator = Some(creator.clone());
                    s.gmeta.mark_dirty();
                    if let Some(g) = s.groups.get_mut(&gid) {
                        g.log.pinned_creator = Some(creator);
                    }
                }
            }
        }
        s.broadcast(&gid, &new, Some(peer));
        if !evidence.is_empty() {
            s.broadcast(&gid, &evidence, None);
        }
        s.send_have(peer, &gid);

        if is_member && !was_member {
            // Just joined: introduce ourselves and reach the other members.
            if let Some(m) = s.gmeta.data.groups.iter_mut().find(|m| m.id == gid) {
                m.joined = now_ms();
                m.last_read = s.groups[&gid].log.view.max_t;
            }
            s.gmeta.mark_dirty();
            // Waits for our own earlier entries if we are rejoining.
            self.after_own_sync(s, &gid);
            for member in s.group_peers() {
                if !s.conns.contains_key(&member) {
                    self.dial(s, &member);
                }
            }
        } else if was_member && !is_member {
            self.close_disallowed_later();
        } else if is_member {
            for member in s.group_peers() {
                if !s.conns.contains_key(&member) {
                    self.dial(s, &member);
                }
            }
            self.close_disallowed_later();
        }

        // Notify the latest new message, not a whole history being synced;
        // rather the latest that names us. A muted group only notifies those.
        let joined = s.meta(&gid).map(|m| m.joined).unwrap_or(0);
        let muted = s.muted(&gid);
        let view = &s.groups[&gid].log.view;
        let recent: Vec<(&group::Message, group::Mention)> = fresh
            .iter()
            .filter(|m| m.ts >= joined && was_member)
            .map(|m| (m, s.mention(view, m)))
            .collect();
        let named = recent.iter().rev().find(|(_, k)| *k == group::Mention::Me);
        let pick = named.or(recent.last().filter(|_| !muted));
        if let Some((m, kind)) = pick {
            let event = Event::GroupMessage {
                gid: gid.clone(),
                group: view.name.clone(),
                author: s.display_name(view, &m.author),
                text: m.text.clone(),
                mention: *kind != group::Mention::None,
            };
            s.events.push(event);
        }
        s.events.push(Event::GroupUpdate { gid });
    }

    // ── API ──

    pub fn groups(&self) -> GroupsOverview {
        self.with(|s| {
            let mut groups: Vec<GroupSummary> = s
                .groups
                .iter()
                .filter(|(gid, _)| !s.is_left(gid))
                .map(|(gid, g)| {
                    let v = &g.log.view;
                    let last_read = s.meta(gid).map(|m| m.last_read).unwrap_or(0);
                    let last = v.messages.last();
                    let unread: Vec<&group::Message> = v
                        .messages
                        .iter()
                        .filter(|m| m.author != s.me && m.ts > last_read)
                        .collect();
                    GroupSummary {
                        id: gid.clone(),
                        name: if v.name.is_empty() {
                            "Groupe".into()
                        } else {
                            v.name.clone()
                        },
                        members: v.members.len(),
                        online: v
                            .members
                            .keys()
                            .filter(|m| **m != s.me && s.conns.contains_key(*m))
                            .count(),
                        unread: unread.len(),
                        unread_mentions: unread
                            .iter()
                            .filter(|m| s.mention(v, m) != group::Mention::None)
                            .count(),
                        muted: s.muted(gid),
                        last_message: last
                            .map(|m| format!("{} : {}", s.display_name(v, &m.author), m.text)),
                        last_ts: last.map(|m| m.ts).unwrap_or(0),
                        syncing: s.syncing(gid),
                        removed: s.removed(gid),
                    }
                })
                .collect();
            groups.sort_by_key(|g| std::cmp::Reverse(g.last_ts));
            let invites = s
                .gmeta
                .data
                .invites
                .iter()
                .map(|i| InviteView {
                    gid: i.gid.clone(),
                    name: i.name.clone(),
                    from: i.from.clone(),
                    from_name: s
                        .friends
                        .data
                        .iter()
                        .find(|f| f.id == i.from)
                        .map(Friend::display_name)
                        .unwrap_or_else(|| "Un ami".into()),
                })
                .collect();
            GroupsOverview { groups, invites }
        })
    }

    pub fn create_group(&self, name: &str, members: &[String]) -> Result<String, &'static str> {
        let name: String = name.trim().chars().take(group::MAX_NAME).collect();
        if name.is_empty() {
            return Err("no_name");
        }
        if members.len() + 1 > group::MAX_MEMBERS {
            return Err("too_many");
        }
        // The id names us as the creator: nobody else can write its creation.
        let nonce = hex::encode(rand::random::<[u8; 16]>());
        let gid = group::group_id(&self.id(), &nonce);
        self.with(|s| {
            let log = new_log(&s.groups_dir, &gid);
            s.groups.insert(gid.clone(), GroupState::new(log, true));
            s.gmeta.data.groups.push(GroupMeta {
                id: gid.clone(),
                last_read: 0,
                joined: now_ms(),
                left: false,
                creator: Some(s.me.clone()),
                pending_sync: false,
                muted: false,
            });
            // Saved before the log gets its first line: a crash in between
            // must not leave a log no group refers to.
            s.gmeta.mark_dirty();
            let _ = s.gmeta.save_now();
            self.post(
                s,
                &gid,
                Op::Create {
                    name,
                    nonce: Some(nonce),
                },
            );
            self.post_profile(s, &gid);
            self.add_members_locked(s, &gid, members);
        });
        Ok(gid)
    }

    fn add_members_locked(&self, s: &mut State, gid: &str, members: &[String]) {
        for m in members {
            let addable = s.is_friend(m)
                && s.groups.get(gid).is_some_and(|g| {
                    let v = &g.log.view;
                    !v.is_member(m) && v.can_add(&s.me, m) && v.members.len() < group::MAX_MEMBERS
                });
            if addable {
                self.post(s, gid, Op::Add { member: m.clone() });
                s.fast_retries.remove(m);
                if s.conns.contains_key(m) {
                    s.send_invites(m);
                } else {
                    self.dial(s, m);
                }
            }
        }
    }

    pub fn group_add_members(&self, gid: &str, members: &[String]) -> Result<(), &'static str> {
        self.with(|s| {
            if !s.member_of(gid) {
                return Err("not_member");
            }
            let count = s.groups[gid].log.view.members.len();
            if count + members.len() > group::MAX_MEMBERS {
                return Err("too_many");
            }
            self.add_members_locked(s, gid, members);
            Ok(())
        })
    }

    pub fn group_remove_member(&self, gid: &str, member: &str) {
        if member == self.id() {
            return self.group_leave(gid);
        }
        self.with(|s| {
            let allowed = s.member_of(gid) && {
                let v = &s.groups[gid].log.view;
                v.is_member(member) && v.can_remove(&s.me, member)
            };
            if allowed {
                self.post(
                    s,
                    gid,
                    Op::Remove {
                        member: member.to_string(),
                    },
                );
                self.close_disallowed_later();
            }
        });
    }

    pub fn group_leave(&self, gid: &str) {
        self.with(|s| {
            if s.removed(gid) || s.syncing(gid) {
                s.forget_group(gid);
                s.events.push(Event::GroupUpdate {
                    gid: gid.to_string(),
                });
                return;
            }
            if !s.member_of(gid) {
                return;
            }
            let me = s.me.clone();
            self.post(s, gid, Op::Remove { member: me });
            let alone = s.groups[gid].log.view.members.is_empty();
            if let Some(m) = s.gmeta.data.groups.iter_mut().find(|m| m.id == gid) {
                m.left = true;
            }
            s.gmeta.mark_dirty();
            let _ = s.gmeta.save_now();
            if alone {
                s.forget_group(gid);
            }
            s.events.push(Event::GroupUpdate {
                gid: gid.to_string(),
            });
        });
    }

    pub fn group_rename(&self, gid: &str, name: &str) {
        let name: String = name.trim().chars().take(group::MAX_NAME).collect();
        self.with(|s| {
            if !name.is_empty() && s.member_of(gid) {
                self.post(s, gid, Op::Rename { name });
            }
        });
    }

    pub fn group_mark_read(&self, gid: &str) {
        self.with(|s| {
            let Some(max) = s
                .groups
                .get(gid)
                .map(|g| g.log.view.messages.last().map(|m| m.ts).unwrap_or(0))
            else {
                return;
            };
            if let Some(m) = s.gmeta.data.groups.iter_mut().find(|m| m.id == gid) {
                if m.last_read < max {
                    m.last_read = max;
                    s.gmeta.mark_dirty();
                    s.events.push(Event::GroupUpdate {
                        gid: gid.to_string(),
                    });
                }
            }
        });
    }

    pub fn respond_to_invite(&self, gid: &str, accept: bool) {
        self.with(|s| {
            let Some(pos) = s.gmeta.data.invites.iter().position(|i| i.gid == gid) else {
                return;
            };
            let invite = s.gmeta.data.invites.remove(pos);
            s.gmeta.mark_dirty();
            if !accept {
                let declined = &mut s.gmeta.data.declined;
                declined.push(invite.via.clone());
                if declined.len() > MAX_DECLINED {
                    declined.remove(0);
                }
                let _ = s.gmeta.save_now();
                // To the inviter, and to whoever added us (only they or the
                // creator can remove us); sent again with each invitation.
                let adder = invite.via.split(':').next().unwrap_or("").to_string();
                s.send(&invite.from, json!({ "type": "group_decline", "gid": gid }));
                if adder != invite.from {
                    s.send(&adder, json!({ "type": "group_decline", "gid": gid }));
                }
                s.events.push(Event::GroupUpdate {
                    gid: gid.to_string(),
                });
                return;
            }
            // Until a member tells us we have all our earlier entries (we may
            // be rejoining), we post nothing: see `State::can_post`.
            let log = new_log(&s.groups_dir, gid);
            s.groups.insert(gid.to_string(), GroupState::new(log, false));
            s.gmeta.data.groups.push(GroupMeta {
                id: gid.to_string(),
                last_read: 0,
                joined: 0,
                left: false,
                creator: None,
                pending_sync: true,
                muted: false,
            });
            let _ = s.gmeta.save_now();
            // Ask the inviter (and any connected friend) for the log.
            let friends: Vec<String> = s.conns.keys().filter(|p| s.is_friend(p)).cloned().collect();
            for f in friends {
                s.send_have(&f, gid);
            }
            if !s.conns.contains_key(&invite.from) {
                self.dial(s, &invite.from);
            }
            s.events.push(Event::GroupUpdate {
                gid: gid.to_string(),
            });
        });
    }

    pub fn group_detail(&self, gid: &str) -> Option<GroupDetail> {
        self.with(|s| {
            let g = s.groups.get(gid)?;
            let v = &g.log.view;
            let mut members: Vec<MemberView> = v
                .members
                .keys()
                .map(|m| MemberView {
                    id: m.clone(),
                    name: s.display_name(v, m),
                    online: *m == s.me || s.conns.contains_key(m),
                    friend: s.is_friend(m),
                    me: *m == s.me,
                    removable: *m != s.me && v.can_remove(&s.me, m),
                    handle: s.handle(v, m),
                })
                .collect();
            members.sort_by_key(|m| (!m.me, !m.online, m.name.to_lowercase()));
            let pins = v
                .pins
                .iter()
                .rev()
                .filter_map(|p| {
                    let (kind, text, author, ts) = match v.files.get(&p.target) {
                        Some(f) => ("file", f.name.clone(), &f.author, f.ts),
                        None => {
                            let m = v.messages.iter().find(|m| m.id == p.target)?;
                            ("message", m.text.clone(), &m.author, m.ts)
                        }
                    };
                    Some(PinView {
                        target: p.target.clone(),
                        kind,
                        text,
                        author_name: s.display_name(v, author),
                        ts,
                        pinned_by: p.by.clone(),
                        pinned_by_name: s.display_name(v, &p.by),
                        pinned_at: p.ts,
                    })
                })
                .collect();
            Some(GroupDetail {
                id: gid.to_string(),
                name: if v.name.is_empty() {
                    "Groupe".into()
                } else {
                    v.name.clone()
                },
                members,
                syncing: s.syncing(gid),
                removed: s.removed(gid),
                muted: s.muted(gid),
                pins,
            })
        })
    }

    pub fn group_messages(&self, gid: &str) -> Vec<GroupMessageView> {
        self.with(|s| {
            let Some(g) = s.groups.get(gid) else {
                return Vec::new();
            };
            let v = &g.log.view;
            let others: Vec<&String> = v.members.keys().filter(|m| **m != s.me).collect();
            let handles = s.own_handles(v);
            let pinned: HashSet<&str> = v.pins.iter().map(|p| p.target.as_str()).collect();
            v.messages
                .iter()
                .map(|m| {
                    let mine = m.author == s.me;
                    let received_by = if mine {
                        others
                            .iter()
                            .filter(|o| {
                                g.peer_have
                                    .get(**o)
                                    .and_then(|h| h.get(&s.me))
                                    .is_some_and(|n| *n >= m.seq)
                            })
                            .count()
                    } else {
                        0
                    };
                    GroupMessageView {
                        id: m.id.clone(),
                        author: m.author.clone(),
                        author_name: s.display_name(v, &m.author),
                        text: m.text.clone(),
                        ts: m.ts,
                        mine,
                        received_by,
                        total: others.len(),
                        mentions_me: !mine
                            && group::mention_of(&m.text, &handles) != group::Mention::None,
                        pinned: pinned.contains(m.id.as_str()),
                    }
                })
                .collect()
        })
    }

    pub fn group_send(&self, gid: &str, text: &str) -> Result<(), &'static str> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty");
        }
        if js_len(text) > MAX_TEXT {
            return Err("too_long");
        }
        self.with(|s| {
            if !s.member_of(gid) {
                return Err("not_member");
            }
            if !s.can_post(gid) {
                return Err("syncing");
            }
            let id = uuid::Uuid::new_v4().to_string();
            let op = Op::Msg {
                id,
                text: text.to_string(),
            };
            if !self.post(s, gid, op) {
                return Err("not_sent");
            }
            if let Some(m) = s.gmeta.data.groups.iter_mut().find(|m| m.id == gid) {
                m.last_read = now_ms().max(m.last_read);
            }
            s.gmeta.mark_dirty();
            Ok(())
        })
    }

    // ── Shared files ──

    pub fn group_files(&self, gid: &str) -> Vec<GroupFileView> {
        self.with(|s| {
            let Some(g) = s.groups.get(gid) else {
                return Vec::new();
            };
            let v = &g.log.view;
            let mut files: Vec<GroupFileView> = v
                .files
                .values()
                .map(|f| GroupFileView {
                    id: f.id.clone(),
                    name: f.name.clone(),
                    dir: f.dir.clone(),
                    pinned: v.pins.iter().any(|p| p.target == f.id),
                    size: f.size,
                    author: f.author.clone(),
                    author_name: s.display_name(v, &f.author),
                    ts: f.ts,
                    mine: f.author == s.me,
                    local: s
                        .gmeta
                        .data
                        .holdings
                        .get(&f.hash)
                        .is_some_and(holding_valid),
                    transfer_id: s
                        .transfers
                        .values()
                        .find(|t| {
                            !t.status.is_final()
                                && t.repo.as_ref().is_some_and(|r| r.hash == f.hash)
                        })
                        .map(|t| t.id.clone()),
                })
                .collect();
            files.sort_by_key(|f| std::cmp::Reverse(f.ts));
            files
        })
    }

    /// Hash the files (outside the lock) and list them in the group.
    pub async fn group_add_files(&self, gid: &str, paths: Vec<PathBuf>) -> usize {
        let files = paths.into_iter().map(|p| (p, None)).collect();
        self.add_shared(gid, files).await
    }

    /// Files and folders dropped on the group: files as they are, folders'
    /// files (recursively, up to `MAX_DROP_FILES`) with their folder.
    pub async fn group_add_paths(&self, gid: &str, paths: Vec<PathBuf>) -> usize {
        let Ok(files) = tokio::task::spawn_blocking(move || expand_paths(paths)).await else {
            return 0;
        };
        self.add_shared(gid, files).await
    }

    async fn add_shared(&self, gid: &str, files: Vec<(PathBuf, Option<String>)>) -> usize {
        let mut added = 0;
        for (path, dir) in files {
            // Not hashing everything for nothing.
            if !self.with(|s| s.member_of(gid) && s.can_post(gid)) {
                break;
            }
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let p = path.clone();
            let Ok(Ok(hash)) = tokio::task::spawn_blocking(move || hash_file(&p)).await else {
                continue;
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ok = self.with(|s| {
                if !s.member_of(gid) || !s.can_post(gid) {
                    return false;
                }
                add_holding(&mut s.gmeta.data, hash.clone(), &path, &meta, gid);
                s.gmeta.mark_dirty();
                let id = hex::encode(rand::random::<[u8; 8]>());
                self.post(
                    s,
                    gid,
                    Op::FileAdd {
                        id,
                        name,
                        size: meta.len(),
                        hash,
                        dir: dir.as_deref().and_then(group::clean_dir),
                    },
                )
            });
            if ok {
                added += 1;
            }
        }
        added
    }

    pub fn set_group_muted(&self, gid: &str, muted: bool) {
        self.with(|s| {
            if let Some(m) = s.gmeta.data.groups.iter_mut().find(|m| m.id == gid) {
                if m.muted != muted {
                    m.muted = muted;
                    s.gmeta.mark_dirty();
                    s.events.push(Event::GroupUpdate {
                        gid: gid.to_string(),
                    });
                }
            }
        });
    }

    /// Pin (or unpin) a message or a shared file for every member.
    pub fn group_pin(&self, gid: &str, target: &str, pin: bool) -> Result<(), &'static str> {
        self.with(|s| {
            if !s.member_of(gid) {
                return Err("not_member");
            }
            if !s.can_post(gid) {
                return Err("syncing");
            }
            let v = &s.groups[gid].log.view;
            let pinned = v.pins.iter().any(|p| p.target == target);
            let op = if pin {
                let exists = v.files.contains_key(target) || v.messages.iter().any(|m| m.id == target);
                if !exists {
                    return Err("unknown");
                }
                if pinned {
                    return Ok(());
                }
                if v.pins.len() >= group::MAX_PINS {
                    return Err("too_many");
                }
                Op::Pin {
                    target: target.to_string(),
                }
            } else {
                if !pinned {
                    return Ok(());
                }
                Op::Unpin {
                    target: target.to_string(),
                }
            };
            if self.post(s, gid, op) {
                Ok(())
            } else {
                Err("not_sent")
            }
        })
    }

    pub fn group_delete_file(&self, gid: &str, file_id: &str) {
        self.with(|s| {
            let entry_id = s
                .groups
                .get(gid)
                .and_then(|g| g.log.view.files.get(file_id))
                .filter(|f| f.author == s.me)
                .map(|f| f.entry_id.clone());
            if let Some(id) = entry_id.filter(|_| s.member_of(gid)) {
                self.post(s, gid, Op::FileDel { id });
            }
        });
    }

    pub fn group_file_path(&self, gid: &str, file_id: &str) -> Option<PathBuf> {
        self.with(|s| {
            let f = s.groups.get(gid)?.log.view.files.get(file_id)?;
            s.gmeta
                .data
                .holdings
                .get(&f.hash)
                .filter(|h| holding_valid(h))
                .map(|h| h.path.clone())
        })
    }

    pub fn group_download(&self, gid: &str, file_id: &str) -> Result<String, &'static str> {
        let base = (self.0.download_dir)();
        self.with(|s| {
            if !s.member_of(gid) {
                return Err("not_member");
            }
            let g = &s.groups[gid];
            let Some(f) = g.log.view.files.get(file_id).cloned() else {
                return Err("unknown");
            };
            if s.gmeta
                .data
                .holdings
                .get(&f.hash)
                .is_some_and(holding_valid)
            {
                return Err("local");
            }
            let running = s.transfers.values().find(|t| {
                !t.status.is_final() && t.repo.as_ref().is_some_and(|r| r.hash == f.hash)
            });
            if let Some(t) = running {
                return Ok(t.id.clone());
            }
            let dir = base
                .join(REPO_FOLDER)
                .join(sanitize_file_name(&g.log.view.name));
            // From a folder: saved in it there, as a folder a friend sends
            // (each name made safe; a path that could leave it: with the others).
            let dest = f.dir.as_deref().and_then(|d| {
                let parts = folder_path(&format!("{d}/{}", f.name), d.split('/').next()?)?;
                Some(parts[..parts.len() - 1].iter().fold(dir.clone(), |p, c| p.join(c)))
            });
            let tid = hex::encode(rand::random::<[u8; TID_BYTES]>());
            let (part, file) =
                create_part(&dir, &f.name).map_err(|_| "Dossier de téléchargement inaccessible")?;
            s.add_transfer(
                tid.clone(),
                Dir::In,
                &f.author,
                f.name.clone(),
                f.size,
                Status::Paused,
            );
            let t = s.transfers.get_mut(&tid).unwrap();
            // Small while waiting: the stream switches to a large buffer.
            if let Ok(mut slot) = t.shared.writer.try_lock() {
                *slot = Some(BufWriter::with_capacity(
                    IDLE_BUFFER,
                    tokio::fs::File::from_std(file),
                ));
            }
            t.part_path = Some(part);
            // Created when the download is saved (see `begin_finalize`).
            t.dest_dir = dest;
            t.repo = Some(Repo::new(gid, &f.hash));
            if f.size == 0 {
                self.begin_finalize(s, &tid);
            } else {
                self.pick_source(s, &tid);
            }
            Ok(tid)
        })
    }

    /// Ask a connected member for the file, preferring members never part
    /// of a corrupt copy, then its author.
    fn pick_source(&self, s: &mut State, tid: &str) {
        let Some(t) = s.transfers.get(tid) else {
            return;
        };
        let Some(repo) = &t.repo else { return };
        if t.status.is_final() || t.status == Status::Finishing {
            return;
        }
        let author = s.groups.get(&repo.gid).and_then(|g| {
            g.log
                .view
                .files
                .values()
                .find(|f| f.hash == repo.hash)
                .map(|f| f.author.clone())
        });
        let mut candidates: Vec<String> = s
            .groups
            .get(&repo.gid)
            .map(|g| {
                g.log
                    .view
                    .members
                    .keys()
                    .filter(|m| {
                        **m != s.me
                            && s.conns.contains_key(*m)
                            && !repo.tried.contains(*m)
                            && !repo.bad.contains(*m)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        candidates.sort_by_key(|m| {
            let strikes = repo.strikes.get(m).copied().unwrap_or(0);
            (strikes, Some(m) != author.as_ref())
        });
        let (hash, offset) = (repo.hash.clone(), t.bytes());
        let t = s.transfers.get_mut(tid).unwrap();
        let Some(r) = t.repo.as_mut() else { return };
        if offset > r.progress {
            r.progress = offset;
            r.rounds = 0;
        }
        match candidates.first() {
            Some(source) => {
                r.sources.insert(source.clone());
                r.picked_at = offset;
                t.friend_id = source.clone();
                t.error = None;
                t.resyncing = false;
                let source = source.clone();
                s.send(
                    &source,
                    json!({ "type": "blob_req", "tid": tid, "hash": hash, "offset": offset }),
                );
                s.set_status(tid, Status::Receiving, None);
            }
            None => {
                // Asked again later, less and less often.
                r.tried.clear();
                r.rounds = r.rounds.saturating_add(1);
                r.next_try = Some(Instant::now() + retry_delay(r.rounds));
                s.set_status(
                    tid,
                    Status::Paused,
                    Some("En attente d’un membre qui a ce fichier".into()),
                );
            }
        }
    }

    /// Ask again for files nobody could serve (`now`: whatever the delay, as
    /// a member just connected). Downloads of files no longer shared stop.
    pub(super) fn retry_downloads(&self, s: &mut State, now: bool) {
        let waiting: Vec<(String, String, String)> = s
            .transfers
            .values()
            .filter(|t| t.status == Status::Paused)
            .filter_map(|t| {
                let r = t.repo.as_ref()?;
                let due = now || r.next_try.is_none_or(|at| at <= Instant::now());
                due.then(|| (t.id.clone(), r.gid.clone(), r.hash.clone()))
            })
            .collect();
        for (tid, gid, hash) in waiting {
            let listed = s.member_of(&gid)
                && s.groups[&gid].log.view.files.values().any(|f| f.hash == hash);
            if listed {
                self.pick_source(s, &tid);
            } else {
                self.stop_transfer(
                    s,
                    &tid,
                    Status::Error,
                    Some("Ce fichier n’est plus partagé dans le groupe".into()),
                );
            }
        }
    }

    /// The source went away or cannot serve: try another member.
    pub(super) fn next_source(&self, s: &mut State, tid: &str, peer: &str) -> bool {
        let Some(t) = s.transfers.get_mut(tid) else {
            return false;
        };
        if t.friend_id != peer || t.status.is_final() {
            return false;
        }
        let bytes = t.bytes();
        let Some(repo) = t.repo.as_mut() else {
            return false;
        };
        repo.tried.insert(peer.to_string());
        if bytes == repo.picked_at {
            // Sent nothing of this copy.
            repo.sources.remove(peer);
        }
        t.shared.next_session();
        // The stream task may still own the file: start from what is written.
        self.pick_source(s, tid);
        true
    }

    pub(super) fn on_blob_missing(&self, s: &mut State, peer: &str, msg: &Value) {
        if let Some(tid) = msg.get("tid").and_then(Value::as_str) {
            self.next_source(s, tid, peer);
        }
    }

    pub(super) fn on_blob_req(&self, s: &mut State, peer: &str, conn_id: u64, msg: &Value) {
        let Some(tid) = msg.get("tid").and_then(Value::as_str).filter(|t| is_tid(t)) else {
            return;
        };
        let hash = msg
            .get("hash")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let offset = msg.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let holding = s
            .gmeta
            .data
            .holdings
            .get(&hash)
            .filter(|h| holding_valid(h))
            .cloned();
        // Only to members of a group we have it from, where it is still
        // listed: knowing a hash is not enough to get the file.
        let listed = holding.as_ref().and_then(|h| {
            h.groups.iter().find_map(|gid| {
                let g = s.groups.get(gid)?;
                let ok = s.member_of(gid) && g.log.view.is_member(peer);
                ok.then(|| g.log.view.files.values().find(|f| f.hash == hash).cloned())
                    .flatten()
            })
        });
        let serving = s
            .transfers
            .values()
            .filter(|t| t.serve && t.friend_id == peer && !t.status.is_final())
            .count();
        let (Some(file), Some(holding)) = (listed, holding) else {
            s.send(peer, json!({ "type": "blob_missing", "tid": tid }));
            return;
        };
        let replaces = s.transfers.get(tid).is_some_and(|t| !t.status.is_final());
        if offset > holding.size || (serving >= MAX_SERVES_PER_PEER && !replaces) {
            s.send(peer, json!({ "type": "blob_missing", "tid": tid }));
            return;
        }
        if let Some(old) = s.transfers.get(tid) {
            if !(old.serve && old.friend_id == peer) {
                return;
            }
            old.shared.next_session();
        }
        s.add_transfer(
            tid.to_string(),
            Dir::Out,
            peer,
            file.name.clone(),
            holding.size,
            Status::Paused,
        );
        let t = s.transfers.get_mut(tid).unwrap();
        t.serve = true;
        t.hidden = true;
        t.src_path = Some(holding.path);
        self.on_accept(s, peer, conn_id, &json!({ "tid": tid, "offset": offset }));
    }

    /// Check a finished shared-file download against the catalog's hash.
    pub(super) async fn verify_download(part: PathBuf, hash: String) -> bool {
        tokio::task::spawn_blocking(move || hash_file(&part).is_ok_and(|h| h == hash))
            .await
            .unwrap_or(false)
    }

    /// A finished download did not match its hash: start over. A member who
    /// sent the whole copy is never asked again; when several sent parts of
    /// it, each gets a strike and is asked last, and is dropped at the second.
    pub(super) async fn retry_corrupt_download(&self, tid: &str, part: PathBuf) {
        let reopened = tokio::fs::File::create(&part).await;
        self.with(|s| {
            let still = s
                .transfers
                .get(tid)
                .is_some_and(|t| t.status == Status::Finishing);
            let attempts = s
                .transfers
                .get(tid)
                .and_then(|t| t.repo.as_ref())
                .map_or(0, |r| r.corrupt + 1);
            let file = match reopened {
                Ok(file) if still && attempts < MAX_CORRUPT => file,
                _ => {
                    let _ = std::fs::remove_file(&part);
                    if still {
                        self.stop_transfer(
                            s,
                            tid,
                            Status::Error,
                            Some("Fichier corrompu : empreinte invalide".into()),
                        );
                    }
                    return;
                }
            };
            let t = s.transfers.get_mut(tid).unwrap();
            let source = t.friend_id.clone();
            t.shared.next_session();
            t.shared.bytes.store(0, Ordering::Relaxed);
            if let Ok(mut slot) = t.shared.writer.try_lock() {
                *slot = Some(BufWriter::with_capacity(IDLE_BUFFER, file));
            }
            if let Some(repo) = t.repo.as_mut() {
                let mut sources = std::mem::take(&mut repo.sources);
                if sources.is_empty() {
                    sources.insert(source);
                }
                let alone = sources.len() == 1;
                for m in sources {
                    let strikes = repo.strikes.entry(m.clone()).or_insert(0);
                    *strikes += 1;
                    if alone || *strikes >= 2 {
                        repo.bad.insert(m);
                    }
                }
                repo.corrupt = attempts;
                repo.tried.clear();
                repo.picked_at = 0;
                repo.progress = 0;
            }
            s.set_status(
                tid,
                Status::Paused,
                Some("Copie corrompue reçue : nouvelle tentative".into()),
            );
            self.pick_source(s, tid);
        });
    }

    pub(super) fn record_download(&self, s: &mut State, tid: &str, target: &Path) {
        let Some(t) = s.transfers.get(tid) else {
            return;
        };
        let Some(repo) = &t.repo else { return };
        let Ok(meta) = std::fs::metadata(target) else {
            return;
        };
        let (gid, hash) = (repo.gid.clone(), repo.hash.clone());
        add_holding(&mut s.gmeta.data, hash, target, &meta, &gid);
        s.gmeta.mark_dirty();
        s.events.push(Event::GroupUpdate { gid });
    }

    pub fn forget_group(&self, gid: &str) {
        self.with(|s| {
            if s.removed(gid) {
                s.forget_group(gid);
                s.events.push(Event::GroupUpdate {
                    gid: gid.to_string(),
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_logs_are_migrated_and_damaged_ones_set_aside() {
        let dir = std::env::temp_dir().join(format!("p2pshare-glog-{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("g.jsonl");
        let e = SignedEntry {
            body: "{\"secret\":1}".into(),
            sig: "00".into(),
        };
        let plain = serde_json::to_string(&e).unwrap() + "\n";
        std::fs::write(&path, format!("{plain}{}", log_line(&e).unwrap())).unwrap();
        let read = read_log(&std::fs::read_to_string(&path).unwrap());
        assert!(read.plain && !read.damaged && read.entries.len() == 2);
        rewrite_log(&path, &read);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("secret") && text.lines().count() == 2);
        assert!(!std::fs::read_to_string(dir.join("g.jsonl.bak")).unwrap().contains("secret"));
        // A damaged line in the middle: the file is kept aside first.
        std::fs::write(&path, format!("pv1:AAAA\n{text}pv1:cut")).unwrap();
        let read = read_log(&std::fs::read_to_string(&path).unwrap());
        assert!(read.damaged && read.cut && read.entries.len() == 2);
        rewrite_log(&path, &read);
        assert_eq!(read_log(&std::fs::read_to_string(&path).unwrap()).entries.len(), 2);
        let aside = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|f| f.file_name().to_string_lossy().starts_with("g.corrupt-"));
        assert!(aside);
        let _ = std::fs::remove_dir_all(&dir);
    }
}