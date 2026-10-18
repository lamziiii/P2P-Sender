//! Group networking: invitations, log sync between members, shared files.
//!
//!   group_invite  {gid, name, via}     a friend added us to a group
//!   group_decline {gid}                we refuse: the inviter removes us
//!   group_have    {gid, have}          what we have, per author
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
/// Shared files one member may download from us at the same time.
const MAX_SERVES_PER_PEER: usize = 4;
/// Entries dated further in the future are refused until that time comes.
const MAX_CLOCK_SKEW_MS: u64 = 5 * 60 * 1000;

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
}

pub(super) struct GroupState {
    pub log: Log,
    pub peer_have: HashMap<String, Have>,
    pub unsaved: Vec<SignedEntry>,
}

/// An incoming transfer that downloads a shared file.
pub(super) struct Repo {
    pub gid: String,
    pub hash: String,
    pub tried: HashSet<String>,
    /// Members who sent a copy that did not match the hash: never asked again.
    pub bad: HashSet<String>,
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
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupDetail {
    pub id: String,
    pub name: String,
    pub members: Vec<MemberView>,
    pub syncing: bool,
    pub removed: bool,
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
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupFileView {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub author: String,
    pub author_name: String,
    pub ts: u64,
    pub mine: bool,
    pub local: bool,
    pub transfer_id: Option<String>,
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

/// Load every group log from disk (our own files: signatures were checked
/// when the entries arrived).
pub(super) fn load(dir: &Path) -> (JsonStore<GroupsMeta>, HashMap<String, GroupState>) {
    let mut meta = JsonStore::load(dir.join("groups.json"), GroupsMeta::default());
    let mut groups = HashMap::new();
    let mut pinned = false;
    for g in &mut meta.data.groups {
        let mut log = Log::new(&g.id);
        log.pinned_creator = g.creator.clone();
        let path = log_path(dir, &g.id);
        if let Ok(text) = std::fs::read_to_string(&path) {
            // A line cut short by a crash would swallow the next one appended.
            if !text.is_empty() && !text.ends_with('\n') {
                let _ = append_lines(&path, "\n");
            }
            let entries = text
                .lines()
                .filter_map(|line| serde_json::from_str::<SignedEntry>(line).ok())
                .filter_map(|s| group::parse(s, &g.id, false))
                .collect();
            log.insert_all(entries);
        }
        log.rebuild();
        log.drop_strangers();
        if g.creator.is_none() && !log.view.creator.is_empty() {
            g.creator = Some(log.view.creator.clone());
            log.pinned_creator = g.creator.clone();
            pinned = true;
        }
        groups.insert(
            g.id.clone(),
            GroupState {
                log,
                peer_have: HashMap::new(),
                unsaved: Vec::new(),
            },
        );
    }
    if pinned {
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
            return f.name.clone();
        }
        // Not a friend: the nickname is theirs to choose, so it always comes
        // with the start of their id and cannot pass for a friend.
        let short = &id[..6.min(id.len())];
        match view.nicks.get(id) {
            Some(n) if !n.is_empty() => format!("{n} ({short})"),
            _ => format!("Membre {short}"),
        }
    }

    fn syncing(&self, gid: &str) -> bool {
        self.groups
            .get(gid)
            .is_some_and(|g| !g.log.view.is_member(&self.me) && !g.log.has_entry_of(&self.me))
    }

    fn removed(&self, gid: &str) -> bool {
        self.groups
            .get(gid)
            .is_some_and(|g| !g.log.view.is_member(&self.me) && g.log.has_entry_of(&self.me))
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
            self.send(
                peer,
                json!({ "type": "group_have", "gid": gid, "have": g.log.have() }),
            );
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

    /// Invite a friend to every group they were added to but never joined.
    fn send_invites(&self, friend: &str) {
        if !self.is_friend(friend) {
            return;
        }
        for (gid, g) in &self.groups {
            let v = &g.log.view;
            if !self.member_of(gid) || !v.is_member(friend) || g.log.has_entry_of(friend) {
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

    /// Group log lines to append to disk, and the meta file if it changed.
    pub(super) fn take_group_writes(&mut self) -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();
        for (gid, g) in &mut self.groups {
            if g.unsaved.is_empty() {
                continue;
            }
            let lines: String = g
                .unsaved
                .drain(..)
                .filter_map(|e| serde_json::to_string(&e).ok())
                .map(|l| l + "\n")
                .collect();
            out.push((log_path(&self.groups_dir, gid), lines));
        }
        out
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
        self.gmeta.mark_dirty();
        let _ = std::fs::remove_file(log_path(&self.groups_dir, gid));
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
        let me = s.me.clone();
        let Some(g) = s.groups.get_mut(gid) else {
            return false;
        };
        let (n, t) = g.log.next_position(&me, now_ms());
        // Members before the change too: a removed member must learn it.
        let before: Vec<String> = g.log.view.members.keys().cloned().collect();
        let signed = group::sign(
            &s.secret,
            &Body {
                g: gid.to_string(),
                a: me,
                n,
                t,
                op,
            },
        );
        let Some(entry) = group::parse(signed.clone(), gid, false) else {
            return false;
        };
        // On disk before anyone sees it: after a crash we must not sign
        // something else with the same seq.
        let line = serde_json::to_string(&signed).unwrap_or_default() + "\n";
        if append_lines(&log_path(&s.groups_dir, gid), &line).is_err() {
            return false;
        }
        if !g.log.insert(entry) {
            return false;
        }
        g.log.rebuild();
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
        self.retry_downloads(s);
    }

    /// Every reconnect tick: refresh what connected members have.
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
        self.retry_downloads(s);
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
        if s.groups.contains_key(gid) || s.gmeta.data.declined.contains(&via) {
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
            .map(|f| f.name.clone())
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
        let pending = s
            .groups
            .get(gid)
            .is_some_and(|g| g.log.view.is_member(peer) && !g.log.has_entry_of(peer));
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
        let have: Have = msg
            .get("have")
            .and_then(|h| serde_json::from_value(h.clone()).ok())
            .unwrap_or_default();
        let me = s.me.clone();
        let left = s.is_left(gid);
        let Some(g) = s.groups.get_mut(gid) else {
            return;
        };
        if !g.log.view.is_member(peer) {
            return;
        }
        let acked_before = g
            .peer_have
            .get(peer)
            .and_then(|h| h.get(&me))
            .copied()
            .unwrap_or(0);
        let acked_now = have.get(&me).copied().unwrap_or(0);
        let missing = g.log.missing_for(&have);
        let we_lack = have
            .iter()
            .any(|(a, n)| *n > g.log.have().get(a).copied().unwrap_or(0));
        let leave_delivered = left && acked_now >= g.log.last_seq_of(&me);
        g.peer_have.insert(peer.to_string(), have);

        s.send_entries(peer, gid, &missing);
        if we_lack {
            s.send_have(peer, gid);
        }
        if leave_delivered {
            s.forget_group(gid);
            s.events.push(Event::GroupUpdate {
                gid: gid.to_string(),
            });
        } else if acked_now != acked_before {
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
        let Some(g) = s.groups.get_mut(&gid) else {
            return;
        };
        // Members only; while we join, also the friend who invited us.
        if !g.log.view.is_member(peer) && !(syncing && from_friend) {
            return;
        }
        let was_member = g.log.view.is_member(&me);
        let known_msgs: HashSet<String> =
            g.log.view.messages.iter().map(|m| m.id.clone()).collect();
        let latest = now_ms().saturating_add(MAX_CLOCK_SKEW_MS);
        let entries = list
            .iter()
            .filter_map(|item| serde_json::from_value::<SignedEntry>(item.clone()).ok())
            .filter_map(|signed| group::parse(signed, &gid, true))
            .filter(|e| e.t <= latest)
            .collect();
        let added = g.log.insert_all(entries);
        if added.is_empty() {
            return;
        }
        g.log.rebuild();
        // Entries of strangers are neither kept nor passed on.
        g.log.drop_strangers();
        let new: Vec<SignedEntry> = added
            .into_iter()
            .filter(|e| {
                group::parse(e.clone(), &gid, false).is_some_and(|e| g.log.has_entry_of(&e.a))
            })
            .collect();
        if new.is_empty() {
            return;
        }
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
        s.send_have(peer, &gid);

        if is_member && !was_member {
            // Just joined: introduce ourselves and reach the other members.
            if let Some(m) = s.gmeta.data.groups.iter_mut().find(|m| m.id == gid) {
                m.joined = now_ms();
                m.last_read = s.groups[&gid].log.view.max_t;
            }
            s.gmeta.mark_dirty();
            self.post_profile(s, &gid);
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

        // Notify the latest new message, not a whole history being synced.
        let joined = s.meta(&gid).map(|m| m.joined).unwrap_or(0);
        if let Some(m) = fresh.iter().rev().find(|m| m.ts >= joined && was_member) {
            let view = &s.groups[&gid].log.view;
            let event = Event::GroupMessage {
                gid: gid.clone(),
                group: view.name.clone(),
                author: s.display_name(view, &m.author),
                text: m.text.clone(),
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
                        unread: v
                            .messages
                            .iter()
                            .filter(|m| m.author != s.me && m.ts > last_read)
                            .count(),
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
                        .map(|f| f.name.clone())
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
            s.groups.insert(
                gid.clone(),
                GroupState {
                    log: Log::new(&gid),
                    peer_have: HashMap::new(),
                    unsaved: Vec::new(),
                },
            );
            s.gmeta.data.groups.push(GroupMeta {
                id: gid.clone(),
                last_read: 0,
                joined: now_ms(),
                left: false,
                creator: Some(s.me.clone()),
            });
            s.gmeta.mark_dirty();
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
            let addable =
                s.is_friend(m) && s.groups.get(gid).is_some_and(|g| !g.log.view.is_member(m));
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
                s.gmeta.data.declined.push(invite.via.clone());
                s.send(&invite.from, json!({ "type": "group_decline", "gid": gid }));
                s.events.push(Event::GroupUpdate {
                    gid: gid.to_string(),
                });
                return;
            }
            s.groups.insert(
                gid.to_string(),
                GroupState {
                    log: Log::new(gid),
                    peer_have: HashMap::new(),
                    unsaved: Vec::new(),
                },
            );
            s.gmeta.data.groups.push(GroupMeta {
                id: gid.to_string(),
                last_read: 0,
                joined: 0,
                left: false,
                creator: None,
            });
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
                })
                .collect();
            members.sort_by_key(|m| (!m.me, !m.online, m.name.to_lowercase()));
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
            let id = uuid::Uuid::new_v4().to_string();
            self.post(
                s,
                gid,
                Op::Msg {
                    id,
                    text: text.to_string(),
                },
            );
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
        let mut added = 0;
        for path in paths {
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
                if !s.member_of(gid) {
                    return false;
                }
                s.gmeta.data.holdings.insert(
                    hash.clone(),
                    Holding {
                        path: path.clone(),
                        size: meta.len(),
                        mtime: mtime(&meta),
                    },
                );
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
                    },
                )
            });
            if ok {
                added += 1;
            }
        }
        added
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
            if let Ok(mut slot) = t.shared.writer.try_lock() {
                *slot = Some(BufWriter::with_capacity(
                    DISK_BUFFER,
                    tokio::fs::File::from_std(file),
                ));
            }
            t.part_path = Some(part);
            t.repo = Some(Repo {
                gid: gid.to_string(),
                hash: f.hash.clone(),
                tried: HashSet::new(),
                bad: HashSet::new(),
            });
            if f.size == 0 {
                self.begin_finalize(s, &tid);
            } else {
                self.pick_source(s, &tid);
            }
            Ok(tid)
        })
    }

