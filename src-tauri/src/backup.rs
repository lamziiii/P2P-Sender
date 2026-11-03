//! Backup of the identity and what goes with it (friends, groups, settings,
//! optionally the chats) in one file protected by a password, and its
//! restore on this or another computer.
//!
//! File: `MAGIC`, format version (1 byte), PBKDF2-HMAC-SHA256 iterations
//! (u32, big endian), salt (16 bytes), nonce (12 bytes), then the payload
//! (JSON, see `Payload`) sealed with ChaCha20-Poly1305 under the key derived
//! from the password; the header is authenticated along with it.
//!
//! The files are kept as they are on disk (history encrypted with the data
//! folder's key), with that key in the payload. A restore is prepared in
//! `PENDING`, inside the data folder, then put in place at the next start
//! (`apply_pending`), before anything is opened: the data it replaces is
//! moved to a dated folder next to it, never deleted.

use std::fs;
use std::io::{self, ErrorKind};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
#[cfg(desktop)]
use tauri::Manager;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::{store, vault, AppState, SHOW_AFTER_RESTART};

const MAGIC: &[u8] = b"P2PSHARE-BACKUP\0";
const VERSION: u8 = 1;
/// Of the payload: older apps refuse a newer one.
const FORMAT: u32 = 1;
const ITERATIONS: u32 = 600_000;
/// Beyond that, a file would make us compute for minutes.
const MAX_ITERATIONS: u32 = 20_000_000;
const SALT_LEN: usize = 16;
const HEADER_LEN: usize = MAGIC.len() + 1 + 4 + SALT_LEN + NONCE_LEN;
const MAX_FILE: u64 = 1 << 30;
pub const MIN_PASSWORD: usize = 8;
const EXTENSION: &str = "p2pbackup";

/// The restore waiting for the next start, inside the data folder.
const PENDING: &str = "restauration-en-attente";
/// While it is being written.
const PENDING_TMP: &str = "restauration-en-cours";
/// The data replaced by a restore: `<ASIDE_PREFIX><date>`.
const ASIDE_PREFIX: &str = "avant-restauration-";

/// Saved in every backup.
const FILES: &[&str] = &[
    "identity.key",
    "friends.json",
    // Sealed with the history key, which the backup holds: links still open
    // keep working once restored.
    "invites.json",
    "config.json",
    "lan.json",
    "localsend.json",
];
/// Saved only with the chats.
const CHAT_FILES: &[&str] = &["localsend-chats.json"];
/// Folders whose files are saved (not subfolders): groups always, chats
/// with friends on request.
const GROUPS_DIR: &str = "groups";
const CHATS_DIR: &str = "messages";
/// What a restore replaces: every entry of the data folder whose name starts
/// with one of these (with its backup, a file set aside…).
const REPLACED: &[&str] = &[
    "identity.",
    "friends.",
    "invites.",
    "config.",
    "lan.",
    "localsend.",
    "localsend-chats.",
    vault::KEY_FILE,
    "messages",
    "groups",
];

const NOT_A_BACKUP: &str = "Ce fichier n'est pas une sauvegarde P2P Share.";
const NEWER: &str = "Cette sauvegarde vient d'une version plus récente de P2P Share : \
                     mettez l'application à jour pour l'importer.";
const WRONG_PASSWORD: &str = "Mot de passe incorrect, ou fichier de sauvegarde endommagé.";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    format: u32,
    created_ms: u64,
    app_version: String,
    /// Our ID when it was made.
    id: String,
    chats: bool,
    /// The key the history in `files` is encrypted with (hex).
    vault_key: String,
    /// Path in the data folder (`/` separated) → content (base64).
    files: std::collections::BTreeMap<String, String>,
}

fn derive_key(password: &str, salt: &[u8], iterations: u32) -> io::Result<LessSafeKey> {
    let iterations = NonZeroU32::new(iterations).ok_or_else(|| io::Error::other(NOT_A_BACKUP))?;
    let mut raw = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        password.as_bytes(),
        &mut raw,
    );
    let key = UnboundKey::new(&CHACHA20_POLY1305, &raw).map_err(|_| io::Error::other("key"))?;
    Ok(LessSafeKey::new(key))
}

