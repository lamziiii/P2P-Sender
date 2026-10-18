pub mod group;
pub mod localsend;
mod notify;
pub mod p2p;
pub mod store;

use std::collections::HashMap;
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

use localsend::LocalSend;
use p2p::{
    ChatMessage, CustomRelay, Event, FriendView, GroupDetail, GroupFileView, GroupMessageView,
    GroupsOverview,
    Messages, Node, RelayStatusView, TransferView,
};
use store::JsonStore;

const WINDOW_MARGIN: f64 = 12.0;
const HIDDEN_ARG: &str = "--hidden";
/// Set before restarting to apply new settings, so the window comes back
/// even if the app was started hidden at login.
const SHOW_AFTER_RESTART: &str = "P2PSHARE_SHOW_WINDOW";

static QUITTING: AtomicBool = AtomicBool::new(false);
/// Chat to open once a freshly created window has loaded: (kind, id).
static PENDING_CHAT: Mutex<Option<(&'static str, String)>> = Mutex::new(None);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    #[serde(default)]
    download_path: String,
    #[serde(default = "enabled")]
    auto_launch: bool,
    /// Display name in groups.
    #[serde(default = "default_nickname")]
    nickname: String,
    /// Self-hosted relay; empty for iroh's public relays.
    #[serde(default)]
    relay_url: String,
    #[serde(default)]
    relay_token: String,
    /// Receive files from the LocalSend app on the local network.
    #[serde(default)]
    localsend: bool,
}

fn enabled() -> bool {
    true
}

/// The setting stays in memory (and is saved again at exit) if writing fails.
fn log_save(result: std::io::Result<()>) {
    if let Err(e) = result {
        eprintln!("config.json not saved: {e}");
    }
}

fn default_nickname() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
        .chars()
        .take(group::MAX_NICK)
        .collect()
}

