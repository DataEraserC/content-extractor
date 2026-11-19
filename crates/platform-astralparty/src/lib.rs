//! 吉星派对 / 星引擎 Party / Astral Party — public match-replay files.
//!
//! ## What a replay is
//!
//! A replay is **not** a video. It is a public static protobuf file on the
//! game's CDN:
//!
//! ```text
//! CN:   https://sereplaycn.feimogames.com/prod/<replayId>
//! INTL: https://sereplayjp.feimogames.com/prod/<replayId>
//! ```
//!
//! No credential is needed. A replay id is 16 digits: unix seconds (10) + room
//! number (6). The two regions' ids do not overlap — an id from one region 404s
//! on the other, which is why `region` is an explicit extractor parameter
//! rather than something guessed.
//!
//! The deliverable is the **match file itself**; rendering it requires the game
//! client, which this project deliberately does not attempt. Metadata (map,
//! mode, rounds, per-player settlement stats) is parsed as a convenience and
//! rides in the open metadata map.
//!
//! ## No login methods
//!
//! The replay CDN is public and the game has no web login surface this project
//! speaks, so only the two universal credential methods are advertised.

use std::sync::Arc;

use serde_json::Value;

use extractor_core::ambient::Ambient;
use extractor_core::content::{ContentKind, CredentialUse, ExtractorInfo};
use extractor_core::cookies;
use extractor_core::error::Result;
use extractor_core::model::{ChannelInfo, MethodInfo, MethodKind};
use extractor_core::provider::{require_method, Challenge, CheckOutcome, Provider};

/// Replay CDN base per region. Public static files.
pub const REPLAY_HOST_CN: &str = "https://sereplaycn.feimogames.com/prod/";
/// International server. Ids are not interchangeable with the CN server.
pub const REPLAY_HOST_INTL: &str = "https://sereplayjp.feimogames.com/prod/";

/// Astral Party provider.
pub struct AstralParty;

impl AstralParty {
    /// Construct a boxed provider for the registry.
    ///
    /// Returns `Arc<dyn Provider>` rather than `Self` on purpose: the
    /// registry stores trait objects, so every platform hands back the same
    /// erased type and the composition root needs no per-platform generics.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

impl Default for AstralParty {
    fn default() -> Self {
        Self
    }
}

/// Whether `id` is a syntactically valid replay id (16 digits).
#[must_use]
pub fn is_replay_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_digit())
}

/// The CDN URL for one replay id and region.
///
/// An unknown region falls back to CN, matching the game's own default.
#[must_use]
pub fn replay_url(id: &str, region: &str) -> String {
    let base = if region == "intl" { REPLAY_HOST_INTL } else { REPLAY_HOST_CN };
    format!("{base}{id}")
}

impl Provider for AstralParty {
    fn extractors(&self) -> Vec<ExtractorInfo> {
        vec![ExtractorInfo::new(
            "astralparty.replay",
            "astralparty",
            "吉星派对对局回放文件",
            ContentKind::Replay,
            // Both the bare-id form (host resolves it) and the CDN URL form.
            vec!["feimogames.com/prod/"],
            CredentialUse::None,
        )
        .with_metadata_keys(vec![
            "region", "room_id", "room_name", "map_id", "map_name", "mode",
            "round", "state", "frame_count", "started_at", "players",
        ])]
    }

    fn info(&self) -> ChannelInfo {
        ChannelInfo {
            id: "astralparty",
            name: "吉星派对",
            hosts: vec!["feimogames.com"],
            cookie_domain: "feimogames.com",
            login_url: "https://se.feimogames.com/",
            methods: vec![
                MethodInfo::live("manual", "手动导入 Cookie", MethodKind::Import),
                MethodInfo::live("browser", "从浏览器读取", MethodKind::Browser),
                MethodInfo::reserved("qr", "扫码登录", MethodKind::Qr),
                MethodInfo::reserved("sms", "短信验证码登录", MethodKind::Credential),
                MethodInfo::reserved("password", "账号密码登录", MethodKind::Credential),
            ],
        }
    }

    fn start(
        &self,
        method: &str,
        params: &Value,
        ambient: Ambient,
    ) -> Result<(Challenge, Value)> {
        require_method(&self.info(), method)?;
        let _ = (params, ambient);
        Err(extractor_core::provider::unsupported(&self.info(), method))
    }

    fn check(&self, cookies_text: &str) -> Result<CheckOutcome> {
        let logged_in = cookies::count_netscape_cookies(cookies_text) > 0;
        Ok(CheckOutcome { logged_in, identity: String::new() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_ids_are_sixteen_digits() {
        assert!(is_replay_id("1795093645690603"));
        assert!(!is_replay_id("179509364569060"));
        assert!(!is_replay_id("17950936456906030"));
        assert!(!is_replay_id("179509364569060a"));
        assert!(!is_replay_id(""));
    }

    #[test]
    fn region_selects_the_host_and_unknown_falls_back_to_cn() {
        assert_eq!(
            replay_url("1795093645690603", "cn"),
            "https://sereplaycn.feimogames.com/prod/1795093645690603"
        );
        assert_eq!(
            replay_url("1795093645690603", "intl"),
            "https://sereplayjp.feimogames.com/prod/1795093645690603"
        );
        assert_eq!(replay_url("1", "nonsense"), "https://sereplaycn.feimogames.com/prod/1");
    }

    #[test]
    fn the_replay_extractor_is_advertised_as_a_replay_not_a_video() {
        let extractors = AstralParty.extractors();
        assert_eq!(extractors.len(), 1);
        assert_eq!(extractors[0].kind, ContentKind::Replay);
        assert!(extractors[0].suggests("https://sereplaycn.feimogames.com/prod/1795093645690603"));
        assert_eq!(extractors[0].credentials, CredentialUse::None);
        assert!(!extractors[0].uses_credentials(), "a public CDN file must not receive cookies");
    }
}
