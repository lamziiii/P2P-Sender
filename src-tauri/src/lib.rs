#[cfg(target_os = "android")]
mod android;
mod backup;
pub mod group;
pub mod invite;
pub mod localsend;
mod notify;
pub mod p2p;
pub mod store;
mod system;
mod update;
pub mod vault;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(desktop)]
use std::time::Instant;

use iroh::SecretKey;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(desktop)]
use tauri::menu::{Menu, MenuItem};
#[cfg(desktop)]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
#[cfg(desktop)]
use tauri::PhysicalPosition;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WebviewWindow, WebviewWindowBuilder};
#[cfg(desktop)]
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use localsend::LocalSend;
use p2p::{
    ChatMessage, CustomRelay, Event, FriendView, GroupDetail, GroupFileView, GroupMessageView,
    GroupsOverview, Node, RelayStatusView, TransferView,
};
use store::JsonStore;
use system::SystemEvent;

#[cfg(desktop)]
const WINDOW_MARGIN: f64 = 12.0;
const HIDDEN_ARG: &str = "--hidden";
/// Set before restarting to apply new settings, so the window comes back
/// even if the app was started hidden at login.
const SHOW_AFTER_RESTART: &str = "P2PSHARE_SHOW_WINDOW";

static QUITTING: AtomicBool = AtomicBool::new(false);
/// Chat to open once a freshly created window has loaded: (kind, id).
static PENDING_CHAT: Mutex<Option<(&'static str, String)>> = Mutex::new(None);
/// An invitation link that opened the app, for the window to ask about.
static PENDING_INVITE: Mutex<Option<String>> = Mutex::new(None);
/// The last link the window was asked about: `get_current` keeps giving it.
static SEEN_INVITE: Mutex<Option<String>> = Mutex::new(None);

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
    /// LocalSend used to be optional: it is always on now (it also finds
    /// friends on the local network). Kept on, for an older version.
    #[serde(default = "enabled")]
    localsend: bool,
    /// The first-launch welcome was shown.
    #[serde(default)]
    welcomed: bool,
    /// Do not disturb until then (unix ms; 0 off, `DND_FOREVER` until
    /// turned off): no system notification meanwhile.
    #[serde(default)]
    dnd_until: u64,
    /// Read receipts and "typing" with friends, both ways.
    #[serde(default = "enabled")]
    read_receipts: bool,
    /// Desktop: a newer version is installed without asking (see update.rs).
    #[serde(default)]
    auto_update: bool,
    /// The version whose install failed last (the old one restarted): not
    /// installed by itself again, only when asked.
    #[serde(default)]
    update_failed: String,
    /// Speed limit of transfers in bytes per second, each way; 0 for none.
    #[serde(default)]
    speed_limit: u64,
    /// Which system notifications show.
    #[serde(default)]
    notifications: NotifySettings,
    /// Android: a foreground service keeps the app reachable once its
    /// screen is closed, like the desktop app in the tray.
    #[serde(default = "enabled")]
    background_service: bool,
}

/// What gives a system notification: all of it, by kind, and the chats
/// left silent one by one (groups keep their own mute, see groups.rs).
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotifySettings {
    #[serde(default = "enabled")]
    enabled: bool,
    /// Messages from friends and LocalSend devices.
    #[serde(default = "enabled")]
    messages: bool,
    /// Files offered and received.
    #[serde(default = "enabled")]
    files: bool,
    /// Group messages, mentions and invitations.
    #[serde(default = "enabled")]
    groups: bool,
    #[serde(default)]
    muted_friends: Vec<String>,
    #[serde(default)]
    muted_localsend: Vec<String>,
}

impl Default for NotifySettings {
    fn default() -> Self {
        NotifySettings {
            enabled: true,
            messages: true,
            files: true,
            groups: true,
            muted_friends: Vec::new(),
            muted_localsend: Vec::new(),
        }
    }
}

