mod notify;
pub mod p2p;
pub mod store;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::SecretKey;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, RunEvent, State, WebviewWindow,
    WebviewWindowBuilder,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use p2p::{ChatMessage, Event, FriendView, Messages, Node, TransferView};
use store::JsonStore;

const WINDOW_MARGIN: f64 = 12.0;
const HIDDEN_ARG: &str = "--hidden";

static QUITTING: AtomicBool = AtomicBool::new(false);
static PENDING_CHAT: Mutex<Option<String>> = Mutex::new(None);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    #[serde(default)]
    download_path: String,
    #[serde(default = "enabled")]
    auto_launch: bool,
}

fn enabled() -> bool {
    true
}

struct AppState {
    node: Node,
    config: Arc<Mutex<JsonStore<Config>>>,
}

impl AppState {
    fn config(&self) -> std::sync::MutexGuard<'_, JsonStore<Config>> {
        self.config.lock().unwrap_or_else(|e| e.into_inner())
    }
}

// ─── Commands (same API as the former Electron preload) ─────────────────────

#[tauri::command]
async fn hide_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        hide_to_tray(&w);
    }
}

#[tauri::command]
fn get_my_id(state: State<AppState>) -> String {
    state.node.id()
}

#[tauri::command]
fn get_friends(state: State<AppState>) -> Vec<FriendView> {
    state.node.friends()
}

#[tauri::command]
fn add_friend(state: State<AppState>, id: String, name: String) -> Value {
    match state.node.add_friend(&id, &name) {
        Ok(friends) => json!({ "friends": friends }),
        Err(error) => json!({ "error": error }),
    }
}

#[tauri::command]
fn remove_friend(state: State<AppState>, id: String) -> Vec<FriendView> {
    state.node.remove_friend(&id)
}

#[tauri::command]
fn get_config(state: State<AppState>) -> Config {
    state.config().data.clone()
}

#[tauri::command]
async fn select_download_dir(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, ()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Choisir le dossier de téléchargement");
    if let Some(w) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&w);
    }
    dialog.pick_folder(move |dir| {
        let _ = tx.send(dir);
    });
    let Some(dir) = rx.await.ok().flatten().and_then(|d| d.into_path().ok()) else {
        return Ok(None);
    };
    let path = dir.to_string_lossy().into_owned();
    let mut config = state.config();
    config.data.download_path = path.clone();
    config.save_now();
    Ok(Some(path))
}

#[tauri::command]
fn set_auto_launch(app: AppHandle, state: State<AppState>, enable: bool) -> bool {
    let mut config = state.config();
    config.data.auto_launch = enable;
    config.save_now();
    apply_auto_launch(&app, enable);
    enable
}

#[tauri::command]
fn get_messages(state: State<AppState>, friend_id: String) -> Vec<ChatMessage> {
    state.node.messages(&friend_id)
}

#[tauri::command]
fn send_message(state: State<AppState>, friend_id: String, text: String) -> Value {
    match state.node.send_message(&friend_id, &text) {
        Ok(message) => json!(message),
        Err(error) => json!({ "error": error }),
    }
}

#[tauri::command]
async fn send_file(
    app: AppHandle,
    state: State<'_, AppState>,
    friend_id: String,
) -> Result<usize, ()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Sélectionner les fichiers à envoyer");
    if let Some(w) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&w);
    }
    dialog.pick_files(move |files| {
        let _ = tx.send(files);
    });
    let files = rx.await.ok().flatten().unwrap_or_default();
    let paths: Vec<PathBuf> = files
        .into_iter()
        .filter_map(|f| f.into_path().ok())
        .collect();
    if paths.is_empty() {
        return Ok(0);
    }
    state.node.send_files(&friend_id, &paths);
    Ok(paths.len())
}

#[tauri::command]
fn get_transfers(state: State<AppState>) -> Vec<TransferView> {
    state.node.transfers()
}

#[tauri::command]
fn clear_finished(state: State<AppState>) {
    state.node.clear_finished();
}

#[tauri::command]
fn take_open_chat() -> Option<String> {
    PENDING_CHAT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
}

#[tauri::command]
fn respond_to_file_offer(state: State<AppState>, id: String, accept: bool) {
    state.node.respond_to_offer(&id, accept);
}

#[tauri::command]
fn cancel_transfer(state: State<AppState>, id: String) {
    state.node.cancel_transfer(&id);
}

#[tauri::command]
fn show_in_folder(app: AppHandle, state: State<AppState>, id: String) {
    if let Some(path) = state.node.saved_path(&id) {
        let _ = app.opener().reveal_item_in_dir(path);
    }
}

// ─── Window & tray ───────────────────────────────────────────────────────────

/// In the tray the window is destroyed, which frees the whole web view
/// (about 100 MB); it is rebuilt from the config when shown again.
fn hide_to_tray(w: &WebviewWindow) {
    let _ = w.hide();
    let _ = w.destroy();
}

fn main_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window("main")
}

