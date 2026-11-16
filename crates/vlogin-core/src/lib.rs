//! `vlogin-core` — harness-independent, multi-channel video-site login.
//!
//! The crate answers one question: **given a video site and a login method,
//! produce a cookie jar that authenticates a download.** It knows nothing about
//! DeepSeek Harness, cordis, or any agent framework. The plugin
//! (`dsh-video-fetch`) is a thin shell over it.
//!
//! ## The layering, and why
//!
//! This crate is a **protocol layer**. It owns wire formats — channel
//! descriptors, HTTP request and response shapes, the Netscape cookie codec, the
//! DevTools conversation, the account-pool layout — and it owns no machine-local
//! decision. Reading a file, resolving a state root from the environment,
//! spawning a browser, binding a port, reading a clock or drawing entropy all
//! happen somewhere else, and the caller passes them in:
//!
//! | crate | role | may touch |
//! |---|---|---|
//! | `vlogin-core` | protocol + transport | network only |
//! | `vlogin-abi` | effects behind the frozen C ABI | fs, env, processes, clock |
//! | `vlogin-cli` | argv, prompts, exit codes | everything |
//!
//! [`store::Storage`] is the seam for bytes on a disk, and [`ambient::Ambient`]
//! is the seam for time and entropy. `tests/purity.rs` enforces the boundary so
//! it cannot erode silently.
//!
//! ## The two dimensions
//!
//! * **channel** — the site (`bilibili`, `douyin`, `youtube`, `tencent`,
//!   `iqiyi`, `youku`, plus `generic` for everything else).
//! * **method** — how the credential is obtained (`qr`, `manual`, `sms`,
//!   `password`, `browser`). Each channel advertises the full enum with
//!   `implemented: false` where it has no support, so a host can render a
//!   complete picker and get a structured refusal rather than a missing
//!   capability.
//!
//! ## The account pool
//!
//! A channel holds many accounts, any of which may have been captured by any
//! method. One is pinned as active; the rest are the rotation fallback when a
//! download is rejected. Cookies never leave the store except through
//! `export` / `materialize`, which feed a downloader directly.
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
pub mod channels;
pub mod cookies;
pub mod error;
pub mod http;
pub mod model;
pub mod session;
pub mod store;
pub mod util;

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::ambient::Ambient;
pub use crate::error::{Error, Result};
pub use crate::model::{
    AccountStatus, AccountView, Challenge, ChannelInfo, MethodInfo, MethodKind, Poll,
};
pub use crate::store::{Pool, Storage, Store};
use crate::channels::Provider;

/// Every registered channel, in picker order.
pub fn registry() -> Vec<Arc<dyn Provider>> {
    vec![
        channels::bilibili::Bilibili::new(),
        channels::douyin::Douyin::new(),
        channels::youtube::YouTube::new(),
        channels::tencent::Tencent::new(),
        channels::iqiyi::IQiyi::new(),
        channels::youku::Youku::new(),
        channels::generic::Generic::new(),
    ]
}

/// A login request from a host.
#[derive(Debug, Clone, Deserialize)]
pub struct StartRequest {
    pub channel: String,
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

/// One channel's pool plus a pointer to the pinned account.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelStatus {
    pub channel: ChannelInfo,
    pub accounts: Vec<AccountView>,
    pub active: Option<String>,
}

/// The channel registry: every advertised capability, and nothing else.
///
/// Pure. Holding no store, no clock and no storage means a caller can ask what a
/// channel supports before it has decided where its state lives.
pub struct Hub {
    providers: HashMap<String, Arc<dyn Provider>>,
    order: Vec<String>,
}

impl Hub {
    /// The registry every channel is advertised from.
    #[must_use]
    pub fn new() -> Self {
        let providers = registry();
        let order = providers.iter().map(|p| p.info().id.to_string()).collect();
        let map = providers
            .into_iter()
            .map(|p| (p.info().id.to_string(), p))
            .collect::<HashMap<_, _>>();
        Self { providers: map, order }
    }

    /// All channels, in picker order. `generic` is last so real sites win the
    /// URL routing too.
    #[must_use]
    pub fn channels(&self) -> Vec<ChannelInfo> {
        self.order.iter().filter_map(|id| self.providers.get(id).map(|p| p.info())).collect()
    }

    /// Advertised channels without `generic`, which is a fallback rather than a
    /// site a user would pick.
    #[must_use]
    pub fn site_channels(&self) -> Vec<ChannelInfo> {
        self.channels().into_iter().filter(|c| c.id != "generic").collect()
    }

    /// One channel's provider, or the structured "unknown channel" error.
    pub fn provider(&self, channel: &str) -> Result<&Arc<dyn Provider>> {
        self.providers.get(channel).ok_or_else(|| {
            Error::channel(format!(
                "unknown channel '{channel}'; known channels: {}",
                self.order.join(", ")
            ))
        })
    }

    /// Route a URL to its channel, falling back to `generic`.
    #[must_use]
    pub fn channel_for_url(&self, url: &str) -> Option<String> {
        let host = util::host_of(url)?;
        for id in &self.order {
            if id == "generic" {
                continue;
            }
            if let Some(provider) = self.providers.get(id) {
                if provider
                    .info()
                    .hosts
                    .iter()
                    .any(|h| host == *h || host.ends_with(&format!(".{h}")))
                {
                    return Some(id.clone());
                }
            }
        }
        Some("generic".to_string())
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_advertises_generic_last() {
        let hub = Hub::new();
        let ids: Vec<&str> = hub.channels().iter().map(|c| c.id).collect();
        assert_eq!(ids.last(), Some(&"generic"));
        assert!(!hub.site_channels().iter().any(|c| c.id == "generic"));
        assert_eq!(hub.site_channels().len(), 6);
    }

    #[test]
    fn url_routing_prefers_a_real_site_and_falls_back_to_generic() {
        let hub = Hub::new();
        assert_eq!(hub.channel_for_url("https://www.bilibili.com/video/BV1").as_deref(), Some("bilibili"));
        assert_eq!(hub.channel_for_url("https://example.com/x").as_deref(), Some("generic"));
        assert_eq!(hub.channel_for_url(""), None);
    }

    #[test]
    fn an_unknown_channel_is_refused_with_the_known_list() {
        let hub = Hub::new();
        let error = match hub.provider("nope") {
            Ok(_) => panic!("an unknown channel must be refused"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unknown channel 'nope'"));
        assert!(error.to_string().contains("bilibili"));
    }

    #[test]
    fn every_advertised_channel_is_reachable_from_the_url_router() {
        let hub = Hub::new();
        for channel in hub.site_channels() {
            if channel.hosts.is_empty() {
                continue;
            }
            let url = format!("https://{}/", channel.hosts[0]);
            assert_eq!(hub.channel_for_url(&url).as_deref(), Some(channel.id));
        }
    }
}