/// What a notification is about, to check it against the settings.
enum Notice<'a> {
    Friend(&'a str),
    LocalSend(&'a str),
    File,
    Group,
}

fn notice_allowed(app: &AppHandle, notice: Notice) -> bool {
    let state = app.state::<AppState>();
    let config = state.config();
    let n = &config.data.notifications;
    n.enabled
        && match notice {
            Notice::Friend(id) => n.messages && !n.muted_friends.iter().any(|f| f == id),
            Notice::LocalSend(fp) => n.messages && !n.muted_localsend.iter().any(|f| f == fp),
            Notice::File => n.files,
            Notice::Group => n.groups,
        }
}

/// Do not disturb until turned off.
const DND_FOREVER: u64 = u64::MAX;

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
    /// A save of the chats is due (see `localsend_chats_changed`).
    localsend_save_armed: AtomicBool,
    /// Session locked or on battery: background work is cut down.
    economy: AtomicBool,
    /// The system reports network changes.
    network_watched: AtomicBool,
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
/// Longer received texts are cut when kept.
const MAX_LOCALSEND_STORED_TEXT: usize = 10_000;
/// Anyone on the network can start a chat (a new certificate each time):
/// beyond that many, or that much text, the least useful chats go.
const MAX_LOCALSEND_CHATS: usize = 100;
const MAX_LOCALSEND_BYTES: usize = 16 * 1024 * 1024;
/// The chats are rewritten whole: at most that often, that long after they
/// changed.
const LOCALSEND_SAVE_DELAY: Duration = Duration::from_secs(3);
/// Network changes come in bursts: handled once, that long after the first.
const NETWORK_SETTLE: Duration = Duration::from_secs(1);

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

    /// Run `f` on the LocalSend service, if it runs.
    fn with_localsend(&self, f: impl FnOnce(&LocalSend)) {
        if let Some(service) = &self.localsend().service {
            f(service);
        }
    }

    fn localsend_chats(&self) -> std::sync::MutexGuard<'_, JsonStore<LocalSendChats>> {
        self.localsend_chats.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Add or update a message of a device's chat, and tell the window; it
    /// is written a moment later. False if the message was refused.
    fn put_localsend_message(
        &self,
        app: &AppHandle,
        fingerprint: &str,
        alias: Option<&str>,
        message: LocalSendMessage,
    ) -> bool {
        // Before taking the chats: never both locks at once.
        let secure_device = message.unverified
            && self.localsend().service.as_ref().is_some_and(|s| {
                s.devices()
                    .iter()
                    .any(|d| d.fingerprint == fingerprint && d.secure)
            });
        let alias = {
            let mut chats = self.localsend_chats();
            // An older LocalSend only claims its fingerprint: anyone could
            // claim that of a device whose certificate proved it.
            let proven = chats.data.get(fingerprint).is_some_and(|c| {
                c.messages.iter().any(|m| !m.mine && !m.unverified)
            });
            if message.unverified && (proven || secure_device) {
                return false;
            }
            if !chats.data.contains_key(fingerprint) {
                make_room_for_chat(&mut chats.data);
            }
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
            trim_localsend_chats(&mut chats.data, fingerprint);
            chats.mark_dirty();
            alias
        };
        self.localsend_chats_changed(app);
        let _ = app.emit(
            "localsend-chat",
            json!({ "fingerprint": fingerprint, "alias": alias, "message": message }),
        );
        true
    }

    /// The chats were marked dirty: write them `LOCALSEND_SAVE_DELAY` later
    /// (and at exit), along with what changes meanwhile.
    fn localsend_chats_changed(&self, app: &AppHandle) {
        if self.localsend_save_armed.swap(true, Ordering::SeqCst) {
            return;
        }
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(LOCALSEND_SAVE_DELAY).await;
            let handle = app.clone();
            let saved = tauri::async_runtime::spawn_blocking(move || {
                let state = handle.state::<AppState>();
                // Changes from now on arm a new save.
                state.localsend_save_armed.store(false, Ordering::SeqCst);
                state.save_localsend_chats()
            })
            .await;
            if matches!(saved, Ok(false)) {
                // Retried after the same delay.
                app.state::<AppState>().localsend_chats_changed(&app);
            }
        });
    }

    /// Write the chats if they changed; called after a change and at exit.
    /// False if writing failed (they stay dirty).
    fn save_localsend_chats(&self) -> bool {
        let Some((path, json)) = self.localsend_chats().take_dirty() else {
            return true;
        };
        if let Err(e) = store::write_atomic(&path, &json) {
            eprintln!("localsend-chats.json not saved: {e}");
            self.localsend_chats().mark_dirty();
            return false;
        }
        true
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
        if let Ok(service) = &started {
            // Proven in an earlier run: a plain claim must not take them over.
            let chats = self.localsend_chats();
            service.remember_verified(
                chats
                    .data
                    .iter()
                    .filter(|(_, c)| c.messages.iter().any(|m| !m.mine && !m.unverified))
                    .map(|(fingerprint, _)| fingerprint.as_str()),
            );
        }
        let mut slot = self.localsend();
        match started {
            Ok(service) => {
                // Under the slot's lock, as `on_system_event` tells it.
                service.set_network_watched(self.network_watched.load(Ordering::SeqCst));
                if self.economy.load(Ordering::SeqCst) {
                    service.set_economy(true);
                }
                *slot = LocalSendSlot {
                    service: Some(service),
                    error: None,
                };
                // Android drops multicast packets (discovery) without it.
                #[cfg(target_os = "android")]
                android::hold_multicast();
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

/// The chat to drop first: one we never wrote in, then the oldest activity.
fn least_useful_chat(chats: &LocalSendChats, keep: &str) -> Option<String> {
    chats
        .iter()
        .filter(|(fingerprint, _)| fingerprint.as_str() != keep)
        .min_by_key(|(_, chat)| {
            let ours = chat.messages.iter().any(|m| m.mine);
            let last = chat.messages.last().map_or(0, |m| m.ts);
            (ours, last)
        })
        .map(|(fingerprint, _)| fingerprint.clone())
}

fn make_room_for_chat(chats: &mut LocalSendChats) {
    while chats.len() >= MAX_LOCALSEND_CHATS {
        match least_useful_chat(chats, "") {
            Some(fingerprint) => chats.remove(&fingerprint),
            None => return,
        };
    }
}

/// Keep the whole file under `MAX_LOCALSEND_BYTES`, `current` chat last.
fn trim_localsend_chats(chats: &mut LocalSendChats, current: &str) {
    // Roughly what a message takes in the file.
    let size = |m: &LocalSendMessage| m.text.len() + 200;
    let mut total: usize = chats.values().flat_map(|c| &c.messages).map(size).sum();
    while total > MAX_LOCALSEND_BYTES {
        if let Some(fingerprint) = least_useful_chat(chats, current) {
            if let Some(chat) = chats.remove(&fingerprint) {
                total -= chat.messages.iter().map(size).sum::<usize>();
            }
        } else if let Some(chat) = chats.get_mut(current).filter(|c| c.messages.len() > 1) {
            total -= size(&chat.messages.remove(0));
        } else {
            return;
        }
    }
}

/// Cut a received text to what is kept, marking the cut.
fn stored_text(text: &str) -> String {
    match text.char_indices().nth(MAX_LOCALSEND_STORED_TEXT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
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
fn add_friend(app: AppHandle, state: State<AppState>, id: String, name: String) -> Value {
    match state.node.add_friend(&id, &name) {
        Ok(friends) => {
            // Now a friend: no longer among the LocalSend devices.
            let _ = app.emit("localsend-devices", ());
            json!({ "friends": friends })
        }
        Err(error) => json!({ "error": error }),
    }
}

#[tauri::command]
fn rename_friend(state: State<AppState>, id: String, name: String) -> Vec<FriendView> {
    state.node.rename_friend(&id, &name)
}

#[tauri::command]
fn remove_friend(app: AppHandle, state: State<AppState>, id: String) -> Vec<FriendView> {
    let friends = state.node.remove_friend(&id);
    // Back among the LocalSend devices, if it is around.
    let _ = app.emit("localsend-devices", ());
    friends
}

// ─── Invitation links (see invite.rs and p2p/invites.rs) ─────────────────────

/// What the window shows about a link before it is accepted.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InviteView {
    /// The link itself, to accept it with.
    link: String,
    /// The nickname of whoever made it, as the link says (may be empty).
    name: String,
    /// They are a friend already, and connected.
    known: bool,
    /// One of our own links.
    own: bool,
}

fn invite_view(state: &AppState, link: String) -> Option<InviteView> {
    let parsed = invite::parse(&link)?;
    let known = state
        .node
        .friends()
        .iter()
        .any(|f| f.id == parsed.id && f.online);
    Some(InviteView {
        own: parsed.id == state.node.id(),
        known,
        name: parsed.name,
        link,
    })
}

/// A link to send to a friend: valid 7 days, for one person.
#[tauri::command]
fn create_invite(state: State<AppState>) -> String {
    let code = state.node.create_invite();
    let nickname = state.config().data.nickname.clone();
    invite::make(&state.node.id(), &code, &nickname)
}

/// An invitation link opened the app, or reached it while it runs: the
/// window asks whether to add whoever made it.
fn on_invite_link(app: &AppHandle, urls: Vec<url::Url>) {
    let Some(link) = urls
        .into_iter()
        .map(String::from)
        .find(|u| invite::parse(u).is_some())
    else {
        return;
    };
    *SEEN_INVITE.lock().unwrap_or_else(|e| e.into_inner()) = Some(link.clone());
    *PENDING_INVITE.lock().unwrap_or_else(|e| e.into_inner()) = Some(link);
    // A window created now takes it once loaded; one already there is told.
    if cfg!(target_os = "android") || !show_window(app) {
        let _ = app.emit("invite-link", ());
    }
}

/// The link that opened the app, if the window was not asked about it yet.
#[tauri::command]
fn take_invite_link(app: AppHandle, state: State<AppState>) -> Option<InviteView> {
    let pending = PENDING_INVITE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    // On Android, a link the app was started with may come before anyone
    // listens: the plugin keeps it.
    let link = pending.or_else(|| {
        use tauri_plugin_deep_link::DeepLinkExt;
        let url = String::from(app.deep_link().get_current().ok()??.into_iter().next()?);
        let mut seen = SEEN_INVITE.lock().unwrap_or_else(|e| e.into_inner());
        (seen.as_deref() != Some(url.as_str())).then(|| {
            *seen = Some(url.clone());
            url
        })
    })?;
    invite_view(&state, link)
}

/// A link pasted in the window (when clicking it did not open the app).
#[tauri::command]
fn read_invite_link(state: State<AppState>, link: String) -> Result<InviteView, String> {
    invite_view(&state, link.trim().to_string())
        .ok_or_else(|| "Ce n’est pas un lien d’invitation P2P Share valide.".to_string())
}

#[tauri::command]
fn accept_invite(state: State<AppState>, link: String) -> Value {
    let Some(parsed) = invite::parse(&link) else {
        return json!({ "error": "invalid_link" });
    };
    match state.node.accept_invite(&parsed.id, &parsed.name, &parsed.code) {
        Ok(friends) => json!({ "friends": friends, "id": parsed.id }),
        Err(error) => json!({ "error": error }),
    }
}

#[tauri::command]
fn get_config(state: State<AppState>) -> Config {
    state.config().data.clone()
}

/// Do not disturb until `until` (unix ms), 0 to turn it off; from 2^53 - 1
/// (`Number.MAX_SAFE_INTEGER`) up, until turned off. The value kept.
#[tauri::command]
fn set_dnd(app: AppHandle, state: State<AppState>, until: f64) -> u64 {
    let until = if until.is_nan() || until <= 0.0 {
        0
    } else if until >= 9_007_199_254_740_991.0 {
        DND_FOREVER
    } else {
        until as u64
    };
    {
        let mut config = state.config();
        config.data.dnd_until = until;
        log_save(config.save_now());
    }
    let _ = app.emit("dnd-changed", json!({ "dndUntil": until }));
    apply_dnd(&app, until);
    until
}

/// Hold notifications until `until`, then tell the window it ended (unless
/// it was changed meanwhile).
fn apply_dnd(app: &AppHandle, until: u64) {
    notify::set_dnd_until(until);
    if until == 0 || until == DND_FOREVER {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let now = now_ms();
            if now >= until {
                break;
            }
            // By steps: a timer may not count the time the computer sleeps.
            tokio::time::sleep(Duration::from_millis((until - now).min(60_000))).await;
        }
        let state = app.state::<AppState>();
        let mut config = state.config();
        if config.data.dnd_until != until {
            return;
        }
        config.data.dnd_until = 0;
        log_save(config.save_now());
        drop(config);
        notify::set_dnd_until(0);
        let _ = app.emit("dnd-changed", json!({ "dndUntil": 0 }));
    });
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
    restart_app(&app);
}

#[tauri::command]
async fn select_download_dir(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, ()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    #[cfg_attr(mobile, allow(unused_mut))]
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Choisir le dossier de téléchargement");
    #[cfg(desktop)]
    if let Some(w) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&w);
    }
    // No folder picker on Android (phase 2: the system's document tree).
    #[cfg(desktop)]
    dialog.pick_folder(move |dir| {
        let _ = tx.send(dir);
    });
    #[cfg(mobile)]
    {
        drop(dialog);
        let _ = tx.send(None::<tauri_plugin_dialog::FilePath>);
    }
    let _dialog = DialogOpen::new();

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

/// Android: keep running once the screen is closed (a foreground service
/// and its notification), or only while the app is open. Elsewhere it is
/// only kept.
#[tauri::command]
fn set_background_service(state: State<AppState>, enable: bool) -> bool {
    let mut config = state.config();
    config.data.background_service = enable;
    log_save(config.save_now());
    #[cfg(target_os = "android")]
    android::set_service(enable);
    enable
}

/// Local paths of the files picked. On Android the picker gives documents,
/// copied into the app first (`keep`: for good, else in the cache).
#[cfg_attr(not(target_os = "android"), allow(unused_variables))]
async fn picked_paths(files: Vec<tauri_plugin_dialog::FilePath>, keep: bool) -> Vec<PathBuf> {
    #[cfg(target_os = "android")]
    return tauri::async_runtime::spawn_blocking(move || android::local_paths(files, keep))
        .await
        .unwrap_or_default();
    #[cfg(not(target_os = "android"))]
    files
        .into_iter()
        .filter_map(|f| f.into_path().ok())
        .collect()
}

/// Open a file with its app.
fn open_path(app: &AppHandle, path: &Path) {
    #[cfg(target_os = "android")]
    {
        let _ = app;
        android::open_file(path);
    }
    #[cfg(not(target_os = "android"))]
    let _ = app
        .opener()
        .open_path(path.to_string_lossy(), None::<&str>);
}

/// Show a file in its folder (on Android, the Downloads view).
fn reveal_path(app: &AppHandle, path: impl AsRef<Path>) {
    #[cfg(target_os = "android")]
    {
        let _ = (app, path);
        android::show_downloads();
    }
    #[cfg(not(target_os = "android"))]
    let _ = app.opener().reveal_item_in_dir(path);
}

/// Restart to apply what is read at start. Android has no relaunching of
/// the program: the activity starts again in a new process.
fn restart_app(app: &AppHandle) -> ! {
    #[cfg(target_os = "android")]
    {
        if let Some(state) = app.try_state::<AppState>() {
            log_save(state.config().save_now());
            state.save_localsend_chats();
            state.node.save_now();
        }
        android::restart();
        std::process::exit(0);
    }
    #[cfg(not(target_os = "android"))]
    app.restart()
}

#[tauri::command]
fn get_messages(state: State<AppState>, friend_id: String) -> Vec<ChatMessage> {
    state.node.messages(&friend_id)
}

#[tauri::command]
fn send_message(
    state: State<AppState>,
    friend_id: String,
    text: String,
    reply_to: Option<String>,
) -> Value {
    match state
        .node
        .send_message(&friend_id, &text, reply_to.as_deref())
    {
        Ok(message) => json!(message),
        Err(error) => json!({ "error": error }),
    }
}

/// Our reaction to a message (`None`, or the same emoji again, takes it
/// back): the message's reactions, or `{ error }`.
#[tauri::command]
fn react_message(
    state: State<AppState>,
    friend_id: String,
    id: String,
    emoji: Option<String>,
) -> Value {
    match state.node.react(&friend_id, &id, emoji.as_deref()) {
        Ok(reactions) => json!({ "reactions": reactions }),
        Err(error) => json!({ "error": error }),
    }
}

/// The chat with the friend is on screen (window focused).
#[tauri::command]
fn mark_chat_read(state: State<AppState>, friend_id: String) {
    state.node.mark_chat_read(&friend_id);
}

/// The user is typing to the friend (at most every 3 s).
#[tauri::command]
fn chat_typing(state: State<AppState>, friend_id: String) {
    state.node.chat_typing(&friend_id);
}

/// The notification settings by kind (the muted chats are kept).
#[tauri::command]
fn set_notifications(
    state: State<AppState>,
    enabled: bool,
    messages: bool,
    files: bool,
    groups: bool,
) -> NotifySettings {
    let mut config = state.config();
    let n = &mut config.data.notifications;
    (n.enabled, n.messages, n.files, n.groups) = (enabled, messages, files, groups);
    let settings = n.clone();
    log_save(config.save_now());
    settings
}

/// No notification for that chat ("friend" or "localsend"), or again.
#[tauri::command]
fn set_chat_muted(state: State<AppState>, kind: String, id: String, muted: bool) -> NotifySettings {
    let mut config = state.config();
    let n = &mut config.data.notifications;
    let list = if kind == "localsend" {
        &mut n.muted_localsend
    } else {
        &mut n.muted_friends
    };
    list.retain(|m| *m != id);
    if muted {
        list.push(id);
    }
    let settings = n.clone();
    log_save(config.save_now());
    settings
}

#[tauri::command]
fn set_read_receipts(state: State<AppState>, enable: bool) -> bool {
    let mut config = state.config();
    config.data.read_receipts = enable;
    log_save(config.save_now());
    state.node.set_read_receipts(enable);
    enable
}

/// Bytes per second each way; `None` or 0 for no limit.
#[tauri::command]
fn set_speed_limit(state: State<AppState>, limit: Option<u64>) -> Option<u64> {
    let limit = limit.filter(|l| *l > 0);
    let mut config = state.config();
    config.data.speed_limit = limit.unwrap_or(0);
    log_save(config.save_now());
    state.node.set_speed_limit(limit);
    limit
}

#[tauri::command]
fn set_friend_auto_accept(state: State<AppState>, id: String, enable: bool) -> Vec<FriendView> {
    state.node.set_friend_auto_accept(&id, enable)
}

#[tauri::command]
fn respond_to_folder(
    state: State<AppState>,
    friend_id: String,
    folder_id: String,
    accept: bool,
) -> usize {
    state.node.respond_to_folder(&friend_id, &folder_id, accept)
}

#[tauri::command]
fn pause_transfer(state: State<AppState>, id: String) -> bool {
    state.node.pause_transfer(&id)
}

#[tauri::command]
fn resume_transfer(state: State<AppState>, id: String) -> bool {
    state.node.resume_transfer(&id)
}

/// Pick a folder and send it with everything in it: `{ sent }` (0 if none
/// picked) or `{ error }` ("empty", "too_many", "too_deep", "not_found").
#[cfg(desktop)]
#[tauri::command]
async fn send_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    friend_id: String,
) -> Result<Value, ()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Sélectionner le dossier à envoyer");
    if let Some(w) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&w);
    }
    dialog.pick_folder(move |dir| {
        let _ = tx.send(dir);
    });
    let _dialog = DialogOpen::new();

    let Some(dir) = rx.await.ok().flatten().and_then(|d| d.into_path().ok()) else {
        return Ok(json!({ "sent": 0 }));
    };
    let node = state.node.clone();
    let sent = tauri::async_runtime::spawn_blocking(move || node.send_folder(&friend_id, &dir))
        .await
        .unwrap_or(Err("not_found"));
    Ok(match sent {
        Ok(sent) => json!({ "sent": sent }),
        Err(error) => json!({ "error": error }),
    })
}