/// Show the window, creating it if needed. Returns true if it was created.
fn show_window(app: &AppHandle) -> bool {
    let (w, created) = match main_window(app) {
        Some(w) => (w, false),
        None => {
            let Some(config) = app.config().app.windows.iter().find(|c| c.label == "main") else {
                return false;
            };
            match WebviewWindowBuilder::from_config(app, config).and_then(|b| b.build()) {
                Ok(w) => (w, true),
                Err(_) => return false,
            }
        }
    };
    if w.is_minimized().unwrap_or(false) {
        let _ = w.unminimize();
    }
    position_window(&w);
    let _ = w.show();
    let _ = w.set_focus();
    created
}

fn toggle_window(app: &AppHandle) {
    match main_window(app) {
        Some(w) if w.is_visible().unwrap_or(false) => hide_to_tray(&w),
        _ => {
            show_window(app);
        }
    }
}

/// Open a chat from outside the UI (notification click). A freshly created
/// window has not loaded yet, so it picks the chat up once mounted.
fn open_chat(app: &AppHandle, friend_id: String) {
    if show_window(app) {
        *PENDING_CHAT.lock().unwrap_or_else(|e| e.into_inner()) = Some(friend_id);
    } else {
        let _ = app.emit("open-chat", friend_id);
    }
}

/// Anchor the window in the corner of the work area next to the taskbar,
/// like the OneDrive flyout (bottom-right on Windows, top-right on macOS).
fn position_window(w: &WebviewWindow) {
    let app = w.app_handle();
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|c| app.monitor_from_point(c.x, c.y).ok().flatten())
        .or_else(|| w.current_monitor().ok().flatten())
        .or_else(|| w.primary_monitor().ok().flatten());
    let (Some(m), Ok(size)) = (monitor, w.outer_size()) else {
        return;
    };
    let margin = (WINDOW_MARGIN * m.scale_factor()).round() as i32;
    let area = m.work_area();
    let (ax, ay) = (area.position.x, area.position.y);
    let (aw, ah) = (area.size.width as i32, area.size.height as i32);
    let taskbar_left = ax > m.position().x;
    let taskbar_top = ay > m.position().y;
    let x = if taskbar_left {
        ax + margin
    } else {
        ax + aw - size.width as i32 - margin
    };
    let y = if taskbar_top {
        ay + margin
    } else {
        ay + ah - size.height as i32 - margin
    };
    let _ = w.set_position(PhysicalPosition::new(x, y));
}

fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let quit = MenuItem::with_id(app, "quit", "Quit P2P Share", true, None::<&str>)?;
    // Linux tray icons do not report clicks: offer the window in the menu.
    #[cfg(target_os = "linux")]
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "show", "Ouvrir P2P Share", true, None::<&str>)?,
            &quit,
        ],
    )?;
    #[cfg(not(target_os = "linux"))]
    let menu = Menu::with_items(app, &[&quit])?;

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("P2P Share")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "quit" => {
                QUITTING.store(true, Ordering::SeqCst);
                app.exit(0);
            }
            "show" => {
                show_window(app);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn apply_auto_launch(app: &AppHandle, enable: bool) {
    // Only installed builds register themselves at login.
    if cfg!(debug_assertions) {
        return;
    }
    let launcher = app.autolaunch();
    let _ = if enable {
        launcher.enable()
    } else {
        launcher.disable()
    };
}

// ─── Events from the P2P core ────────────────────────────────────────────────

fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024f64.powi(3) {
        format!("{:.2} Go", b / 1024f64.powi(3))
    } else {
        format!("{:.2} Mo", b / 1024f64.powi(2))
    }
}

fn friend_name(app: &AppHandle, id: &str) -> String {
    app.try_state::<AppState>()
        .and_then(|s| s.node.friend_name(id))
        .unwrap_or_else(|| "Inconnu".into())
}

fn on_p2p_event(app: &AppHandle, event: Event) {
    match event {
        Event::FriendStatus { friend_id, online } => {
            let _ = app.emit(
                "friend-status",
                json!({ "friendId": friend_id, "online": online }),
            );
        }
        Event::Transfer(t) => {
            let _ = app.emit("transfer-update", t);
        }
        Event::MessageStatus { friend_id, id } => {
            let _ = app.emit(
                "message-status",
                json!({ "friendId": friend_id, "id": id, "status": "delivered" }),
            );
        }
        Event::Message { friend_id, message } => {
            let _ = app.emit(
                "new-message",
                json!({ "friendId": friend_id, "message": message }),
            );
            let focused = main_window(app)
                .and_then(|w| w.is_focused().ok())
                .unwrap_or(false);
            if !focused {
                let handle = app.clone();
                notify::notify(
                    app,
                    &friend_name(app, &friend_id),
                    &message.text,
                    move || open_chat(&handle, friend_id),
                );
            }
        }
        Event::FileOffer(t) => {
            let handle = app.clone();
            notify::notify(
                app,
                &format!("{} vous envoie un fichier", friend_name(app, &t.friend_id)),
                &format!(
                    "\"{}\" ({}) — Cliquez pour accepter ou refuser.",
                    t.file_name,
                    format_size(t.file_size)
                ),
                move || {
                    show_window(&handle);
                },
            );
        }
        Event::TransferComplete(t) => {
            let handle = app.clone();
            notify::notify(
                app,
                &format!("Fichier reçu de {}", friend_name(app, &t.friend_id)),
                &format!("\"{}\" a été enregistré.", t.file_name),
                move || {
                    if let Some(path) = t.saved_path {
                        let _ = handle.opener().reveal_item_in_dir(path);
                    }
                },
            );
        }
    }
}

