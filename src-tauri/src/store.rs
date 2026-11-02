use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::vault;

/// Reading a file another program holds (antivirus, sync client) is tried
/// that many times before giving up.
const READ_ATTEMPTS: u32 = 5;
const READ_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Small JSON file store: synchronous load, explicit (atomic) saves.
/// `mark_dirty` + `take_dirty` let a caller batch frequent writes.
/// Either kind of file is read: plain JSON or sealed by `vault`.
pub struct JsonStore<T> {
    path: PathBuf,
    pub data: T,
    dirty: bool,
    /// False when the file exists but could not be read: saving the
    /// defaults over it would wipe what it holds.
    writable: bool,
    /// Written encrypted (`vault::seal`).
    sealed: bool,
}

/// Why a file's content could not be used.
enum Unreadable {
    /// Damaged, or sealed with a lost key: set aside.
    Corrupt(String),
    /// The history key is unavailable this run: like a locked file.
    Locked(String),
}

/// Plain or sealed JSON; true when it was plain.
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<(T, bool), Unreadable> {
    let (json, plain) = if vault::is_sealed(bytes) {
        match vault::open(bytes) {
            Ok(json) => (json, false),
            Err(e) if e.kind() == ErrorKind::InvalidData => {
                return Err(Unreadable::Corrupt(e.to_string()))
            }
            Err(e) => return Err(Unreadable::Locked(e.to_string())),
        }
    } else {
        (bytes.to_vec(), true)
    };
    serde_json::from_slice(&json)
        .map(|data| (data, plain))
        .map_err(|e| Unreadable::Corrupt(e.to_string()))
}

impl<T: Serialize + DeserializeOwned> JsonStore<T> {
    pub fn load(path: PathBuf, default: T) -> Self {
        Self::load_as(path, default, false)
    }

    /// Like `load`, written encrypted. A plain file (from an earlier
    /// version) is read, then rewritten sealed right away.
    pub fn load_sealed(path: PathBuf, default: T) -> Self {
        Self::load_as(path, default, true)
    }

    fn load_as(path: PathBuf, default: T, sealed: bool) -> Self {
        let mut migrate = false;
        let (data, writable) = match read_retrying(&path) {
            Ok(bytes) => match decode(&bytes) {
                Ok((data, plain)) => {
                    migrate = sealed && plain;
                    (data, true)
                }
                Err(Unreadable::Locked(e)) => {
                    eprintln!(
                        "{} not readable ({e}): changes will not be saved",
                        path.display()
                    );
                    (load_backup(&path).unwrap_or(default), false)
                }
                Err(Unreadable::Corrupt(e)) => {
                    // Kept aside for a manual recovery, then the last good
                    // version is used; writing over the file is safe now
                    // (never while it is still there).
                    let moved = match set_aside(&path) {
                        Ok(aside) => {
                            eprintln!(
                                "{} unreadable ({e}), moved to {}",
                                path.display(),
                                aside.display()
                            );
                            true
                        }
                        Err(err) => {
                            eprintln!(
                                "{} unreadable ({e}), could not be moved aside: {err}",
                                path.display()
                            );
                            false
                        }
                    };
                    (load_backup(&path).unwrap_or(default), moved)
                }
            },
            // First run, or the data was removed on purpose.
            Err(e) if e.kind() == ErrorKind::NotFound => (default, true),
            // Locked or forbidden: show the last good version if there is one,
            // but never save over the file during this run.
            Err(e) => {
                eprintln!(
                    "{} not readable ({e}): changes will not be saved",
                    path.display()
                );
                (load_backup(&path).unwrap_or(default), false)
            }
        };
        let mut store = Self {
            path,
            data,
            dirty: false,
            writable,
            sealed,
        };
        if migrate {
            match store.save_now() {
                // The replaced, plain version must not linger as the backup.
                Ok(()) => keep_backup(&store.path),
                Err(e) => eprintln!("{} not encrypted yet: {e}", store.path.display()),
            }
        }
        store
    }

    /// Data shown but never saved, like a file found locked at startup.
    pub fn read_only(path: PathBuf, data: T) -> Self {
        Self {
            path,
            data,
            dirty: false,
            writable: false,
            sealed: false,
        }
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Serialized snapshot to write if something changed since the last call.
    pub fn take_dirty(&mut self) -> Option<(PathBuf, Vec<u8>)> {
        if !self.dirty || !self.writable {
            return None;
        }
        let bytes = self.encode().ok()?;
        self.dirty = false;
        Some((self.path.clone(), bytes))
    }

    /// What the file holds: the JSON, sealed if the store is.
    fn encode(&self) -> io::Result<Vec<u8>> {
        let json = serde_json::to_vec(&self.data).map_err(io::Error::other)?;
        if self.sealed {
            vault::seal(&json)
        } else {
            Ok(json)
        }
    }

    /// Write now; on failure the store stays dirty, so a later save retries.
    pub fn save_now(&mut self) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                format!("{} could not be read at startup", self.path.display()),
            ));
        }
        let result = self.encode().and_then(|bytes| write_atomic(&self.path, &bytes));
        self.dirty = result.is_err();
        result
    }
}