fn encrypt(payload: &[u8], password: &str) -> io::Result<Vec<u8>> {
    let rng = SystemRandom::new();
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    rng.fill(&mut salt)
        .and_then(|()| rng.fill(&mut nonce))
        .map_err(|_| io::Error::other("system random unavailable"))?;
    let mut out = MAGIC.to_vec();
    out.push(VERSION);
    out.extend_from_slice(&ITERATIONS.to_be_bytes());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    let key = derive_key(password, &salt, ITERATIONS)?;
    let mut body = payload.to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(&out[..]),
        &mut body,
    )
    .map_err(|_| io::Error::other("encryption failed"))?;
    out.append(&mut body);
    Ok(out)
}

/// The payload of a backup file, or a French message saying why not.
fn decrypt(data: &[u8], password: &str) -> Result<Vec<u8>, String> {
    if !data.starts_with(MAGIC) || data.len() <= MAGIC.len() {
        return Err(NOT_A_BACKUP.into());
    }
    if data[MAGIC.len()] > VERSION {
        return Err(NEWER.into());
    }
    if data[MAGIC.len()] != VERSION || data.len() < HEADER_LEN + CHACHA20_POLY1305.tag_len() {
        return Err(NOT_A_BACKUP.into());
    }
    let (header, body) = data.split_at(HEADER_LEN);
    let at = MAGIC.len() + 1;
    let iterations = u32::from_be_bytes(header[at..at + 4].try_into().expect("4 bytes"));
    if iterations > MAX_ITERATIONS {
        return Err(NOT_A_BACKUP.into());
    }
    let salt = &header[at + 4..at + 4 + SALT_LEN];
    let nonce = Nonce::try_assume_unique_for_key(&header[at + 4 + SALT_LEN..])
        .map_err(|_| NOT_A_BACKUP.to_string())?;
    let key = derive_key(password, salt, iterations).map_err(|e| e.to_string())?;
    let mut body = body.to_vec();
    let len = key
        .open_in_place(nonce, Aad::from(header), &mut body)
        .map_err(|_| WRONG_PASSWORD.to_string())?
        .len();
    body.truncate(len);
    Ok(body)
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Files that are ours in a folder: not a backup, a temporary file or one
/// set aside.
fn kept(name: &str) -> bool {
    !(name.ends_with(".bak")
        || name.ends_with(".tmp")
        || name.contains(".corrupt-")
        || name.contains(".migrated"))
}

fn collect(dir: &Path, chats: bool, id: String) -> io::Result<Payload> {
    let mut files = std::collections::BTreeMap::new();
    let mut add = |rel: String, path: &Path| -> io::Result<()> {
        match fs::read(path) {
            Ok(bytes) => {
                files.insert(rel, b64().encode(bytes));
                Ok(())
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io::Error::new(e.kind(), format!("{} : {e}", path.display()))),
        }
    };
    let top = FILES.iter().chain(if chats { CHAT_FILES } else { &[] });
    for name in top {
        add(name.to_string(), &dir.join(name))?;
    }
    let folders = [GROUPS_DIR].into_iter().chain(chats.then_some(CHATS_DIR));
    for folder in folders {
        let Ok(entries) = fs::read_dir(dir.join(folder)) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if kept(&name) && entry.file_type().is_ok_and(|t| t.is_file()) {
                add(format!("{folder}/{name}"), &entry.path())?;
            }
        }
    }
    // Plain inside the backup (sealed by its password): the computer it is
    // restored on seals it with its own key at the next start.
    let Some(identity) = files.get_mut("identity.key") else {
        return Err(io::Error::other("identity.key introuvable"));
    };
    let bytes = b64()
        .decode(identity.as_bytes())
        .map_err(|_| io::Error::other("identity.key illisible"))?;
    if vault::is_sealed(&bytes) {
        *identity = b64().encode(vault::open(&bytes)?);
    }
    Ok(Payload {
        format: FORMAT,
        created_ms: crate::now_ms(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        id,
        chats,
        vault_key: hex::encode(vault::raw_key()?),
        files,
    })
}

/// A path of a payload, if it is one a backup may hold.
fn allowed_path(rel: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = rel.split('/').collect();
    let plain = |s: &str| {
        !s.is_empty() && s != "." && s != ".." && !s.contains(['\\', ':']) && kept(s)
    };
    match parts[..] {
        [name] if FILES.contains(&name) || CHAT_FILES.contains(&name) => Some(PathBuf::from(name)),
        [folder, name] if (folder == GROUPS_DIR || folder == CHATS_DIR) && plain(name) => {
            Some(Path::new(folder).join(name))
        }
        _ => None,
    }
}

/// Check a decrypted payload and write it, ready, to `PENDING`.
fn prepare(dir: &Path, payload: &[u8]) -> Result<(), String> {
    let format: serde_json::Value =
        serde_json::from_slice(payload).map_err(|_| NOT_A_BACKUP.to_string())?;
    if format["format"].as_u64().is_some_and(|f| f > FORMAT as u64) {
        return Err(NEWER.into());
    }
    let payload: Payload =
        serde_json::from_value(format).map_err(|_| NOT_A_BACKUP.to_string())?;
    let vault_key: [u8; 32] = hex::decode(&payload.vault_key)
        .ok()
        .and_then(|k| k.try_into().ok())
        .ok_or_else(|| NOT_A_BACKUP.to_string())?;
    let mut files = Vec::new();
    for (rel, content) in &payload.files {
        let path = allowed_path(rel).ok_or_else(|| NOT_A_BACKUP.to_string())?;
        let bytes = b64().decode(content).map_err(|_| NOT_A_BACKUP.to_string())?;
        files.push((path, bytes));
    }
    let identity_ok = files.iter().any(|(path, bytes)| {
        path == Path::new("identity.key")
            && std::str::from_utf8(bytes)
                .ok()
                .and_then(|t| hex::decode(t.trim()).ok())
                .is_some_and(|seed| seed.len() == 32)
    });
    if !identity_ok {
        return Err(NOT_A_BACKUP.into());
    }

    let failed = |e: io::Error| format!("Préparation de la restauration impossible : {e}");
    let tmp = dir.join(PENDING_TMP);
    // Ours, left by an attempt cut short.
    if tmp.exists() {
        fs::remove_dir_all(&tmp).map_err(failed)?;
    }
    store::create_private_dir(&tmp).map_err(failed)?;
    for (path, mut bytes) in files {
        if path == Path::new("config.json") {
            bytes = adapt_config(bytes);
        }
        let target = tmp.join(&path);
        if let Some(parent) = target.parent() {
            store::create_private_dir(parent).map_err(failed)?;
        }
        store::write_atomic(&target, &bytes).map_err(failed)?;
        let _ = fs::remove_file(PathBuf::from(format!("{}.bak", target.display())));
    }
    vault::write_key(&tmp, &vault_key).map_err(failed)?;
    let _ = fs::remove_file(tmp.join(format!("{}.bak", vault::KEY_FILE)));
    // An earlier restore not applied yet is replaced by this one.
    let pending = dir.join(PENDING);
    if pending.exists() {
        fs::remove_dir_all(&pending).map_err(failed)?;
    }
    fs::rename(&tmp, &pending).map_err(failed)
}

/// A download folder that does not exist here falls back to the default;
/// the welcome is not shown again.
fn adapt_config(bytes: Vec<u8>) -> Vec<u8> {
    let Ok(mut config) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return bytes;
    };
    let Some(map) = config.as_object_mut() else {
        return bytes;
    };
    let missing = map
        .get("downloadPath")
        .and_then(|p| p.as_str())
        .is_some_and(|p| !p.is_empty() && !Path::new(p).is_dir());
    if missing {
        map.insert("downloadPath".into(), "".into());
    }
    map.insert("welcomed".into(), true.into());
    serde_json::to_vec(&config).unwrap_or(bytes)
}