// ─── Storage ─────────────────────────────────────────────────────────────────

fn data_dir(app: &AppHandle) -> PathBuf {
    if let Some(dir) = std::env::var_os("P2PSHARE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Import the identity, friends and messages of the former Electron app, so
/// the ID shared with friends stays the same.
fn migrate_from_electron(app: &AppHandle, dir: &Path) {
    if dir.join("friends.json").exists() {
        return;
    }
    let Ok(base) = app.path().config_dir() else {
        return;
    };
    let Some(old) = ["P2PShare", "p2p"]
        .iter()
        .map(|n| base.join(n))
        .find(|d| d.join("identity.key").is_file())
    else {
        return;
    };
    let key = dir.join("identity.key");
    if key.exists() {
        let _ = std::fs::rename(&key, dir.join("identity.key.bak"));
    }
    for file in [
        "identity.key",
        "friends.json",
        "messages.json",
        "config.json",
    ] {
        let _ = std::fs::copy(old.join(file), dir.join(file));
    }
}

/// The identity is a persistent Ed25519 seed: its public key is the ID we
/// share, and connections are authenticated against it.
fn load_identity(path: &Path) -> SecretKey {
    let stored = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| hex::decode(s.trim()).ok());
    if let Some(seed) = stored.and_then(|b| <[u8; 32]>::try_from(b).ok()) {
        return SecretKey::from_bytes(&seed);
    }
    let seed: [u8; 32] = rand::random();
    let _ = std::fs::write(path, hex::encode(seed));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    SecretKey::from_bytes(&seed)
}

fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let dir = data_dir(app);
    std::fs::create_dir_all(&dir)?;
    migrate_from_electron(app, &dir);

    let default_downloads = app
        .path()
        .download_dir()
        .unwrap_or_else(|_| dir.join("downloads"));
    let mut config = JsonStore::load(
        dir.join("config.json"),
        Config {
            download_path: String::new(),
            auto_launch: true,
        },
    );
    if config.data.download_path.is_empty() {
        config.data.download_path = default_downloads.to_string_lossy().into_owned();
        config.save_now();
    }
    let auto_launch = config.data.auto_launch;
    let config = Arc::new(Mutex::new(config));

    let mut friends = JsonStore::<Vec<p2p::Friend>>::load(dir.join("friends.json"), Vec::new());
    for f in &mut friends.data {
        f.id = f.id.trim().to_lowercase();
        f.name = f.name.trim().to_string();
    }
    let messages = JsonStore::<Messages>::load(dir.join("messages.json"), Messages::new());

    let handle = app.clone();
    let cfg = config.clone();
    let node = tauri::async_runtime::block_on(Node::start(p2p::Options {
        secret: load_identity(&dir.join("identity.key")),
        friends,
        messages,
        download_dir: Arc::new(move || {
            let path = cfg
                .lock()
                .map(|c| c.data.download_path.clone())
                .unwrap_or_default();
            if path.is_empty() {
                default_downloads.clone()
            } else {
                PathBuf::from(path)
            }
        }),
        on_event: Arc::new(move |e| on_p2p_event(&handle, e)),
    }))?;

    app.manage(AppState { node, config });

    apply_auto_launch(app, auto_launch);
    create_tray(app)?;
    // Started at login: stay in the tray without creating the web view.
    if !std::env::args().any(|a| a == HIDDEN_ARG) {
        show_window(app);
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_ARG]),
        ))
        .invoke_handler(tauri::generate_handler![
            hide_window,
            get_my_id,
            get_friends,
            add_friend,
            remove_friend,
            get_config,
            select_download_dir,
            set_auto_launch,
            get_messages,
            send_message,
            send_file,
            get_transfers,
            clear_finished,
            take_open_chat,
            respond_to_file_offer,
            cancel_transfer,
            show_in_folder,
        ])
        .setup(|app| setup(app.handle()))
        .build(tauri::generate_context!())
        .expect("failed to start P2P Share");

    app.run(|app, event| match event {
        // Tray app: closing the last window (which is how it hides) must not
        // quit; only an explicit exit (tray menu) does.
        RunEvent::ExitRequested { code, api, .. } => {
            if code.is_none() && !QUITTING.load(Ordering::SeqCst) {
                api.prevent_exit();
            } else {
                QUITTING.store(true, Ordering::SeqCst);
            }
        }
        RunEvent::Exit => {
            if let Some(state) = app.try_state::<AppState>() {
                state.config().save_now();
                let node = state.node.clone();
                tauri::async_runtime::block_on(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(2), node.shutdown()).await;
                });
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => {
            show_window(app);
        }
        _ => {}
    });
}