/// No folder picker on phones (the system gives document trees, not
/// folders): `{ error: "unsupported" }`.
#[cfg(mobile)]
#[tauri::command]
#[allow(unused_variables)]
async fn send_folder(friend_id: String) -> Result<Value, ()> {
    Ok(json!({ "error": "unsupported" }))
}

#[tauri::command]
async fn send_file(
    app: AppHandle,
    state: State<'_, AppState>,
    friend_id: String,
) -> Result<usize, ()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    #[cfg_attr(mobile, allow(unused_mut))]
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Sélectionner les fichiers à envoyer");
    #[cfg(desktop)]
    if let Some(w) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&w);
    }
    dialog.pick_files(move |files| {
        let _ = tx.send(files);
    });
    let _dialog = DialogOpen::new();

    let files = rx.await.ok().flatten().unwrap_or_default();
    let paths = picked_paths(files, false).await;
    if paths.is_empty() {
        return Ok(0);
    }
    state.node.send_files(&friend_id, &paths);
    Ok(paths.len())
}

/// Files and folders dropped on a friend: `{ sent, skipped, errors }`, the
/// files sent (those of folders included), the paths left out, and why
/// folders were (see `send_folder`).
#[tauri::command]
async fn send_paths(
    state: State<'_, AppState>,
    friend_id: String,
    paths: Vec<String>,
) -> Result<Value, ()> {
    let friend = state
        .node
        .friends()
        .iter()
        .any(|f| f.id == friend_id && !f.legacy);
    if !friend {
        return Ok(json!({ "sent": 0, "skipped": paths.len(), "errors": [] }));
    }
    let node = state.node.clone();
    // Folders are read whole: not on the async threads.
    let result = tauri::async_runtime::spawn_blocking(move || {
        let (dirs, rest): (Vec<PathBuf>, Vec<PathBuf>) =
            paths.iter().map(PathBuf::from).partition(|p| p.is_dir());
        let files: Vec<PathBuf> = rest.iter().filter(|p| p.is_file()).cloned().collect();
        let mut skipped = rest.len() - files.len();
        let mut sent = if files.is_empty() {
            0
        } else {
            node.send_files(&friend_id, &files)
        };
        let mut errors = Vec::new();
        for dir in dirs {
            match node.send_folder(&friend_id, &dir) {
                Ok(n) => sent += n,
                Err(e) => {
                    skipped += 1;
                    errors.push(e);
                }
            }
        }
        json!({ "sent": sent, "skipped": skipped, "errors": errors })
    })
    .await;
    Ok(result.unwrap_or_else(|_| json!({ "sent": 0, "skipped": 0, "errors": [] })))
}

