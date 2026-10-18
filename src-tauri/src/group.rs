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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;

use iroh::{PublicKey, SecretKey, Signature};
use serde::{Deserialize, Serialize};

pub const MAX_MEMBERS: usize = 32;
pub const MAX_NAME: usize = 60;
pub const MAX_NICK: usize = 40;
pub const MAX_BODY: usize = 64 * 1024;
pub const MAX_TEXT: usize = 5000;

/// Seq per author up to which a peer has every entry.
pub type Have = HashMap<String, u64>;

/// An entry as stored and sent: the exact signed JSON text and its signature.
#[derive(Clone, Serialize, Deserialize)]
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
    },
    FileDel {
        id: String,
    },
}

/// Header fields, readable even for entry kinds this version does not know.
#[derive(Deserialize)]
struct Header {
    g: String,
    a: String,
    n: u64,
    t: u64,
}

#[derive(Clone)]
pub struct Entry {
    pub signed: SignedEntry,
    pub a: String,
    pub n: u64,
    pub t: u64,
    /// None for kinds from a newer version: kept and relayed, not applied.
    pub op: Option<Op>,
}

/// The id of a group created by `creator` with `nonce`: nobody else can
/// write a creation entry that matches it.
pub fn group_id(creator: &str, nonce: &str) -> String {
    let hash = blake3::hash(format!("{creator}:{nonce}").as_bytes());
    hash.to_hex()[..32].to_string()
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
    if header.g != gid || header.n == 0 {
        return None;
    }
    if verify {
        let key = PublicKey::from_str(&header.a).ok()?;
        let mut sig = [0u8; 64];
        hex::decode_to_slice(&signed.sig, &mut sig).ok()?;
        key.verify(signed.body.as_bytes(), &Signature::from_bytes(&sig))
            .ok()?;
    }
    let op = serde_json::from_str::<Body>(&signed.body)
        .ok()
        .map(|b| b.op);
    Some(Entry {
        a: header.a,
        n: header.n,
        t: header.t,
        op,
        signed,
    })
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
    pub size: u64,
    pub hash: String,
    pub author: String,
    pub ts: u64,
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
    /// (author, seq) of the entry that last added each member.
    pub added_by: HashMap<String, (String, u64)>,
    /// Everyone who was a member at some point.
    pub ever: HashSet<String>,
    pub max_t: u64,
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
}

#[derive(Default)]
pub struct Log {
    pub id: String,
    entries: HashMap<String, BTreeMap<u64, Entry>>,
    pub view: View,
    /// For groups made before ids were tied to their creator: the creator
    /// seen when we first had the log, so a later forged creation entry
    /// cannot take the group over.
    pub pinned_creator: Option<String>,
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

    /// Add an entry; returns false if it was already known or cannot be
    /// kept: an author's entries are kept in seq order without gaps, each
    /// with a later timestamp than the one before.
    pub fn insert(&mut self, entry: Entry) -> bool {
        let by_author = self.entries.entry(entry.a.clone()).or_default();
        let last = by_author.last_key_value().map(|(n, e)| (*n, e.t));
        let fits = match last {
            None => entry.n == 1,
            Some((n, t)) => Some(entry.n) == n.checked_add(1) && entry.t > t,
        };
        if !fits {
            if by_author.is_empty() {
                self.entries.remove(&entry.a);
            }
            return false;
        }
        by_author.insert(entry.n, entry);
        true
    }

    /// Insert entries in any order; returns those that were kept.
    pub fn insert_all(&mut self, mut entries: Vec<Entry>) -> Vec<SignedEntry> {
        entries.sort_by(|x, y| (&x.a, x.n).cmp(&(&y.a, y.n)));
        let mut kept = Vec::new();
        for e in entries {
            let signed = e.signed.clone();
            if self.insert(e) {
                kept.push(signed);
            }
        }
        kept
    }

    /// After a rebuild, forget the entries of authors who were never
    /// members. Entries that arrived before the one adding their author come
    /// back on the next sync.
    pub fn drop_strangers(&mut self) {
        let ever = &self.view.ever;
        self.entries.retain(|a, _| ever.contains(a));
    }

