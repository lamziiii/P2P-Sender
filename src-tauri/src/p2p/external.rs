//! Transfers with peers outside the P2P network (LocalSend): they share the
//! transfer list, the consent prompt and the download folder with the
//! others, but the peer is not a friend and nothing goes over QUIC.
//!
//! There is no resuming: an upload that stops short fails the transfer.

use super::*;

/// A file offered by a sender outside the P2P network.
pub struct ExternalFile {
    pub name: String,
    pub size: u64,
}

/// A file we send outside the P2P network: the sender reports its progress
/// here and stops when the user cancels it.
pub struct OutgoingFile {
    pub tid: String,
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    shared: Arc<Shared>,
    session: u64,
}

impl OutgoingFile {
    pub fn set_sent(&self, bytes: u64) {
        self.shared.bytes.store(bytes, Ordering::Relaxed);
    }

    pub fn sent(&self) -> u64 {
        self.shared.bytes.load(Ordering::Relaxed)
    }

    /// Resolves once the transfer is stopped from our side.
    pub async fn cancelled(&self) {
        let mut rx = self.shared.session.subscribe();
        session_changed(&mut rx, self.session).await;
    }
}

impl Node {
    /// Show `files` as incoming offers from `peer_name`. Each receiver
    /// resolves to true once the user accepts the file, and to false (or is
    /// closed) when it is declined or cancelled.
    pub fn external_offer(
        &self,
        peer_id: &str,
        peer_name: &str,
        files: &[ExternalFile],
    ) -> Vec<(String, oneshot::Receiver<bool>)> {
        self.with(|s| {
            let offers: Vec<(String, oneshot::Receiver<bool>)> = files
                .iter()
                .map(|f| {
                    let tid = hex::encode(rand::random::<[u8; TID_BYTES]>());
                    let (tx, rx) = oneshot::channel();
                    s.add_transfer(
                        tid.clone(),
                        Dir::In,
                        peer_id,
                        sanitize_file_name(&f.name),
                        f.size,
                        Status::Pending,
                    );
                    let t = s.transfers.get_mut(&tid).unwrap();
                    t.peer_name = Some(peer_name.to_string());
                    t.decision = Some(tx);
                    s.emit_transfer(&tid);
                    (tid, rx)
                })
                .collect();
            // One notification for the whole request.
            if let Some((tid, _)) = offers.first() {
                let view = s.transfers[tid].view();
                s.events.push(Event::FileOffer(view, offers.len()));
            }
            offers
        })
    }

    /// Write an accepted file from the chunks arriving on `data`, then save
    /// it. The sender closes `data` after the last byte; closing it earlier
    /// fails the transfer.
    pub async fn external_receive(
        &self,
        tid: &str,
        mut data: mpsc::Receiver<Bytes>,
    ) -> Result<(), &'static str> {
        let found = self.with(|s| {
            let t = s.transfers.get(tid)?;
            Some((t.status, t.shared.clone(), t.shared.session(), t.file_size))
        });
        let Some((status, shared, session, size)) = found else {
            return Err("Transfert inconnu");
        };
        match status {
            Status::Receiving => {}
            // Empty files are saved as soon as they are accepted.
            Status::Finishing | Status::Completed if size == 0 => return Ok(()),
            _ => return Err("Transfert non accepté"),
        }

        let mut slot = shared.writer.lock().await;
        let Some(writer) = slot.as_mut() else {
            return Err("Transfert non accepté");
        };
        if shared.session() != session || shared.bytes.load(Ordering::Relaxed) != 0 {
            return Err("Transfert déjà commencé");
        }
        let mut cancel = shared.session.subscribe();
        let mut pos = 0;
        let failure = loop {
            let chunk = tokio::select! {
                c = data.recv() => c,
                // Cancelled here: already stopped by whoever cancelled it.
                _ = session_changed(&mut cancel, session) => return Err("Transfert annulé"),
            };
            let Some(chunk) = chunk else {
                break Some("Transfert interrompu par l’expéditeur");
            };
            if pos + chunk.len() as u64 > size {
                break Some("Taille reçue incohérente");
            }
            if writer.write_all(&chunk).await.is_err() {
                break Some("Écriture sur le disque impossible");
            }
            pos += chunk.len() as u64;
            shared.bytes.store(pos, Ordering::Relaxed);
            if pos == size {
                break None;
            }
        };
        drop(slot);
        match failure {
            None => {
                self.with(|s| self.begin_finalize(s, tid));
                Ok(())
            }
            Some(error) => {
                self.external_stop(tid, Status::Error, Some(error));
                Err(error)
            }
        }
    }

    /// List files to send to `peer_name`, waiting for its answer.
    pub fn external_send(
        &self,
        peer_id: &str,
        peer_name: &str,
        files: &[(PathBuf, u64)],
    ) -> Vec<OutgoingFile> {
        self.with(|s| {
            files
                .iter()
                .map(|(path, size)| {
                    let tid = hex::encode(rand::random::<[u8; TID_BYTES]>());
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    s.add_transfer(
                        tid.clone(),
                        Dir::Out,
                        peer_id,
                        name.clone(),
                        *size,
                        Status::WaitingConsent,
                    );
                    let t = s.transfers.get_mut(&tid).unwrap();
                    t.src_path = Some(path.clone());
                    t.peer_name = Some(peer_name.to_string());
                    let file = OutgoingFile {
                        tid: tid.clone(),
                        name,
                        path: path.clone(),
                        size: *size,
                        shared: t.shared.clone(),
                        session: t.shared.session(),
                    };
                    s.emit_transfer(&tid);
                    file
                })
                .collect()
        })
    }

    /// The peer accepted the file and its bytes start flowing; false if the
    /// transfer was stopped meanwhile.
    pub fn external_sending(&self, tid: &str) -> bool {
        self.with(|s| {
            let Some(t) = s.transfers.get(tid).filter(|t| !t.status.is_final()) else {
                return false;
            };
            t.shared.bytes.store(0, Ordering::Relaxed);
            s.set_status(tid, Status::Sending, None);
            true
        })
    }

    /// The peer received the whole file.
    pub fn external_sent(&self, tid: &str) {
        self.with(|s| {
            let Some(t) = s.transfers.get(tid).filter(|t| !t.status.is_final()) else {
                return;
            };
            t.shared.bytes.store(t.file_size, Ordering::Relaxed);
            s.set_status(tid, Status::Completed, None);
        });
    }

    /// End a transfer from the peer's side. Does nothing once it is over,
    /// nor once all its bytes are in: it is being saved.
    pub fn external_stop(&self, tid: &str, status: Status, error: Option<&str>) {
        self.with(|s| {
            let Some(t) = s.transfers.get(tid) else {
                return;
            };
            if t.status == Status::Finishing {
                return;
            }
            let unanswered = t.status == Status::Pending;
            self.stop_transfer(s, tid, status, error.map(String::from));
            // Withdrawn before the user answered: no need to keep it around.
            if unanswered {
                if let Some(t) = s.transfers.get_mut(tid) {
                    t.hidden = true;
                }
            }
        });
    }

    /// Bytes received so far over `tids`, and whether they are all over.
    pub fn external_progress(&self, tids: &[String]) -> (u64, bool) {
        self.with(|s| {
            tids.iter()
                .filter_map(|tid| s.transfers.get(tid))
                .fold((0, true), |(bytes, done), t| {
                    (bytes + t.bytes(), done && t.status.is_final())
                })
        })
    }
}