/// A small copy of a chat image, as a data URL: the page may not load files.
#[tauri::command]
async fn chat_image(
    state: State<'_, AppState>,
    friend_id: String,
    id: String,
) -> Result<Option<String>, ()> {
    let Some(path) = state.node.chat_file_path(&friend_id, &id) else {
        return Ok(None);
    };
    Ok(tauri::async_runtime::spawn_blocking(move || thumbnail(&path))
        .await
        .ok()
        .flatten())
}

/// Longest side of a chat preview, in pixels (about twice its size on screen).
const THUMB_SIZE: u32 = 480;
/// Bigger pictures get no preview: decoding them would take too long.
const MAX_IMAGE_BYTES: u64 = 64 << 20;

fn thumbnail(path: &Path) -> Option<String> {
    use base64::Engine as _;
    use image::{DynamicImage, ImageDecoder as _, ImageFormat};

    if std::fs::metadata(path).ok()?.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(512 << 20);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    // Phone photos are often stored sideways, with how to turn them.
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(orientation) = orientation {
        img.apply_orientation(orientation);
    }
    let img = img.thumbnail(THUMB_SIZE, THUMB_SIZE);
    let mut out = std::io::Cursor::new(Vec::new());
    let mime = if img.color().has_alpha() {
        img.write_to(&mut out, ImageFormat::Png).ok()?;
        "image/png"
    } else {
        DynamicImage::ImageRgb8(img.to_rgb8())
            .write_to(&mut out, ImageFormat::Jpeg)
            .ok()?;
        "image/jpeg"
    };
    let data = base64::engine::general_purpose::STANDARD.encode(out.into_inner());
    Some(format!("data:{mime};base64,{data}"))
}