    /// Ask a connected member for the file, preferring its author.
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
        candidates.sort_by_key(|m| Some(m) != author.as_ref());
        let (hash, offset) = (repo.hash.clone(), t.bytes());
        let t = s.transfers.get_mut(tid).unwrap();
        match candidates.first() {
            Some(source) => {
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
                if let Some(r) = t.repo.as_mut() {
                    r.tried.clear();
                }
                s.set_status(
                    tid,
                    Status::Paused,
                    Some("En attente d’un membre qui a ce fichier".into()),
                );
            }
        }
    }

    fn retry_downloads(&self, s: &mut State) {
        let waiting: Vec<String> = s
            .transfers
            .values()
            .filter(|t| t.repo.is_some() && t.status == Status::Paused)
            .map(|t| t.id.clone())
            .collect();
        for tid in waiting {
            self.pick_source(s, &tid);
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
        let Some(repo) = t.repo.as_mut() else {
            return false;
        };
        repo.tried.insert(peer.to_string());
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
        // Only members of a group listing this file may get it.
        let listed = s.groups.iter().find_map(|(gid, g)| {
            let ok = s.member_of(gid) && g.log.view.is_member(peer);
            ok.then(|| g.log.view.files.values().find(|f| f.hash == hash).cloned())
                .flatten()
        });
        let holding = s
            .gmeta
            .data
            .holdings
            .get(&hash)
            .filter(|h| holding_valid(h))
            .cloned();
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

    /// A finished download did not match its hash: never ask that member
    /// again, and start over from another one.
    pub(super) async fn retry_corrupt_download(&self, tid: &str, part: PathBuf) {
        let reopened = tokio::fs::File::create(&part).await;
        self.with(|s| {
            let still = s
                .transfers
                .get(tid)
                .is_some_and(|t| t.status == Status::Finishing);
            let file = match reopened {
                Ok(file) if still => file,
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
                *slot = Some(BufWriter::with_capacity(DISK_BUFFER, file));
            }
            if let Some(repo) = t.repo.as_mut() {
                repo.bad.insert(source);
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
        s.gmeta.data.holdings.insert(
            hash,
            Holding {
                path: target.to_path_buf(),
                size: meta.len(),
                mtime: mtime(&meta),
            },
        );
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