fn read_retrying(path: &Path) -> io::Result<Vec<u8>> {
    let mut attempt = 1;
    loop {
        match fs::read(path) {
            Err(e) if e.kind() != ErrorKind::NotFound && attempt < READ_ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(READ_RETRY_DELAY);
            }
            result => return result,
        }
    }
}

/// `<path>.bak`: the version replaced by the last save.
fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

fn load_backup<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let backup = backup_path(path);
    let (data, _) = decode(&fs::read(&backup).ok()?).ok()?;
    eprintln!("{} restored from {}", path.display(), backup.display());
    Some(data)
}

/// Rename a corrupted `name.json` to `name.corrupt-<time>.json`.
pub fn set_aside(path: &Path) -> io::Result<PathBuf> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let aside = path.with_file_name(format!("{stem}.corrupt-{secs}{ext}"));
    fs::rename(path, &aside)?;
    Ok(aside)
}

/// One lock per file: two saves of the same file (timer and exit) must not
/// write the temporary file at the same time.
fn file_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: Mutex<Option<HashMap<PathBuf, Arc<Mutex<()>>>>> = Mutex::new(None);
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    locks
        .get_or_insert_with(HashMap::new)
        .entry(path.to_path_buf())
        .or_default()
        .clone()
}

/// Write to a temporary file then rename it, so a crash never leaves a
/// truncated file behind. The replaced version is kept as `<path>.bak`,
/// used if the file is ever found corrupted. On Unix only the user can read
/// them (keys, chats).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let lock = file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut tmp: OsString = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    keep_backup(path);
    fs::rename(&tmp, path)
}

/// Best effort: a hard link costs nothing, a copy where links are unsupported.
pub fn keep_backup(path: &Path) {
    if !path.exists() {
        return;
    }
    let backup = backup_path(path);
    let _ = fs::remove_file(&backup);
    if fs::hard_link(path, &backup).is_err() {
        let _ = fs::copy(path, &backup);
    }
}

/// Create a folder only the user can open (on Unix).
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "p2pshare-store-{name}-{}",
            rand::random::<u32>()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn corrupted_file_is_kept_and_backup_used() {
        let dir = temp_dir("corrupt");
        let path = dir.join("data.json");
        let mut store = JsonStore::load(path.clone(), vec![0u32]);
        store.data = vec![1];
        store.save_now().unwrap();
        store.data = vec![2];
        store.save_now().unwrap();
        fs::write(&path, b"{ not json").unwrap();

        let store = JsonStore::load(path.clone(), Vec::<u32>::new());
        assert_eq!(store.data, vec![1]);
        let aside = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().starts_with("data.corrupt-"));
        assert!(aside);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_file_is_encrypted_and_undecryptable_one_set_aside() {
        let dir = temp_dir("sealed");
        let path = dir.join("chat.json");
        fs::write(&path, b"[1,2]").unwrap();
        let store = JsonStore::load_sealed(path.clone(), Vec::<u32>::new());
        assert_eq!(store.data, vec![1, 2]);
        assert!(vault::is_sealed(&fs::read(&path).unwrap()));
        assert!(vault::is_sealed(&fs::read(backup_path(&path)).unwrap()));
        assert_eq!(JsonStore::load_sealed(path.clone(), Vec::<u32>::new()).data, vec![1, 2]);

        // Sealed with a key we no longer have: kept aside, never written over.
        let mut foreign = fs::read(&path).unwrap();
        *foreign.last_mut().unwrap() ^= 1;
        fs::write(&path, &foreign).unwrap();
        let _ = fs::remove_file(backup_path(&path));
        let store = JsonStore::load_sealed(path.clone(), Vec::<u32>::new());
        assert!(store.data.is_empty() && store.is_writable() && !path.exists());
        let aside = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .find(|e| e.file_name().to_string_lossy().starts_with("chat.corrupt-"))
            .unwrap();
        assert_eq!(fs::read(aside.path()).unwrap(), foreign);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = temp_dir("missing");
        let store = JsonStore::load(dir.join("none.json"), vec![7u32]);
        assert_eq!(store.data, vec![7]);
        assert!(store.writable);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_saves_stay_valid() {
        let dir = temp_dir("concurrent");
        let path = dir.join("data.json");
        let threads: Vec<_> = (0..8u32)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let json = serde_json::to_vec(&vec![i; 10_000]).unwrap();
                    for _ in 0..5 {
                        write_atomic(&path, &json).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let data: Vec<u32> = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(data.len(), 10_000);
        let _ = fs::remove_dir_all(&dir);
    }
}
