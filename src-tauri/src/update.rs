//! Updates, on desktop: the app looks for a newer release (latest.json of
//! the latest GitHub release, signed with the updater key) a minute after
//! starting and then every few hours, and tells the window. The user
//! installs it from there, or it is installed by itself once no transfer
//! runs (setting `auto_update`): the data is saved, the new version
//! installed and the app restarted. An install that fails (an installer
//! blocked by the system…) restarts the old version, and that version is
//! then only installed when asked, so it cannot loop.
//!
//! Android reads the same latest.json (its `android` entry, which the
//! desktop updater does not read) and, when asked, downloads the APK and
//! opens the system's installer: it asks the user, and refuses an APK not
//! signed like the app installed. Never by itself.

use serde::Serialize;

use crate::lang::tr;

/// A version as shown: its optional fourth number is semver build metadata
/// in the files (`3.4.7+2`, which Cargo, npm and Tauri take), shown as one,
/// as in the tags and CHANGELOG.md (3.4.7.2).
fn shown(version: &str) -> String {
    match version.split_once('+') {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{base}.{n}")
        }
        _ => version.to_string(),
    }
}

/// The updater's test: semver precedence, then the fourth number, which
/// precedence leaves out (3.4.7.2 comes after 3.4.7).
pub fn is_newer(current: &semver::Version, release: &semver::Version) -> bool {
    let fourth = |v: &semver::Version| v.build.as_str().parse::<u64>().unwrap_or(0);
    release
        .cmp_precedence(current)
        .then_with(|| fourth(release).cmp(&fourth(current)))
        .is_gt()
}

/// Looking for a version failed (offline...), as the window says it.
fn check_failed() -> String {
    tr(
        "Impossible de vérifier les mises à jour (pas de connexion ?)",
        "Could not check for updates (no connection?)",
    )
    .to_string()
}

/// A newer version, for the window.
#[derive(Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateView {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Its install failed already.
    pub failed: bool,
}

