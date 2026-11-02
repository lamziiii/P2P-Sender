//! Encryption at rest of the history (chats, group logs) with a key made once
//! per data folder and protected by the system where it can be: Windows
//! DPAPI (user scope), the macOS Keychain; a file only the user can read
//! elsewhere (Secret Service is often missing on Linux).
//!
//! - `seal` / `open`: a whole file. `is_sealed` tells an older, plain file
//!   (to read as it is, then rewrite sealed) from a sealed one.
//! - `seal_line` / `open_line`: one line of an append-only log; a sealed
//!   line never holds a line break, and `open_line` returns a plain line
//!   (written before encryption) unchanged.
//!
//! Errors of `open` / `open_line`: `ErrorKind::InvalidData` means these bytes
//! will never open (damaged, or sealed with a key that is lost): treat them
//! like a corrupted file, set aside and never written over. Any other kind
//! means the key is not available during this run (Keychain access refused,
//! key file locked): treat it like a locked file, shown or skipped but never
//! saved over. `seal` fails only in that second case.
//!
//! `init` reads (or makes) the key of the data folder before any store is
//! loaded; without it (tests, examples) a key for this run only is used.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine as _;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};

use crate::store;

/// The key's file in the data folder: how to find the key, or the key
/// protected by the system.
pub const KEY_FILE: &str = "vault.key";
/// Starts every sealed file: name and format version.
const MAGIC: &[u8] = b"P2PV\x01";
/// Starts every sealed line.
const LINE_PREFIX: &str = "pv1:";
const KEY_LEN: usize = 32;
const READ_ATTEMPTS: u32 = 5;
const READ_RETRY_DELAY: Duration = Duration::from_millis(200);
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "com.p2pshare.app.vault";

enum Vault {
    Ready { key: LessSafeKey, raw: [u8; KEY_LEN] },
    Unavailable(String),
}

static VAULT: OnceLock<Vault> = OnceLock::new();

/// Read or make the key of `dir`; the first call wins. False when the key
/// is not available this run (sealed data is then neither read nor saved).
pub fn init(dir: &Path) -> bool {
    let vault = VAULT.get_or_init(|| match load_or_create(dir) {
        Ok(raw) => ready(raw),
        Err(e) => {
            eprintln!("history key unavailable: {e}");
            Vault::Unavailable(e)
        }
    });
    matches!(vault, Vault::Ready { .. })
}

fn ready(raw: [u8; KEY_LEN]) -> Vault {
    let key = UnboundKey::new(&CHACHA20_POLY1305, &raw).expect("key length");
    Vault::Ready {
        key: LessSafeKey::new(key),
        raw,
    }
}

fn vault() -> &'static Vault {
    VAULT.get_or_init(|| ready(random_key()))
}

fn key() -> io::Result<&'static LessSafeKey> {
    match vault() {
        Vault::Ready { key, .. } => Ok(key),
        Vault::Unavailable(e) => Err(io::Error::other(format!(
            "clé de chiffrement de l'historique indisponible : {e}"
        ))),
    }
}

/// The key itself, for a backup (which protects it with its password).
pub(crate) fn raw_key() -> io::Result<[u8; KEY_LEN]> {
    key()?;
    match vault() {
        Vault::Ready { raw, .. } => Ok(*raw),
        Vault::Unavailable(_) => unreachable!(),
    }
}

fn random_key() -> [u8; KEY_LEN] {
    let mut raw = [0u8; KEY_LEN];
    SystemRandom::new().fill(&mut raw).expect("system random");
    raw
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.to_string())
}

fn seal_raw(plain: &[u8], aad: &[u8]) -> io::Result<Vec<u8>> {
    let key = key()?;
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| io::Error::other("system random unavailable"))?;
    let mut body = plain.to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(aad),
        &mut body,
    )
    .map_err(|_| io::Error::other("encryption failed"))?;
    let mut out = nonce.to_vec();
    out.append(&mut body);
    Ok(out)
}

fn open_raw(sealed: &[u8], aad: &[u8]) -> io::Result<Vec<u8>> {
    let key = key()?;
    if sealed.len() < NONCE_LEN + CHACHA20_POLY1305.tag_len() {
        return Err(invalid("sealed data too short"));
    }
    let (nonce, body) = sealed.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| invalid("bad nonce"))?;
    let mut body = body.to_vec();
    let plain = key
        .open_in_place(nonce, Aad::from(aad), &mut body)
        .map_err(|_| invalid("damaged, or sealed with another key"))?;
    let len = plain.len();
    body.truncate(len);
    Ok(body)
}

