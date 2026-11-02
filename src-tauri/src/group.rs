//! Groups: one signed, append-only log per group, merged by every member.
//!
//! Everything that happens in a group (creation, members added or removed,
//! renames, nicknames, messages, shared files) is an entry signed with its
//! author's key and numbered per author (1, 2, 3…). Members exchange what
//! they are missing, so an entry reaches everyone even when its author is
//! offline, and nobody can forge another member's entries.
//!
//! The group's state is rebuilt by replaying the entries in one order that
//! every member computes the same way: (timestamp, author, seq). An entry
//! only counts if its author is a member at that point, so concurrent
//! removals resolve the same way everywhere: the first one wins.
//!
//! Timestamps are chosen by authors, so the rules never trust them alone:
//! the group id is derived from its creator's key (nobody else can create
//! it), an author's entries are only kept in seq order with increasing
//! timestamps, and only the creator or whoever added a member may remove
//! them (anyone may leave).
//!
//! A removal dated in the past (on purpose, or written offline long ago)
//! must not erase what the group already had: each entry tells what its
//! author had when writing it (`d`, the last seq it had of each author), and
//! an entry of a removed member still counts when another member had it
//! before learning of the removal (`Log::acknowledged`). Entries written
//! before `d` existed cannot vouch for anything: there the removal applies
//! from its timestamp, as before.
//!
//! An author who signs two different entries with the same seq would leave
//! members with different logs for good: members compare the hash of each
//! author's last entry, swap the two versions when they differ, and keep the
//! pair as proof (`Fork`). From that seq on, the author's entries are ignored
//! by everyone.
//!
//! The rules are the same as 3.3's for every log those versions accept,
//! except: the protection above, the `Fork` cut, the per-author quotas,
//! `MAX_MEMBERS_REPLAY`, and `View::can_add` after a removal written with
//! `d`. 3.3 parses, keeps and relays entries with `d` (an unknown field), and
//! keeps its own rules for them.
//!
//! Pins (`Pin`, `Unpin`) and a shared file's folder (`FileAdd::dir`) came
//! later: earlier versions keep and relay pin entries without applying them
//! (unknown kind) and read a file without its folder (unknown field), so
//! they sync as before and simply show neither.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;

use iroh::{PublicKey, SecretKey, Signature};
use serde::{Deserialize, Serialize};

/// Members a group may have: what this app lets anyone add up to.
pub const MAX_MEMBERS: usize = 32;
/// The cap when replaying. Members adding people at the same time near
/// `MAX_MEMBERS` must not cancel each other's additions (and with them
/// everything the people added wrote): only someone adding people on purpose
/// past `MAX_MEMBERS` reaches this one.
pub const MAX_MEMBERS_REPLAY: usize = 2 * MAX_MEMBERS;
pub const MAX_NAME: usize = 60;
pub const MAX_NICK: usize = 40;
pub const MAX_BODY: usize = 64 * 1024;
pub const MAX_TEXT: usize = 5000;
/// Items pinned at once in a group: pins past it are ignored everywhere.
pub const MAX_PINS: usize = 50;
/// Length of a pin's target ("author/id").
const MAX_TARGET: usize = 200;
/// Length of a shared file's folder.
const MAX_DIR: usize = 255;
/// Quotas per author, the same for every member: entries past them are
/// refused everywhere, so a member cannot fill everyone's disk.
pub const MAX_ENTRIES_PER_AUTHOR: u64 = 100_000;
pub const MAX_BYTES_PER_AUTHOR: usize = 64 << 20;
/// Authors one entry's context may name.
const MAX_DEPS: usize = 1024;

/// Seq per author up to which a peer has every entry.
pub type Have = HashMap<String, u64>;

/// An entry as stored and sent: the exact signed JSON text and its signature.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct SignedEntry {
    pub body: String,
    pub sig: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Body {
    pub g: String,
    pub a: String,
    pub n: u64,
    pub t: u64,
    /// What the author had when writing it, beyond what its previous entry
    /// already said: the last seq it had of each author. Absent in entries
    /// written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<BTreeMap<String, u64>>,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Op {
    Create {
        name: String,
        /// With the creator's key, gives the group id (see `group_id`).
        /// Absent in groups made before it existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<String>,
    },
    Add {
        member: String,
    },
    Remove {
        member: String,
    },
    Rename {
        name: String,
    },
    Profile {
        nick: String,
    },
    Msg {
        id: String,
        text: String,
    },
    FileAdd {
        id: String,
        name: String,
        size: u64,
        hash: String,
        /// Folder of the file within what was shared ("Photos/2024"), when
        /// a folder was dropped. Earlier versions ignore it (name only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dir: Option<String>,
    },
    FileDel {
        id: String,
    },
    /// Pin a message or a shared file ("author/id") for everyone. Earlier
    /// versions keep and relay these entries without applying them.
    Pin {
        target: String,
    },
    Unpin {
        target: String,
    },
}

/// Header fields, readable even for entry kinds this version does not know.
#[derive(Deserialize)]
struct Header {
    g: String,
    a: String,
    n: u64,
    t: u64,
    #[serde(default)]
    d: Option<BTreeMap<String, u64>>,
}

#[derive(Clone)]
pub struct Entry {
    pub signed: SignedEntry,
    pub a: String,
    pub n: u64,
    pub t: u64,
    pub d: Option<BTreeMap<String, u64>>,
    /// Short hash of the body, compared between members to find forks.
    pub hash: String,
    /// None for kinds from a newer version: kept and relayed, not applied.
    pub op: Option<Op>,
}

/// The id of a group created by `creator` with `nonce`: nobody else can
/// write a creation entry that matches it.
pub fn group_id(creator: &str, nonce: &str) -> String {
    let hash = blake3::hash(format!("{creator}:{nonce}").as_bytes());
    hash.to_hex()[..32].to_string()
}

fn entry_hash(body: &str) -> String {
    blake3::hash(body.as_bytes()).to_hex()[..32].to_string()
}

pub fn sign(secret: &SecretKey, body: &Body) -> SignedEntry {
    let text = serde_json::to_string(body).unwrap_or_default();
    let sig = secret.sign(text.as_bytes());
    SignedEntry {
        body: text,
        sig: hex::encode(sig.to_bytes()),
    }
}

/// Parse an entry of group `gid`; with `verify`, also check its signature.
pub fn parse(signed: SignedEntry, gid: &str, verify: bool) -> Option<Entry> {
    if signed.body.len() > MAX_BODY {
        return None;
    }
    let header: Header = serde_json::from_str(&signed.body).ok()?;
    if header.g != gid || header.n == 0 || header.d.as_ref().is_some_and(|d| d.len() > MAX_DEPS)
    {
        return None;
    }
    let op = serde_json::from_str::<Body>(&signed.body)
        .ok()
        .map(|b| b.op);
    let entry = Entry {
        a: header.a,
        n: header.n,
        t: header.t,
        d: header.d,
        hash: entry_hash(&signed.body),
        op,
        signed,
    };
    (!verify || verify_entry(&entry)).then_some(entry)
}

/// Whether the entry was signed by its author.
pub fn verify_entry(e: &Entry) -> bool {
    let Ok(key) = PublicKey::from_str(&e.a) else {
        return false;
    };
    let mut sig = [0u8; 64];
    if hex::decode_to_slice(&e.signed.sig, &mut sig).is_err() {
        return false;
    }
    key.verify(e.signed.body.as_bytes(), &Signature::from_bytes(&sig))
        .is_ok()
}

#[derive(Clone, Serialize)]
pub struct Message {
    pub id: String,
    pub author: String,
    pub text: String,
    pub ts: u64,
    pub seq: u64,
}

#[derive(Clone, Serialize)]
pub struct SharedFile {
    /// "author/entry id": entry ids are chosen by authors, so two authors
    /// can never claim the same file.
    pub id: String,
    /// The id in the author's entries (what `FileDel` names).
    #[serde(skip)]
    pub entry_id: String,
    pub name: String,
    /// See `Op::FileAdd::dir`, cleaned: "/"-separated safe folder names.
    pub dir: Option<String>,
    pub size: u64,
    pub hash: String,
    pub author: String,
    pub ts: u64,
}