/// Open a chat image with the system's viewer.
#[tauri::command]
fn open_chat_file(app: AppHandle, state: State<AppState>, friend_id: String, id: String) {
    if let Some(path) = state.node.chat_file_path(&friend_id, &id) {
        open_path(&app, &path);
    }
}

/// A link clicked in a chat: web pages only.
#[tauri::command]
fn open_url(app: AppHandle, url: String) {
    let Ok(parsed) = url::Url::parse(&url) else {
        return;
    };
    if matches!(parsed.scheme(), "http" | "https") {
        let _ = app.opener().open_url(parsed.as_str(), None::<&str>);
    }
}

/// The first-launch welcome is done: not shown again.
#[tauri::command]
fn finish_welcome(state: State<AppState>) {
    let mut config = state.config();
    config.data.welcomed = true;
    log_save(config.save_now());
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
    let mut devices = state
        .localsend()
        .service
        .as_ref()
        .map(LocalSend::devices)
        .unwrap_or_default();
    // A friend's P2P Share, announced on the network (a false claim only
    // hides a device).
    for d in &mut devices {
        d.friend = d.p2p_id.as_deref().is_some_and(|id| state.node.is_friend(id));
    }
    devices
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
    #[cfg_attr(mobile, allow(unused_mut))]
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Sélectionner les fichiers à envoyer");
    #[cfg(desktop)]
    if let Some(w) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&w);
    }
    dialog.pick_files(move |files| {
        let _ = tx.send(files);
    });
    let _dialog = DialogOpen::new();

    let files = rx.await.ok().flatten().unwrap_or_default();
    let paths = picked_paths(files, false).await;
    if paths.is_empty() {
        return Ok(0);
    }
    match &state.localsend().service {
        Some(service) => service.send_files(&fingerprint, &paths).map_err(String::from),
        None => Err("LocalSend est désactivé".into()),
    }
}

/// Files dropped on a LocalSend device: `{ sent, skipped }` (folders and
/// what is not a file are skipped).
#[tauri::command]
async fn send_localsend_paths(
    state: State<'_, AppState>,
    fingerprint: String,
    paths: Vec<String>,
) -> Result<Value, String> {
    let (files, rest): (Vec<PathBuf>, Vec<PathBuf>) =
        paths.into_iter().map(PathBuf::from).partition(|p| p.is_file());
    let sent = match &state.localsend().service {
        Some(_) if files.is_empty() => 0,
        Some(service) => service.send_files(&fingerprint, &files)?,
        None => return Err("LocalSend est désactivé".into()),
    };
    Ok(json!({ "sent": sent, "skipped": rest.len() }))
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
fn mark_localsend_read(app: AppHandle, state: State<AppState>, fingerprint: String) {
    let mut chats = state.localsend_chats();
    if let Some(chat) = chats.data.get_mut(&fingerprint).filter(|c| c.unread > 0) {
        chat.unread = 0;
        chats.mark_dirty();
        drop(chats);
        state.localsend_chats_changed(&app);
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
    let text = stored_text(&text);
    let message = LocalSendMessage {
        id: uuid::Uuid::new_v4().to_string(),
        mine: false,
        text: text.clone(),
        ts: now_ms(),
        status: None,
        error: None,
        unverified: !from.verified,
    };
    if !state.put_localsend_message(app, &from.fingerprint, Some(&from.alias), message) {
        eprintln!(
            "LocalSend: unverified text claiming {} dropped (verified device)",
            from.fingerprint
        );
        return;
    }
    if !window_focused(app) && notice_allowed(app, Notice::LocalSend(&from.fingerprint)) {
        notify_chat(
            app,
            &format!("{} (LocalSend)", from.alias),
            &text,
            "localsend",
            from.fingerprint,
        );
    }
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
    #[cfg_attr(mobile, allow(unused_mut))]
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Ajouter des fichiers au dépôt du groupe");
    #[cfg(desktop)]
    if let Some(w) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&w);
    }
    dialog.pick_files(move |files| {
        let _ = tx.send(files);
    });
    let _dialog = DialogOpen::new();

    let files = rx.await.ok().flatten().unwrap_or_default();
    // Shared until removed: kept in the app's files on Android.
    let paths = picked_paths(files, true).await;
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
        reveal_path(&app, path);
    }
}

/// Files and folders dropped on a group: added to its shared files.
#[tauri::command]
async fn add_group_paths(
    state: State<'_, AppState>,
    gid: String,
    paths: Vec<String>,
) -> Result<usize, ()> {
    let node = state.node.clone();
    let paths = paths.into_iter().map(PathBuf::from).collect();
    Ok(node.group_add_paths(&gid, paths).await)
}

#[tauri::command]
fn set_group_muted(state: State<AppState>, gid: String, muted: bool) {
    state.node.set_group_muted(&gid, muted);
}

#[tauri::command]
fn pin_group_item(state: State<AppState>, gid: String, target: String) -> Value {
    match state.node.group_pin(&gid, &target, true) {
        Ok(()) => json!({ "ok": true }),
        Err(error) => json!({ "error": error }),
    }
}

