//! Pending login challenges.
//!
//! A login is a multi-step conversation: `start` hands back a challenge (a QR
//! payload, a set of credential fields, a URL to open), and the host polls or
//! submits until the credential arrives. The state in between has to outlive a
//! single process — the CLI is one-shot (`vlogin login start` then
//! `vlogin login poll`), and the web route handles each HTTP request in a
//! separate turn — so sessions are persisted as 0600 files under
//! `<root>/sessions/<id>.json`.
//!
//! The *format* is this module's business; touching the disk is not. Reading
//! and writing go through a [`Storage`], the same seam the account pool uses.
//!
//! Secrets boundary: `state` can carry a half-finished credential (an OAuth
//! `code_verifier`, a refresh token). It must stay in the file; never echo it
//! into a challenge, a log, or a tool result.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ambient::Ambient;
use crate::error::{Error, Result};
use crate::store::Storage;

/// How long a pending challenge stays valid.
pub const DEFAULT_TTL_MS: i64 = 5 * 60 * 1000;

/// One in-flight login attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub channel: String,
    pub method: String,
    pub created_at: i64,
    pub ttl_ms: i64,
    /// Provider-owned scratch space. May contain secrets.
    pub state: serde_json::Value,
}

impl Session {
    /// Create a session for `channel`/`method` with the given initial state.
    ///
    /// The id and timestamp come from `ambient`, so this crate never reads a
    /// clock or an entropy source.
    #[must_use]
    pub fn create(channel: &str, method: &str, state: serde_json::Value, ambient: Ambient) -> Self {
        Self {
            id: ambient.id(),
            channel: channel.to_string(),
            method: method.to_string(),
            created_at: ambient.now_ms(),
            ttl_ms: DEFAULT_TTL_MS,
            state,
        }
    }

    /// True when the session has outlived its TTL.
    #[must_use]
    pub fn expired(&self, now_ms: i64) -> bool {
        now_ms - self.created_at > self.ttl_ms
    }

    /// Whether `id` is a syntactically valid session id.
    ///
    /// Only the 16-hex ids [`Ambient::id`] mints are accepted, so a hostile id
    /// can never be used to walk the filesystem by way of `..` or a separator.
    #[must_use]
    pub fn valid_id(id: &str) -> bool {
        id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
    }

    /// Path of this session's file inside `dir`.
    #[must_use]
    pub fn path_in(&self, dir: &Path) -> PathBuf {
        path_of(dir, &self.id)
    }

    /// Write the session to `dir` (creating it 0700, file 0600).
    pub fn save(&self, dir: &Path, storage: &(impl Storage + ?Sized)) -> Result<()> {
        storage.create_dir(dir, 0o700)?;
        let text = serde_json::to_string(self)?;
        storage.write(&self.path_in(dir), &text, 0o600)
    }

    /// Load a session by id. `None` when missing, unreadable, or expired.
    pub fn load(
        dir: &Path,
        id: &str,
        now_ms: i64,
        storage: &(impl Storage + ?Sized),
    ) -> Result<Option<Self>> {
        if !Self::valid_id(id) {
            return Err(Error::channel("malformed session id"));
        }
        let path = path_of(dir, id);
        let Some(text) = storage.read(&path) else {
            return Ok(None);
        };
        let Ok(session) = serde_json::from_str::<Self>(&text) else {
            return Ok(None);
        };
        if session.expired(now_ms) {
            storage.remove_file(&path);
            return Ok(None);
        }
        Ok(Some(session))
    }

    /// Drop a session file; a missing file is not an error.
    pub fn remove(dir: &Path, id: &str, storage: &(impl Storage + ?Sized)) -> Result<()> {
        storage.remove_file(&path_of(dir, id));
        Ok(())
    }
}

/// Path of one session file inside `dir`.
fn path_of(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// Every `sessions/*.json` entry with the `createdAt` it records.
///
/// A file that is not readable, is not JSON, or carries no `createdAt` comes
/// back as `None` for the timestamp, which the sweeper treats as expired: an
/// unparseable session file is garbage, not a live login.
pub(crate) fn list(
    dir: &Path,
    storage: &(impl Storage + ?Sized),
) -> Result<Vec<(PathBuf, Option<i64>)>> {
    let mut out = Vec::new();
    for (path, text) in storage.list(dir, "json") {
        let created = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| value.get("createdAt")?.as_i64());
        out.push((path, created));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ambient() -> Ambient {
        Ambient::new(1_730_000_000_000, 42)
    }

    #[test]
    fn only_sixteen_hex_ids_are_accepted() {
        assert!(Session::valid_id("0123456789abcdef"));
        assert!(!Session::valid_id("../etc/passwd"));
        assert!(!Session::valid_id("0123456789abcde")); // 15
        assert!(!Session::valid_id("0123456789abcdefg"));
        assert!(!Session::valid_id("0123456789abcdeZ"));
    }

    #[test]
    fn expiry_is_relative_to_the_supplied_clock() {
        let session = Session::create("bilibili", "qr", serde_json::json!({}), ambient());
        assert!(!session.expired(1_730_000_000_000));
        assert!(!session.expired(1_730_000_000_000 + DEFAULT_TTL_MS));
        assert!(session.expired(1_730_000_000_000 + DEFAULT_TTL_MS + 1));
    }

    #[test]
    fn the_session_file_lives_under_sessions_by_id() {
        let session = Session::create("bilibili", "qr", serde_json::json!({}), ambient());
        let path = session.path_in(Path::new("/state/sessions"));
        assert_eq!(path, PathBuf::from(format!("/state/sessions/{}.json", session.id)));
        assert_eq!(session.id.len(), 16);
    }
}
