//! "Envoyer avec P2P Share": files given to the app from outside, which the
//! window then asks where to send (a friend, a group, a LocalSend device).
//!
//! - Windows: an entry of the Explorer's right-click menu, added or removed
//!   by the setting (`apply_menu`), removed by the uninstaller.
//! - Linux: an entry of "Open with" in the file managers, the same way.
//! - macOS: "Open with" in the Finder (Info.plist), always there.
//! - Android: the share menu of other apps (the manifest), text included.
//!
//! On the desktop the files come as `p2pshare --send <path>...`, to a second
//! launch that hands them to the running app (single-instance plugin).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// The paths to send follow it on the command line (desktop).
#[cfg_attr(mobile, allow(dead_code))]
pub const SEND_ARG: &str = "--send";

/// The Android package: its content providers hold the app's own files.
#[cfg(target_os = "android")]
const ANDROID_APP_ID: &str = "com.p2pshare.app";

#[derive(Clone, Serialize)]
pub struct SharedItem {
    /// A path; on Android a document (content://), copied when sent.
    pub path: String,
    pub name: String,
    pub dir: bool,
}

#[derive(Default, Serialize)]
pub struct Shared {
    pub items: Vec<SharedItem>,
    /// Android: text shared (a link…), sent as a message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// What came and the window has not taken yet.
static PENDING: Mutex<Option<Shared>> = Mutex::new(None);

/// The paths after `--send`; relative ones from `cwd`. Only as the first
/// argument, as the menus give it, and never with a link: the single-instance
/// plugin splits a second launch's arguments on "|", which a link may hold.
#[cfg_attr(mobile, allow(dead_code))]
pub fn paths_from_args(args: &[String], cwd: &Path) -> Vec<PathBuf> {
    if args.get(1).map(String::as_str) != Some(SEND_ARG)
        || args
            .iter()
            .any(|a| a.trim_start().to_ascii_lowercase().starts_with("p2pshare:"))
    {
        return Vec::new();
    }
    args[2..]
        .iter()
        .filter(|a| !a.is_empty())
        .map(|a| cwd.join(a))
        .collect()
}

/// Files and folders that exist, as the window lists them (desktop).
#[cfg_attr(mobile, allow(dead_code))]
pub fn items(paths: &[PathBuf]) -> Vec<SharedItem> {
    paths
        .iter()
        .filter_map(|path| {
            let meta = std::fs::metadata(path).ok()?;
            let name = path
                .file_name()
                .map_or_else(|| path.as_os_str(), |n| n)
                .to_string_lossy()
                .into_owned();
            Some(SharedItem {
                path: path.to_string_lossy().into_owned(),
                name,
                dir: meta.is_dir(),
            })
        })
        .collect()
}

/// Files (or text) to send came: kept with those not taken yet, and the
/// window asks where they go. Nothing usable: the window only shows.
pub fn receive(app: &AppHandle, items: Vec<SharedItem>, text: Option<String>) {
    let text = text.filter(|t| !t.trim().is_empty());
    if items.is_empty() && text.is_none() {
        crate::show_window(app);
        return;
    }
    {
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let shared = pending.get_or_insert_with(Shared::default);
        for item in items {
            if !shared.items.iter().any(|i| i.path == item.path) {
                shared.items.push(item);
            }
        }
        if text.is_some() {
            shared.text = text;
        }
    }
    // A window created now takes them once loaded; one already there is told.
    if cfg!(target_os = "android") || !crate::show_window(app) {
        let _ = app.emit("shared", ());
    }
}

/// What came since the window last asked (each item once).
#[tauri::command]
pub fn take_shared() -> Option<Shared> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Paths to send what was shared. On Android its documents are copied into
/// the app first: for good (`keep`, a group's shared files) or in the cache.
#[tauri::command]
#[cfg_attr(not(target_os = "android"), allow(unused_variables))]
pub async fn shared_paths(paths: Vec<String>, keep: bool) -> Vec<String> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_dialog::FilePath;
        // Read with the app's rights: only another app's documents, never a
        // path or our own provider (they would reach the app's own files).
        let files: Vec<FilePath> = paths
            .into_iter()
            .filter_map(|p| url::Url::parse(&p).ok())
            .filter(|url| {
                url.scheme() == "content"
                    && url.host_str().is_some_and(|host| {
                        !host
                            .split(';')
                            .any(|a| a == ANDROID_APP_ID || a.starts_with(&format!("{ANDROID_APP_ID}.")))
                    })
            })
            .map(FilePath::Url)
            .collect();
        return tauri::async_runtime::spawn_blocking(move || {
            crate::android::local_paths(files, keep)
        })
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    }
    #[cfg(not(target_os = "android"))]
    paths
}

/// Add the entry where the system takes one (Windows, Linux), or remove it.
/// Never from a development build: it would open that build.
pub fn apply_menu(enabled: bool) {
    if cfg!(debug_assertions) {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = menu::apply(enabled) {
            eprintln!("send menu not {}: {e}", if enabled { "added" } else { "removed" });
        }
    });
}

#[cfg(windows)]
mod menu {
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    use super::SEND_ARG;
    use crate::lang::tr;