/// `YYYY-MM-DD` and `HH-MM-SS` (UTC) of a unix time in ms.
fn date_time(ms: u64) -> (String, String) {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Days to civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        format!("{y:04}-{m:02}-{d:02}"),
        format!("{:02}-{:02}-{:02}", rem / 3600, rem / 60 % 60, rem % 60),
    )
}

fn rename_retrying(from: &Path, to: &Path) -> io::Result<()> {
    // The previous run may still be letting go of its files.
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Err(e) if attempt < 25 && e.kind() != ErrorKind::NotFound => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
            result => return result,
        }
    }
}

/// Put a prepared restore in place: the replaced data moves to a dated
/// folder. All or nothing: on failure everything goes back where it was
/// and it is tried again at the next start.
pub fn apply_pending(dir: &Path) {
    let pending = dir.join(PENDING);
    if !pending.is_dir() {
        return;
    }
    let (date, time) = date_time(crate::now_ms());
    let mut aside = dir.join(format!("{ASIDE_PREFIX}{date}_{time}"));
    while aside.exists() {
        aside = dir.join(format!("{ASIDE_PREFIX}{date}_{time}-{}", rand::random::<u16>()));
    }
    if let Err(e) = store::create_private_dir(&aside) {
        eprintln!("restore not applied ({} not created: {e})", aside.display());
        return;
    }
    let names = |folder: &Path, filter: &dyn Fn(&str) -> bool| -> Vec<String> {
        fs::read_dir(folder)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| filter(n))
            .collect()
    };
    let current = names(dir, &|n| REPLACED.iter().any(|p| n.starts_with(p)));
    let restored = names(&pending, &|_| true);
    // (from, to) of what was moved, to undo it.
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut result = Ok(());
    for name in &current {
        let (from, to) = (dir.join(name), aside.join(name));
        result = rename_retrying(&from, &to);
        if result.is_err() {
            break;
        }
        moved.push((from, to));
    }
    if result.is_ok() {
        for name in &restored {
            let (from, to) = (pending.join(name), dir.join(name));
            result = rename_retrying(&from, &to);
            if result.is_err() {
                break;
            }
            moved.push((from, to));
        }
    }
    match result {
        Ok(()) => {
            let _ = fs::remove_dir(&pending);
            eprintln!("backup restored; former data in {}", aside.display());
        }
        Err(e) => {
            eprintln!("restore not applied ({e}): tried again next start");
            for (from, to) in moved.iter().rev() {
                if let Err(e) = rename_retrying(to, from) {
                    eprintln!("{} not moved back: {e}", to.display());
                }
            }
            let _ = fs::remove_dir(&aside);
        }
    }
}