/// Whether `data` was written by `seal` (else it is an older, plain file).
pub fn is_sealed(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

/// Encrypt a whole file's content.
pub fn seal(plain: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = MAGIC.to_vec();
    out.extend(seal_raw(plain, MAGIC)?);
    Ok(out)
}

/// Decrypt what `seal` wrote; `InvalidData` for anything else.
pub fn open(data: &[u8]) -> io::Result<Vec<u8>> {
    let body = data
        .strip_prefix(MAGIC)
        .ok_or_else(|| invalid("not sealed"))?;
    open_raw(body, MAGIC)
}

/// Whether a log line was written by `seal_line`.
pub fn is_sealed_line(line: &str) -> bool {
    line.starts_with(LINE_PREFIX)
}

/// Encrypt one log line: printable ASCII, no line break (add your own).
pub fn seal_line(plain: &[u8]) -> io::Result<String> {
    let sealed = seal_raw(plain, LINE_PREFIX.as_bytes())?;
    Ok(format!(
        "{LINE_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sealed)
    ))
}

/// Decrypt a line of `seal_line` (its line break may be left on); a plain
/// line, from before encryption, comes back unchanged.
pub fn open_line(line: &str) -> io::Result<Vec<u8>> {
    let line = line.trim_end_matches(['\r', '\n']);
    let Some(body) = line.strip_prefix(LINE_PREFIX) else {
        return Ok(line.as_bytes().to_vec());
    };
    let sealed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body.trim())
        .map_err(|_| invalid("damaged line"))?;
    open_raw(&sealed, LINE_PREFIX.as_bytes())
}

// ─── The key ─────────────────────────────────────────────────────────────────

enum KeyError {
    /// Never coming back (protected for another user or computer, gone
    /// from the Keychain, damaged): a new key is made.
    Lost(String),
    /// Not this time: nothing is made nor written over.
    #[cfg_attr(not(any(target_os = "macos", target_os = "android")), allow(dead_code))]
    Unavailable(String),
}

fn load_or_create(dir: &Path) -> Result<[u8; KEY_LEN], String> {
    let path = dir.join(KEY_FILE);
    match read_retrying(&path) {
        Ok(text) => match parse_key(text.trim()) {
            Ok(raw) => {
                #[cfg(target_os = "android")]
                if text.trim().starts_with("raw:") {
                    migrate_to_keystore(&path, &raw);
                }
                return Ok(raw);
            }
            Err(KeyError::Unavailable(e)) => return Err(e),
            Err(KeyError::Lost(e)) => {
                // Kept: it might still be recovered by hand.
                let aside = aside_path(&path);
                fs::rename(&path, &aside).map_err(|err| format!("{e} ({err})"))?;
                eprintln!(
                    "history key lost ({e}), moved to {}: older history is set aside",
                    aside.display()
                );
            }
        },
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(format!("lecture de {} impossible : {e}", path.display())),
    }
    let raw = random_key();
    write_key(dir, &raw).map_err(|e| format!("{} not written: {e}", path.display()))?;
    Ok(raw)
}

/// A key kept in a plain file (first Android builds) goes to the Keystore;
/// the file is replaced only by a sealed key known to open, and stays as it
/// was otherwise.
#[cfg(target_os = "android")]
fn migrate_to_keystore(path: &Path, raw: &[u8; KEY_LEN]) {
    let Ok(text) = protect_key(raw) else {
        return;
    };
    match store::write_atomic(path, text.as_bytes()) {
        Ok(()) => {
            // The plain copy the atomic write keeps.
            let mut bak = path.as_os_str().to_owned();
            bak.push(".bak");
            let _ = fs::remove_file(PathBuf::from(bak));
        }
        Err(e) => eprintln!("history key not moved to the Keystore: {e}"),
    }
}

fn read_retrying(path: &Path) -> io::Result<String> {
    let mut attempt = 1;
    loop {
        match fs::read_to_string(path) {
            Err(e) if e.kind() != ErrorKind::NotFound && attempt < READ_ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(READ_RETRY_DELAY);
            }
            result => return result,
        }
    }
}

fn aside_path(path: &Path) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    path.with_file_name(format!("{KEY_FILE}.lost-{secs}"))
}

fn key_from(bytes: &[u8]) -> Result<[u8; KEY_LEN], KeyError> {
    <[u8; KEY_LEN]>::try_from(bytes).map_err(|_| KeyError::Lost("wrong key length".into()))
}

fn parse_key(text: &str) -> Result<[u8; KEY_LEN], KeyError> {
    let (kind, value) = text
        .split_once(':')
        .ok_or_else(|| KeyError::Lost("unknown key format".into()))?;
    match kind {
        "raw" => key_from(&hex::decode(value).map_err(|e| KeyError::Lost(e.to_string()))?),
        #[cfg(windows)]
        "dpapi" => {
            let blob = hex::decode(value).map_err(|e| KeyError::Lost(e.to_string()))?;
            // Fails for another user or computer: that key will not come back.
            key_from(&dpapi::unprotect(&blob).map_err(|e| KeyError::Lost(e.to_string()))?)
        }
        #[cfg(target_os = "macos")]
        "keychain" => match security_framework::passwords::get_generic_password(
            KEYCHAIN_SERVICE,
            value,
        ) {
            Ok(bytes) => key_from(&bytes),
            // errSecItemNotFound
            Err(e) if e.code() == -25300 => {
                Err(KeyError::Lost("not in the Keychain".into()))
            }
            // Access refused, Keychain locked…
            Err(e) => Err(KeyError::Unavailable(format!("trousseau : {e}"))),
        },
        // Sealed by an AES key of the Android Keystore (never leaves it).
        #[cfg(target_os = "android")]
        "keystore" => match crate::android::keystore_unwrap(value) {
            Ok(bytes) => key_from(&bytes),
            Err(crate::android::KeystoreError::Lost(e)) => {
                Err(KeyError::Lost(format!("Keystore : {e}")))
            }
            Err(crate::android::KeystoreError::Unavailable(e)) => {
                Err(KeyError::Unavailable(format!("Keystore : {e}")))
            }
        },
        _ => Err(KeyError::Lost(format!("key kept by \"{kind}\", not here"))),
    }
}

