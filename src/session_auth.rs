//! Server-side sessions for Open World.
//!
//! The previous cookie check treated any `openworld_auth=1` cookie as
//! authenticated. Tokens are now 32 random bytes, url-safe base64
//! (the same shape as Python `secrets.token_urlsafe(32)`), stored in
//! this process, and compared in constant time. One axum worker
//! process serves the world, so an in-memory map is enough.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore;

/// Public mount of the world UI on selenaastra.com. Caddy strips this
/// prefix before proxying, but the browser only attaches the cookie
/// when the request path is under it.
pub(crate) const COOKIE_PATH: &str = "/open-world/";

/// Real sessions last at least 12 hours. `settings.json` still carries
/// the old 3600s client-cookie duration from the forgeable-cookie era;
/// values below this floor are raised so a stolen cookie does not
/// outlive the window this change asked for, and a longer operator
/// setting is kept.
pub(crate) const SESSION_TTL_FLOOR_SECS: u64 = 12 * 60 * 60;

/// Refresh the in-process scheduler token when less than this remains,
/// so a cycle cannot start with a token that expires mid-request.
const INTERNAL_REFRESH_SLACK_SECS: u64 = 120;

pub(crate) fn session_ttl_secs(configured: u64) -> u64 {
    if configured < SESSION_TTL_FLOOR_SECS {
        SESSION_TTL_FLOOR_SECS
    } else {
        configured
    }
}

struct Inner {
    /// token -> expiry as unix seconds.
    tokens: HashMap<String, u64>,
    /// Token the in-process scheduler presents. Not a user session.
    internal: Option<String>,
}

pub(crate) struct SessionStore {
    inner: Mutex<Inner>,
}

impl SessionStore {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                tokens: HashMap::new(),
                internal: None,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Mint a user session. The returned token is the cookie value.
    pub(crate) fn issue(&self, ttl_secs: u64) -> String {
        let token = token_urlsafe(32);
        let now = now_unix();
        let exp = now.saturating_add(ttl_secs.max(1));
        let mut inner = self.lock();
        purge(&mut inner, now);
        inner.tokens.insert(token.clone(), exp);
        token
    }

    /// Scheduler credential. Reuses the current token until it is close
    /// to expiry, then rotates. Logout of a user session does not
    /// revoke this unless that exact token is presented.
    pub(crate) fn internal_token(&self, ttl_secs: u64) -> String {
        let now = now_unix();
        let mut inner = self.lock();
        purge(&mut inner, now);
        let still_good = match &inner.internal {
            Some(tok) => match inner.tokens.get(tok) {
                Some(exp) => exp.saturating_sub(now) >= INTERNAL_REFRESH_SLACK_SECS,
                None => false,
            },
            None => false,
        };
        if still_good {
            return inner.internal.clone().expect("checked above");
        }
        if let Some(old) = inner.internal.take() {
            inner.tokens.remove(&old);
        }
        let token = token_urlsafe(32);
        let ttl = ttl_secs.max(INTERNAL_REFRESH_SLACK_SECS);
        inner.tokens.insert(token.clone(), now.saturating_add(ttl));
        inner.internal = Some(token.clone());
        token
    }

    pub(crate) fn service_cookie(&self, cookie_name: &str, ttl_secs: u64) -> String {
        format!("{}={}", cookie_name, self.internal_token(ttl_secs))
    }

    pub(crate) fn validate(&self, presented: &str) -> bool {
        // The legacy forgeable value is never a session, even if it
        // were somehow inserted into the map.
        if presented.is_empty() || presented == "1" {
            return false;
        }
        let now = now_unix();
        let mut inner = self.lock();
        purge(&mut inner, now);
        let keys: Vec<String> = inner.tokens.keys().cloned().collect();
        let mut ok = false;
        for key in keys {
            if constant_time_eq(key.as_bytes(), presented.as_bytes()) {
                if let Some(exp) = inner.tokens.get(&key) {
                    if *exp > now {
                        ok = true;
                    }
                }
            }
        }
        ok
    }