/// A pinned message or shared file.
#[derive(Clone)]
pub struct Pinned {
    /// The message's or the file's id ("author/id").
    pub target: String,
    pub by: String,
    pub ts: u64,
}

/// What a message mentions of us: nothing, everyone (@tous, @all), or us.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Mention {
    None,
    All,
    Me,
}

/// Whether `text` mentions one of `handles` ("@handle", any case, not
/// followed by a letter or digit), or everyone.
pub fn mention_of(text: &str, handles: &[String]) -> Mention {
    let lower = text.to_lowercase();
    let handles: Vec<String> = handles
        .iter()
        .map(|h| h.trim().to_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    let mut found = Mention::None;
    for (i, _) in lower.match_indices('@') {
        // Not inside a word (an e-mail address).
        if lower[..i].chars().next_back().is_some_and(char::is_alphanumeric) {
            continue;
        }
        let rest = &lower[i + 1..];
        let ends = |h: &str| {
            rest.starts_with(h)
                && rest[h.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        };
        if handles.iter().any(|h| ends(h)) {
            return Mention::Me;
        }
        if ends("tous") || ends("all") {
            found = Mention::All;
        }
    }
    found
}

/// A shared file's folder made safe: names cleaned one by one, no "." or
/// "..", "/" between them.
pub fn clean_dir(dir: &str) -> Option<String> {
    let parts: Vec<String> = dir
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .take(32)
        .map(crate::p2p::sanitize_file_name)
        .collect();
    let joined: String = parts.join("/").chars().take(MAX_DIR).collect();
    let joined = joined.trim_end_matches('/').to_string();
    (!joined.is_empty()).then_some(joined)
}

/// The last removal of a member.
#[derive(Clone)]
pub struct Removal {
    pub by: String,
    pub seq: u64,
    /// Removed by someone else, in an entry with `d`: see `View::can_add`.
    sticky: bool,
}

/// The group's state, rebuilt from the log.
#[derive(Default)]
pub struct View {
    pub name: String,
    pub creator: String,
    pub members: BTreeMap<String, u64>,
    pub nicks: HashMap<String, String>,
    pub messages: Vec<Message>,
    pub files: BTreeMap<String, SharedFile>,
    /// Pinned items, oldest pin first (targets may not exist, or no longer).
    pub pins: Vec<Pinned>,
    /// (author, seq) of the entry that last added each member.
    pub added_by: HashMap<String, (String, u64)>,
    pub removed: HashMap<String, Removal>,
    /// Everyone who was a member at some point.
    pub ever: HashSet<String>,
    pub max_t: u64,
    /// Removed members with entries that did not count (yet): an entry
    /// vouching for them must replay everything.
    skipped: HashSet<String>,
}

impl View {
    pub fn is_member(&self, id: &str) -> bool {
        self.members.contains_key(id)
    }

    /// Whether `by` may remove `member`: themselves, or as the group's
    /// creator, or as whoever added them.
    pub fn can_remove(&self, by: &str, member: &str) -> bool {
        by == member
            || by == self.creator
            || self.added_by.get(member).is_some_and(|(a, _)| a == by)
    }

    /// Whether `by` may add `member`: anyone, unless another member removed
    /// them; then only that member or the creator, so a removal sticks.
    pub fn can_add(&self, by: &str, member: &str) -> bool {
        match self.removed.get(member) {
            Some(r) if r.sticky => by == r.by || by == self.creator,
            _ => true,
        }
    }
}

/// Proof that an author signed two different entries numbered `n`.
#[derive(Clone)]
pub struct Fork {
    pub n: u64,
    pub pair: [SignedEntry; 2],
}

/// What `insert` did with an entry.
#[derive(Debug, PartialEq)]
pub enum Insert {
    New,
    /// A second version of a kept entry, at the lowest seq known so far.
    Fork,
    Known,
    Refused,
}

/// What `insert_all` kept.
#[derive(Default)]
pub struct Inserted {
    /// Entries to save and pass on.
    pub kept: Vec<SignedEntry>,
    /// Authors with a new (lower) fork: see `Log::fork_evidence`.
    pub forked: Vec<String>,
}

type Key = (u64, String, u64);

fn key(e: &Entry) -> Key {
    (e.t, e.a.clone(), e.n)
}

/// `Entry::hash` as bytes.
fn hash_bytes(hash: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let _ = hex::decode_to_slice(hash, &mut out);
    out
}

/// Where the signed text of a kept entry is.
enum Text {
    /// Not found in the log file (yet): just received, or no file.
    Mem(Box<SignedEntry>),
    /// A line of the log file: its offset and length.
    Disk(u64, u32),
}

/// What stays in memory of a kept entry: what replaying it and deciding
/// about others need. Its signed text is read back from the log file when
/// a peer needs it (`Log::texts`).
struct Kept {
    t: u64,
    hash: [u8; 16],
    /// Length of the signed body, for the quotas.
    len: u32,
    /// Written with `d` (see `Removal::sticky`).
    has_d: bool,
    op: Option<Op>,
    text: Text,
}

/// Read entries in more than this from a log at once: the whole file.
const WHOLE_FILE_READS: usize = 64;

/// Reads lines of a log file back.
struct Reader {
    file: Option<std::fs::File>,
    whole: Option<Vec<u8>>,
}

impl Reader {
    fn new(path: Option<&std::path::Path>, lines: usize) -> Self {
        let Some(path) = path.filter(|_| lines > 0) else {
            return Self { file: None, whole: None };
        };
        if lines > WHOLE_FILE_READS {
            return Self {
                file: None,
                whole: std::fs::read(path).ok(),
            };
        }
        Self {
            file: std::fs::File::open(path).ok(),
            whole: None,
        }
    }

    /// The entry at `at`, if it is still the one hashed `hash`.
    fn read(&mut self, at: u64, len: u32, hash: &[u8; 16]) -> Option<SignedEntry> {
        use std::io::{Read, Seek, SeekFrom};
        let line = match (&self.whole, &mut self.file) {
            (Some(whole), _) => {
                let at = usize::try_from(at).ok()?;
                whole.get(at..at.checked_add(len as usize)?)?.to_vec()
            }
            (None, Some(file)) => {
                let mut line = vec![0u8; len as usize];
                file.seek(SeekFrom::Start(at)).ok()?;
                file.read_exact(&mut line).ok()?;
                line
            }
            (None, None) => return None,
        };
        let signed = read_line(&line)?;
        let same = blake3::hash(signed.body.as_bytes()).as_bytes()[..16] == hash[..];
        if !same {
            eprintln!("group log changed under us: an entry could not be read back");
        }
        same.then_some(signed)
    }
}

/// One line of a log file (`vault::seal_line`, or plain from before).
fn read_line(line: &[u8]) -> Option<SignedEntry> {
    let json = crate::vault::open_line(std::str::from_utf8(line).ok()?).ok()?;
    serde_json::from_slice(&json).ok()
}

#[derive(Default)]
pub struct Log {
    pub id: String,
    /// Each author's entries, seq 1 first (kept without gaps).
    entries: HashMap<String, Vec<Kept>>,
    bytes: HashMap<String, usize>,
    /// For each author M and each other author A: (seq of M, last seq of A
    /// that M had by then), one step each time it grew.
    seen: HashMap<String, HashMap<String, Vec<(u64, u64)>>>,
    forks: HashMap<String, Fork>,
    pub view: View,
    /// For groups made before ids were tied to their creator: the creator
    /// seen when we first had the log, so a later forged creation entry
    /// cannot take the group over.
    pub pinned_creator: Option<String>,
    /// The view replays every entry up to `applied`; entries inserted since
    /// (`pending`) all come after it, so they can be replayed on top.
    current: bool,
    applied: Option<Key>,
    pending: Vec<Key>,
    creator: Option<String>,
    /// The log file the texts of entries are read back from (none: they
    /// stay in memory), and how far it was looked through (`index_new`).
    file: Option<std::path::PathBuf>,
    scanned: u64,
}

impl Log {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            ..Default::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn kept(&self, author: &str, n: u64) -> Option<&Kept> {
        let i = usize::try_from(n.checked_sub(1)?).ok()?;
        self.entries.get(author)?.get(i)
    }

    /// The signed texts of `items`, in order; any that cannot be read back
    /// from the file (changed under us) are left out.
    fn texts<'a>(&self, items: impl IntoIterator<Item = &'a Kept>) -> Vec<SignedEntry> {
        let items: Vec<&Kept> = items.into_iter().collect();
        let on_disk = items
            .iter()
            .filter(|k| matches!(k.text, Text::Disk(..)))
            .count();
        let mut reader = Reader::new(self.file.as_deref(), on_disk);
        items
            .into_iter()
            .filter_map(|k| match &k.text {
                Text::Mem(s) => Some((**s).clone()),
                Text::Disk(at, len) => reader.read(*at, *len, &k.hash),
            })
            .collect()
    }

    fn text_of(&self, k: &Kept) -> Option<SignedEntry> {
        self.texts([k]).pop()
    }

    /// Read the texts of entries back from `path`, our log file, from now
    /// on; what is in it up to `scanned` was located already (`locate`).
    pub fn attach(&mut self, path: std::path::PathBuf, scanned: u64) {
        self.file = Some(path);
        self.scanned = scanned;
        for list in self.entries.values_mut() {
            list.shrink_to_fit();
        }
        self.view.messages.shrink_to_fit();
    }

    /// Entry `n` of `author` (hashed `hash`) is the line at `at`, `len`
    /// long, of the log file: its text no longer stays in memory.
    pub fn locate(&mut self, author: &str, n: u64, hash: &str, at: u64, len: u32) {
        let Some(i) = n.checked_sub(1).and_then(|i| usize::try_from(i).ok()) else {
            return;
        };
        if let Some(k) = self.entries.get_mut(author).and_then(|l| l.get_mut(i)) {
            if matches!(k.text, Text::Mem(_)) && k.hash == hash_bytes(hash) {
                k.text = Text::Disk(at, len);
            }
        }
    }

    /// Whether texts of entries are held in memory, waiting to be found in
    /// the log file (`index_new`).
    pub fn has_unlocated(&self) -> bool {
        self.file.is_some()
            && self
                .entries
                .values()
                .flatten()
                .any(|k| matches!(k.text, Text::Mem(_)))
    }

    /// Look through what was appended to the log file since last time for
    /// the entries whose texts are in memory, and let those go.
    pub fn index_new(&mut self) {
        use std::io::{Read, Seek, SeekFrom};
        if !self.has_unlocated() {
            return;
        }
        let Some(path) = self.file.clone() else {
            return;
        };
        let Ok(mut file) = std::fs::File::open(&path) else {
            return;
        };
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        if size < self.scanned {
            // Written anew.
            self.scanned = 0;
        }
        let mut data = Vec::new();
        if file.seek(SeekFrom::Start(self.scanned)).is_err() || file.read_to_end(&mut data).is_err() {
            return;
        }
        // Only whole lines: one may be being appended.
        let Some(end) = data.iter().rposition(|b| *b == b'\n') else {
            return;
        };
        let mut at = self.scanned;
        for line in data[..end].split(|b| *b == b'\n') {
            if let Some(s) = read_line(line) {
                if let Ok(h) = serde_json::from_str::<Header>(&s.body) {
                    let hash = entry_hash(&s.body);
                    self.locate(&h.a, h.n, &hash, at, line.len() as u32);
                }
            }
            at += line.len() as u64 + 1;
        }
        self.scanned += end as u64 + 1;
    }

    /// Before the log file is written anew: every text back in memory (the
    /// offsets will change). `reindex` once written.
    pub fn hold_texts(&mut self) {
        let on_disk = self
            .entries
            .values()
            .flatten()
            .filter(|k| matches!(k.text, Text::Disk(..)))
            .count();
        let mut reader = Reader::new(self.file.as_deref(), on_disk);
        for k in self.entries.values_mut().flatten() {
            if let Text::Disk(at, len) = k.text {
                if let Some(s) = reader.read(at, len, &k.hash) {
                    k.text = Text::Mem(Box::new(s));
                }
            }
        }
    }

    /// The log file was written anew (after `hold_texts`).
    pub fn reindex(&mut self) {
        self.scanned = 0;
        self.index_new();
    }

    /// Add an entry whose signature was checked; returns `Known` if it was
    /// already there, `Refused` if it cannot be kept: an author's entries are
    /// kept in seq order without gaps, each with a later timestamp than the
    /// one before, within the author's quotas.
    pub fn insert(&mut self, entry: Entry) -> Insert {
        let hash = hash_bytes(&entry.hash);
        if let Some(known) = self.kept(&entry.a, entry.n) {
            if known.hash == hash {
                return Insert::Known;
            }
            let Some(ours) = self.text_of(known) else {
                return Insert::Known;
            };
            let pair = [ours, entry.signed.clone()];
            return if self.note_fork(&entry.a, entry.n, pair) {
                Insert::Fork
            } else {
                Insert::Known
            };
        }
        let last = self
            .entries
            .get(&entry.a)
            .and_then(|l| Some((l.len() as u64, l.last()?.t)));
        let fits = match last {
            None => entry.n == 1,
            Some((n, t)) => Some(entry.n) == n.checked_add(1) && entry.t > t,
        };
        let bytes = self.bytes.get(&entry.a).copied().unwrap_or(0) + entry.signed.body.len();
        if !fits || entry.n > MAX_ENTRIES_PER_AUTHOR || bytes > MAX_BYTES_PER_AUTHOR {
            return Insert::Refused;
        }
        self.bytes.insert(entry.a.clone(), bytes);
        let steps = self.seen.entry(entry.a.clone()).or_default();
        for (other, seq) in entry.d.iter().flatten() {
            let list = steps.entry(other.clone()).or_default();
            if *other != entry.a && list.last().is_none_or(|(_, s)| seq > s) {
                list.push((entry.n, *seq));
            }
        }
        // Replaying it on top of the view gives what a rebuild would, unless
        // it sorts before what was replayed, may change who the creator is,
        // or vouches for entries that did not count.
        let k = key(&entry);
        let creation = entry.n == 1 && matches!(entry.op, Some(Op::Create { .. }));
        let vouches = entry
            .d
            .iter()
            .flatten()
            .any(|(a, _)| self.view.skipped.contains(a));
        if creation || vouches || self.applied.as_ref().is_some_and(|a| k <= *a) {
            self.current = false;
        } else {
            self.pending.push(k);
        }
        let kept = Kept {
            t: entry.t,
            hash,
            len: entry.signed.body.len() as u32,
            has_d: entry.d.is_some(),
            op: entry.op,
            text: Text::Mem(Box::new(entry.signed)),
        };
        self.entries.entry(entry.a).or_default().push(kept);
        Insert::New
    }

    /// Keep `pair` as proof that `author` signed two entries numbered `n`
    /// if no lower one is known; returns whether it was kept.
    fn note_fork(&mut self, author: &str, n: u64, pair: [SignedEntry; 2]) -> bool {
        if self.forks.get(author).is_some_and(|f| f.n <= n) {
            return false;
        }
        self.forks.insert(author.to_string(), Fork { n, pair });
        self.current = false;
        true
    }

    /// Insert entries in any order. With `verify`, signatures are checked,
    /// but only for entries that can be kept: what a peer sends again, or
    /// cannot fit, costs nothing.
    pub fn insert_all(&mut self, entries: Vec<Entry>, verify: bool) -> Inserted {
        self.insert_batch(entries, verify, true)
    }

    /// `insert_all` for entries read from our log file: nothing to pass on.
    pub fn insert_loaded(&mut self, entries: Vec<Entry>) {
        self.insert_batch(entries, false, false);
    }

    fn insert_batch(&mut self, mut entries: Vec<Entry>, verify: bool, report: bool) -> Inserted {
        entries.sort_by(|x, y| (&x.a, x.n, &x.signed.body).cmp(&(&y.a, y.n, &y.signed.body)));
        entries.dedup_by(|x, y| x.a == y.a && x.n == y.n && x.signed.body == y.signed.body);
        let twins: Vec<bool> = (0..entries.len())
            .map(|i| {
                let e = &entries[i];
                [i.checked_sub(1), Some(i + 1)]
                    .into_iter()
                    .flatten()
                    .filter_map(|j| entries.get(j))
                    .any(|o| o.a == e.a && o.n == e.n)
            })
            .collect();
        let mut out = Inserted::default();
        // Two versions of an entry we cannot place yet are proof all the same.
        let mut loose: Vec<Entry> = Vec::new();
        for (e, twin) in entries.into_iter().zip(twins) {
            let stored = self.kept(&e.a, e.n);
            let next = self.last_seq_of(&e.a).checked_add(1) == Some(e.n);
            let worth = match stored {
                Some(s) => s.hash != hash_bytes(&e.hash),
                None => next || twin,
            };
            if !worth || (verify && !verify_entry(&e)) {
                continue;
            }
            if stored.is_none() && !next {
                loose.push(e);
                continue;
            }
            let a = e.a.clone();
            let signed = report.then(|| e.signed.clone());
            match self.insert(e) {
                Insert::New => out.kept.extend(signed),
                Insert::Fork => {
                    out.kept.extend(signed);
                    out.forked.push(a);
                }
                Insert::Known | Insert::Refused => {}
            }
        }
        for pair in loose.windows(2) {
            let [x, y] = pair else { continue };
            if x.a == y.a && x.n == y.n && self.note_fork(&x.a, x.n, [x.signed.clone(), y.signed.clone()]) {
                out.kept.extend([x.signed.clone(), y.signed.clone()]);
                out.forked.push(x.a.clone());
            }
        }
        out
    }

    /// After a rebuild, forget the entries of authors who were never
    /// members. Entries that arrived before the one adding their author come
    /// back on the next sync.
    pub fn drop_strangers(&mut self) {
        let ever = &self.view.ever;
        self.entries.retain(|a, _| ever.contains(a));
        self.bytes.retain(|a, _| ever.contains(a));
        self.seen.retain(|a, _| ever.contains(a));
        self.forks.retain(|a, _| ever.contains(a));
    }

    pub fn have(&self) -> Have {
        self.entries
            .iter()
            .map(|(author, list)| (author.clone(), list.len() as u64))
            .collect()
    }

    /// Hash of each author's last entry, for `conflicts_with`.
    pub fn heads(&self) -> HashMap<String, String> {
        self.entries
            .iter()
            .filter_map(|(a, l)| Some((a.clone(), hex::encode(l.last()?.hash))))
            .collect()
    }

    /// Our version of entries a peer has another version of (it reported
    /// `have` with `heads`): once it has both, it knows of the fork.
    pub fn conflicts_with(&self, have: &Have, heads: &HashMap<String, String>) -> Vec<SignedEntry> {
        let differ: Vec<&Kept> = heads
            .iter()
            .filter_map(|(a, hash)| {
                let k = self.kept(a, *have.get(a)?)?;
                (hex::encode(k.hash) != *hash).then_some(k)
            })
            .collect();
        self.texts(differ)
    }

    /// Entries a peer with `have` is missing.
    pub fn missing_for(&self, have: &Have) -> Vec<SignedEntry> {
        let mut out = Vec::new();
        for (author, list) in &self.entries {
            let known = have.get(author).copied().unwrap_or(0);
            let from = usize::try_from(known).unwrap_or(usize::MAX).min(list.len());
            out.extend(&list[from..]);
        }
        self.texts(out)
    }

    /// Whether `have` is missing anything we have.
    pub fn is_behind(&self, have: &Have) -> bool {
        self.entries
            .iter()
            .any(|(author, list)| list.len() as u64 > have.get(author).copied().unwrap_or(0))
    }

    /// The seq from which `author`'s entries are ignored, if they forked.
    pub fn fork_of(&self, author: &str) -> Option<u64> {
        self.forks.get(author).map(|f| f.n)
    }

    /// Lowest fork known per author.
    pub fn forks(&self) -> HashMap<String, u64> {
        self.forks.iter().map(|(a, f)| (a.clone(), f.n)).collect()
    }

    /// What to send members once a fork of `author` is known: both versions,
    /// and our entries just before, so that whoever holds other versions of
    /// those finds the seq where the two versions really start.
    pub fn fork_evidence(&self, author: &str) -> Vec<SignedEntry> {
        let Some(f) = self.forks.get(author) else {
            return Vec::new();
        };
        let mut out = f.pair.to_vec();
        if let Some(list) = self.entries.get(author) {
            let from = f.n.saturating_sub(16).max(1);
            let end = (f.n - 1).min(list.len() as u64);
            if from <= end {
                out.extend(self.texts(&list[from as usize - 1..end as usize]));
            }
        }
        out
    }

    /// Fork proofs a peer reporting `forks` lacks.
    pub fn evidence_for(&self, forks: &HashMap<String, u64>) -> Vec<SignedEntry> {
        self.forks
            .iter()
            .filter(|(a, f)| forks.get(*a).is_none_or(|n| *n > f.n))
            .flat_map(|(_, f)| f.pair.clone())
            .collect()
    }

    /// Every entry and fork proof, to write the log anew.
    pub fn all_signed(&self) -> Vec<SignedEntry> {
        let mut out = self.texts(self.entries.values().flatten());
        for f in self.forks.values() {
            for s in &f.pair {
                if !out.contains(s) {
                    out.push(s.clone());
                }
            }
        }
        out
    }

    /// Remove `author`'s entries from seq `from` on, and return them.
    pub fn truncate(&mut self, author: &str, from: u64) -> Vec<Entry> {
        let Some(list) = self.entries.get_mut(author) else {
            return Vec::new();
        };
        let at = usize::try_from(from.saturating_sub(1)).unwrap_or(usize::MAX).min(list.len());
        let cut_off: Vec<Kept> = list.split_off(at);
        let bytes = list.iter().map(|k| k.len as usize).sum();
        if list.is_empty() {
            self.entries.remove(author);
        }
        self.bytes.insert(author.to_string(), bytes);
        if let Some(steps) = self.seen.get_mut(author) {
            for list in steps.values_mut() {
                list.retain(|(n, _)| *n < from);
            }
        }
        self.current = false;
        self.texts(&cut_off)
            .into_iter()
            .filter_map(|s| parse(s, &self.id, false))
            .collect()
    }

    /// The last seq of `a` that `m` had when writing its entry `k`.
    fn seen_at(&self, m: &str, k: u64, a: &str) -> u64 {
        let Some(steps) = self.seen.get(m).and_then(|s| s.get(a)) else {
            return 0;
        };
        let i = steps.partition_point(|(n, _)| *n <= k);
        i.checked_sub(1).map_or(0, |i| steps[i].1)
    }

    /// Whether an entry of a removed member still counts: another member had
    /// it before learning of the removal, so it was part of the group by
    /// then, and a removal written late, or dated in the past on purpose,
    /// cannot erase it. Only entries with `d` vouch for others.
    fn acknowledged(&self, v: &View, a: &str, n: u64) -> bool {
        let Some(r) = v.removed.get(a) else {
            return false;
        };
        v.members.keys().filter(|m| *m != a).any(|m| {
            let top = self.last_seq_of(m);
            // m's last entry written without the removal.
            let mut before = if *m == r.by {
                r.seq - 1
            } else {
                let steps = self.seen.get(m).and_then(|s| s.get(&r.by));
                let steps = steps.map(Vec::as_slice).unwrap_or_default();
                let i = steps.partition_point(|(_, s)| *s < r.seq);
                steps.get(i).map_or(top, |(k, _)| k - 1)
            };
            if let Some(f) = self.forks.get(m) {
                before = before.min(f.n - 1);
            }
            self.seen_at(m, before.min(top), a) >= n
        })
    }

    /// Next seq for `author` and a timestamp after everything seen, so a new
    /// entry always sorts after the entries its author already knew about.
    pub fn next_position(&self, author: &str, now: u64) -> (u64, u64) {
        let n = self.last_seq_of(author) + 1;
        let last_t = self
            .entries
            .get(author)
            .and_then(|l| l.last())
            .map(|k| k.t)
            .unwrap_or(0);
        let t = now
            .max(self.view.max_t.saturating_add(1))
            .max(last_t.saturating_add(1));
        (n, t)
    }

    /// `next_position`, and what `author` has beyond what its previous entry
    /// said (`Body::d`).
    pub fn draft(&self, author: &str, now: u64) -> (u64, u64, BTreeMap<String, u64>) {
        let (n, t) = self.next_position(author, now);
        let d = self
            .entries
            .iter()
            .filter(|(a, _)| *a != author)
            .filter_map(|(a, l)| {
                let top = l.len() as u64;
                (top > 0 && top > self.seen_at(author, n - 1, a)).then(|| (a.clone(), top))
            })
            .take(MAX_DEPS)
            .collect();
        (n, t, d)
    }

    /// The author of the entry that created the group: the one matching the
    /// group id; for older groups, the pinned creator, or else the earliest.
    fn find_creator(&self) -> Option<String> {
        let creations: Vec<(&String, u64, Option<&String>)> = self
            .entries
            .iter()
            .filter_map(|(a, l)| Some((a, l.first()?)))
            .filter_map(|(a, k)| match &k.op {
                Some(Op::Create { nonce, .. }) => Some((a, k.t, nonce.as_ref())),
                _ => None,
            })
            .collect();
        let bound = creations
            .iter()
            .find(|(a, _, nonce)| nonce.is_some_and(|n| group_id(a, n) == self.id));
        if let Some((a, _, _)) = bound {
            return Some((*a).clone());
        }
        let mut legacy = creations
            .iter()
            .filter(|(_, _, nonce)| nonce.is_none())
            .map(|(a, t, _)| (*a, *t));
        match &self.pinned_creator {
            Some(p) => legacy.find(|(a, _)| a.as_str() == p.as_str()).map(|(a, _)| a.clone()),
            None => legacy
                .min_by(|x, y| (x.1, x.0).cmp(&(y.1, y.0)))
                .map(|(a, _)| a.clone()),
        }
    }

    /// Replay every entry.
    pub fn rebuild(&mut self) {
        let creator = self.find_creator();
        let mut ordered: Vec<(u64, &String, u64, &Kept)> = self
            .entries
            .iter()
            .flat_map(|(a, l)| l.iter().zip(1..).map(move |(k, n)| (k.t, a, n, k)))
            .collect();
        ordered.sort_by(|x, y| (x.0, x.1, x.2).cmp(&(y.0, y.1, y.2)));
        let mut v = View::default();
        for (_, a, n, k) in &ordered {
            self.apply(&mut v, a, *n, k, creator.as_deref());
        }
        self.applied = ordered.last().map(|(t, a, n, _)| (*t, (*a).clone(), *n));
        self.view = v;
        self.creator = creator;
        self.pending.clear();
        self.current = true;
    }

    /// Bring the view up to date: replay only the new entries when they all
    /// sort after those already replayed, else everything.
    pub fn refresh(&mut self) {
        if !self.current {
            return self.rebuild();
        }
        let mut pending = std::mem::take(&mut self.pending);
        pending.sort();
        let mut v = std::mem::take(&mut self.view);
        for (_, a, n) in &pending {
            if let Some(k) = self.kept(a, *n) {
                self.apply(&mut v, a, *n, k, self.creator.as_deref());
            }
        }
        self.view = v;
        if let Some(last) = pending.pop() {
            self.applied = Some(last);
        }
    }

    /// Replay entry `n` of `a`.
    fn apply(&self, v: &mut View, a: &str, n: u64, e: &Kept, creator: Option<&str>) {
        let Some(op) = &e.op else { return };
        if self.forks.get(a).is_some_and(|f| n >= f.n) {
            // Two versions of this entry exist: its author is out from here.
            v.members.remove(a);
            return;
        }
        if let Op::Create { name, .. } = op {
            if n == 1 && creator == Some(a) {
                v.creator = a.to_string();
                v.name = clip(name, MAX_NAME);
                v.members.insert(a.to_string(), e.t);
                v.ever.insert(a.to_string());
                v.max_t = v.max_t.max(e.t);
            }
            return;
        }
        if !v.is_member(a) && !self.acknowledged(v, a, n) {
            if v.removed.contains_key(a) {
                v.skipped.insert(a.to_string());
            }
            return;
        }
        v.max_t = v.max_t.max(e.t);
        match op {
            Op::Create { .. } => {}
            Op::Add { member } => {
                let valid = crate::p2p::is_valid_id(member);
                if valid
                    && !v.is_member(member)
                    && v.members.len() < MAX_MEMBERS_REPLAY
                    && v.can_add(a, member)
                {
                    v.members.insert(member.clone(), e.t);
                    v.added_by.insert(member.clone(), (a.to_string(), n));
                    v.removed.remove(member);
                    v.ever.insert(member.clone());
                }
            }
            Op::Remove { member } => {
                if v.is_member(member) && v.can_remove(a, member) {
                    v.members.remove(member);
                    let removal = Removal {
                        by: a.to_string(),
                        seq: n,
                        sticky: a != member && e.has_d,
                    };
                    v.removed.insert(member.clone(), removal);
                }
            }
            Op::Rename { name } => {
                let name = clip(name, MAX_NAME);
                if !name.is_empty() {
                    v.name = name;
                }
            }
            Op::Profile { nick } => {
                v.nicks.insert(a.to_string(), clip(nick, MAX_NICK));
            }
            Op::Msg { id, text } => {
                if !text.trim().is_empty() {
                    v.messages.push(Message {
                        id: format!("{a}/{id}"),
                        author: a.to_string(),
                        text: text.chars().take(MAX_TEXT).collect(),
                        ts: e.t,
                        seq: n,
                    });
                }
            }
            Op::FileAdd {
                id,
                name,
                size,
                hash,
                dir,
            } => {
                let well_formed = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
                let key = format!("{a}/{id}");
                if well_formed && !v.files.contains_key(&key) {
                    v.files.insert(
                        key.clone(),
                        SharedFile {
                            id: key,
                            entry_id: id.clone(),
                            name: crate::p2p::sanitize_file_name(name),
                            dir: dir.as_deref().and_then(clean_dir),
                            size: *size,
                            hash: hash.to_lowercase(),
                            author: a.to_string(),
                            ts: e.t,
                        },
                    );
                }
            }
            Op::FileDel { id } => {
                let key = format!("{a}/{id}");
                if v.files.remove(&key).is_some() {
                    v.pins.retain(|p| p.target != key);
                }
            }
            Op::Pin { target } => {
                let known = v.pins.iter().any(|p| p.target == *target);
                if !known && target.len() <= MAX_TARGET && v.pins.len() < MAX_PINS {
                    v.pins.push(Pinned {
                        target: target.clone(),
                        by: a.to_string(),
                        ts: e.t,
                    });
                }
            }
            Op::Unpin { target } => {
                v.pins.retain(|p| p.target != *target);
            }
        }
    }

    pub fn has_entry_of(&self, author: &str) -> bool {
        self.entries.contains_key(author)
    }

    pub fn last_seq_of(&self, author: &str) -> u64 {
        self.entries.get(author).map_or(0, |l| l.len() as u64)
    }

    /// What we have of entry `n` of `author`, if any.
    pub fn get(&self, author: &str, n: u64) -> Option<Stored> {
        self.kept(author, n).map(|k| Stored {
            t: k.t,
            hash: hex::encode(k.hash),
        })
    }
}

/// An entry we have, as `Log::get` gives it.
pub struct Stored {
    pub t: u64,
    /// See `Entry::hash`.
    pub hash: String,
}

fn clip(s: &str, max: usize) -> String {
    crate::p2p::strip_invisible(s).trim().chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Member {
        key: SecretKey,
        id: String,
    }

    fn member() -> Member {
        let key = SecretKey::generate();
        let id = key.public().to_string();
        Member { key, id }
    }

    fn signed(log: &Log, m: &Member, n: u64, t: u64, d: Option<BTreeMap<String, u64>>, op: Op) -> Entry {
        let body = Body {
            g: log.id.clone(),
            a: m.id.clone(),
            n,
            t,
            d,
            op,
        };
        parse(sign(&m.key, &body), &log.id, true).unwrap()
    }

    fn entry(log: &Log, m: &Member, n: u64, t: u64, op: Op) -> Entry {
        signed(log, m, n, t, None, op)
    }

    /// An entry as this version writes it (with what its author has).
    fn post(log: &mut Log, m: &Member, t: u64, op: Op) -> SignedEntry {
        let (n, _, d) = log.draft(&m.id, 0);
        let e = signed(log, m, n, t, Some(d), op);
        let s = e.signed.clone();
        log.insert(e);
        log.refresh();
        s
    }

    /// An entry as 3.3 wrote it (no context).
    fn post_legacy(log: &mut Log, m: &Member, t: u64, op: Op) -> SignedEntry {
        let n = log.last_seq_of(&m.id) + 1;
        let e = entry(log, m, n, t, op);
        let s = e.signed.clone();
        log.insert(e);
        log.refresh();
        s
    }

    /// An entry that hides what its author has (backdating on purpose).
    fn post_blind(log: &mut Log, m: &Member, t: u64, op: Op) -> SignedEntry {
        let n = log.last_seq_of(&m.id) + 1;
        let e = signed(log, m, n, t, Some(BTreeMap::new()), op);
        let s = e.signed.clone();
        assert_eq!(log.insert(e), Insert::New);
        log.refresh();
        s
    }

    fn msg(text: &str) -> Op {
        Op::Msg {
            id: text.into(),
            text: text.into(),
        }
    }

    fn create(name: &str) -> Op {
        Op::Create {
            name: name.into(),
            nonce: None,
        }
    }

    fn add(m: &Member) -> Op {
        Op::Add {
            member: m.id.clone(),
        }
    }

    fn remove(m: &Member) -> Op {
        Op::Remove {
            member: m.id.clone(),
        }
    }

    fn texts(log: &Log) -> Vec<String> {
        log.view.messages.iter().map(|m| m.text.clone()).collect()
    }

    fn sync(from: &Log, to: &mut Log) {
        let entries = from
            .missing_for(&to.have())
            .into_iter()
            .map(|e| parse(e, &from.id, false).unwrap())
            .collect();
        to.insert_all(entries, true);
        to.refresh();
    }

    #[test]
    fn signatures_are_checked() {
        let a = member();
        let mut log = Log::new("g");
        let signed = post(&mut log, &a, 1, create("G"));
        let mut forged = signed.clone();
        forged.body = forged.body.replace("\"G\"", "\"H\"");
        assert!(parse(forged.clone(), "g", true).is_none());
        let unchecked = parse(forged, "g", false).unwrap();
        let mut other = Log::new("g");
        assert!(other.insert_all(vec![unchecked], true).kept.is_empty());
        assert!(parse(signed.clone(), "other", true).is_none());
        assert!(parse(signed, "g", true).is_some());
    }

    #[test]
    fn membership_and_messages() {
        let (a, b, c) = (member(), member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("Amis"));
        post(&mut log, &a, 2, add(&b));
        // c is not a member: ignored.
        post(&mut log, &c, 3, msg("intrus"));
        post(&mut log, &b, 4, msg("salut"));
        assert_eq!(log.view.name, "Amis");
        assert!(log.view.is_member(&b.id) && !log.view.is_member(&c.id));
        assert_eq!(texts(&log), ["salut"]);
        log.drop_strangers();
        assert!(!log.has_entry_of(&c.id));
    }

    #[test]
    fn sync_between_logs_converges() {
        let (a, b) = (member(), member());
        let mut la = Log::new("g");
        post(&mut la, &a, 1, create("G"));
        post(&mut la, &a, 2, add(&b));
        let mut lb = Log::new("g");
        sync(&la, &mut lb);
        post(&mut lb, &b, 3, msg("hello"));
        assert!(lb.is_behind(&la.have()));
        sync(&lb, &mut la);
        assert!(!lb.is_behind(&la.have()) && !la.is_behind(&lb.have()));
        assert_eq!(la.view.messages.len(), 1);
        assert_eq!(la.have(), lb.have());
    }

    #[test]
    fn concurrent_removals_resolve_the_same_way() {
        let (a, b, c) = (member(), member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        post(&mut log, &a, 2, add(&b));
        post(&mut log, &b, 3, add(&c));
        // a removes b while b removes c: the earlier one wins.
        post_blind(&mut log, &a, 11, remove(&b));
        post_blind(&mut log, &b, 10, remove(&c));
        assert!(!log.view.is_member(&b.id));
        assert!(!log.view.is_member(&c.id));
    }

    #[test]
    fn only_the_creator_or_the_adder_removes() {
        let (a, b, c, d) = (member(), member(), member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        post(&mut log, &a, 2, add(&b));
        post(&mut log, &a, 3, add(&c));
        post(&mut log, &b, 4, add(&d));
        post(&mut log, &c, 5, remove(&b));
        post(&mut log, &c, 6, remove(&a));
        assert!(log.view.is_member(&a.id) && log.view.is_member(&b.id));
        post(&mut log, &b, 7, remove(&d));
        assert!(!log.view.is_member(&d.id));
        post(&mut log, &c, 8, remove(&c));
        assert!(!log.view.is_member(&c.id));
    }

    #[test]
    fn a_forged_creation_does_not_take_the_group_over() {
        let (a, m) = (member(), member());
        let nonce = "abcd".to_string();
        let gid = group_id(&a.id, &nonce);
        let mut log = Log::new(&gid);
        post(
            &mut log,
            &a,
            100,
            Op::Create {
                name: "G".into(),
                nonce: Some(nonce),
            },
        );
        post(&mut log, &a, 101, add(&m));
        // m's first entry: a creation dated before the real one.
        post(&mut log, &m, 1, create("Mine"));
        assert_eq!(log.view.creator, a.id);
        assert_eq!(log.view.name, "G");
        assert!(log.view.is_member(&a.id) && log.view.is_member(&m.id));
    }

    #[test]
    fn a_pinned_creator_protects_older_groups() {
        let (a, m) = (member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 100, create("G"));
        post(&mut log, &a, 101, add(&m));
        log.pinned_creator = Some(a.id.clone());
        post(&mut log, &m, 1, create("Mine"));
        assert_eq!(log.view.creator, a.id);
        assert!(log.view.is_member(&a.id));
    }

    #[test]
    fn entries_need_order_and_later_timestamps() {
        let a = member();
        let mut log = Log::new("g");
        assert_eq!(log.insert(entry(&log, &a, 2, 5, msg("gap"))), Insert::Refused);
        assert_eq!(log.insert(entry(&log, &a, 1, 5, create("G"))), Insert::New);
        assert_eq!(log.insert(entry(&log, &a, 3, 6, msg("gap"))), Insert::Refused);
        assert_eq!(log.insert(entry(&log, &a, 2, 5, msg("same time"))), Insert::Refused);
        assert_eq!(log.insert(entry(&log, &a, 2, 1, msg("backdated"))), Insert::Refused);
        assert_eq!(log.insert(entry(&log, &a, 2, 6, msg("ok"))), Insert::New);
        assert_eq!(log.have().get(&a.id), Some(&2));
        let huge = Have::from([(a.id.clone(), u64::MAX)]);
        assert!(log.missing_for(&huge).is_empty());
    }

    #[test]
    fn files_are_keyed_by_author() {
        let (a, b) = (member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        post(&mut log, &a, 2, add(&b));
        let file = |hash: &str| Op::FileAdd {
            id: "f1".into(),
            name: "x.txt".into(),
            size: 3,
            hash: hash.repeat(32),
            dir: None,
        };
        post(&mut log, &a, 3, file("ab"));
        // b reuses the id: a second file, not a replacement.
        post(&mut log, &b, 4, file("cd"));
        assert_eq!(log.view.files.len(), 2);
        let mine = format!("{}/f1", a.id);
        assert_eq!(log.view.files[&mine].hash, "ab".repeat(32));
        post(&mut log, &b, 5, Op::FileDel { id: "f1".into() });
        assert!(log.view.files.contains_key(&mine));
        post(&mut log, &a, 6, Op::FileDel { id: "f1".into() });
        assert!(log.view.files.is_empty());
    }

    #[test]
    fn unknown_kinds_are_kept() {
        let a = member();
        let body =
            r#"{"g":"g","a":"AUTHOR","n":1,"t":5,"k":"poll","q":"?"}"#.replace("AUTHOR", &a.id);
        let sig = a.key.sign(body.as_bytes());
        let signed = SignedEntry {
            body,
            sig: hex::encode(sig.to_bytes()),
        };
        let e = parse(signed, "g", true).unwrap();
        assert!(e.op.is_none());
        let mut log = Log::new("g");
        assert_eq!(log.insert(e), Insert::New);
        assert_eq!(log.have().get(&a.id), Some(&1));
    }

    #[test]
    fn group_size_is_capped() {
        let a = member();
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        for i in 0..80 {
            post(&mut log, &a, 2 + i, add(&member()));
        }
        assert_eq!(log.view.members.len(), MAX_MEMBERS_REPLAY);
    }

    /// The body 3.3 parses: entries with `d` must still be read (and applied).
    #[test]
    fn older_versions_read_new_entries() {
        #[derive(Deserialize)]
        struct OldBody {
            #[allow(dead_code)]
            g: String,
            #[allow(dead_code)]
            n: u64,
            #[serde(flatten)]
            op: Op,
        }
        let (a, b) = (member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        post(&mut log, &a, 2, add(&b));
        let s = post(&mut log, &b, 3, msg("hi"));
        assert!(s.body.contains("\"d\":{"));
        let old: OldBody = serde_json::from_str(&s.body).unwrap();
        assert!(matches!(old.op, Op::Msg { text, .. } if text == "hi"));
    }

    #[test]
    fn pins_are_shared_and_capped() {
        let (a, b) = (member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        post(&mut log, &a, 2, add(&b));
        post(&mut log, &a, 3, msg("hello"));
        let target = format!("{}/hello", a.id);
        post(&mut log, &b, 4, Op::Pin { target: target.clone() });
        post(&mut log, &a, 5, Op::Pin { target: target.clone() });
        assert_eq!(log.view.pins.len(), 1);
        assert_eq!(log.view.pins[0].by, b.id);
        post(&mut log, &a, 6, Op::Unpin { target });
        assert!(log.view.pins.is_empty());
        for i in 0..MAX_PINS as u64 + 5 {
            post(&mut log, &b, 10 + i, Op::Pin { target: format!("x/{i}") });
        }
        assert_eq!(log.view.pins.len(), MAX_PINS);
    }

    /// What 3.3 does with a pin and a file with a folder: keeps the pin as
    /// an unknown kind, reads the file without its folder.
    #[test]
    fn older_versions_keep_pins_and_read_folders() {
        #[derive(Deserialize)]
        #[serde(tag = "k", rename_all = "snake_case")]
        #[allow(dead_code)]
        enum OldOp {
            Msg { id: String, text: String },
            FileAdd { id: String, name: String, size: u64, hash: String },
        }
        #[derive(Deserialize)]
        struct OldBody {
            #[serde(flatten)]
            op: OldOp,
        }
        let (a, b) = (member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        post(&mut log, &a, 2, add(&b));
        let pin = post(&mut log, &b, 3, Op::Pin { target: "x/y".into() });
        assert!(serde_json::from_str::<OldBody>(&pin.body).is_err());
        let file = Op::FileAdd {
            id: "f".into(),
            name: "a.txt".into(),
            size: 1,
            hash: "ab".repeat(32),
            dir: Some("Photos\\2024/../x".into()),
        };
        let file = post(&mut log, &b, 4, file);
        let old: OldBody = serde_json::from_str(&file.body).unwrap();
        assert!(matches!(old.op, OldOp::FileAdd { name, .. } if name == "a.txt"));
        let f = log.view.files.values().next().unwrap();
        assert_eq!(f.dir.as_deref(), Some("Photos/2024/x"));
    }

    #[test]
    fn mentions() {
        let me = vec!["Bob".to_string(), "Jean Dupont".to_string()];
        assert_eq!(mention_of("salut @bob !", &me), Mention::Me);
        assert_eq!(mention_of("@BOB", &me), Mention::Me);
        assert_eq!(mention_of("@bobby", &me), Mention::None);
        assert_eq!(mention_of("bob@bob.fr", &me), Mention::None);
        assert_eq!(mention_of("(@bob)", &me), Mention::Me);
        assert_eq!(mention_of("@jean dupont, tu viens ?", &me), Mention::Me);
        assert_eq!(mention_of("@Tous réunion", &me), Mention::All);
        assert_eq!(mention_of("@toussaint", &me), Mention::None);
        assert_eq!(mention_of("@all @bob", &me), Mention::Me);
        assert_eq!(mention_of("rien", &[]), Mention::None);
    }

    #[test]
    fn a_backdated_removal_keeps_what_the_group_had() {
        let (a, b, x, y) = (member(), member(), member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        post(&mut log, &a, 2, add(&b));
        post(&mut log, &a, 3, add(&x));
        post(&mut log, &x, 10, msg("x1"));
        post(&mut log, &x, 11, add(&y));
        post(&mut log, &y, 12, msg("y1"));
        // b has all of that when writing.
        post(&mut log, &b, 13, msg("b1"));
        post(&mut log, &x, 14, msg("x2, nobody has it yet"));
        // a removes x now, dated before everything x wrote.
        post_blind(&mut log, &a, 4, remove(&x));
        assert!(!log.view.is_member(&x.id));
        assert!(log.view.is_member(&y.id));
        assert_eq!(texts(&log), ["x1", "y1", "b1"]);
        // A later entry from someone who has x2 but not the removal vouches.
        post_blind(&mut log, &b, 15, msg("b2"));
        let mut d = BTreeMap::new();
        d.insert(x.id.clone(), 3);
        let n = log.last_seq_of(&y.id) + 1;
        log.insert(signed(&log, &y, n, 16, Some(d), msg("y2")));
        log.refresh();
        assert_eq!(texts(&log), ["x1", "y1", "b1", "x2, nobody has it yet", "b2", "y2"]);
        // Once members have the removal, what x writes no longer counts.
        post(&mut log, &b, 17, msg("b3"));
        post(&mut log, &x, 18, msg("x3"));
        post(&mut log, &b, 19, msg("b4"));
        assert!(!texts(&log).contains(&"x3".to_string()));
        let mut full = Log::new("g");
        for e in log.all_signed() {
            full.insert_all(vec![parse(e, "g", false).unwrap()], false);
        }
        full.rebuild();
        assert_eq!(texts(&full), texts(&log));
    }

    #[test]
    fn older_removals_apply_from_their_timestamp() {
        let (a, x, b) = (member(), member(), member());
        let mut log = Log::new("g");
        post_legacy(&mut log, &a, 1, create("G"));
        post_legacy(&mut log, &a, 2, add(&x));
        post_legacy(&mut log, &a, 3, add(&b));
        post_legacy(&mut log, &x, 10, msg("x1"));
        post_legacy(&mut log, &b, 11, msg("b1"));
        post_legacy(&mut log, &a, 5, remove(&x));
        assert_eq!(texts(&log), ["b1"]);
    }

    #[test]
    fn a_removal_sticks() {
        let (a, b, c, x) = (member(), member(), member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        post(&mut log, &a, 2, add(&b));
        post(&mut log, &a, 3, add(&c));
        post(&mut log, &b, 4, add(&x));
        post(&mut log, &b, 5, remove(&x));
        post(&mut log, &c, 6, add(&x));
        assert!(!log.view.is_member(&x.id));
        post(&mut log, &b, 7, add(&x));
        assert!(log.view.is_member(&x.id));
        // Who left may be added back by anyone.
        post(&mut log, &x, 8, remove(&x));
        post(&mut log, &c, 9, add(&x));
        assert!(log.view.is_member(&x.id));
        // The creator may always.
        post(&mut log, &c, 10, remove(&x));
        post(&mut log, &a, 11, add(&x));
        assert!(log.view.is_member(&x.id));
    }

    #[test]
    fn additions_at_the_same_time_near_the_cap_all_count() {
        let a = member();
        let b = member();
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        post(&mut log, &a, 2, add(&b));
        for i in 0..MAX_MEMBERS as u64 - 3 {
            post(&mut log, &a, 3 + i, add(&member()));
        }
        assert_eq!(log.view.members.len(), MAX_MEMBERS - 1);
        let (c, d) = (member(), member());
        post_blind(&mut log, &a, 100, add(&c));
        post_blind(&mut log, &b, 100, add(&d));
        post(&mut log, &d, 101, msg("hello"));
        assert_eq!(log.view.members.len(), MAX_MEMBERS + 1);
        assert_eq!(texts(&log), ["hello"]);
    }

    #[test]
    fn two_versions_of_an_entry_cut_their_author_everywhere() {
        let (a, b) = (member(), member());
        let mut la = Log::new("g");
        post(&mut la, &a, 1, create("G"));
        post(&mut la, &a, 2, add(&b));
        post(&mut la, &b, 3, msg("b1"));
        let mut lb = Log::new("g");
        sync(&la, &mut lb);
        // b signs two different entries 2 and 3, one for each log.
        let n = la.last_seq_of(&b.id) + 1;
        la.insert(entry(&la, &b, n, 4, msg("to la")));
        la.insert(entry(&la, &b, n + 1, 5, msg("to la again")));
        lb.insert(entry(&lb, &b, n, 4, msg("to lb")));
        lb.insert(entry(&lb, &b, n + 1, 5, msg("to lb again")));
        la.refresh();
        lb.refresh();
        assert_eq!(la.have(), lb.have());
        // Comparing the hash of b's last entry reveals it.
        let to_lb = la.conflicts_with(&lb.have(), &lb.heads());
        assert_eq!(to_lb.len(), 1);
        let got = lb.insert_all(to_lb.into_iter().map(|e| parse(e, "g", false).unwrap()).collect(), true);
        assert_eq!(got.forked, [b.id.clone()]);
        assert_eq!(lb.forks()[&b.id], n + 1);
        // Swapping proofs walks down to the first entry that differs.
        loop {
            let to_la = lb.fork_evidence(&b.id);
            let got = la.insert_all(to_la.into_iter().map(|e| parse(e, "g", false).unwrap()).collect(), true);
            let to_lb = la.fork_evidence(&b.id);
            let back = lb.insert_all(to_lb.into_iter().map(|e| parse(e, "g", false).unwrap()).collect(), true);
            if got.forked.is_empty() && back.forked.is_empty() {
                break;
            }
        }
        assert_eq!(la.forks(), lb.forks());
        assert_eq!(la.forks()[&b.id], n);
        la.refresh();
        lb.refresh();
        assert_eq!(texts(&la), ["b1"]);
        assert_eq!(texts(&lb), ["b1"]);
        assert!(!la.view.is_member(&b.id) && !lb.view.is_member(&b.id));
        // A member holding neither version takes the proof as it is.
        let mut lc = Log::new("g");
        let proof = la.evidence_for(&HashMap::new());
        let got = lc.insert_all(proof.into_iter().map(|e| parse(e, "g", false).unwrap()).collect(), true);
        assert_eq!(got.kept.len(), 2);
        assert_eq!(lc.forks(), la.forks());
    }

    #[test]
    fn replaying_new_entries_matches_a_rebuild() {
        let (a, b, c) = (member(), member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        post(&mut log, &a, 2, add(&b));
        post(&mut log, &b, 3, add(&c));
        for i in 0..20 {
            let m = [&a, &c][i % 2];
            post(&mut log, m, 10 + i as u64, msg(&format!("m{i}")));
        }
        // Dated before all of them: everything is replayed again.
        post_blind(&mut log, &b, 5, remove(&c));
        assert!(!log.view.is_member(&c.id));
        post(&mut log, &c, 35, msg("after"));
        post(&mut log, &a, 40, msg("late"));
        let incremental = texts(&log);
        // c's messages a had before the removal count, not the last one.
        assert!(incremental.contains(&"m17".to_string()));
        assert!(!incremental.contains(&"m19".to_string()));
        assert!(!incremental.contains(&"after".to_string()));
        log.rebuild();
        assert_eq!(texts(&log), incremental);
    }

    #[test]
    fn our_own_entries_can_be_cut_and_written_again() {
        let (a, b) = (member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        post(&mut log, &a, 2, add(&b));
        post(&mut log, &b, 3, msg("one"));
        post(&mut log, &b, 4, msg("two"));
        let cut = log.truncate(&b.id, 1);
        assert_eq!(cut.len(), 2);
        log.refresh();
        assert!(log.view.messages.is_empty() && !log.has_entry_of(&b.id));
        post(&mut log, &b, 5, msg("again"));
        assert_eq!(log.last_seq_of(&b.id), 1);
        assert_eq!(texts(&log), ["again"]);
    }

    /// Texts read back from the log file give what memory gave.
    #[test]
    fn texts_are_read_back_from_the_file() {
        let (a, b) = (member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        post(&mut log, &a, 2, add(&b));
        for i in 0..100 {
            post(&mut log, [&a, &b][i % 2], 10 + i as u64, msg(&format!("m{i}")));
        }
        let all = log.all_signed();
        let path = std::env::temp_dir().join(format!("p2pshare-texts-{}.jsonl", rand::random::<u32>()));
        let line = |e: &SignedEntry| {
            crate::vault::seal_line(&serde_json::to_vec(e).unwrap()).unwrap() + "
"
        };
        std::fs::write(&path, all.iter().map(line).collect::<String>()).unwrap();
        log.attach(path.clone(), 0);
        log.index_new();
        assert!(!log.has_unlocated());
        assert!(log.all_signed() == all);
        let have = Have::from([(a.id.clone(), 40)]);
        assert_eq!(log.missing_for(&have).len(), 102 - 40);
        // Appended later: in memory until found in the file.
        let s = post(&mut log, &b, 200, msg("late"));
        assert!(log.has_unlocated());
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line(&s).as_bytes()))
            .unwrap();
        log.index_new();
        assert!(!log.has_unlocated());
        // Written anew (other offsets).
        log.hold_texts();
        let again = log.all_signed();
        std::fs::write(&path, again.iter().rev().map(line).collect::<String>()).unwrap();
        log.reindex();
        assert!(!log.has_unlocated());
        assert!(log.all_signed() == again);
        let cut = log.truncate(&b.id, 30);
        assert_eq!(cut.len(), 22);
        assert!(matches!(&cut[21].op, Some(Op::Msg { text, .. }) if text == "late"));
        let mut copy = Log::new("g");
        copy.insert_all(log.all_signed().into_iter().filter_map(|e| parse(e, "g", false)).collect(), false);
        copy.rebuild();
        log.refresh();
        assert_eq!(texts(&copy), texts(&log));
        let _ = std::fs::remove_file(&path);
    }
}
