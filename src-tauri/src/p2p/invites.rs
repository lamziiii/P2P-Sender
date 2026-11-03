//! Invitation links: instead of both friends typing each other's ID, one
//! sends a link (see `crate::invite` for its format) holding their ID and a
//! code made for it; the other accepts it in a popup.
//!
//!   maker:    `create_invite` keeps the code (`invites.json`), valid
//!             `INVITE_DAYS` days, for one person.
//!   receiver: `accept_invite` adds the maker as a friend with the code,
//!             sent in the hello of the next connection to them, first thing
//!             on one they open, or over the one already open.
//!   maker:    a peer who is not a friend may connect while a code is still
//!             open; with a valid code in its hello it becomes a friend (the
//!             code is used up), otherwise it is turned away ("invite").
//!   receiver: the maker's first message means it let us in; turned away,
//!             the friend shows that the link no longer works.
//!
//! Older versions ignore what they do not know: they neither make nor
//! accept links (the ID, in the advanced settings, is for them).

use iroh::endpoint::{ConnectError, ConnectingError, ConnectionError};

use super::*;

/// How long a link works.
const INVITE_DAYS: u64 = 7;
const INVITE_LIFETIME_MS: u64 = INVITE_DAYS * 24 * 3600 * 1000;
/// Links still open at once, at most: the oldest goes past that.
const MAX_INVITES: usize = 20;
/// Peers who are not friends trying a code, per `RATE_WINDOW`.
const INVITE_TRIES: usize = 20;
const CODE_BYTES: usize = 16;

/// A link we made, until someone uses it.
#[derive(Clone, Serialize, Deserialize)]
pub struct Invite {
    code: String,
    /// Unix ms.
    expires: u64,
}