    pub(crate) fn revoke(&self, presented: &str) -> bool {
        if presented.is_empty() || presented == "1" {
            return false;
        }
        let mut inner = self.lock();
        let keys: Vec<String> = inner.tokens.keys().cloned().collect();
        let mut removed = false;
        for key in keys {
            if constant_time_eq(key.as_bytes(), presented.as_bytes()) {
                inner.tokens.remove(&key);
                if inner.internal.as_deref() == Some(key.as_str()) {
                    inner.internal = None;
                }
                removed = true;
            }
        }
        removed
    }

    /// Test-only: force a token to look expired and drop it.
    #[cfg(test)]
    pub(crate) fn expire_token(&self, presented: &str) {
        let mut inner = self.lock();
        let keys: Vec<String> = inner.tokens.keys().cloned().collect();
        for key in keys {
            if constant_time_eq(key.as_bytes(), presented.as_bytes()) {
                inner.tokens.insert(key, 0);
            }
        }
        purge(&mut inner, now_unix());
    }
}

fn purge(inner: &mut Inner, now: u64) {
    inner.tokens.retain(|_, exp| *exp > now);
    if let Some(tok) = &inner.internal {
        if !inner.tokens.contains_key(tok) {
            inner.internal = None;
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `secrets.token_urlsafe(nbytes)`: `nbytes` of OS entropy, standard
/// base64 with a URL alphabet and no padding.
fn token_urlsafe(nbytes: usize) -> String {
    let mut buf = vec![0u8; nbytes];
    let mut rng = rand::rngs::OsRng;
    rng.fill_bytes(&mut buf);
    base64_url_nopad(&buf)
}

fn base64_url_nopad(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 3 <= data.len() {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | (data[i + 2] as u32);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
        out.push(TABLE[(n & 63) as usize] as char);
        i += 3;
    }
    if i < data.len() {
        let rem = data.len() - i;
        let mut n = (data[i] as u32) << 16;
        if rem == 2 {
            n |= (data[i + 1] as u32) << 8;
        }
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if rem == 2 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        }
    }
    out
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    // black_box keeps this from collapsing into an early-out compare.
    std::hint::black_box(diff) == 0
}

/// Cookie values for `name` in a request `Cookie` header.
/// Empty values are dropped. The legacy value `1` is kept so callers
/// can see it was presented; [`SessionStore::validate`] rejects it.
pub(crate) fn cookie_values<'a>(header: Option<&'a str>, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let Some(cookies) = header else {
        return out;
    };
    for cookie in cookies.split(';') {
        let cookie = cookie.trim();
        if let Some((n, v)) = cookie.split_once('=') {
            if n.trim() == name {
                let v = v.trim().trim_matches('"');
                if !v.is_empty() {
                    out.push(v);
                }
            }
        }
    }
    out
}

pub(crate) fn header_grants(
    cookies_header: Option<&str>,
    cookie_name: &str,
    sessions: &SessionStore,
) -> bool {
    cookie_values(cookies_header, cookie_name)
        .into_iter()
        .any(|v| sessions.validate(v))
}

pub(crate) fn set_cookie_value(name: &str, token: &str, max_age: u64) -> String {
    format!("{name}={token}; HttpOnly; Secure; SameSite=Lax; Path={COOKIE_PATH}; Max-Age={max_age}")
}

pub(crate) fn clear_cookie_value(name: &str, path: &str) -> String {
    format!("{name}=; HttpOnly; Secure; SameSite=Lax; Path={path}; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_url_matches_known_vector() {
        // Python: base64.urlsafe_b64encode(bytes([0, 1, 2])).rstrip(b"=").decode()
        assert_eq!(base64_url_nopad(&[0, 1, 2]), "AAEC");
    }

    #[test]
    fn issued_token_is_urlsafe_32_and_not_the_legacy_flag() {
        let store = SessionStore::new();
        let token = store.issue(SESSION_TTL_FLOOR_SECS);
        assert_ne!(token, "1");
        assert_eq!(token.len(), 43);
        assert!(token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert!(store.validate(&token));
    }

    #[test]
    fn legacy_flag_and_empty_and_unknown_are_rejected() {
        let store = SessionStore::new();
        let _ = store.issue(SESSION_TTL_FLOOR_SECS);
        assert!(!store.validate("1"));
        assert!(!store.validate(""));
        assert!(!store.validate("not-a-session"));
        assert!(!header_grants(
            Some("openworld_auth=1"),
            "openworld_auth",
            &store
        ));
    }

    #[test]
    fn legacy_flag_beside_a_real_token_still_grants() {
        let store = SessionStore::new();
        let token = store.issue(SESSION_TTL_FLOOR_SECS);
        let header = format!("openworld_auth=1; openworld_auth={token}");
        assert!(header_grants(Some(&header), "openworld_auth", &store));
        let reversed = format!("other=1; openworld_auth={token}; openworld_auth=1");
        assert!(header_grants(Some(&reversed), "openworld_auth", &store));
    }

    #[test]
    fn expired_token_is_purged_and_rejected() {
        let store = SessionStore::new();
        let token = store.issue(SESSION_TTL_FLOOR_SECS);
        assert!(store.validate(&token));
        store.expire_token(&token);
        assert!(!store.validate(&token));
    }

    #[test]
    fn revoke_removes_only_that_token() {
        let store = SessionStore::new();
        let a = store.issue(SESSION_TTL_FLOOR_SECS);
        let b = store.issue(SESSION_TTL_FLOOR_SECS);
        assert!(store.revoke(&a));
        assert!(!store.validate(&a));
        assert!(store.validate(&b));
        assert!(!store.revoke("1"));
    }

    #[test]
    fn service_token_rotates_after_expiry_and_survives_user_revoke() {
        let store = SessionStore::new();
        let user = store.issue(SESSION_TTL_FLOOR_SECS);
        let first = store.service_cookie("openworld_auth", SESSION_TTL_FLOOR_SECS);
        let first_token = first.split_once('=').unwrap().1.to_string();
        assert!(store.validate(&first_token));
        assert!(store.revoke(&user));
        assert!(store.validate(&first_token));
        store.expire_token(&first_token);
        let second = store.service_cookie("openworld_auth", SESSION_TTL_FLOOR_SECS);
        let second_token = second.split_once('=').unwrap().1.to_string();
        assert_ne!(first_token, second_token);
        assert!(!store.validate(&first_token));
        assert!(store.validate(&second_token));
    }

    #[test]
    fn ttl_floor_is_twelve_hours() {
        assert_eq!(session_ttl_secs(0), SESSION_TTL_FLOOR_SECS);
        assert_eq!(session_ttl_secs(3600), SESSION_TTL_FLOOR_SECS);
        assert_eq!(
            session_ttl_secs(SESSION_TTL_FLOOR_SECS + 5),
            SESSION_TTL_FLOOR_SECS + 5
        );
    }

    #[test]
    fn set_cookie_is_httponly_secure_lax_and_scoped() {
        let c = set_cookie_value("openworld_auth", "abc", 43200);
        assert!(c.starts_with("openworld_auth=abc;"));
        assert!(c.contains("HttpOnly"));
        assert!(c.contains("Secure"));
        assert!(c.contains("SameSite=Lax"));
        assert!(c.contains("Path=/open-world/"));
        assert!(c.contains("Max-Age=43200"));
        assert!(!c.contains("=1;"));
        let clear = clear_cookie_value("openworld_auth", "/");
        assert!(clear.contains("Path=/"));
        assert!(clear.contains("Max-Age=0"));
    }

    #[test]
    fn constant_time_eq_matches_length_and_bytes() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
