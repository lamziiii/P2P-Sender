//! Unfinished transfers with friends, kept across restarts (`transfers.json`,
//! sealed like the chats). On start they come back paused: the sender offers
//! its files again and the receiver answers with what its `.part` file holds,
//! as after any reconnection. Shared files of groups and LocalSend are not
//! kept (they are asked for again or offered again from scratch).
//!
//! The protocol does not change: a peer still on an older version sees a
//! re-offer it does not know (a new offer), or a resume it asked for itself.

use super::*;

#[derive(Serialize, Deserialize)]
pub struct Kept {
    id: String,
    out: bool,
    friend: String,
    name: String,
    size: u64,
    /// Outgoing: the receiver had taken it, so it only needs the rest.
    #[serde(default)]
    accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    src: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mtime: Option<SystemTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    part: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dest: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<KeptFolder>,
    #[serde(default)]
    held: bool,
    #[serde(default)]
    in_chat: bool,
}

#[derive(Serialize, Deserialize)]
struct KeptFolder {
    id: String,
    name: String,
    files: u64,
    size: u64,
}

impl Transfer {
    /// What is kept of it, if it is to come back after a restart.
    pub(super) fn kept(&self) -> Option<Kept> {
        if self.serve
            || self.repo.is_some()
            || self.peer_name.is_some()
            || self.hidden
            || self.status.is_final()
        {
            return None;
        }
        let out = self.dir == Dir::Out;
        if out {
            self.src_path.as_ref()?;
        } else {
            // Accepted, with its file: what is still pending is offered again.
            self.part_path.as_ref()?;
            if !matches!(
                self.status,
                Status::Receiving | Status::Paused | Status::Finishing
            ) {
                return None;
            }
        }
        Some(Kept {
            id: self.id.clone(),
            out,
            friend: self.friend_id.clone(),
            name: self.file_name.clone(),
            size: self.file_size,
            accepted: out
                && matches!(
                    self.status,
                    Status::Sending | Status::Paused | Status::Finishing
                ),
            src: self.src_path.clone(),
            mtime: self.src_mtime,
            part: self.part_path.clone(),
            dest: self.dest_dir.clone(),
            root: self.folder_root.clone(),
            rel: self.rel_path.clone(),
            folder: self.folder.as_ref().map(|f| KeptFolder {
                id: f.id.clone(),
                name: f.name.clone(),
                files: f.files,
                size: f.size,
            }),
            held: self.held,
            in_chat: self.in_chat,
        })
    }
}

impl State {
    /// The kept list, when it changed since last written.
    pub(super) fn take_kept(&mut self) -> Option<(PathBuf, Vec<u8>)> {
        if !self.kept_changed {
            return None;
        }
        self.kept_changed = false;
        let mut list: Vec<(u64, Kept)> = self
            .transfers
            .values()
            .filter_map(|t| Some((t.seq, t.kept()?)))
            .collect();
        list.sort_by_key(|(seq, _)| *seq);
        self.kept.data = list.into_iter().map(|(_, k)| k).collect();
        self.kept.mark_dirty();
        self.kept.take_dirty()
    }
}

impl Node {
    /// Bring back the transfers kept by the previous run; the `.part` files
    /// they go on with.
    pub(super) async fn restore_transfers(&self) -> HashSet<PathBuf> {
        let kept = self.with(|s| std::mem::take(&mut s.kept.data));
        let mut parts = HashSet::new();
        let mut folders: HashMap<String, Arc<FolderMeta>> = HashMap::new();
        let mut complete = Vec::new();
        for k in kept {
            if !is_tid(&k.id) || !self.with(|s| s.is_friend(&k.friend)) {
                continue;
            }
            // Outgoing: the same file as when offered, or nothing to resume.
            // Incoming: what the `.part` file holds, and the file to go on.
            let (bytes, writer) = if k.out {
                let Some(src) = &k.src else { continue };
                let same = std::fs::metadata(src).is_ok_and(|m| {
                    m.is_file() && m.len() == k.size && (k.mtime.is_none() || m.modified().ok() == k.mtime)
                });
                if !same {
                    continue;
                }
                (0, None)
            } else {
                let Some(part) = &k.part else { continue };
                let Ok(len) = std::fs::metadata(part).map(|m| m.len()) else {
                    continue;
                };
                if len > k.size {
                    let _ = std::fs::remove_file(part);
                    continue;
                }
                let Ok(file) = std::fs::OpenOptions::new().append(true).open(part) else {
                    continue;
                };
                parts.insert(part.clone());
                let writer = BufWriter::with_capacity(IDLE_BUFFER, tokio::fs::File::from_std(file));
                (len, Some(writer))
            };
            let folder = k.folder.map(|f| {
                folders
                    .entry(f.id.clone())
                    .or_insert_with(|| {
                        Arc::new(FolderMeta {
                            id: f.id,
                            name: f.name,
                            files: f.files,
                            size: f.size,
                        })
                    })
                    .clone()
            });
            // Taken already: only the rest goes. Not yet: offered again.
            let status = if !k.out || k.accepted {
                Status::Paused
            } else {
                Status::Connecting
            };
            let dir = if k.out { Dir::Out } else { Dir::In };
            self.with(|s| {
                let id = s.add_transfer(k.id, dir, &k.friend, k.name, k.size, status);
                let t = s.transfers.get_mut(&id).unwrap();
                t.src_path = k.src;
                t.src_mtime = k.mtime;
                t.part_path = k.part;
                t.dest_dir = k.dest;
                t.folder_root = k.root.clone();
                t.rel_path = k.rel;
                t.held = k.held;
                t.in_chat = k.in_chat;
                t.shared.bytes.store(bytes, Ordering::Relaxed);
                if let Some(writer) = writer {
                    if let Ok(mut slot) = t.shared.writer.try_lock() {
                        *slot = Some(writer);
                    }
                }
                if let Some(folder) = &folder {
                    t.folder = Some(folder.clone());
                    if k.out {
                        s.folders_out.entry(folder.id.clone()).or_insert(None);
                    } else {
                        // Its other files go to the same place, without asking again.
                        let entry = s
                            .folders_in
                            .entry((k.friend.clone(), folder.id.clone()))
                            .or_default();
                        entry.decision = Some(true);
                        entry.root = entry.root.take().or(k.root);
                        entry.files += 1;
                        entry.notified = true;
                    }
                }
                if !k.out && bytes == k.size {
                    complete.push(id);
                }
            });
        }
        // All in before the previous run could save it.
        self.with(|s| {
            for tid in complete {
                self.begin_finalize(s, &tid);
            }
            s.kept_changed = true;
        });
        parts
    }
}
