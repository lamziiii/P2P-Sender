//! Chats with friends: one file per friend in `messages/`, so that a message
//! rewrites only its chat. A chat stays in memory while in use, and always
//! while it has messages still to send; the others keep only their last
//! message, for the friend list. The files are encrypted (`vault`); plain
//! ones from earlier versions are rewritten encrypted when read.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{ChatMessage, MessageStatus, Messages, MAX_HISTORY};
use crate::store::{self, JsonStore};

/// A chat nobody read or wrote for that long leaves memory.
const IDLE: Duration = Duration::from_secs(10 * 60);

struct Chat {
    store: JsonStore<Vec<ChatMessage>>,
    used: Instant,
}

impl Chat {
    /// What must stay in memory: unsaved changes, messages to send, or data
    /// that could not be read from its file.
    fn keep(&self) -> bool {
        self.store.is_dirty()
            || !self.store.is_writable()
            || self
                .store
                .data
                .iter()
                .any(|m| m.status == Some(MessageStatus::Pending))
    }
}

pub(super) struct Chats {
    dir: PathBuf,
    loaded: HashMap<String, Chat>,
    /// The last message of the chats not in memory.
    last: HashMap<String, ChatMessage>,
    /// A chat changed since the timer task last saved (see
    /// `State::schedule_saves`).
    pub changed: bool,
}

impl Chats {
    /// Read the chats of `dir`, after moving there those of `<dir>.json`,
    /// the single file of earlier versions.
    pub fn open(dir: PathBuf) -> Self {
        if let Err(e) = store::create_private_dir(&dir) {
            eprintln!("{} not created: {e}", dir.display());
        }
        let mut chats = Chats {
            dir,
            loaded: HashMap::new(),
            last: HashMap::new(),
            changed: false,
        };
        let ids: Vec<String> = fs::read_dir(&chats.dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|e| id_of(Path::new(&e.file_name())))
            .collect();
        for id in ids {
            chats.chat(&id);
        }
        chats.migrate();
        seal_migrated(&chats.dir);
        let now = Instant::now();
        chats.unload_idle(now + IDLE);
        chats
    }

    fn migrate(&mut self) {
        let mut name: OsString = self.dir.as_os_str().to_owned();
        name.push(".json");
        let legacy = PathBuf::from(name);
        if !legacy.exists() {
            return;
        }
        let old = JsonStore::load(legacy.clone(), Messages::new());
        let writable = old.is_writable();
        let mut written = true;
        for (id, list) in old.data {
            let path = self.dir.join(file_name(&id));
            let chat = self.chat(&id);
            // Already there when an earlier migration was cut short: what
            // the file holds is newer.
            let known: HashSet<&str> = chat.store.data.iter().map(|m| m.id.as_str()).collect();
            let mut merged: Vec<ChatMessage> = list
                .into_iter()
                .filter(|m| !known.contains(m.id.as_str()))
                .collect();
            merged.append(&mut chat.store.data);
            let extra = merged.len().saturating_sub(MAX_HISTORY);
            merged.drain(..extra);
            if writable {
                chat.store.data = merged;
                if chat.store.save_now().is_err() {
                    written = false;
                    self.changed = true;
                }
            } else {
                // Locked: shown as it is, nothing saved during this run (the
                // migration is tried again next time).
                chat.store = JsonStore::read_only(path, merged);
            }
        }
        if !writable {
            return;
        }
        if !written {
            eprintln!("{} not migrated yet: tried again next time", legacy.display());
            return;
        }
        // Kept, never deleted. Gone already when it was found corrupted
        // (then moved aside).
        let mut aside = legacy.with_extension("json.migrated");
        if aside.exists() {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            aside = legacy.with_extension(format!("json.migrated-{secs}"));
        }
        if let Err(e) = fs::rename(&legacy, &aside) {
            if e.kind() != io::ErrorKind::NotFound {
                eprintln!("{} not renamed: {e}", legacy.display());
            }
        }
    }

    /// The chat with `id`, read from its file if it is not in memory.
    fn chat(&mut self, id: &str) -> &mut Chat {
        let now = Instant::now();
        let (dir, last) = (&self.dir, &mut self.last);
        let chat = self.loaded.entry(id.to_string()).or_insert_with(|| {
            last.remove(id);
            Chat {
                store: JsonStore::load_sealed(dir.join(file_name(id)), Vec::new()),
                used: now,
            }
        });
        chat.used = now;
        chat
    }

    pub fn list(&mut self, id: &str) -> &Vec<ChatMessage> {
        &self.chat(id).store.data
    }

    /// To change a chat; `mark_dirty` then saves it.
    pub fn list_mut(&mut self, id: &str) -> &mut Vec<ChatMessage> {
        &mut self.chat(id).store.data
    }

    /// Only chats in memory, which those with messages to send always are.
    pub fn loaded(&self, id: &str) -> Option<&Vec<ChatMessage>> {
        self.loaded.get(id).map(|c| &c.store.data)
    }

    pub fn loaded_mut(&mut self, id: &str) -> Option<&mut Vec<ChatMessage>> {
        self.loaded.get_mut(id).map(|c| &mut c.store.data)
    }

