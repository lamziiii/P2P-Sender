//! Native notifications whose click runs a callback (show the window, open a
//! chat, reveal a file), on every desktop platform.

use tauri::AppHandle;

#[cfg(windows)]
pub fn notify(app: &AppHandle, title: &str, body: &str, on_click: impl FnOnce() + Send + 'static) {
    use std::sync::Mutex;
    use tauri_winrt_notification::Toast;

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

#[cfg(not(windows))]
pub fn notify(app: &AppHandle, title: &str, body: &str, on_click: impl FnOnce() + Send + 'static) {
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

    let mut n = notify_rust::Notification::new();
    n.summary(title).body(body);
    #[cfg(not(target_os = "macos"))]
    n.appname("P2P Share").action("default", "Ouvrir");
    let n = n.finalize();
    // Waiting for the click blocks, so it gets its own thread.
    std::thread::spawn(move || {
        if let Ok(handle) = n.show() {
            handle.wait_for_action(|action| {
                if action == "default" {
                    on_click();
                }
            });
        }
    });
}