    /// The entry for files, and for folders. The uninstaller removes the
    /// same keys (windows/hooks.nsh).
    const KEYS: [&str; 2] = [
        r"Software\Classes\*\shell\P2PShare",
        r"Software\Classes\Directory\shell\P2PShare",
    ];

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the key was opened by RegCreateKeyExW and is closed once.
            unsafe { RegCloseKey(self.0) };
        }
    }

    fn create(path: &str) -> Result<Key, String> {
        let name = wide(path);
        let mut key: HKEY = null_mut();
        // SAFETY: `name` is NUL-terminated and outlives the call; `key` is
        // written by it.
        let r = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                name.as_ptr(),
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                null(),
                &mut key,
                null_mut(),
            )
        };
        if r == ERROR_SUCCESS {
            Ok(Key(key))
        } else {
            Err(format!("{path}: error {r}"))
        }
    }

    /// A string value; `None` for the key's default one.
    fn set(key: &Key, name: Option<&str>, value: &str) -> Result<(), String> {
        let name = name.map(wide);
        let data = wide(value);
        // SAFETY: both strings are NUL-terminated and outlive the call; the
        // size is that of `data` in bytes.
        let r = unsafe {
            RegSetValueExW(
                key.0,
                name.as_ref().map_or(null(), |n| n.as_ptr()),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        if r == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("value: error {r}"))
        }
    }

    pub fn apply(enabled: bool) -> Result<(), String> {
        if !enabled {
            for path in KEYS {
                let name = wide(path);
                // SAFETY: `name` is NUL-terminated and outlives the call.
                let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, name.as_ptr()) };
                if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
                    return Err(format!("{path}: error {r}"));
                }
            }
            return Ok(());
        }
        // Written again at each start: a moved (portable) app stays the one opened.
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe = exe.to_string_lossy();
        for path in KEYS {
            let verb = create(path)?;
            set(&verb, None, tr("Envoyer avec P2P Share", "Send with P2P Share"))?;
            set(&verb, Some("Icon"), &format!("\"{exe}\",0"))?;
            // Kept for any number of files: each starts the app, which hands
            // it to the one running.
            set(&verb, Some("MultiSelectModel"), "Player")?;
            let command = create(&format!(r"{path}\command"))?;
            set(&command, None, &format!("\"{exe}\" {SEND_ARG} \"%1\""))?;
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod menu {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use super::SEND_ARG;
    use crate::lang::tr;

    /// Listed in "Open with" for every file and folder, not in the menus.
    fn file() -> Option<PathBuf> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
        Some(data.join("applications").join("p2pshare-send.desktop"))
    }

    /// A word of `Exec`, quoted as the desktop entry specification asks.
    fn quoted(arg: &str) -> String {
        let mut out = String::from("\"");
        for c in arg.chars() {
            match c {
                '"' | '`' | '$' | '\\' => {
                    out.push('\\');
                    out.push(c);
                }
                '%' => out.push_str("%%"),
                _ => out.push(c),
            }
        }
        out.push('"');
        // A string value too: its backslashes are escaped once more.
        out.replace('\\', "\\\\")
    }

    /// The file managers read which types an app opens from a cache.
    fn refresh(path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = Command::new("update-desktop-database")
                .arg(dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    pub fn apply(enabled: bool) -> Result<(), String> {
        let path = file().ok_or("no home folder")?;
        if !enabled {
            return match std::fs::remove_file(&path) {
                Ok(()) => {
                    refresh(&path);
                    Ok(())
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            };
        }
        // An AppImage runs from a mount that changes each time: the file itself.
        let exe = std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .or_else(|| std::env::current_exe().ok())
            .ok_or("no program path")?;
        // The .deb installs its icon under the program's name.
        let icon = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let entry = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name={}\n\
             Comment={}\n\
             Exec={} {SEND_ARG} %F\n\
             Icon={icon}\n\
             Terminal=false\n\
             NoDisplay=true\n\
             MimeType=application/octet-stream;inode/directory;\n",
            tr("Envoyer avec P2P Share", "Send with P2P Share"),
            tr(
                "Envoyer à un ami, un groupe ou un appareil LocalSend",
                "Send to a friend, a group or a LocalSend device"
            ),
            quoted(&exe.to_string_lossy()),
        );
        if std::fs::read_to_string(&path).is_ok_and(|old| old == entry) {
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, entry).map_err(|e| e.to_string())?;
        refresh(&path);
        Ok(())
    }
}

/// macOS (Info.plist) and Android (manifest) declare it once for all.
#[cfg(not(any(windows, target_os = "linux")))]
mod menu {
    pub fn apply(_enabled: bool) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_after_the_flag() {
        let args: Vec<String> = ["p2pshare", "--send", "a.txt", "/abs/b"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let cwd = Path::new("/home/me");
        assert_eq!(
            paths_from_args(&args, cwd),
            vec![PathBuf::from("/home/me/a.txt"), PathBuf::from("/abs/b")]
        );
        assert!(paths_from_args(&args[..1], cwd).is_empty());
        // A link split on "|" by the single-instance plugin.
        let link: Vec<String> = ["p2pshare", "p2pshare://add", "--send", r"\\host\share"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(paths_from_args(&link, cwd).is_empty());
    }
}