    pub fn have(&self) -> Have {
        self.entries
            .iter()
            .map(|(author, list)| {
                let top = list.keys().next_back().copied().unwrap_or(0);
                (author.clone(), top)
            })
            .collect()
    }

    /// Entries a peer with `have` is missing.
    pub fn missing_for(&self, have: &Have) -> Vec<SignedEntry> {
        let mut out = Vec::new();
        for (author, list) in &self.entries {
            let known = have.get(author).copied().unwrap_or(0);
            let Some(from) = known.checked_add(1) else {
                continue;
            };
            out.extend(list.range(from..).map(|(_, e)| e.signed.clone()));
        }
        out
    }

    /// Whether `have` is missing anything we have.
    pub fn is_behind(&self, have: &Have) -> bool {
        self.entries.iter().any(|(author, list)| {
            list.keys().next_back().copied().unwrap_or(0) > have.get(author).copied().unwrap_or(0)
        })
    }

    /// Next seq for `author` and a timestamp after everything seen, so a new
    /// entry always sorts after the entries its author already knew about.
    pub fn next_position(&self, author: &str, now: u64) -> (u64, u64) {
        let n = self
            .entries
            .get(author)
            .and_then(|l| l.keys().next_back().copied())
            .unwrap_or(0)
            + 1;
        let last_t = self
            .entries
            .get(author)
            .and_then(|l| l.values().next_back())
            .map(|e| e.t)
            .unwrap_or(0);
        let t = now
            .max(self.view.max_t.saturating_add(1))
            .max(last_t.saturating_add(1));
        (n, t)
    }

    /// The author of the entry that created the group: the one matching the
    /// group id; for older groups, the pinned creator, or else the earliest.
    fn creator(&self) -> Option<String> {
        let creations: Vec<(&Entry, Option<&String>)> = self
            .entries
            .values()
            .filter_map(|l| l.get(&1))
            .filter_map(|e| match &e.op {
                Some(Op::Create { nonce, .. }) => Some((e, nonce.as_ref())),
                _ => None,
            })
            .collect();
        let bound = creations
            .iter()
            .find(|(e, nonce)| nonce.is_some_and(|n| group_id(&e.a, n) == self.id));
        if let Some((e, _)) = bound {
            return Some(e.a.clone());
        }
        let mut legacy = creations
            .iter()
            .filter(|(_, nonce)| nonce.is_none())
            .map(|(e, _)| *e);
        match &self.pinned_creator {
            Some(p) => legacy.find(|e| e.a == *p).map(|e| e.a.clone()),
            None => legacy
                .min_by(|x, y| (x.t, &x.a).cmp(&(y.t, &y.a)))
                .map(|e| e.a.clone()),
        }
    }

    pub fn rebuild(&mut self) {
        let creator = self.creator();
        let mut ordered: Vec<&Entry> = self.entries.values().flat_map(|l| l.values()).collect();
        ordered.sort_by(|x, y| (x.t, &x.a, x.n).cmp(&(y.t, &y.a, y.n)));

        let mut v = View::default();
        for e in ordered {
            v.max_t = v.max_t.max(e.t);
            let Some(op) = &e.op else { continue };
            if let Op::Create { name, .. } = op {
                if e.n == 1 && creator.as_ref() == Some(&e.a) {
                    v.creator = e.a.clone();
                    v.name = clip(name, MAX_NAME);
                    v.members.insert(e.a.clone(), e.t);
                    v.ever.insert(e.a.clone());
                }
                continue;
            }
            if !v.is_member(&e.a) {
                continue;
            }
            match op {
                Op::Create { .. } => {}
                Op::Add { member } => {
                    let valid = crate::p2p::is_valid_id(member);
                    if valid && !v.is_member(member) && v.members.len() < MAX_MEMBERS {
                        v.members.insert(member.clone(), e.t);
                        v.added_by.insert(member.clone(), (e.a.clone(), e.n));
                        v.ever.insert(member.clone());
                    }
                }
                Op::Remove { member } => {
                    if v.can_remove(&e.a, member) {
                        v.members.remove(member);
                    }
                }
                Op::Rename { name } => {
                    let name = clip(name, MAX_NAME);
                    if !name.is_empty() {
                        v.name = name;
                    }
                }
                Op::Profile { nick } => {
                    v.nicks.insert(e.a.clone(), clip(nick, MAX_NICK));
                }
                Op::Msg { id, text } => {
                    if !text.trim().is_empty() {
                        v.messages.push(Message {
                            id: format!("{}/{id}", e.a),
                            author: e.a.clone(),
                            text: text.chars().take(MAX_TEXT).collect(),
                            ts: e.t,
                            seq: e.n,
                        });
                    }
                }
                Op::FileAdd {
                    id,
                    name,
                    size,
                    hash,
                } => {
                    let well_formed =
                        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
                    let key = format!("{}/{id}", e.a);
                    if well_formed && !v.files.contains_key(&key) {
                        v.files.insert(
                            key.clone(),
                            SharedFile {
                                id: key,
                                entry_id: id.clone(),
                                name: crate::p2p::sanitize_file_name(name),
                                size: *size,
                                hash: hash.to_lowercase(),
                                author: e.a.clone(),
                                ts: e.t,
                            },
                        );
                    }
                }
                Op::FileDel { id } => {
                    v.files.remove(&format!("{}/{id}", e.a));
                }
            }
        }
        self.view = v;
    }

