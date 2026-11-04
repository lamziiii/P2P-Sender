//! Updates, on desktop: the app looks for a newer release (latest.json of
//! the latest GitHub release, signed with the updater key) a minute after
//! starting and then every few hours, and tells the window. The user
//! installs it from there, or it is installed by itself once no transfer
//! runs (setting `auto_update`): the data is saved, the new version
//! installed and the app restarted. An install that fails (an installer
//! blocked by the system…) restarts the old version, and that version is
//! then only installed when asked, so it cannot loop. Android has none: the
//! window links to the releases.

use serde::Serialize;

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

    use super::UpdateView;
    use crate::{log_save, AppState, SHOW_AFTER_RESTART};

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

    /// A version as shown: its optional fourth number is semver build
    /// metadata in the files (`3.4.7+2`, which Cargo, npm and Tauri take),
    /// shown as one, as in the tags and CHANGELOG.md (3.4.7.2).
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
            .ok_or("Aucune mise à jour à installer.")?;
        if pending.installing.swap(true, Ordering::SeqCst) {
            return Err("La mise à jour est déjà en cours.".into());
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
                let message = format!("Téléchargement impossible : {e}");
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
        let found = check(&app)
            .await
            .map_err(|_| "Impossible de vérifier les mises à jour (pas de connexion ?)".to_string())?;
        if found.is_some() && auto_update(&app) {
            install_when_idle(&app);
        }
        Ok(found)
    }

    #[tauri::command]
    pub async fn install_update(app: AppHandle) -> Result<(), String> {
        install(&app, true).await
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
}

#[cfg(desktop)]
pub use imp::*;

#[cfg(mobile)]
mod imp {
    use super::UpdateView;

    const ANDROID: &str = "Sur Android, installez la nouvelle version depuis la page des versions.";

    #[tauri::command]
    pub fn get_update() -> Option<UpdateView> {
        None
    }

    #[tauri::command]
    pub async fn check_update() -> Result<Option<UpdateView>, String> {
        Err(ANDROID.into())
    }

    #[tauri::command]
    pub async fn install_update() -> Result<(), String> {
        Err(ANDROID.into())
    }

    #[tauri::command]
    pub fn set_auto_update() -> bool {
        false
    }
}

#[cfg(mobile)]
pub use imp::*;