#[tauri::command]
fn unpin_group_item(state: State<AppState>, gid: String, target: String) -> Value {
    match state.node.group_pin(&gid, &target, false) {
        Ok(()) => json!({ "ok": true }),
        Err(error) => json!({ "error": error }),
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

/// Open a received file with its app (a click in the transfer list).
#[tauri::command]
fn open_transfer(app: AppHandle, state: State<AppState>, id: String) {
    if let Some(path) = state.node.openable_path(&id) {
        open_path(&app, &path);
    }
}

#[tauri::command]
fn show_in_folder(app: AppHandle, state: State<AppState>, id: String) {
    if let Some(path) = state.node.saved_path(&id) {
        reveal_path(&app, path);
    }
}

// ─── Window & tray ───────────────────────────────────────────────────────────

/// Like the OneDrive flyout, the window goes away when another window or the
/// desktop takes the focus, unless the user pinned it (to drag files from the
/// file explorer) or one of our file dialogs is open.
static PINNED: AtomicBool = AtomicBool::new(false);
static DIALOGS: AtomicUsize = AtomicUsize::new(0);
/// When the window last went away that way: the click on the tray icon that
/// took its focus must not bring it back at once.
#[cfg(desktop)]
static BLUR_HIDDEN: Mutex<Option<Instant>> = Mutex::new(None);
/// A focus lost for less than this is the system's, not the user's.
#[cfg(desktop)]
const BLUR_DELAY: Duration = Duration::from_millis(120);
#[cfg(desktop)]
const TRAY_GRACE: Duration = Duration::from_millis(400);

/// Held while a file dialog is open: it takes the focus from the window.
pub(crate) struct DialogOpen;

impl DialogOpen {
    pub(crate) fn new() -> Self {
        DIALOGS.fetch_add(1, Ordering::SeqCst);
        DialogOpen
    }
}

impl Drop for DialogOpen {
    fn drop(&mut self) {
        DIALOGS.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(desktop)]
fn on_focus_lost(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(BLUR_DELAY).await;
        if PINNED.load(Ordering::SeqCst) || DIALOGS.load(Ordering::SeqCst) > 0 {
            return;
        }
        let Some(w) = main_window(&app) else { return };
        if w.is_focused().unwrap_or(true) || !w.is_visible().unwrap_or(false) {
            return;
        }
        *BLUR_HIDDEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        hide_to_tray(&w);
    });
}

#[tauri::command]
fn set_window_pinned(pinned: bool) -> bool {
    PINNED.store(pinned, Ordering::SeqCst);
    pinned
}

#[tauri::command]
fn get_window_pinned() -> bool {
    PINNED.load(Ordering::SeqCst)
}

/// In the tray the window is destroyed, which frees the whole web view
/// (about 100 MB); it is rebuilt from the config when shown again.
fn hide_to_tray(w: &WebviewWindow) {
    // On a phone the system owns the activity: nothing to hide or free.
    #[cfg(desktop)]
    {
        let _ = w.hide();
        let _ = w.destroy();
    }
    #[cfg(mobile)]
    let _ = w;
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
                Ok(w) => {
                    #[cfg(target_os = "macos")]
                    join_current_space(&w);
                    (w, true)
                }
                Err(_) => return false,
            }
        }
    };
    #[cfg(desktop)]
    {
        if w.is_minimized().unwrap_or(false) {
            let _ = w.unminimize();
        }
        position_window(&w);
    }
    let _ = w.show();
    let _ = w.set_focus();
    created
}

/// Like a menu bar flyout, the window belongs to every Space, so it opens on
/// the current one, over a full-screen app too. Otherwise macOS switches to
/// the desktop to show it, and the focus lost on the way hides it at once.
#[cfg(target_os = "macos")]
fn join_current_space(w: &WebviewWindow) {
    use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior};
    // Run on the main thread, where AppKit windows live.
    let _ = w.with_webview(|webview| {
        let window = unsafe { &*webview.ns_window().cast::<NSWindow>() };
        window.setCollectionBehavior(
            window.collectionBehavior()
                | NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
    });
}

#[cfg(desktop)]
fn toggle_window(app: &AppHandle) {
    let just_hidden = BLUR_HIDDEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|t| t.elapsed() < TRAY_GRACE);
    if just_hidden {
        return;
    }
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
    // Kept even for a window already there: it may be (re)loading and miss
    // the event; the page takes it when it gets the event or once loaded.
    *PENDING_CHAT.lock().unwrap_or_else(|e| e.into_inner()) = Some((kind, id.clone()));
    // On Android the tap brought the activity up already.
    if cfg!(target_os = "android") || !show_window(app) {
        let _ = app.emit("open-chat", json!({ "kind": kind, "id": id }));
    }
}

/// A notification whose click opens that chat (on Android, through the
/// notification's intent: the app may have been closed meanwhile).
fn notify_chat(app: &AppHandle, title: &str, body: &str, kind: &'static str, id: String) {
    #[cfg(target_os = "android")]
    notify::notify_chat(app, title, body, kind, &id);
    #[cfg(not(target_os = "android"))]
    {
        let handle = app.clone();
        notify::notify(app, title, body, move || open_chat(&handle, kind, id));
    }
}

fn window_focused(app: &AppHandle) -> bool {
    // The window cannot tell there: the activity does.
    #[cfg(target_os = "android")]
    {
        let _ = app;
        android::foreground()
    }
    #[cfg(not(target_os = "android"))]
    main_window(app)
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false)
}

#[cfg(desktop)]
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

#[cfg(desktop)]
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

