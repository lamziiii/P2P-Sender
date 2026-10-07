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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;

use iroh::{PublicKey, SecretKey, Signature};
use serde::{Deserialize, Serialize};

pub const MAX_MEMBERS: usize = 32;
pub const MAX_NAME: usize = 60;
pub const MAX_NICK: usize = 40;
pub const MAX_BODY: usize = 64 * 1024;

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
    pub id: String,
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
    pub max_t: u64,
}

impl View {
    pub fn is_member(&self, id: &str) -> bool {
        self.members.contains_key(id)
    }
}

#[derive(Default)]
pub struct Log {
    pub id: String,
    entries: HashMap<String, BTreeMap<u64, Entry>>,
    pub view: View,
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

    /// Add an entry; returns false if it was already known.
    pub fn insert(&mut self, entry: Entry) -> bool {
        let by_author = self.entries.entry(entry.a.clone()).or_default();
        if by_author.contains_key(&entry.n) {
            return false;
        }
        by_author.insert(entry.n, entry);
        true
    }

    pub fn have(&self) -> Have {
        self.entries
            .iter()
            .map(|(author, list)| {
                let mut top = 0;
                for n in list.keys() {
                    if *n != top + 1 {
                        break;
                    }
                    top = *n;
                }
                (author.clone(), top)
            })
            .collect()
    }

    /// Entries a peer with `have` is missing.
    pub fn missing_for(&self, have: &Have) -> Vec<SignedEntry> {
        let mut out = Vec::new();
        for (author, list) in &self.entries {
            let known = have.get(author).copied().unwrap_or(0);
            out.extend(list.range(known + 1..).map(|(_, e)| e.signed.clone()));
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
        (n, now.max(self.view.max_t + 1))
    }

    pub fn rebuild(&mut self) {
        let mut ordered: Vec<&Entry> = self.entries.values().flat_map(|l| l.values()).collect();
        ordered.sort_by(|x, y| (x.t, &x.a, x.n).cmp(&(y.t, &y.a, y.n)));

        let mut v = View::default();
        for e in ordered {
            v.max_t = v.max_t.max(e.t);
            let Some(op) = &e.op else { continue };
            if let Op::Create { name } = op {
                if v.creator.is_empty() {
                    v.creator = e.a.clone();
                    v.name = clip(name, MAX_NAME);
                    v.members.insert(e.a.clone(), e.t);
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
                    }
                }
                Op::Remove { member } => {
                    v.members.remove(member);
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
                            id: id.clone(),
                            author: e.a.clone(),
                            text: text.clone(),
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
                    if well_formed && !v.files.contains_key(id) {
                        v.files.insert(
                            id.clone(),
                            SharedFile {
                                id: id.clone(),
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
                    if v.files.get(id).is_some_and(|f| f.author == e.a) {
                        v.files.remove(id);
                    }
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
    s.trim().chars().take(max).collect()
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

    fn post(log: &mut Log, m: &Member, t: u64, op: Op) -> SignedEntry {
        let (n, _) = log.next_position(&m.id, 0);
        let body = Body {
            g: log.id.clone(),
            a: m.id.clone(),
            n,
            t,
            op,
        };
        let signed = sign(&m.key, &body);
        let gid = log.id.clone();
        log.insert(parse(signed.clone(), &gid, true).unwrap());
        log.rebuild();
        signed
    }

    fn msg(text: &str) -> Op {
        Op::Msg {
            id: text.into(),
            text: text.into(),
        }
    }

    #[test]
    fn signatures_are_checked() {
        let a = member();
        let mut log = Log::new("g");
        let signed = post(&mut log, &a, 1, Op::Create { name: "G".into() });
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
        post(
            &mut log,
            &a,
            1,
            Op::Create {
                name: "Amis".into(),
            },
        );
        post(
            &mut log,
            &a,
            2,
            Op::Add {
                member: b.id.clone(),
            },
        );
        // c is not a member: ignored.
        post(&mut log, &c, 3, msg("intrus"));
        post(&mut log, &b, 4, msg("salut"));
        assert_eq!(log.view.name, "Amis");
        assert!(log.view.is_member(&b.id) && !log.view.is_member(&c.id));
        assert_eq!(log.view.messages.len(), 1);
        assert_eq!(log.view.messages[0].text, "salut");
    }

    #[test]
    fn sync_between_logs_converges() {
        let (a, b) = (member(), member());
        let mut la = Log::new("g");
        post(&mut la, &a, 1, Op::Create { name: "G".into() });
        post(
            &mut la,
            &a,
            2,
            Op::Add {
                member: b.id.clone(),
            },
        );
        let mut lb = Log::new("g");
        for e in la.missing_for(&lb.have()) {
            lb.insert(parse(e, "g", true).unwrap());
        }
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
        post(&mut log, &a, 1, Op::Create { name: "G".into() });
        post(
            &mut log,
            &a,
            2,
            Op::Add {
                member: b.id.clone(),
            },
        );
        post(
            &mut log,
            &a,
            3,
            Op::Add {
                member: c.id.clone(),
            },
        );
        // b and c remove each other "at the same time": the earlier one wins.
        post(
            &mut log,
            &c,
            11,
            Op::Remove {
                member: b.id.clone(),
            },
        );
        post(
            &mut log,
            &b,
            10,
            Op::Remove {
                member: c.id.clone(),
            },
        );
        assert!(log.view.is_member(&b.id));
        assert!(!log.view.is_member(&c.id));
    }

    #[test]
    fn only_the_author_deletes_a_file() {
        let (a, b) = (member(), member());
        let mut log = Log::new("g");
        post(&mut log, &a, 1, Op::Create { name: "G".into() });
        post(
            &mut log,
            &a,
            2,
            Op::Add {
                member: b.id.clone(),
            },
        );
        let hash = "ab".repeat(32);
        post(
            &mut log,
            &a,
            3,
            Op::FileAdd {
                id: "f1".into(),
                name: "x.txt".into(),
                size: 3,
                hash,
            },
        );
        post(&mut log, &b, 4, Op::FileDel { id: "f1".into() });
        assert!(log.view.files.contains_key("f1"));
        post(&mut log, &a, 5, Op::FileDel { id: "f1".into() });
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
        post(&mut log, &a, 1, Op::Create { name: "G".into() });
        for i in 0..40 {
            post(
                &mut log,
                &a,
                2 + i,
                Op::Add {
                    member: member().id,
                },
            );
        }
        assert_eq!(log.view.members.len(), MAX_MEMBERS);
    }
}