/// The file chosen, and on Android the document to copy the backup into
/// once written.
async fn pick(app: &AppHandle, save: bool) -> Option<(PathBuf, Option<String>)> {
    let _dialog = crate::DialogOpen::new();
    let (tx, rx) = tokio::sync::oneshot::channel();
    #[cfg_attr(mobile, allow(unused_mut))]
    let mut dialog = app
        .dialog()
        .file()
        .add_filter("Sauvegarde P2P Share", &[EXTENSION]);
    #[cfg(desktop)]
    if let Some(w) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&w);
    }
    if save {
        let (date, _) = date_time(crate::now_ms());
        dialog
            .set_title("Enregistrer la sauvegarde")
            .set_file_name(format!("P2PShare-sauvegarde-{date}.{EXTENSION}"))
            .save_file(move |p| {
                let _ = tx.send(p);
            });
    } else {
        dialog
            .set_title("Choisir une sauvegarde à restaurer")
            .pick_file(move |p| {
                let _ = tx.send(p);
            });
    }
    let picked = rx.await.ok().flatten()?;
    // Android's picker gives a document: a backup is written in the app,
    // then copied into it; one to restore is copied into the app first.
    #[cfg(target_os = "android")]
    if let tauri_plugin_dialog::FilePath::Url(url) = &picked {
        if url.scheme() != "file" {
            if save {
                let dir = tauri::Manager::path(app).app_cache_dir().ok()?;
                let _ = fs::create_dir_all(&dir);
                return Some((dir.join(format!("export.{EXTENSION}")), Some(url.to_string())));
            }
            let picked = vec![picked];
            return tauri::async_runtime::spawn_blocking(move || {
                crate::android::local_paths(picked, false)
            })
            .await
            .ok()?
            .pop()
            .map(|p| (p, None));
        }
    }
    picked.into_path().ok().map(|p| (p, None))
}