#[cfg(desktop)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use serde_json::json;
    use tauri::{AppHandle, Emitter, Manager, State};
    use tauri_plugin_updater::{Update, UpdaterExt};

    use super::{check_failed, shown, UpdateView};
    use crate::lang::tr;
    use crate::{log_save, trf, AppState, SHOW_AFTER_RESTART};

    const FIRST_CHECK: Duration = Duration::from_secs(60);
    const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
    /// Installing by itself: how often to look again while transfers run.
    const IDLE_RETRY: Duration = Duration::from_secs(60);

    /// The newer version found, if any.
    #[derive(Default)]
    pub struct Pending {
        update: Mutex<Option<Update>>,
        installing: AtomicBool,
        /// An automatic install waits for the transfers to end.
        waiting: AtomicBool,
    }

    fn failed_version(app: &AppHandle) -> String {
        app.try_state::<AppState>()
            .map(|s| s.config().data.update_failed.clone())
            .unwrap_or_default()
    }

    fn view(app: &AppHandle, update: &Update) -> UpdateView {
        UpdateView {
            version: shown(&update.version),
            notes: update.body.clone().filter(|b| !b.trim().is_empty()),
            failed: failed_version(app) == update.version,
        }
    }

    fn auto_update(app: &AppHandle) -> bool {
        app.try_state::<AppState>()
            .is_some_and(|s| s.config().data.auto_update)
    }

    fn transfers_running(app: &AppHandle) -> bool {
        app.try_state::<AppState>()
            .is_some_and(|s| s.node.transfers().iter().any(|t| t.status.is_active()))
    }

    /// Look now; a newer version is kept and told to the window.
    async fn check(app: &AppHandle) -> Result<Option<UpdateView>, String> {
        let found = app
            .updater()
            .map_err(|e| e.to_string())?
            .check()
            .await
            .map_err(|e| e.to_string())?;
        let found_view = found.as_ref().map(|u| view(app, u));
        let pending = app.state::<Pending>();
        let mut slot = pending.update.lock().unwrap_or_else(|e| e.into_inner());
        let known = slot.as_ref().map(|u| view(app, u));
        *slot = found;
        drop(slot);
        if found_view.is_some() && found_view != known {
            let _ = app.emit("update-available", &found_view);
        }
        Ok(found_view)
    }

    /// Download the version found (progress in `update-progress`), then
    /// save everything, install it and restart. `show`: the window opens
    /// after the restart.
    async fn install(app: &AppHandle, show: bool) -> Result<(), String> {
        let pending = app.state::<Pending>();
        let update = pending
            .update
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(tr("Aucune mise à jour à installer.", "No update to install."))?;
        if pending.installing.swap(true, Ordering::SeqCst) {
            return Err(tr("La mise à jour est déjà en cours.", "The update is already running.").into());
        }
        // Every view of the update shows it started, not only the one clicked.
        let _ = app.emit("update-progress", json!({ "done": 0, "total": null }));
        let mut done = 0u64;
        let progress = app.clone();
        let bytes = update
            .download(
                move |chunk, total| {
                    done += chunk as u64;
                    let _ = progress.emit("update-progress", json!({ "done": done, "total": total }));
                },
                || {},
            )
            .await
            .map_err(|e| {
                pending.installing.store(false, Ordering::SeqCst);
                let message = trf!("Téléchargement impossible : {e}", "Download failed: {e}");
                // Also for an install started by itself: the window stops
                // showing the download.
                let _ = app.emit("update-error", &message);
                message
            })?;
        // Installing by itself: a transfer started during the download would
        // be cut by the restart, and it does not resume after one.
        if !show && transfers_running(app) {
            let _ = app.emit("update-progress", json!({ "done": bytes.len(), "total": bytes.len(), "waiting": true }));
            while auto_update(app) && transfers_running(app) {
                tokio::time::sleep(IDLE_RETRY).await;
            }
            if !auto_update(app) {
                // Turned off meanwhile: left for the user to install.
                pending.installing.store(false, Ordering::SeqCst);
                let _ = app.emit("update-progress", serde_json::Value::Null);
                return Ok(());
            }
        }
        if let Some(state) = app.try_state::<AppState>() {
            log_save(state.config().save_now());
            state.save_localsend_chats();
            let node = state.node.clone();
            let _ = tokio::time::timeout(Duration::from_secs(2), node.shutdown()).await;
        }
        if show {
            std::env::set_var(SHOW_AFTER_RESTART, "1");
        }
        // On Windows the installer takes over and this process ends here.
        // Failed, the network is closed already: the old version restarts,
        // and does not try this one again by itself.
        if let Err(e) = update.install(bytes) {
            eprintln!("mise à jour : installation impossible : {e}");
            if let Some(state) = app.try_state::<AppState>() {
                let mut config = state.config();
                config.data.update_failed = update.version.clone();
                log_save(config.save_now());
            }
            std::env::set_var(SHOW_AFTER_RESTART, "1");
        }
        app.restart()
    }

    /// With the setting on, install the version found once no transfer runs,
    /// unless its install failed already.
    fn install_when_idle(app: &AppHandle) {
        let pending = app.state::<Pending>();
        let version = pending
            .update
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|u| u.version.clone());
        if version.is_none_or(|v| v == failed_version(app))
            || pending.waiting.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            while auto_update(&app) && transfers_running(&app) {
                tokio::time::sleep(IDLE_RETRY).await;
            }
            app.state::<Pending>().waiting.store(false, Ordering::SeqCst);
            if auto_update(&app) {
                if let Err(e) = install(&app, false).await {
                    eprintln!("mise à jour : {e}");
                }
            }
        });
    }

    pub fn start(app: &AppHandle) {
        app.manage(Pending::default());
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(FIRST_CHECK).await;
            loop {
                // Offline or no release yet: tried again next time.
                match check(&app).await {
                    Ok(Some(_)) if auto_update(&app) => install_when_idle(&app),
                    Err(e) => eprintln!("mise à jour : {e}"),
                    _ => {}
                }
                tokio::time::sleep(CHECK_EVERY).await;
            }
        });
    }

    #[tauri::command]
    pub fn get_update(app: AppHandle, pending: State<Pending>) -> Option<UpdateView> {
        pending
            .update
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|u| view(&app, u))
    }

    #[tauri::command]
    pub async fn check_update(app: AppHandle) -> Result<Option<UpdateView>, String> {
        let found = check(&app).await.map_err(|_| check_failed())?;
        if found.is_some() && auto_update(&app) {
            install_when_idle(&app);
        }
        Ok(found)
    }

    #[tauri::command]
    pub async fn install_update(app: AppHandle) -> Result<(), String> {
        install(&app, true).await
    }

    /// Updates installed from the .deb (or an .rpm) ask for the password:
    /// the window says so next to installing them by themselves.
    #[tauri::command]
    pub fn update_asks_password() -> bool {
        use tauri::utils::config::BundleType;
        matches!(
            tauri::utils::platform::bundle_type(),
            Some(BundleType::Deb | BundleType::Rpm)
        )
    }

    /// Install newer versions without asking; one already found goes now
    /// (once no transfer runs).
    #[tauri::command]
    pub fn set_auto_update(app: AppHandle, state: State<AppState>, enable: bool) -> bool {
        {
            let mut config = state.config();
            config.data.auto_update = enable;
            log_save(config.save_now());
        }
        if enable {
            install_when_idle(&app);
        }
        enable
    }
}

#[cfg(desktop)]
pub use imp::*;