    pub fn push(&mut self, id: &str, entry: ChatMessage) {
        let list = self.list_mut(id);
        list.push(entry);
        if list.len() > MAX_HISTORY {
            let extra = list.len() - MAX_HISTORY;
            list.drain(..extra);
        }
        self.mark_dirty(id);
    }

    pub fn mark_dirty(&mut self, id: &str) {
        if let Some(chat) = self.loaded.get_mut(id) {
            chat.store.mark_dirty();
            self.changed = true;
        }
    }

    pub fn last(&self, id: &str) -> Option<&ChatMessage> {
        match self.loaded.get(id) {
            Some(chat) => chat.store.data.last(),
            None => self.last.get(id),
        }
    }

    /// Snapshots of the chats that changed, by friend. A chat stays in
    /// memory until the timer task, which writes them, unloads it: a
    /// failed write marks it dirty again.
    pub fn take_dirty(&mut self) -> Vec<(String, PathBuf, Vec<u8>)> {
        self.changed = false;
        self.loaded
            .iter_mut()
            .filter_map(|(id, c)| {
                let (path, json) = c.store.take_dirty()?;
                Some((id.clone(), path, json))
            })
            .collect()
    }

    /// On failure the timer task tries again.
    pub fn save_now(&mut self) {
        let mut failed = false;
        for chat in self.loaded.values_mut().filter(|c| c.store.is_dirty()) {
            failed |= chat.store.save_now().is_err();
        }
        self.changed = failed;
    }

    /// Unload the chats unused since before `now - IDLE`.
    pub fn unload_idle(&mut self, now: Instant) {
        let last = &mut self.last;
        self.loaded.retain(|id, chat| {
            if chat.keep() || now.saturating_duration_since(chat.used) < IDLE {
                return true;
            }
            if let Some(m) = chat.store.data.pop() {
                last.insert(id.clone(), m);
            }
            false
        });
    }
}

/// The single file of earlier versions, kept after its migration: encrypted
/// too (once), so no history stays readable on disk.
fn seal_migrated(dir: &Path) {
    let mut name: OsString = dir.as_os_str().to_owned();
    name.push(".json.migrated");
    let migrated = PathBuf::from(name);
    let Ok(bytes) = fs::read(&migrated) else {
        return;
    };
    if crate::vault::is_sealed(&bytes) {
        return;
    }
    let written = crate::vault::seal(&bytes).and_then(|sealed| store::write_atomic(&migrated, &sealed));
    let mut bak: OsString = migrated.as_os_str().to_owned();
    bak.push(".bak");
    match written {
        // The plain copy write_atomic kept.
        Ok(()) => drop(fs::remove_file(PathBuf::from(bak))),
        Err(e) => eprintln!("{} not encrypted: {e}", migrated.display()),
    }
}

fn is_hex_id(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `<id>.json`; other ids (from the former app) are hex-encoded after an `x`.
fn file_name(id: &str) -> String {
    if is_hex_id(id) {
        format!("{id}.json")
    } else {
        format!("x{}.json", hex::encode(id))
    }
}

/// The friend whose chat `name` holds, if it is one of ours (not a backup or
/// a file set aside).
fn id_of(name: &Path) -> Option<String> {
    if name.extension()? != "json" {
        return None;
    }
    let stem = name.file_stem()?.to_str()?;
    if is_hex_id(stem) {
        return Some(stem.to_string());
    }
    String::from_utf8(hex::decode(stem.strip_prefix('x')?).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, pending: bool) -> ChatMessage {
        ChatMessage {
            id: id.to_string(),
            from: "me".to_string(),
            text: id.to_string(),
            ts: 1,
            status: pending.then_some(MessageStatus::Pending),
            ..Default::default()
        }
    }

    #[test]
    fn migrated_and_reloaded() {
        let root = std::env::temp_dir().join(format!("p2pshare-chats-{}", rand::random::<u32>()));
        fs::create_dir_all(&root).unwrap();
        let dir = root.join("messages");
        let (a, b) = ("a".repeat(64), "old friend".to_string());
        let old: Messages = [
            (a.clone(), vec![message("1", false), message("2", true)]),
            (b.clone(), vec![message("3", false)]),
        ]
        .into();
        store::write_atomic(&root.join("messages.json"), &serde_json::to_vec(&old).unwrap()).unwrap();

        let mut chats = Chats::open(dir.clone());
        assert!(root.join("messages.json.migrated").exists());
        assert!(!root.join("messages.json").exists());
        // Pending: in memory; the other one only by its last message.
        assert!(chats.loaded(&a).is_some() && chats.loaded(&b).is_none());
        assert_eq!(chats.last(&b).unwrap().id, "3");
        chats.push(&b, message("4", false));
        assert_eq!(chats.list(&b).len(), 2);
        chats.save_now();
        let sealed = |path: PathBuf| crate::vault::is_sealed(&fs::read(path).unwrap());
        assert!(sealed(dir.join(file_name(&b))) && sealed(root.join("messages.json.migrated")));

        let mut chats = Chats::open(dir);
        assert_eq!(chats.list(&a).len(), 2);
        assert_eq!(chats.list(&b).len(), 2);
        let _ = fs::remove_dir_all(&root);
    }
}
