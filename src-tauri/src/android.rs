//! What Android only gives to its own code: the Kotlin plugin in
//! `gen/android/app/src/main/java/com/p2pshare/app/P2PSharePlugin.kt`
//! (background service, notifications that open a chat, documents picked
//! through the system, opening files, multicast, the Keystore).
//!
//! Every call goes through the UI thread of the app and waits for its
//! answer: long ones (copying a document) are done on a thread of their own
//! on the Kotlin side, and are called here from a blocking thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::plugin::mobile::{ErrorResponse, PluginInvokeError};
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{AppHandle, Wry};
use tauri_plugin_dialog::FilePath;

static PLUGIN: OnceLock<PluginHandle<Wry>> = OnceLock::new();
/// The activity is on screen (between its onResume and onPause).
static FOREGROUND: AtomicBool = AtomicBool::new(false);
static DIRS: OnceLock<Dirs> = OnceLock::new();

#[derive(Deserialize)]
struct Dirs {
    /// Download/P2PShare in the shared storage, where users look.
    public: PathBuf,
    /// The app's own folder in the external storage, when the shared one
    /// cannot be written (Android 9 and 10 without the permission).
    fallback: PathBuf,
}

pub fn init() -> TauriPlugin<Wry> {
    Builder::new("p2pshare-android")
        .setup(|_app, api| {
            let handle = api.register_android_plugin("com.p2pshare.app", "P2PSharePlugin")?;
            let _ = PLUGIN.set(handle);
            Ok(())
        })
        .build()
}

fn call<T: DeserializeOwned>(command: &str, args: Value) -> Result<T, PluginInvokeError> {
    let plugin = PLUGIN.get().ok_or_else(|| {
        PluginInvokeError::InvokeRejected(ErrorResponse {
            code: None,
            message: Some("module Android absent".into()),
            data: (),
        })
    })?;
    plugin.run_mobile_plugin(command, args)
}

fn call_unit(command: &str, args: Value) {
    if let Err(e) = call::<Value>(command, args) {
        eprintln!("android {command}: {e}");
    }
}

fn message(e: PluginInvokeError) -> String {
    match e {
        PluginInvokeError::InvokeRejected(r) => {
            r.message.unwrap_or_else(|| "erreur inconnue".into())
        }
        e => e.to_string(),
    }
}

/// Hear the activity: notification taps (open that chat) and whether it is
/// on screen.
pub fn listen(app: &AppHandle) {
    let app = app.clone();
    let channel: Channel<Value> = Channel::new(move |body| {
        let InvokeResponseBody::Json(json) = body else {
            return Ok(());
        };
        let Ok(event) = serde_json::from_str::<Value>(&json) else {
            return Ok(());
        };
        if let Some(foreground) = event.get("foreground").and_then(Value::as_bool) {
            FOREGROUND.store(foreground, Ordering::SeqCst);
        }
        let kind = match event.get("kind").and_then(Value::as_str) {
            Some("friend") => "friend",
            Some("group") => "group",
            Some("localsend") => "localsend",
            _ => return Ok(()),
        };
        if let Some(id) = event.get("id").and_then(Value::as_str) {
            crate::open_chat(&app, kind, id.to_string());
        }
        Ok(())
    });
    call_unit("listen", json!({ "channel": channel }));
}

pub fn foreground() -> bool {
    FOREGROUND.load(Ordering::SeqCst)
}

/// Run (or stop) the foreground service that keeps the app reachable.
pub fn set_service(enabled: bool) {
    call_unit("setService", json!({ "enabled": enabled }));
}

/// A notification; with a chat, tapping it opens that chat.
pub fn notify(title: &str, body: &str, chat: Option<(&str, &str)>) {
    let (kind, target) = chat.unzip();
    call_unit(
        "notify",
        json!({ "title": title, "body": body, "kind": kind, "target": target }),
    );
}

pub fn hold_multicast() {
    call_unit("holdMulticast", json!({}));
}

/// Where received files go: Download/P2PShare if it can be written, else
/// the app's own folder.
pub fn download_dir() -> PathBuf {
    let dirs = DIRS.get_or_init(|| {
        call::<Dirs>("downloadDirs", json!({})).unwrap_or_else(|e| {
            eprintln!("android downloadDirs: {e}");
            Dirs {
                public: PathBuf::from("/storage/emulated/0/Download/P2PShare"),
                fallback: PathBuf::from("/storage/emulated/0/Download/P2PShare"),
            }
        })
    });
    if std::fs::create_dir_all(&dirs.public).is_ok() {
        dirs.public.clone()
    } else {
        dirs.fallback.clone()
    }
}

/// Open a file with the app the user picks for its type.
pub fn open_file(path: &Path) {
    call_unit("openFile", json!({ "path": path }));
}

/// "Show in folder": the system's Downloads view (no file manager is sure
/// to be there, nor to take a folder).
pub fn show_downloads() {
    call_unit("showDownloads", json!({}));
}

#[derive(Deserialize)]
struct Imported {
    path: PathBuf,
}

/// Local paths for what the system picker gave: its documents (content://)
/// are copied into the app, to the cache for a send (`keep` false), to the
/// app's files for what is shared until removed (group files). Blocking.
pub fn local_paths(files: Vec<FilePath>, keep: bool) -> Vec<PathBuf> {
    files
        .into_iter()
        .filter_map(|file| match file {
            FilePath::Path(p) => Some(p),
            FilePath::Url(url) if url.scheme() == "file" => url.to_file_path().ok(),
            FilePath::Url(url) => {
                call::<Imported>("importUri", json!({ "uri": url.as_str(), "keep": keep }))
                    .map(|i| i.path)
                    .inspect_err(|e| eprintln!("{url} not imported: {e}"))
                    .ok()
            }
        })
        .collect()
}

#[derive(Deserialize)]
struct Exported {
    name: String,
}

/// Copy a finished file into the document the user chose. The document's
/// name. Blocking.
pub fn export_to(path: &Path, uri: &str) -> Result<String, String> {
    call::<Exported>("exportUri", json!({ "path": path, "uri": uri }))
        .map(|e| e.name)
        .map_err(message)
}

/// Start the app again, in a new process (settings read at start).
pub fn restart() {
    call_unit("restart", json!({}));
}

// ─── The history key ─────────────────────────────────────────────────────────

pub enum KeystoreError {
    /// Gone from the Keystore, or not sealed by its key: never coming back.
    Lost(String),
    /// Not this time.
    Unavailable(String),
}

#[derive(Deserialize)]
struct Data {
    data: String,
}

/// Seal the key with an AES-GCM key kept by the Android Keystore (made on
/// first use): what to write in the key file.
pub fn keystore_wrap(raw: &[u8]) -> Result<String, String> {
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(raw);
    call::<Data>("keystoreWrap", json!({ "data": data }))
        .map(|d| d.data)
        .map_err(message)
}

pub fn keystore_unwrap(blob: &str) -> Result<Vec<u8>, KeystoreError> {
    use base64::Engine as _;
    match call::<Data>("keystoreUnwrap", json!({ "data": blob })) {
        Ok(d) => base64::engine::general_purpose::STANDARD
            .decode(d.data)
            .map_err(|e| KeystoreError::Unavailable(e.to_string())),
        Err(PluginInvokeError::InvokeRejected(r))
            if r.code.as_deref() == Some("lost") =>
        {
            Err(KeystoreError::Lost(r.message.unwrap_or_default()))
        }
        Err(e) => Err(KeystoreError::Unavailable(message(e))),
    }
}