#[cfg(mobile)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use serde_json::Value;
    use tauri::{AppHandle, Emitter, Manager, State};

    use super::{check_failed, is_newer, shown, UpdateView};
    use crate::lang::tr;
    use crate::{log_save, AppState};

    /// The desktop updater's (tauri.conf.json).
    const LATEST: &str =
        "https://github.com/lamziiii/P2P-Sender/releases/latest/download/latest.json";
    const FIRST_CHECK: Duration = Duration::from_secs(60);
    const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

    #[derive(Clone, PartialEq)]
    struct Found {
        version: String,
        notes: Option<String>,
        apk: String,
    }

    impl Found {
        fn view(&self) -> UpdateView {
            UpdateView {
                version: shown(&self.version),
                notes: self.notes.clone(),
                failed: false,
            }
        }
    }

    #[derive(Default)]
    pub struct Pending {
        found: Mutex<Option<Found>>,
        installing: AtomicBool,
    }

    /// The release's version and APK, if newer than this app. A release
    /// without an APK in latest.json (before 3.4.9) counts as none.
    fn parse(app: &AppHandle, latest: &str) -> Option<Found> {
        let latest: Value = serde_json::from_str(latest).ok()?;
        let version = latest.get("version")?.as_str()?.to_string();
        let apk = latest.get("android")?.get("url")?.as_str()?.to_string();
        let release = semver::Version::parse(&version).ok()?;
        if !is_newer(&app.package_info().version, &release) {
            return None;
        }
        let notes = latest
            .get("notes")
            .and_then(Value::as_str)
            .filter(|n| !n.trim().is_empty())
            .map(str::to_string);
        Some(Found { version, notes, apk })
    }

    /// Look now; a newer version is kept and told to the window.
    async fn check(app: &AppHandle) -> Result<Option<UpdateView>, String> {
        let text = tauri::async_runtime::spawn_blocking(|| crate::android::fetch_text(LATEST))
            .await
            .map_err(|e| e.to_string())??;
        let found = parse(app, &text);
        let view = found.as_ref().map(Found::view);
        let known = std::mem::replace(
            &mut *app
                .state::<Pending>()
                .found
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
            found.clone(),
        );
        if found.is_some() && known != found {
            let _ = app.emit("update-available", &view);
        }
        Ok(view)
    }

    pub fn start(app: &AppHandle) {
        app.manage(Pending::default());
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(FIRST_CHECK).await;
            loop {
                // Offline or no release yet: tried again next time.
                if let Err(e) = check(&app).await {
                    eprintln!("update: {e}");
                }
                tokio::time::sleep(CHECK_EVERY).await;
            }
        });
    }

    #[tauri::command]
    pub fn get_update(pending: State<Pending>) -> Option<UpdateView> {
        pending
            .found
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(Found::view)
    }

    #[tauri::command]
    pub async fn check_update(app: AppHandle) -> Result<Option<UpdateView>, String> {
        check(&app).await.map_err(|_| check_failed())
    }

    /// Everything saved, then the APK downloaded (progress in
    /// `update-progress`) and handed to the installer, which closes the app
    /// once the user agrees.
    #[tauri::command]
    pub async fn install_update(app: AppHandle) -> Result<(), String> {
        let pending = app.state::<Pending>();
        let found = pending
            .found
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(tr("Aucune mise à jour à installer.", "No update to install."))?;
        if pending.installing.swap(true, Ordering::SeqCst) {
            return Err(tr("La mise à jour est déjà en cours.", "The update is already running.").into());
        }
        if let Some(state) = app.try_state::<AppState>() {
            log_save(state.config().save_now());
            state.save_localsend_chats();
            state.node.save_now();
        }
        let _ = app.emit("update-progress", serde_json::json!({ "done": 0, "total": null }));
        let apk = found.apk;
        let result = tauri::async_runtime::spawn_blocking(move || crate::android::install_apk(&apk))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
        pending.installing.store(false, Ordering::SeqCst);
        // The installer is open (or it failed): the button comes back, for
        // an install cancelled there.
        let _ = app.emit("update-progress", Value::Null);
        result
    }

    /// Nothing installs by itself: Android always asks.
    #[tauri::command]
    pub fn set_auto_update() -> bool {
        false
    }

    #[tauri::command]
    pub fn update_asks_password() -> bool {
        false
    }
}

#[cfg(mobile)]
pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourth_number() {
        let v = |s: &str| semver::Version::parse(s).unwrap();
        assert!(is_newer(&v("3.4.7"), &v("3.4.7+2")));
        assert!(is_newer(&v("3.4.7+2"), &v("3.4.7+10")));
        assert!(is_newer(&v("3.4.7+9"), &v("3.4.8")));
        assert!(is_newer(&v("3.4.6"), &v("3.4.7")));
        assert!(!is_newer(&v("3.4.7+2"), &v("3.4.7")));
        assert!(!is_newer(&v("3.4.7"), &v("3.4.7")));
        assert!(!is_newer(&v("3.4.7"), &v("3.4.7-beta.1")));
        assert_eq!(shown("3.4.7+2"), "3.4.7.2");
        assert_eq!(shown("3.4.7"), "3.4.7");
        assert_eq!(shown("3.4.7-beta.1"), "3.4.7-beta.1");
    }
}