#[cfg_attr(mobile, allow(unused_variables))]
fn apply_auto_launch(app: &AppHandle, enable: bool) {
    // Only installed desktop builds register themselves at login.
    if cfg!(debug_assertions) || cfg!(mobile) {
        return;
    }
    #[cfg(desktop)]
    {
        let launcher = app.autolaunch();
        let _ = if enable {
            launcher.enable()
        } else {
            launcher.disable()
        };
    }
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
        Event::FriendRoute { friend_id, route } => {
            let _ = app.emit(
                "friend-route",
                json!({ "friendId": friend_id, "route": route }),
            );
        }
        Event::Transfer(t) => {
            let _ = app.emit("transfer-update", t);
        }
        Event::FriendsChanged => {
            let _ = app.emit("friends-changed", ());
            // A friend added or gone: also the list of LocalSend devices.
            let _ = app.emit("localsend-devices", ());
        }
        Event::InviteAccepted { friend_id, name } => {
            if notice_allowed(app, Notice::Friend(&friend_id)) {
                let who = if name.is_empty() { "Votre ami" } else { name.as_str() };
                notify_chat(
                    app,
                    "Invitation acceptée",
                    &format!("{who} est maintenant dans vos amis."),
                    "friend",
                    friend_id,
                );
            }
        }
        Event::MessageStatus { friend_id, id } => {
            let _ = app.emit(
                "message-status",
                json!({ "friendId": friend_id, "id": id, "status": "delivered" }),
            );
        }
        Event::MessagesRead { friend_id, ids } => {
            let _ = app.emit("messages-read", json!({ "friendId": friend_id, "ids": ids }));
        }
        Event::Reactions {
            friend_id,
            id,
            reactions,
        } => {
            let _ = app.emit(
                "message-reactions",
                json!({ "friendId": friend_id, "id": id, "reactions": reactions }),
            );
        }
        Event::Typing { friend_id } => {
            let _ = app.emit("friend-typing", json!({ "friendId": friend_id }));
        }
        Event::Message { friend_id, message } => {
            let _ = app.emit(
                "new-message",
                json!({ "friendId": friend_id, "message": message }),
            );
            // An image's offer has its own notification.
            if !window_focused(app)
                && message.file.is_none()
                && notice_allowed(app, Notice::Friend(&friend_id))
            {
                notify_chat(
                    app,
                    &friend_name(app, &friend_id),
                    &message.text,
                    "friend",
                    friend_id,
                );
            }
        }
        Event::FileOffer(t, count) => {
            if !notice_allowed(app, Notice::File) {
                return;
            }
            let handle = app.clone();
            let from = sender_name(app, &t);
            let (title, what) = if let Some(folder) = &t.folder {
                (
                    format!("{from} vous envoie un dossier"),
                    format!(
                        "\"{}\" ({} fichiers, {})",
                        folder.name,
                        folder.files,
                        format_size(folder.size)
                    ),
                )
            } else if count > 1 {
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
            if !notice_allowed(app, Notice::File) {
                return;
            }
            let handle = app.clone();
            let from = sender_name(app, &t);
            // A folder's: once all its files are in.
            let (title, body, shown) = match &t.folder {
                Some(folder) => (
                    format!("Dossier reçu de {from}"),
                    format!("\"{}\" a été enregistré.", folder.name),
                    folder.root.clone(),
                ),
                None => (
                    format!("Fichier reçu de {from}"),
                    format!("\"{}\" a été enregistré.", t.file_name),
                    t.saved_path.clone(),
                ),
            };
            notify::notify(app, &title, &body, move || {
                if let Some(path) = shown {
                    reveal_path(&handle, path);
                }
            });
        }
        Event::GroupUpdate { gid } => {
            let _ = app.emit("group-update", json!({ "gid": gid }));
        }
        Event::GroupMessage {
            gid,
            group,
            author,
            text,
            mention,
        } => {
            // Muted groups only send what mentions us (see groups.rs).
            if !window_focused(app) && notice_allowed(app, Notice::Group) {
                let title = if mention {
                    format!("{author} vous a mentionné dans {group}")
                } else {
                    format!("{group} — {author}")
                };
                notify_chat(app, &title, &text, "group", gid);
            }
        }
        Event::RelayStatus(status) => {
            let _ = app.emit("relay-status", status);
        }
        Event::GroupInvite { gid, group, from } => {
            let _ = app.emit("group-update", json!({ "gid": gid }));
            if !notice_allowed(app, Notice::Group) {
                return;
            }
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

// ─── System events ───────────────────────────────────────────────────────────

/// Follow what the system tells (Windows): sleep, session lock, power
/// source, network changes.
fn watch_system(app: &AppHandle) {
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let watched = system::watch(tx);
    app.state::<AppState>()
        .network_watched
        .store(watched, Ordering::SeqCst);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let (mut locked, mut on_battery) = (false, false);
        let mut network_due: Option<tokio::time::Instant> = None;
        loop {
            let due = network_due.unwrap_or_else(tokio::time::Instant::now);
            tokio::select! {
                event = events.recv() => {
                    let Some(event) = event else { return };
                    match event {
                        SystemEvent::NetworkChanged => {
                            network_due.get_or_insert_with(|| {
                                tokio::time::Instant::now() + NETWORK_SETTLE
                            });
                        }
                        SystemEvent::Locked(on) => locked = on,
                        SystemEvent::OnBattery(on) => on_battery = on,
                        event => on_system_event(&app, event),
                    }
                    set_economy(&app, locked || on_battery);
                }
                _ = tokio::time::sleep_until(due), if network_due.is_some() => {
                    network_due = None;
                    on_system_event(&app, SystemEvent::NetworkChanged);
                }
            }
        }
    });
}

fn on_system_event(app: &AppHandle, event: SystemEvent) {
    let state = app.state::<AppState>();
    match event {
        // Dropped once done, which lets the system go to sleep.
        SystemEvent::Suspend(_handled) => state.node.suspend(),
        SystemEvent::Resume => {
            state.node.resume();
            state.with_localsend(LocalSend::resumed);
        }
        SystemEvent::NetworkChanged => {
            state.node.network_changed();
            state.with_localsend(LocalSend::network_changed);
        }
        SystemEvent::Locked(_) | SystemEvent::OnBattery(_) => {}
    }
}

/// Session locked or on battery: background work is cut down until both
/// are over, then catches up.
fn set_economy(app: &AppHandle, on: bool) {
    let state = app.state::<AppState>();
    if state.economy.swap(on, Ordering::SeqCst) == on {
        return;
    }
    state.node.set_economy(on);
    state.with_localsend(|service| service.set_economy(on));
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
/// The identity, sealed with the history key (see vault.rs): a copy of the
/// file is of no use on another account or computer. An older plain file is
/// sealed on first start. A sealed one that can't be opened stops the start:
/// a new identity would lose every friend, a backup brings it back.
fn load_identity(path: &Path) -> std::io::Result<SecretKey> {
    use std::io::{Error, ErrorKind};
    let parse = |bytes: &[u8]| {
        std::str::from_utf8(bytes)
            .ok()
            .and_then(|text| hex::decode(text.trim()).ok())
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
    };
    match std::fs::read(path) {
        Ok(bytes) if vault::is_sealed(&bytes) => {
            let plain = vault::open(&bytes).map_err(|e| {
                // InvalidData: sealed with a key this session will never have.
                let message = if e.kind() == ErrorKind::InvalidData {
                    format!(
                        "votre identité ne peut pas être déchiffrée sur cette session ({e}). \
                         Restaurez une sauvegarde (Paramètres → Sauvegarde) pour retrouver votre ID."
                    )
                } else {
                    format!(
                        "la clé qui protège votre identité est indisponible ({e}). \
                         Relancez l'application et autorisez son accès."
                    )
                };
                Error::new(e.kind(), message)
            })?;
            return parse(&plain)
                .map(|seed| SecretKey::from_bytes(&seed))
                .ok_or_else(|| Error::new(ErrorKind::InvalidData, "identity.key déchiffré mais illisible"));
        }
        Ok(bytes) => match parse(&bytes) {
            Some(seed) => {
                seal_identity(path, &seed);
                return Ok(SecretKey::from_bytes(&seed));
            }
            None => {
                // Kept aside: it might still be recovered by hand.
                let aside = path.with_extension(format!("bad-{}", rand::random::<u32>()));
                std::fs::rename(path, &aside)?;
                eprintln!("identity.key unreadable, moved to {}", aside.display());
            }
        },
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
    match vault::seal(hex::encode(seed).as_bytes()) {
        Ok(sealed) => store::write_atomic(path, &sealed)?,
        // No history key this run (keychain refused…): plain, sealed later.
        Err(_) => store::write_atomic(path, hex::encode(seed).as_bytes())?,
    }
    let _ = std::fs::remove_file(bak_of(path));
    Ok(SecretKey::from_bytes(&seed))
}

/// Replace a plain identity file with a sealed one, and drop the plain copy
/// the atomic write keeps. Left plain if the key is unavailable this run.
fn seal_identity(path: &Path, seed: &[u8; 32]) {
    let Ok(sealed) = vault::seal(hex::encode(seed).as_bytes()) else {
        return;
    };
    match store::write_atomic(path, &sealed) {
        Ok(()) => {
            let _ = std::fs::remove_file(bak_of(path));
        }
        Err(e) => eprintln!("identity.key not sealed: {e}"),
    }
}

fn bak_of(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let dir = data_dir(app);
    // A restored backup takes the place of the data folder before anything
    // in it is opened.
    backup::apply_pending(&dir);
    store::create_private_dir(&dir)?;
    migrate_from_electron(app, &dir);
    // Before any history is read: it is encrypted with this key.
    vault::init(&dir);

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
            localsend: true,
            welcomed: false,
            dnd_until: 0,
            read_receipts: true,
            auto_update: false,
            update_failed: String::new(),
            speed_limit: 0,
            notifications: NotifySettings::default(),
            background_service: true,
        },
    );
    if config.data.download_path.is_empty() {
        config.data.download_path = default_downloads.to_string_lossy().into_owned();
        log_save(config.save_now());
    }
    let auto_launch = config.data.auto_launch;
    config.data.localsend = true;
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
    // Users of an earlier version who have friends need no welcome.
    if !friends.data.is_empty() {
        let mut config = config.lock().unwrap_or_else(|e| e.into_inner());
        if !config.data.welcomed {
            config.data.welcomed = true;
            config.mark_dirty();
        }
    }

    let handle = app.clone();
    let cfg = config.clone();
    let node = tauri::async_runtime::block_on(Node::start(p2p::Options {
        secret: load_identity(&dir.join("identity.key"))?,
        friends,
        // Encrypted: whoever reads a code may use the link.
        invites: JsonStore::load_sealed(dir.join("invites.json"), Vec::new()),
        messages_dir: dir.join("messages"),
        download_dir: Arc::new(move || {
            // No folder picker there: always the shared Downloads.
            #[cfg(target_os = "android")]
            if cfg!(target_os = "android") {
                return android::download_dir();
            }
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
    {
        let config = config.lock().unwrap_or_else(|e| e.into_inner());
        node.set_read_receipts(config.data.read_receipts);
        node.set_speed_limit(Some(config.data.speed_limit));
    }

    app.manage(AppState {
        node,
        config,
        localsend: Mutex::new(LocalSendSlot::default()),
        localsend_chats: Mutex::new(JsonStore::load_sealed(
            dir.join("localsend-chats.json"),
            LocalSendChats::new(),
        )),
        localsend_save_armed: AtomicBool::new(false),
        economy: AtomicBool::new(false),
        network_watched: AtomicBool::new(false),
        data_dir: dir,
    });
    let dnd_until = app.state::<AppState>().config().data.dnd_until;
    apply_dnd(app, dnd_until);
    watch_system(app);
    {
        // In the background: the port may take a moment to give up on.
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            app.state::<AppState>().start_localsend(&app).await;
        });
    }

    apply_auto_launch(app, auto_launch);
    #[cfg(target_os = "android")]
    {
        android::listen(app);
        android::set_service(app.state::<AppState>().config().data.background_service);
    }
    #[cfg(desktop)]
    {
        create_tray(app)?;
        update::start(app);
    }
    {
        use tauri_plugin_deep_link::DeepLinkExt;
        // The installers register the links too; an AppImage, a portable or
        // moved app only this way. Never a development build.
        #[cfg(any(windows, target_os = "linux"))]
        if !cfg!(debug_assertions) {
            if let Err(e) = app.deep_link().register_all() {
                eprintln!("invitation links not registered: {e}");
            }
        }
        let handle = app.clone();
        app.deep_link()
            .on_open_url(move |event| on_invite_link(&handle, event.urls()));
        // Started by a link (Windows, Linux; macOS and Android tell it later).
        #[cfg(desktop)]
        if let Ok(Some(urls)) = app.deep_link().get_current() {
            on_invite_link(app, urls);
        }
    }
    // Started at login: stay in the tray without creating the web view.
    let restarted = std::env::var_os(SHOW_AFTER_RESTART).is_some();
    std::env::remove_var(SHOW_AFTER_RESTART);
    if cfg!(mobile) || restarted || !std::env::args().any(|a| a == HIDDEN_ARG) {
        show_window(app);
    }
    Ok(())
}

/// Without a console (release build on Windows), the error would go unseen.
fn startup_failed(message: &str) {
    eprintln!("{message}");
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(
                hwnd: *mut std::ffi::c_void,
                text: *const u16,
                caption: *const u16,
                kind: u32,
            ) -> i32;
        }
        const MB_OK_ICONERROR: u32 = 0x10;
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (text, caption) = (wide(message), wide("P2P Share"));
        // SAFETY: both strings are NUL-terminated and outlive the call.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                caption.as_ptr(),
                MB_OK_ICONERROR,
            );
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK's DMA-BUF renderer shows a blank window with NVIDIA's
    // driver; set before any thread starts (unless the user chose).
    #[cfg(target_os = "linux")]
    if std::path::Path::new("/proc/driver/nvidia").exists()
        && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
    {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    // Two workers are plenty for what the app does (the default is one per
    // core, as many idle threads); blocking work has its own pool.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build();
    // Kept for the life of the app.
    let _runtime = match runtime {
        Ok(runtime) => {
            tauri::async_runtime::set(runtime.handle().clone());
            Some(runtime)
        }
        Err(e) => {
            eprintln!("own runtime unavailable, Tauri's used: {e}");
            None
        }
    };
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();
    // An instance with its own data folder (tests) runs alongside the real one.
    #[cfg(desktop)]
    if std::env::var_os("P2PSHARE_DATA_DIR").is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_window(app);
        }));
    }
    #[cfg(desktop)]
    {
        builder = builder
            .on_window_event(|window, event| {
                if window.label() == "main" {
                    if let tauri::WindowEvent::Focused(false) = event {
                        on_focus_lost(window.app_handle());
                    }
                }
            })
            .plugin(tauri_plugin_autostart::init(
                MacosLauncher::LaunchAgent,
                Some(vec![HIDDEN_ARG]),
            ))
            .plugin(tauri_plugin_updater::Builder::new().build());
    }
    #[cfg(target_os = "android")]
    {
        builder = builder.plugin(android::init());
    }
    let app = builder
        // After single-instance, which hands it the links a second launch got.
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            hide_window,
            update::get_update,
            update::check_update,
            update::install_update,
            update::set_auto_update,
            get_my_id,
            get_friends,
            add_friend,
            remove_friend,
            create_invite,
            take_invite_link,
            read_invite_link,
            accept_invite,
            rename_friend,
            get_config,
            select_download_dir,
            set_auto_launch,
            set_background_service,
            get_messages,
            send_message,
            send_file,
            send_paths,
            chat_image,
            open_chat_file,
            open_url,
            finish_welcome,
            get_transfers,
            clear_finished,
            take_open_chat,
            respond_to_file_offer,
            cancel_transfer,
            pause_transfer,
            resume_transfer,
            set_speed_limit,
            set_friend_auto_accept,
            respond_to_folder,
            send_folder,
            react_message,
            mark_chat_read,
            chat_typing,
            set_read_receipts,
            set_notifications,
            set_window_pinned,
            get_window_pinned,
            set_chat_muted,
            show_in_folder,
            open_transfer,
            set_nickname,
            get_relay_status,
            set_relay,
            get_localsend_status,
            get_localsend_chats,
            get_localsend_chat,
            mark_localsend_read,
            send_localsend_text,
            get_localsend_devices,
            scan_localsend,
            send_localsend_files,
            send_localsend_paths,
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
            set_dnd,
            backup::export_backup,
            backup::import_backup,
            add_group_paths,
            set_group_muted,
            pin_group_item,
            unpin_group_item,
        ])
        .setup(|app| {
            setup(app.handle()).inspect_err(|e| {
                // Kept for a bug report: the message box goes away.
                let dir = data_dir(app.handle());
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(
                    dir.join("startup-error.log"),
                    format!("{} : {e}\n", now_ms()),
                );
            })
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(e) => {
            startup_failed(&format!("P2P Share n'a pas pu démarrer :\n\n{e}"));
            std::process::exit(1);
        }
    };

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
                state.save_localsend_chats();
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
