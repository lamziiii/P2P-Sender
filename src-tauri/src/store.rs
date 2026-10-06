use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Small JSON file store: synchronous load, explicit (atomic) saves.
/// `mark_dirty` + `take_dirty` let a caller batch frequent writes.
pub struct JsonStore<T> {
    path: PathBuf,
    pub data: T,
    dirty: bool,
}

impl<T: Serialize + DeserializeOwned> JsonStore<T> {
    pub fn load(path: PathBuf, default: T) -> Self {
        // A missing or corrupted file falls back to the defaults.
        let data = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(default);
        Self {
            path,
            data,
            dirty: false,
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Serialized snapshot to write if something changed since the last call.
    pub fn take_dirty(&mut self) -> Option<(PathBuf, Vec<u8>)> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        serde_json::to_vec(&self.data)
            .ok()
            .map(|json| (self.path.clone(), json))
    }

    pub fn save_now(&mut self) {
        self.dirty = false;
        if let Ok(json) = serde_json::to_vec(&self.data) {
            let _ = write_atomic(&self.path, &json);
        }
    }
}

/// Write to a temporary file then rename it, so a crash never leaves a
/// truncated file behind.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}