/// Write `raw` as the key of `dir`, protected the system's way when it can
/// be (also used to give a restored data folder its key).
pub(crate) fn write_key(dir: &Path, raw: &[u8; KEY_LEN]) -> io::Result<()> {
    let text = protect_key(raw).unwrap_or_else(|e| {
        eprintln!("history key not protected by the system ({e}): kept in a private file");
        format!("raw:{}", hex::encode(raw))
    });
    store::write_atomic(&dir.join(KEY_FILE), text.as_bytes())
}

#[cfg(windows)]
fn protect_key(raw: &[u8; KEY_LEN]) -> io::Result<String> {
    Ok(format!("dpapi:{}", hex::encode(dpapi::protect(raw)?)))
}

#[cfg(target_os = "macos")]
fn protect_key(raw: &[u8; KEY_LEN]) -> io::Result<String> {
    // One Keychain item per data folder: the folder kept aside by a
    // restore still opens with its own.
    let mut account = [0u8; 16];
    SystemRandom::new()
        .fill(&mut account)
        .map_err(|_| io::Error::other("system random unavailable"))?;
    let account = hex::encode(account);
    security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, &account, raw)
        .map_err(io::Error::other)?;
    Ok(format!("keychain:{account}"))
}

#[cfg(target_os = "android")]
fn protect_key(raw: &[u8; KEY_LEN]) -> io::Result<String> {
    let blob = crate::android::keystore_wrap(raw).map_err(io::Error::other)?;
    // Written only once it is known to open again.
    match crate::android::keystore_unwrap(&blob) {
        Ok(back) if back == raw => Ok(format!("keystore:{blob}")),
        _ => Err(io::Error::other("Keystore: sealed key does not open")),
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "android")))]
fn protect_key(_raw: &[u8; KEY_LEN]) -> io::Result<String> {
    Err(io::Error::new(ErrorKind::Unsupported, "no system key store used"))
}

#[cfg(windows)]
mod dpapi {
    use std::io;
    use std::ptr;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn run(data: &[u8], protect: bool) -> io::Result<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(data.len()).map_err(io::Error::other)?,
            pbData: data.as_ptr().cast_mut(),
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: ptr::null_mut(),
        };
        // SAFETY: valid input blob; the output is allocated by the system
        // and freed below with LocalFree.
        let ok = unsafe {
            if protect {
                CryptProtectData(
                    &input,
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            }
        };
        if ok == 0 || output.pbData.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the system wrote `cbData` bytes at `pbData`.
        let bytes =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
        unsafe { LocalFree(output.pbData.cast()) };
        Ok(bytes)
    }

    pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
        run(data, true)
    }

    pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
        run(data, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_data_opens_and_tampering_is_caught() {
        let sealed = seal(b"{\"a\":1}").unwrap();
        assert!(is_sealed(&sealed));
        assert_eq!(open(&sealed).unwrap(), b"{\"a\":1}");
        let mut bad = sealed.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(open(&bad).unwrap_err().kind(), ErrorKind::InvalidData);
        assert_eq!(open(b"[]").unwrap_err().kind(), ErrorKind::InvalidData);

        let line = seal_line(b"{\"t\":\"x\"}").unwrap();
        assert!(is_sealed_line(&line) && !line.contains('\n'));
        assert_eq!(open_line(&format!("{line}\n")).unwrap(), b"{\"t\":\"x\"}");
        assert_eq!(open_line("{\"plain\":1}\r\n").unwrap(), b"{\"plain\":1}");
    }

    // Not on macOS: it would leave an item in the Keychain.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn key_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("p2pshare-vault-{}", rand::random::<u32>()));
        fs::create_dir_all(&dir).unwrap();
        let raw = random_key();
        write_key(&dir, &raw).unwrap();
        assert_eq!(load_or_create(&dir).ok(), Some(raw));
        // Damaged: set aside, a new key is made.
        fs::write(dir.join(KEY_FILE), "nonsense").unwrap();
        let fresh = load_or_create(&dir).unwrap();
        assert_ne!(fresh, raw);
        assert!(fs::read_dir(&dir).unwrap().filter_map(Result::ok).any(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with("vault.key.lost-")));
        let _ = fs::remove_dir_all(&dir);
    }
}
