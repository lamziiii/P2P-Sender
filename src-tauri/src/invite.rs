//! What an invitation link looks like (see `p2p/invites.rs` for what it
//! does). The link sent is a web page, which mail and chat apps make
//! clickable; the page opens the app with the same part after `#`:
//!
//!   https://lamziiii.github.io/P2P-Sender/ami/#i=<ID>&c=<code>&n=<name>
//!   p2pshare://ami?i=<ID>&c=<code>&n=<name>
//!
//! The part after `#` never reaches the web server. Either form can also be
//! pasted in the app, when the link does not open it.

use crate::p2p;

/// The page links go to (`docs/ami/` of the repository, on GitHub Pages).
const PAGE: &str = "https://lamziiii.github.io/P2P-Sender/ami/";
/// The scheme the app is opened with (`plugins.deep-link` in tauri.conf.json).
const SCHEME: &str = "p2pshare";

#[derive(Debug, PartialEq)]
pub struct Link {
    /// P2P ID of whoever made it.
    pub id: String,
    pub code: String,
    /// Their nickname when they made it (shown, never trusted as proof).
    pub name: String,
}

/// The link to send for our `id`, a `code` from `Node::create_invite` and
/// our nickname.
pub fn make(id: &str, code: &str, name: &str) -> String {
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("i", id)
        .append_pair("c", code)
        .append_pair("n", name)
        .finish();
    format!("{PAGE}#{query}")
}

/// A link of either form, or what follows its `#` / `?`.
pub fn parse(text: &str) -> Option<Link> {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    let query = if lower.starts_with(&format!("{SCHEME}:")) {
        text.split_once('?').or_else(|| text.split_once('#'))?.1
    } else if lower.starts_with("https://") || lower.starts_with("http://") {
        text.split_once('#')?.1
    } else {
        text
    };
    let (mut id, mut code, mut name) = (None, None, String::new());
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "i" => id = Some(value.trim().to_ascii_lowercase()),
            "c" => code = Some(value.trim().to_ascii_lowercase()),
            "n" => name = value.into_owned(),
            _ => {}
        }
    }
    let (id, code) = (id?, code?);
    if !p2p::is_valid_id(&id) || !p2p::is_invite_code(&code) {
        return None;
    }
    Some(Link {
        id,
        code,
        name: p2p::clean_nickname(&name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> String {
        // A real key: not every 64 hex characters are one.
        iroh::SecretKey::from_bytes(&[3u8; 32]).public().to_string()
    }

    #[test]
    fn made_links_read_back() {
        let code = "0123456789abcdef0123456789abcdef";
        let link = make(&id(), code, "Zoé & Léa");
        assert!(link.starts_with(PAGE));
        let read = parse(&link).unwrap();
        assert_eq!(read, Link { id: id(), code: code.into(), name: "Zoé & Léa".into() });
        // As the page opens the app, and pasted without the address.
        let query = link.split_once('#').unwrap().1;
        assert_eq!(parse(&format!("p2pshare://ami?{query}")).unwrap(), read);
        assert_eq!(parse(&format!("  P2PSHARE://ami/?{query}\n")).unwrap(), read);
        assert_eq!(parse(query).unwrap(), read);
    }

    #[test]
    fn bad_links_are_refused() {
        let code = "0123456789abcdef0123456789abcdef";
        let ok = format!("i={}&c={code}&n=x", id());
        assert!(parse(&ok).is_some());
        assert!(parse(&format!("i={}&c={code}", "ab".repeat(31))).is_none());
        assert!(parse(&format!("i={}&c=1234", id())).is_none());
        assert!(parse(&format!("c={code}")).is_none());
        assert!(parse("https://example.com/").is_none());
        assert!(parse("").is_none());
        // Invisible characters and long names.
        let long = "a".repeat(100);
        let read = parse(&format!("i={}&c={code}&n=%E2%80%AEevil{long}", id())).unwrap();
        // Cut as friends' names are, at 40.
        assert!(read.name.starts_with("evil") && read.name.chars().count() == 40);
    }
}
