//! `extractor-core` — the harness-independent content-extractorion kernel.
//!
//! The crate answers two questions: **what content could be extracted from this
//! input, and by which extractor** (the caller chooses), and **given a platform
//! and a login method, produce a cookie jar that authenticates extraction**. It
//! knows nothing about any agent framework, and it names no platform: each
//! site lives in its own `platform-*` crate and is handed to [`Registry::new`]
//! by the composition root.
//!
//! ## The layering, and why
//!
//! This crate is a **protocol layer**. It owns wire formats — platform
//! descriptors, HTTP request and response shapes, the Netscape cookie codec, the
//! DevTools conversation, the account-pool layout — and it owns no machine-local
//! decision. Reading a file, resolving a state root from the environment,
//! spawning a browser, binding a port, reading a clock or drawing entropy all
//! happen somewhere else, and the caller passes them in:
//!
//! | crate | role | may touch |
//! |---|---|---|
//! | `extractor-core` | protocol + transport | network only |
//! | `platform-*` | one site's login + content descriptors | network only |
//! | `extractor-abi` | effects behind the C ABI | fs, env, processes, clock |
//! | `extractor-cli` | argv, prompts, exit codes | everything |
//!
//! [`store::Storage`] is the seam for bytes on a disk, and [`ambient::Ambient`]
//! is the seam for time and entropy. `tests/purity.rs` enforces the boundary so
//! it cannot erode silently.
//!
//! ## Selection is explicit
//!
//! [`Registry::candidates`] returns **every** extractor whose patterns suggest
//! it could handle an input, ranked by specificity. [`Registry::extractor`]
//! resolves an id the caller chose. There is no first-match-wins dispatch: one
//! host serves many content kinds, and guessing would send an article down a
//! video path.
//!
//! ## The account pool
//!
//! A platform holds many accounts, any of which may have been captured by any
//! method. One is pinned as active; the rest are the rotation fallback when an
//! extraction is rejected. Cookies never leave the store except through
//! `export` / `materialize`, which feed an extractor directly.
//!
//! ## Secrets boundary
//!
//! Cookie text and `Set-Cookie` lines are credentials. Throughout this crate:
//!
//! * errors carry status codes and truncated bodies, never cookie values;
//! * the safe projection for display is [`AccountView`], which carries a cookie
//!   *count* and no values;
//! * account and session files are described as 0600 inside 0700 directories,
//!   and enforcing that is the storage implementation's job.

pub mod ambient;
pub mod browser;
pub mod content;
pub mod cookies;
pub mod error;
pub mod http;
pub mod model;
pub mod provider;
pub mod registry;
pub mod session;
pub mod store;
pub mod util;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::ambient::Ambient;
pub use crate::content::{ContentKind, ExtractorInfo};
pub use crate::error::{Error, Result};
pub use crate::model::{
    AccountStatus, AccountView, Challenge, ChannelInfo, MethodInfo, MethodKind, Poll,
};
pub use crate::provider::{CheckOutcome, Provider};
pub use crate::registry::{Candidate, Registry};
pub use crate::store::{Pool, Storage, Store};

/// A login request from a host.
#[derive(Debug, Clone, Deserialize)]
pub struct StartRequest {
    /// Platform id (the field was `channel` before the rename; both accepted).
    #[serde(alias = "channel")]
    pub platform: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub label: Option<String>,
}

/// What the host renders while a login is in flight.
#[derive(Debug, Clone, Serialize)]
pub struct StartResponse {
    pub session_id: String,
    pub challenge: Challenge,
}

/// The state of a login after a `poll` or `submit`.
#[derive(Debug, Clone, Serialize)]
pub struct PollResponse {
    pub result: Poll,
    /// Present once the credential is stored.
    pub account: Option<AccountView>,
}

/// One platform's pool plus a pointer to the pinned account.
#[derive(Debug, Clone, Serialize)]
pub struct PlatformStatus {
    pub platform: ChannelInfo,
    pub accounts: Vec<AccountView>,
    pub active: Option<String>,
    /// Content extractors this platform advertises.
    pub extractors: Vec<ExtractorInfo>,
}

/// Build a registry from the platforms a composition root linked.
///
/// Kept as a free function so `extractor-abi` has one obvious entry point and this
/// crate still names no platform itself.
#[must_use]
pub fn registry_of(providers: Vec<Arc<dyn Provider>>) -> Registry {
    Registry::new(providers)
}