/// What a link's code looks like: `CODE_BYTES` in lowercase hex.
pub fn is_code(code: &str) -> bool {
    code.len() == CODE_BYTES * 2 && code.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// In constant time: how long a guess took says nothing of the code.
fn same_code(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The nickname a link or a hello gives, as friends' names are kept.
pub fn clean_nickname(name: &str) -> String {
    strip_invisible(name)
        .trim()
        .chars()
        .take(MAX_FRIEND_NAME)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The maker turned us away: no link of theirs is open any more, or the
/// code did not work. Other refusals (busy, a peer not reading…) are not
/// about the link.
fn turned_away(err: &ConnectionError) -> bool {
    matches!(err, ConnectionError::ApplicationClosed(c)
        if c.error_code == VarInt::from_u32(CLOSE_REJECTED)
            && matches!(c.reason.as_ref(), b"not a friend" | b"invite"))
}

impl State {
    /// Whether `peer`, dialing us, goes on: `Ok(false)` for a friend or a
    /// co-member, `Ok(true)` for someone else while one of our links is
    /// open (it must bring its code), else why not.
    pub(super) fn admit(&mut self, peer: &str) -> Result<bool, &'static [u8]> {
        if self.is_allowed(peer) {
            return Ok(false);
        }
        let now = now_ms();
        if !self.invites.data.iter().any(|i| i.expires > now) {
            return Err(b"not a friend");
        }
        if self.over_limit("invite", INVITE_TRIES) {
            return Err(b"busy");
        }
        Ok(true)
    }

    /// What we open a connection to `peer` with: the code of their link
    /// while they have not let us in yet.
    pub(super) fn hello(&self, peer: &str) -> Value {
        let code = self
            .friends
            .data
            .iter()
            .find(|f| f.id == peer)
            .and_then(|f| f.invite.as_ref());
        match code {
            Some(code) => json!({ "type": "hello", "invite": code, "name": self.nickname }),
            None => json!({ "type": "hello" }),
        }
    }

    /// `peer` brings the code of one of our links (`msg`, its hello or an
    /// `invite` message): the code is used up, and `peer` becomes a friend.
    /// True if it just did.
    pub(super) fn redeem_invite(&mut self, peer: &str, msg: &Value) -> bool {
        let Some(code) = msg.get("invite").and_then(Value::as_str) else {
            return false;
        };
        let now = now_ms();
        self.invites.data.retain(|i| i.expires > now);
        let Some(at) = self.invites.data.iter().position(|i| same_code(&i.code, code)) else {
            return false;
        };
        self.invites.data.remove(at);
        // On failure, the code stays used for this run only.
        let _ = self.invites.save_now();
        // A friend already (added by ID meanwhile): the link is used all the
        // same, nobody else may take it.
        if self.is_friend(peer) {
            return false;
        }
        let nickname = clean_nickname(msg.get("name").and_then(Value::as_str).unwrap_or_default());
        self.friends.data.push(Friend {
            id: peer.to_string(),
            name: String::new(),
            nickname: nickname.clone(),
            auto_accept: false,
            unsent_reactions: HashMap::new(),
            invite: None,
            invite_refused: false,
        });
        // Stays dirty on failure: the timer retries.
        let _ = self.friends.save_now();
        self.events.push(Event::InviteAccepted {
            friend_id: peer.to_string(),
            name: nickname,
        });
        self.events.push(Event::FriendsChanged);
        true
    }

    /// A connection to `peer` ended with `err`: if it was to bring the code
    /// of their link and they turned us away, that link no longer works.
    pub(super) fn note_refusal(&mut self, peer: &str, err: &ConnectionError) {
        if !turned_away(err) {
            return;
        }
        let Some(f) = self.friends.data.iter_mut().find(|f| f.id == peer && f.invite.is_some()) else {
            return;
        };
        f.invite = None;
        f.invite_refused = true;
        self.friends.mark_dirty();
        self.events.push(Event::FriendsChanged);
    }

    /// Dialing `peer` failed with `err`: turned away so soon, the
    /// connection did not even open (see `note_refusal`).
    pub(super) fn note_dial_error(&mut self, peer: &str, err: &ConnectError) {
        let closed = match err {
            ConnectError::Connection { source, .. } => source,
            ConnectError::Connecting {
                source: ConnectingError::ConnectionError { source, .. },
                ..
            } => source,
            _ => return,
        };
        self.note_refusal(peer, closed);
    }

    /// `peer` opened a connection to us: our hello did not carry the code of
    /// their link, so it goes now (as a co-member of a group, they may dial
    /// us first). Before anything else on it, our messages included.
    pub(super) fn send_pending_invite(&mut self, peer: &str) {
        let Some(code) = self
            .friends
            .data
            .iter()
            .find(|f| f.id == peer)
            .and_then(|f| f.invite.clone())
        else {
            return;
        };
        let name = self.nickname.clone();
        self.send(peer, json!({ "type": "invite", "invite": code, "name": name }));
    }

    /// `peer` sent us something: whatever link we came with, they let us in.
    pub(super) fn invite_answered(&mut self, peer: &str) {
        let Some(f) = self
            .friends
            .data
            .iter_mut()
            .find(|f| f.id == peer && (f.invite.is_some() || f.invite_refused))
        else {
            return;
        };
        f.invite = None;
        f.invite_refused = false;
        self.friends.mark_dirty();
        self.events.push(Event::FriendsChanged);
    }
}

impl Node {
    /// A new code for a link, open for `INVITE_DAYS` days and one person.
    pub fn create_invite(&self) -> String {
        let code = hex::encode(rand::random::<[u8; CODE_BYTES]>());
        self.with(|s| {
            let now = now_ms();
            let list = &mut s.invites.data;
            list.retain(|i| i.expires > now);
            if list.len() >= MAX_INVITES {
                list.remove(0);
            }
            list.push(Invite {
                code: code.clone(),
                expires: now + INVITE_LIFETIME_MS,
            });
            // On failure, the link still works until the app closes.
            let _ = s.invites.save_now();
        });
        code
    }

    /// The link of `id` (named `nickname` there) with `code` was accepted:
    /// they become a friend, and the code goes to them so that they add us.
    pub fn accept_invite(
        &self,
        id: &str,
        nickname: &str,
        code: &str,
    ) -> Result<Vec<FriendView>, &'static str> {
        let id = id.trim().to_lowercase();
        if !is_valid_id(&id) || !is_code(code) {
            return Err("invalid_link");
        }
        if id == self.id() {
            return Err("self");
        }
        let nickname = clean_nickname(nickname);
        Ok(self.with(|s| {
            let online = s.conns.contains_key(&id);
            if online {
                // Already connected (a co-member of a group): over that connection.
                s.send(&id, json!({ "type": "invite", "invite": code, "name": s.nickname }));
            }
            let invite = (!online).then(|| code.to_string());
            match s.friends.data.iter_mut().find(|f| f.id == id) {
                Some(f) => {
                    if invite.is_some() {
                        f.invite = invite;
                        f.invite_refused = false;
                    }
                    if f.nickname.is_empty() {
                        f.nickname = nickname;
                    }
                }
                None => s.friends.data.push(Friend {
                    id: id.clone(),
                    name: String::new(),
                    nickname,
                    auto_accept: false,
                    unsent_reactions: HashMap::new(),
                    invite,
                    invite_refused: false,
                }),
            }
            // Stays dirty on failure: the timer retries.
            let _ = s.friends.save_now();
            s.fast_retries.remove(&id);
            self.dial(s, &id);
            friend_views(s)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        let code = hex::encode([0xab; CODE_BYTES]);
        assert!(is_code(&code));
        assert!(!is_code(&code.to_uppercase()));
        assert!(!is_code(&code[1..]));
        assert!(same_code(&code, &code.clone()));
        assert!(!same_code(&code, &hex::encode([8u8; CODE_BYTES])));
        assert!(!same_code(&code, ""));
        assert_eq!(clean_nickname("  Al\u{202e}ice  "), "Alice");
        assert_eq!(clean_nickname(&"x".repeat(100)).len(), MAX_FRIEND_NAME);
    }
}