struct AppState {
    node: Node,
    config: Arc<Mutex<JsonStore<Config>>>,
    localsend: Mutex<LocalSendSlot>,
    /// Text messages exchanged with LocalSend devices, by fingerprint.
    localsend_chats: Mutex<JsonStore<LocalSendChats>>,
    /// Serializes turning LocalSend on and off.
    localsend_toggle: tokio::sync::Mutex<()>,
    data_dir: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalSendMessage {
    id: String,
    /// Sent by us.
    mine: bool,
    text: String,
    ts: u64,
    /// Ours: "pending", "delivered" or "failed".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Theirs, from an older LocalSend that proves nothing about who it is.
    #[serde(default)]
    unverified: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct LocalSendChat {
    /// The name the device last gave itself.
    alias: String,
    messages: Vec<LocalSendMessage>,
    #[serde(default)]
    unread: usize,
}

type LocalSendChats = HashMap<String, LocalSendChat>;

/// Oldest messages are dropped beyond that, per device.
const MAX_LOCALSEND_MESSAGES: usize = 1000;
/// Longest text sent (the UI limits it too).
const MAX_LOCALSEND_TEXT: usize = 5000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalSendChatView {
    fingerprint: String,
    alias: String,
    unread: usize,
    last_text: String,
    last_ts: u64,
}

#[derive(Default)]
struct LocalSendSlot {
    service: Option<LocalSend>,
    /// Why it could not start.
    error: Option<String>,
}

impl AppState {
    fn config(&self) -> std::sync::MutexGuard<'_, JsonStore<Config>> {
        self.config.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn localsend(&self) -> std::sync::MutexGuard<'_, LocalSendSlot> {
        self.localsend.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn localsend_chats(&self) -> std::sync::MutexGuard<'_, JsonStore<LocalSendChats>> {
        self.localsend_chats.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Add or update a message of a device's chat, save, and tell the window.
    fn put_localsend_message(
        &self,
        app: &AppHandle,
        fingerprint: &str,
        alias: Option<&str>,
        message: LocalSendMessage,
    ) {
        let alias = {
            let mut chats = self.localsend_chats();
            let chat = chats.data.entry(fingerprint.to_string()).or_default();
            if let Some(alias) = alias {
                chat.alias = alias.to_string();
            }
            match chat.messages.iter_mut().find(|m| m.id == message.id) {
                Some(known) => *known = message.clone(),
                None => {
                    if !message.mine {
                        chat.unread += 1;
                    }
                    chat.messages.push(message.clone());
                    let excess = chat.messages.len().saturating_sub(MAX_LOCALSEND_MESSAGES);
                    chat.messages.drain(..excess);
                }
            }
            let alias = chat.alias.clone();
            log_save(chats.save_now());
            alias
        };
        let _ = app.emit(
            "localsend-chat",
            json!({ "fingerprint": fingerprint, "alias": alias, "message": message }),
        );
    }

    async fn start_localsend(&self, app: &AppHandle) {
        let alias = self.config().data.nickname.clone();
        let (text_app, devices_app) = (app.clone(), app.clone());
        let hooks = localsend::Hooks {
            on_text: Arc::new(move |from, text| on_localsend_text(&text_app, from, text)),
            on_devices: Arc::new(move || {
                let _ = devices_app.emit("localsend-devices", ());
            }),
        };
        let started =
            match localsend::Identity::load_or_create(&self.data_dir.join("localsend.json")) {
                Ok(id) => LocalSend::start(self.node.clone(), &id, &alias, hooks).await,
                Err(e) => Err(e),
            };
        let mut slot = self.localsend();
        match started {
            Ok(service) => {
                *slot = LocalSendSlot {
                    service: Some(service),
                    error: None,
                }
            }
            Err(e) => {
                eprintln!("LocalSend unavailable: {e}");
                *slot = LocalSendSlot {
                    service: None,
                    error: Some(format!("Démarrage impossible : {e}")),
                };
            }
        }
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
fn get_relay_status(state: State<AppState>) -> Option<RelayStatusView> {
    state.node.relay_status()
}

/// Saves the relay setting and restarts the app to apply it.
#[tauri::command]
async fn set_relay(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    token: String,
) -> Result<(), String> {
    let (url, token) = if url.trim().is_empty() {
        (String::new(), String::new())
    } else {
        let relay = CustomRelay::parse(&url, &token)?;
        (relay.url.to_string(), relay.token.unwrap_or_default())
    };
    {
        let mut config = state.config();
        if config.data.relay_url == url && config.data.relay_token == token {
            return Ok(());
        }
        config.data.relay_url = url;
        config.data.relay_token = token;
        // Restarting without it saved would silently drop the setting.
        config
            .save_now()
            .map_err(|e| format!("Enregistrement du réglage impossible : {e}"))?;
    }
    std::env::set_var(SHOW_AFTER_RESTART, "1");
    app.restart();
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
    log_save(config.save_now());
    Ok(Some(path))
}

#[tauri::command]
fn set_auto_launch(app: AppHandle, state: State<AppState>, enable: bool) -> bool {
    let mut config = state.config();
    config.data.auto_launch = enable;
    log_save(config.save_now());
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
fn take_open_chat() -> Option<Value> {
    PENDING_CHAT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .map(|(kind, id)| json!({ "kind": kind, "id": id }))
}

#[tauri::command]
fn set_nickname(state: State<AppState>, nickname: String) -> String {
    let nickname: String = nickname.trim().chars().take(group::MAX_NICK).collect();
    let mut config = state.config();
    config.data.nickname = nickname.clone();
    log_save(config.save_now());
    drop(config);
    state.node.set_nickname(&nickname);
    if let Some(service) = &state.localsend().service {
        service.set_alias(&nickname);
    }
    nickname
}

// ─── LocalSend ───────────────────────────────────────────────────────────────

#[tauri::command]
fn get_localsend_status(state: State<AppState>) -> Value {
    localsend_status(&state)
}

fn localsend_status(state: &AppState) -> Value {
    let slot = state.localsend();
    match (&slot.service, &slot.error) {
        (Some(service), _) => json!({ "running": true, "status": service.status() }),
        (None, Some(error)) => json!({ "running": false, "error": error }),
        (None, None) => json!({ "running": false }),
    }
}

#[tauri::command]
fn get_localsend_devices(state: State<AppState>) -> Vec<localsend::DeviceView> {
    state
        .localsend()
        .service
        .as_ref()
        .map(LocalSend::devices)
        .unwrap_or_default()
}

/// Look for devices again; the list comes back through `localsend-devices`.
#[tauri::command]
fn scan_localsend(state: State<AppState>) {
    if let Some(service) = &state.localsend().service {
        service.scan();
    }
}

#[tauri::command]
async fn send_localsend_files(
    app: AppHandle,
    state: State<'_, AppState>,
    fingerprint: String,
) -> Result<usize, String> {
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
    match &state.localsend().service {
        Some(service) => service.send_files(&fingerprint, &paths).map_err(String::from),
        None => Err("LocalSend est désactivé".into()),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The devices we have exchanged text with, latest first.
#[tauri::command]
fn get_localsend_chats(state: State<AppState>) -> Vec<LocalSendChatView> {
    let chats = state.localsend_chats();
    let mut list: Vec<LocalSendChatView> = chats
        .data
        .iter()
        .filter_map(|(fingerprint, chat)| {
            let last = chat.messages.last()?;
            Some(LocalSendChatView {
                fingerprint: fingerprint.clone(),
                alias: chat.alias.clone(),
                unread: chat.unread,
                last_text: last.text.chars().take(100).collect(),
                last_ts: last.ts,
            })
        })
        .collect();
    list.sort_by_key(|c| std::cmp::Reverse(c.last_ts));
    list
}

#[tauri::command]
fn get_localsend_chat(state: State<AppState>, fingerprint: String) -> Value {
    let chats = state.localsend_chats();
    match chats.data.get(&fingerprint) {
        Some(chat) => json!({ "alias": chat.alias, "messages": chat.messages }),
        None => json!({ "alias": "", "messages": [] }),
    }
}

#[tauri::command]
fn mark_localsend_read(state: State<AppState>, fingerprint: String) {
    let mut chats = state.localsend_chats();
    if let Some(chat) = chats.data.get_mut(&fingerprint).filter(|c| c.unread > 0) {
        chat.unread = 0;
        log_save(chats.save_now());
    }
}

/// Send text to a device's chat; it shows as pending until the device has it.
#[tauri::command]
fn send_localsend_text(
    app: AppHandle,
    state: State<AppState>,
    fingerprint: String,
    text: String,
) -> Result<LocalSendMessage, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("Message vide".into());
    }
    if text.chars().count() > MAX_LOCALSEND_TEXT {
        return Err(format!("Message trop long ({MAX_LOCALSEND_TEXT} caractères max)"));
    }
    let message = LocalSendMessage {
        id: uuid::Uuid::new_v4().to_string(),
        mine: true,
        text: text.clone(),
        ts: now_ms(),
        status: Some("pending".into()),
        error: None,
        unverified: false,
    };
    let alias = state
        .localsend()
        .service
        .as_ref()
        .and_then(|s| s.devices().into_iter().find(|d| d.fingerprint == fingerprint))
        .map(|d| d.alias);
    // Shown before it leaves: the answer may come back right away.
    state.put_localsend_message(&app, &fingerprint, alias.as_deref(), message.clone());
    let (handle, fp, sent) = (app.clone(), fingerprint.clone(), message.clone());
    let started = match &state.localsend().service {
        Some(service) => service.send_text(&fingerprint, text, move |result| {
            let Some(state) = handle.try_state::<AppState>() else {
                return;
            };
            let mut message = sent;
            match result {
                Ok(()) => message.status = Some("delivered".into()),
                Err(e) => {
                    message.status = Some("failed".into());
                    message.error = Some(e);
                }
            }
            state.put_localsend_message(&handle, &fp, None, message);
        }),
        None => Err("LocalSend est désactivé"),
    };
    let mut message = message;
    if let Err(e) = started {
        message.status = Some("failed".into());
        message.error = Some(e.into());
        state.put_localsend_message(&app, &fingerprint, None, message.clone());
    }
    Ok(message)
}

fn on_localsend_text(app: &AppHandle, from: localsend::TextSender, text: String) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let message = LocalSendMessage {
        id: uuid::Uuid::new_v4().to_string(),
        mine: false,
        text: text.clone(),
        ts: now_ms(),
        status: None,
        error: None,
        unverified: !from.verified,
    };
    state.put_localsend_message(app, &from.fingerprint, Some(&from.alias), message);
    if !window_focused(app) {
        let handle = app.clone();
        notify::notify(
            app,
            &format!("{} (LocalSend)", from.alias),
            &text,
            move || open_chat(&handle, "localsend", from.fingerprint),
        );
    }
}

#[tauri::command]
async fn set_localsend(
    app: AppHandle,
    state: State<'_, AppState>,
    enable: bool,
) -> Result<Value, ()> {
    let _toggle = state.localsend_toggle.lock().await;
    {
        let mut config = state.config();
        config.data.localsend = enable;
        log_save(config.save_now());
    }
    // Stop first, so a restart can take the port back.
    let old = std::mem::take(&mut *state.localsend());
    drop(old);
    if enable {
        state.start_localsend(&app).await;
    }
    Ok(localsend_status(&state))
}

// ─── Groups ──────────────────────────────────────────────────────────────────

#[tauri::command]
fn get_groups(state: State<AppState>) -> GroupsOverview {
    state.node.groups()
}

#[tauri::command]
fn create_group(state: State<AppState>, name: String, members: Vec<String>) -> Value {
    match state.node.create_group(&name, &members) {
        Ok(id) => json!({ "id": id }),
        Err(error) => json!({ "error": error }),
    }
}

#[tauri::command]
fn respond_to_group_invite(state: State<AppState>, gid: String, accept: bool) {
    state.node.respond_to_invite(&gid, accept);
}

#[tauri::command]
fn get_group(state: State<AppState>, gid: String) -> Option<GroupDetail> {
    state.node.group_detail(&gid)
}

#[tauri::command]
fn get_group_messages(state: State<AppState>, gid: String) -> Vec<GroupMessageView> {
    state.node.group_messages(&gid)
}

#[tauri::command]
fn send_group_message(state: State<AppState>, gid: String, text: String) -> Value {
    match state.node.group_send(&gid, &text) {
        Ok(()) => json!({ "ok": true }),
        Err(error) => json!({ "error": error }),
    }
}

#[tauri::command]
fn add_group_members(state: State<AppState>, gid: String, members: Vec<String>) -> Value {
    match state.node.group_add_members(&gid, &members) {
        Ok(()) => json!({ "ok": true }),
        Err(error) => json!({ "error": error }),
    }
}

#[tauri::command]
fn remove_group_member(state: State<AppState>, gid: String, member: String) {
    state.node.group_remove_member(&gid, &member);
}

#[tauri::command]
fn leave_group(state: State<AppState>, gid: String) {
    state.node.group_leave(&gid);
}

#[tauri::command]
fn forget_group(state: State<AppState>, gid: String) {
    state.node.forget_group(&gid);
}

#[tauri::command]
fn rename_group(state: State<AppState>, gid: String, name: String) {
    state.node.group_rename(&gid, &name);
}

#[tauri::command]
fn mark_group_read(state: State<AppState>, gid: String) {
    state.node.group_mark_read(&gid);
}

#[tauri::command]
fn get_group_files(state: State<AppState>, gid: String) -> Vec<GroupFileView> {
    state.node.group_files(&gid)
}

#[tauri::command]
async fn add_group_files(
    app: AppHandle,
    state: State<'_, AppState>,
    gid: String,
) -> Result<usize, ()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Ajouter des fichiers au dépôt du groupe");
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
    let node = state.node.clone();
    Ok(node.group_add_files(&gid, paths).await)
}

#[tauri::command]
fn download_group_file(state: State<AppState>, gid: String, file_id: String) -> Value {
    match state.node.group_download(&gid, &file_id) {
        Ok(id) => json!({ "id": id }),
        Err(error) => json!({ "error": error }),
    }
}

#[tauri::command]
fn delete_group_file(state: State<AppState>, gid: String, file_id: String) {
    state.node.group_delete_file(&gid, &file_id);
}

#[tauri::command]
fn show_group_file(app: AppHandle, state: State<AppState>, gid: String, file_id: String) {
    if let Some(path) = state.node.group_file_path(&gid, &file_id) {
        let _ = app.opener().reveal_item_in_dir(path);
    }
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

/// Open a chat ("friend", "group" or "localsend") from outside the UI (notification
/// click). A freshly created window has not loaded yet, so it picks the chat
/// up once mounted.
fn open_chat(app: &AppHandle, kind: &'static str, id: String) {
    if show_window(app) {
        *PENDING_CHAT.lock().unwrap_or_else(|e| e.into_inner()) = Some((kind, id));
    } else {
        let _ = app.emit("open-chat", json!({ "kind": kind, "id": id }));
    }
}

fn window_focused(app: &AppHandle) -> bool {
    main_window(app)
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false)
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

/// A friend's name, or the name a LocalSend device gives itself, marked as
/// such: anyone on the network can call their device "Alice".
fn sender_name(app: &AppHandle, t: &TransferView) -> String {
    match &t.peer_name {
        Some(name) => format!("{name} (LocalSend)"),
        None => friend_name(app, &t.friend_id),
    }
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
            if !window_focused(app) {
                let handle = app.clone();
                notify::notify(
                    app,
                    &friend_name(app, &friend_id),
                    &message.text,
                    move || open_chat(&handle, "friend", friend_id),
                );
            }
        }
        Event::FileOffer(t, count) => {
            let handle = app.clone();
            let from = sender_name(app, &t);
            let (title, what) = if count > 1 {
                (
                    format!("{from} vous envoie {count} fichiers"),
                    format!("dont \"{}\"", t.file_name),
                )
            } else {
                (
                    format!("{from} vous envoie un fichier"),
                    format!("\"{}\" ({})", t.file_name, format_size(t.file_size)),
                )
            };
            let warning = if t.executable {
                " Attention : ce fichier peut exécuter du code."
            } else {
                ""
            };
            notify::notify(
                app,
                &title,
                &format!("{what} — Cliquez pour accepter ou refuser.{warning}"),
                move || {
                    show_window(&handle);
                },
            );
        }
        Event::TransferComplete(t) => {
            let handle = app.clone();
            let from = sender_name(app, &t);
            notify::notify(
                app,
                &format!("Fichier reçu de {from}"),
                &format!("\"{}\" a été enregistré.", t.file_name),
                move || {
                    if let Some(path) = t.saved_path {
                        let _ = handle.opener().reveal_item_in_dir(path);
                    }
                },
            );
        }
        Event::GroupUpdate { gid } => {
            let _ = app.emit("group-update", json!({ "gid": gid }));
        }
        Event::GroupMessage {
            gid,
            group,
            author,
            text,
        } => {
            if !window_focused(app) {
                let handle = app.clone();
                notify::notify(app, &format!("{group} — {author}"), &text, move || {
                    open_chat(&handle, "group", gid)
                });
            }
        }
        Event::GroupInvite { gid, group, from } => {
            let _ = app.emit("group-update", json!({ "gid": gid }));
            let handle = app.clone();
            notify::notify(
                app,
                &format!("{from} vous invite dans un groupe"),
                &format!("« {group} » — Cliquez pour accepter ou refuser."),
                move || {
                    show_window(&handle);
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
        // Rewritten rather than copied, so they get our permissions.
        if let Ok(bytes) = std::fs::read(old.join(file)) {
            let _ = store::write_atomic(&dir.join(file), &bytes);
        }
    }
}

/// The identity is a persistent Ed25519 seed: its public key is the ID we
/// share, and connections are authenticated against it. A new one is made
/// only when there is none: losing it would lose every friend.
fn load_identity(path: &Path) -> std::io::Result<SecretKey> {
    use std::io::{Error, ErrorKind};
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let seed = hex::decode(text.trim())
                .ok()
                .and_then(|b| <[u8; 32]>::try_from(b).ok());
            match seed {
                Some(seed) => return Ok(SecretKey::from_bytes(&seed)),
                None => {
                    // Kept aside: it might still be recovered by hand.
                    let aside = path.with_extension(format!("bad-{}", rand::random::<u32>()));
                    std::fs::rename(path, &aside)?;
                    eprintln!("identity.key unreadable, moved to {}", aside.display());
                }
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        // Locked by another program, permissions…: try again later rather
        // than replace it.
        Err(e) => {
            return Err(Error::new(
                e.kind(),
                format!("lecture de {} impossible : {e}", path.display()),
            ))
        }
    }
    let seed: [u8; 32] = rand::random();
    store::write_atomic(path, hex::encode(seed).as_bytes())?;
    Ok(SecretKey::from_bytes(&seed))
}

fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let dir = data_dir(app);
    store::create_private_dir(&dir)?;
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
            nickname: default_nickname(),
            relay_url: String::new(),
            relay_token: String::new(),
            localsend: false,
        },
    );
    if config.data.download_path.is_empty() {
        config.data.download_path = default_downloads.to_string_lossy().into_owned();
        log_save(config.save_now());
    }
    let auto_launch = config.data.auto_launch;
    let localsend = config.data.localsend;
    let nickname = config.data.nickname.clone();
    let relay = if config.data.relay_url.is_empty() {
        None
    } else {
        CustomRelay::parse(&config.data.relay_url, &config.data.relay_token)
            .inspect_err(|e| eprintln!("relay setting ignored: {e}"))
            .ok()
    };
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
        secret: load_identity(&dir.join("identity.key"))?,
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
        groups_dir: dir.join("groups"),
        nickname,
        relay,
        // A certificate of its own, not LocalSend's: not linked to what this
        // computer announces to every LocalSend device.
        lan: localsend::Identity::load_or_create(&dir.join("lan.json"))
            .inspect_err(|e| eprintln!("local network fast path off: {e}"))
            .ok(),
    }))?;

    app.manage(AppState {
        node,
        config,
        localsend: Mutex::new(LocalSendSlot::default()),
        localsend_chats: Mutex::new(JsonStore::load(
            dir.join("localsend-chats.json"),
            LocalSendChats::new(),
        )),
        localsend_toggle: tokio::sync::Mutex::new(()),
        data_dir: dir,
    });
    if localsend {
        // In the background: the port may take a moment to give up on.
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<AppState>();
            let _toggle = state.localsend_toggle.lock().await;
            // Unless it was turned off in the meantime.
            if state.config().data.localsend {
                state.start_localsend(&app).await;
            }
        });
    }

    apply_auto_launch(app, auto_launch);
    create_tray(app)?;
    // Started at login: stay in the tray without creating the web view.
    let restarted = std::env::var_os(SHOW_AFTER_RESTART).is_some();
    std::env::remove_var(SHOW_AFTER_RESTART);
    if restarted || !std::env::args().any(|a| a == HIDDEN_ARG) {
        show_window(app);
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();
    // An instance with its own data folder (tests) runs alongside the real one.
    if std::env::var_os("P2PSHARE_DATA_DIR").is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_window(app);
        }));
    }
    let app = builder
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
            set_nickname,
            get_relay_status,
            set_relay,
            get_localsend_status,
            set_localsend,
            get_localsend_chats,
            get_localsend_chat,
            mark_localsend_read,
            send_localsend_text,
            get_localsend_devices,
            scan_localsend,
            send_localsend_files,
            get_groups,
            create_group,
            respond_to_group_invite,
            get_group,
            get_group_messages,
            send_group_message,
            add_group_members,
            remove_group_member,
            leave_group,
            forget_group,
            rename_group,
            mark_group_read,
            get_group_files,
            add_group_files,
            download_group_file,
            delete_group_file,
            show_group_file,
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
                log_save(state.config().save_now());
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