/// Ask where, then write the backup there. The path written, or `None` if
/// the dialog was closed.
#[tauri::command]
pub async fn export_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    password: String,
    include_chats: bool,
) -> Result<Option<String>, String> {
    if password.chars().count() < MIN_PASSWORD {
        return Err(format!(
            "Le mot de passe doit faire au moins {MIN_PASSWORD} caractères."
        ));
    }
    #[cfg_attr(not(target_os = "android"), allow(unused_variables))]
    let Some((mut path, document)) = pick(&app, true).await else {
        return Ok(None);
    };
    if path.extension().is_none() {
        path.set_extension(EXTENSION);
    }
    // What is still in memory goes to disk first.
    state.node.save_now();
    state.save_localsend_chats();
    let _ = state.config().save_now();
    let (dir, id) = (state.data_dir.clone(), state.node.id());
    let target = path.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let payload = collect(&dir, include_chats, id)
            .map_err(|e| format!("Lecture des données impossible : {e}"))?;
        let json = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
        let bytes = encrypt(&json, &password).map_err(|e| e.to_string())?;
        store::write_atomic(&target, &bytes)
            .map_err(|e| format!("Écriture de la sauvegarde impossible : {e}"))?;
        // Not in the user's folder: the file replaced, if any.
        let _ = fs::remove_file(PathBuf::from(format!("{}.bak", target.display())));
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;
    #[cfg(target_os = "android")]
    if let Some(uri) = document {
        let written = path.clone();
        let name = tauri::async_runtime::spawn_blocking(move || {
            let copied = crate::android::export_to(&written, &uri);
            let _ = fs::remove_file(&written);
            copied
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("Écriture de la sauvegarde impossible : {e}"))?;
        return Ok(Some(name));
    }
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Ask for a backup file, check it with `password`, then restart on it.
/// `false` if the dialog was closed; never returns otherwise but with an
/// error.
#[tauri::command]
pub async fn import_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    password: String,
) -> Result<bool, String> {
    let Some((path, _)) = pick(&app, false).await else {
        return Ok(false);
    };
    let dir = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let read = |e: io::Error| format!("Lecture du fichier impossible : {e}");
        if fs::metadata(&path).map_err(read)?.len() > MAX_FILE {
            return Err(NOT_A_BACKUP.into());
        }
        let data = fs::read(&path).map_err(read)?;
        let payload = decrypt(&data, &password)?;
        prepare(&dir, &payload)
    })
    .await
    .map_err(|e| e.to_string())??;
    std::env::set_var(SHOW_AFTER_RESTART, "1");
    crate::restart_app(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_and_format_checks() {
        let sealed = encrypt(b"{}", "correct horse").unwrap();
        assert_eq!(decrypt(&sealed, "correct horse").unwrap(), b"{}");
        assert_eq!(decrypt(&sealed, "wrong horse").unwrap_err(), WRONG_PASSWORD);
        assert_eq!(decrypt(b"PK\x03\x04 zip", "x").unwrap_err(), NOT_A_BACKUP);
        let mut newer = sealed.clone();
        newer[MAGIC.len()] = VERSION + 1;
        assert_eq!(decrypt(&newer, "correct horse").unwrap_err(), NEWER);
        assert_eq!(date_time(1_792_454_400_000).0, "2026-10-20");
        assert!(allowed_path("../identity.key").is_none());
        assert!(allowed_path("groups/..").is_none());
        assert!(allowed_path("groups/abc.jsonl").is_some());
    }

    #[test]
    fn restore_replaces_and_keeps_former_data() {
        let dir = std::env::temp_dir().join(format!("p2pshare-backup-{}", rand::random::<u32>()));
        let source = dir.join("source");
        fs::create_dir_all(source.join("groups")).unwrap();
        fs::write(source.join("identity.key"), "11".repeat(32)).unwrap();
        fs::write(source.join("friends.json"), "[]").unwrap();
        fs::write(source.join("groups").join("g.jsonl"), "line\n").unwrap();
        let payload = serde_json::to_vec(&collect(&source, true, "me".into()).unwrap()).unwrap();

        let data = dir.join("data");
        fs::create_dir_all(data.join("messages")).unwrap();
        fs::write(data.join("identity.key"), "22".repeat(32)).unwrap();
        fs::write(data.join("messages").join("a.json"), "[]").unwrap();
        fs::write(data.join("startup-error.log"), "kept").unwrap();
        prepare(&data, &payload).unwrap();
        apply_pending(&data);

        assert_eq!(fs::read_to_string(data.join("identity.key")).unwrap(), "11".repeat(32));
        assert!(data.join("groups").join("g.jsonl").exists());
        assert!(data.join(vault::KEY_FILE).exists());
        assert!(!data.join("messages").exists() && !data.join(PENDING).exists());
        assert!(data.join("startup-error.log").exists());
        let aside = fs::read_dir(&data)
            .unwrap()
            .filter_map(Result::ok)
            .find(|e| e.file_name().to_string_lossy().starts_with(ASIDE_PREFIX))
            .unwrap()
            .path();
        assert_eq!(fs::read_to_string(aside.join("identity.key")).unwrap(), "22".repeat(32));
        assert!(aside.join("messages").join("a.json").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
