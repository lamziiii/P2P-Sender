//! Native notifications whose click runs a callback (show the window, open a
//! chat, reveal a file), on every desktop platform.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::AppHandle;

/// Notifications shown per `WINDOW`: beyond, they are dropped (a peer
/// flooding us must not bury the desktop in toasts).
const MAX_SHOWN: usize = 10;
const WINDOW: Duration = Duration::from_secs(10);

/// Do not disturb until then (unix ms; 0 off, `u64::MAX` until turned off):
/// no notification at all meanwhile.
static DND_UNTIL: AtomicU64 = AtomicU64::new(0);

pub fn set_dnd_until(until: u64) {
    DND_UNTIL.store(until, AtomicOrdering::SeqCst);
}

fn do_not_disturb() -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    now < DND_UNTIL.load(AtomicOrdering::SeqCst)
}

fn allowed() -> bool {
    if do_not_disturb() {
        return false;
    }
    static SHOWN: Mutex<VecDeque<Instant>> = Mutex::new(VecDeque::new());
    let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    while shown.front().is_some_and(|t| t.elapsed() >= WINDOW) {
        shown.pop_front();
    }
    if shown.len() >= MAX_SHOWN {
        return false;
    }
    shown.push_back(Instant::now());
    true
}

#[cfg(windows)]
pub fn notify(app: &AppHandle, title: &str, body: &str, on_click: impl FnOnce() + Send + 'static) {
    use tauri_winrt_notification::Toast;

    if !allowed() {
        return;
    }

    // Installed builds have a Start menu shortcut registered with the app
    // identifier; dev builds borrow PowerShell's so toasts still show.
    let app_id = if cfg!(debug_assertions) {
        Toast::POWERSHELL_APP_ID.to_string()
    } else {
        app.config().identifier.clone()
    };
    let on_click = Mutex::new(Some(on_click));
    let _ = Toast::new(&app_id)
        .title(title)
        .text1(body)
        .on_activated(move |_| {
            if let Some(f) = on_click.lock().ok().and_then(|mut f| f.take()) {
                f();
            }
            Ok(())
        })
        .show();
}

#[cfg(not(any(windows, target_os = "android", target_os = "ios")))]
pub fn notify(app: &AppHandle, title: &str, body: &str, on_click: impl FnOnce() + Send + 'static) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    /// Threads waiting for a click.
    const MAX_WAITING: usize = 16;
    static WAITING: AtomicUsize = AtomicUsize::new(0);

    if !allowed() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        use std::sync::Once;
        static INIT: Once = Once::new();
        let identifier = app.config().identifier.clone();
        INIT.call_once(move || {
            let _ = notify_rust::set_application(&identifier);
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;

    // Linux notification servers read the body as markup: a friend's
    // message shows as written, never as bold text or a link.
    #[cfg(not(target_os = "macos"))]
    let body = &body.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut n = notify_rust::Notification::new();
    n.summary(title).body(body);
    #[cfg(not(target_os = "macos"))]
    n.appname("P2P Share").action("default", "Ouvrir");
    let n = n.finalize();
    // Waiting for the click blocks, so it gets its own thread (a bounded
    // number of them: past that, the notification shows without a click).
    if WAITING.fetch_add(1, Ordering::SeqCst) >= MAX_WAITING {
        WAITING.fetch_sub(1, Ordering::SeqCst);
        let _ = n.show();
        return;
    }
    std::thread::spawn(move || {
        if let Ok(handle) = n.show() {
            handle.wait_for_action(|action| {
                if action == "default" {
                    on_click();
                }
            });
        }
        WAITING.fetch_sub(1, Ordering::SeqCst);
    });
}

// Android: posted by the app's Kotlin side (channel « Messages et
// fichiers »), whose activity asks for the permission (Android 13+). A tap
// opens the app; one about a chat opens that chat, through the
// notification's intent, so the callback is not used.
#[cfg(target_os = "android")]
pub fn notify(_app: &AppHandle, title: &str, body: &str, _on_click: impl FnOnce() + Send + 'static) {
    if allowed() {
        crate::android::notify(title, body, None);
    }
}

/// A notification whose tap opens that chat ("friend", "group" or
/// "localsend"); one per chat, the latest replacing the one before.
#[cfg(target_os = "android")]
pub fn notify_chat(_app: &AppHandle, title: &str, body: &str, kind: &str, id: &str) {
    if allowed() {
        crate::android::notify(title, body, Some((kind, id)));
    }
}

#[cfg(target_os = "ios")]
pub fn notify(_app: &AppHandle, _title: &str, _body: &str, _on_click: impl FnOnce() + Send + 'static) {
    let _ = allowed();
}
