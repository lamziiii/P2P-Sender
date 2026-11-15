//! The language of what the backend writes for the user (notifications,
//! tray menu, errors shown in the window): the window's, which it tells at
//! each start (`set_language`). French until then, and by default.

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

/// "en" for English; anything else is French.
pub fn set(lang: &str) {
    ENGLISH.store(lang == "en", Ordering::Relaxed);
}

pub fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// The same text in both languages.
pub fn tr(fr: &'static str, en: &'static str) -> &'static str {
    if english() {
        en
    } else {
        fr
    }
}

/// The same, formatted: `trf!("{n} fichiers", "{n} files")`.
#[macro_export]
macro_rules! trf {
    ($fr:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::lang::english() {
            format!($en $(, $arg)*)
        } else {
            format!($fr $(, $arg)*)
        }
    };
}