    /// Authors who have written at least one entry (i.e. joined at some point).
    pub fn authors(&self) -> HashSet<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    pub fn has_entry_of(&self, author: &str) -> bool {
        self.entries.contains_key(author)
    }

    pub fn last_seq_of(&self, author: &str) -> u64 {
        self.entries
            .get(author)
            .and_then(|l| l.keys().next_back().copied())
            .unwrap_or(0)
    }
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

    fn entry(log: &Log, m: &Member, n: u64, t: u64, op: Op) -> Entry {
        let body = Body {
            g: log.id.clone(),
            a: m.id.clone(),
            n,
            t,
            op,
        };
        parse(sign(&m.key, &body), &log.id, true).unwrap()
    }

    fn post(log: &mut Log, m: &Member, t: u64, op: Op) -> SignedEntry {
        let (n, _) = log.next_position(&m.id, 0);
        let e = entry(log, m, n, t, op);
        let signed = e.signed.clone();
        log.insert(e);
        log.rebuild();
        signed
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

    #[test]
    fn signatures_are_checked() {
        let a = member();
        let mut log = Log::new("g");
        let signed = post(&mut log, &a, 1, create("G"));
        let mut forged = signed.clone();
        forged.body = forged.body.replace("\"G\"", "\"H\"");
        assert!(parse(forged, "g", true).is_none());
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
        assert_eq!(log.view.messages.len(), 1);
        assert_eq!(log.view.messages[0].text, "salut");
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
        let entries = la
            .missing_for(&lb.have())
            .into_iter()
            .map(|e| parse(e, "g", true).unwrap())
            .collect();
        lb.insert_all(entries);
        lb.rebuild();
        post(&mut lb, &b, 3, msg("hello"));
        assert!(lb.is_behind(&la.have()));
        for e in lb.missing_for(&la.have()) {
            la.insert(parse(e, "g", true).unwrap());
        }
        la.rebuild();
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
        post(&mut log, &a, 11, remove(&b));
        post(&mut log, &b, 10, remove(&c));
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
        assert!(!log.insert(entry(&log, &a, 2, 5, msg("gap"))));
        assert!(log.insert(entry(&log, &a, 1, 5, create("G"))));
        assert!(!log.insert(entry(&log, &a, 3, 6, msg("gap"))));
        assert!(!log.insert(entry(&log, &a, 2, 5, msg("same time"))));
        assert!(!log.insert(entry(&log, &a, 2, 1, msg("backdated"))));
        assert!(log.insert(entry(&log, &a, 2, 6, msg("ok"))));
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
        assert!(log.insert(e));
        assert_eq!(log.have().get(&a.id), Some(&1));
    }

    #[test]
    fn group_size_is_capped() {
        let a = member();
        let mut log = Log::new("g");
        post(&mut log, &a, 1, create("G"));
        for i in 0..40 {
            post(&mut log, &a, 2 + i, add(&member()));
        }
        assert_eq!(log.view.members.len(), MAX_MEMBERS);
    }
}
