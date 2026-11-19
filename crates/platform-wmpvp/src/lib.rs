//! 完美世界电竞 — `news.wmpvp.com` community content.
//!
//! The crate is named after the host (`wmpvp`) rather than an abbreviation, so
//! "which site is this?" is answerable from the directory name alone.
//!
//! ## What this platform hosts
//!
//! `news.wmpvp.com/community-detail.html?id=…` is a Vue SPA with no
//! server-rendered content, so a generic page extractor finds nothing. The
//! data comes from one public JSON API the page itself calls:
//!
//! ```text
//! GET https://appengine.wmpvp.com/steamcn/community/post/getPostById
//!     ?postId=<id>&token=
//! ```
//!
//! Public posts need no credential (`token` may be empty). A post is either a
//! **video** (a CS/DOTA highlight, `videoInfo.playInfoList[]` with direct MP4
//! URLs) or an **image/text note**. Those are two content kinds and therefore
//! two extractors: the caller picks, because the URL alone does not say which
//! one a given id is.
//!
//! ## The CDN referer
//!
//! `highlights.wmpvp.com` answers 403 to a bare request; it requires
//! `Referer: https://news.wmpvp.com/`. That is a property of this platform's
//! CDN, so it is recorded here as [`CDN_REFERER`] for the extracting host to
//! send — the kernel's HTTP helper stays site-agnostic.
//!
//! ## No login methods
//!
//! There is no headless login protocol implemented for 完美世界电竞. Both credential
//! methods that work anywhere are advertised (`manual`, `browser`) and the
//! rest are reserved, so a host renders a complete picker and gets a typed
//! refusal instead of a missing button.

use std::sync::Arc;

use serde_json::Value;

use extractor_core::ambient::Ambient;
use extractor_core::content::{ContentKind, CredentialUse, ExtractorInfo};
use extractor_core::cookies;
use extractor_core::error::Result;
use extractor_core::model::{ChannelInfo, MethodInfo, MethodKind};
use extractor_core::provider::{require_method, Challenge, CheckOutcome, Provider};

/// The public post API the SPA calls. No credential needed for public posts.
pub const POST_API: &str = "https://appengine.wmpvp.com/steamcn/community/post/getPostById";

/// Referer the highlights CDN checks; without it every play URL 403s.
pub const CDN_REFERER: &str = "https://news.wmpvp.com/";

/// 完美世界电竞 (`news.wmpvp.com`) provider.
pub struct WmPvp;

impl WmPvp {
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

impl Default for WmPvp {
    fn default() -> Self {
        Self
    }
}

impl Provider for WmPvp {
    fn extractors(&self) -> Vec<ExtractorInfo> {
        vec![
            ExtractorInfo::new(
                "wmpvp.video",
                "wmpvp",
                "完美世界电竞社区视频",
                ContentKind::Video,
                vec!["news.wmpvp.com/community-", "id="],
                CredentialUse::Enhances,
            )
            .with_metadata_keys(vec![
                "vid", "steam_id", "play_formats", "read_count", "like_count",
                "reply_count", "tags", "width", "height",
            ]),
            ExtractorInfo::new(
                "wmpvp.note",
                "wmpvp",
                "完美世界电竞社区图文",
                ContentKind::Post,
                vec!["news.wmpvp.com/community-", "id="],
                CredentialUse::Enhances,
            )
            .with_metadata_keys(vec![
                "text", "images", "image_count", "read_count", "like_count", "tags",
            ]),
        ]
    }

    fn info(&self) -> ChannelInfo {
        ChannelInfo {
            id: "wmpvp",
            name: "完美世界电竞",
            hosts: vec!["wmpvp.com", "pwesports.cn"],
            cookie_domain: "wmpvp.com",
            login_url: "https://news.wmpvp.com/",
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
        // Public content needs no credential, and there is no unauthenticated
        // status endpoint to probe. Presence of cookies is the honest claim.
        let logged_in = cookies::count_netscape_cookies(cookies_text) > 0;
        Ok(CheckOutcome { logged_in, identity: String::new() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_content_kinds_are_advertised_for_the_same_url_shape() {
        let extractors = WmPvp.extractors();
        let url = "https://news.wmpvp.com/community-detail.html?id=392049040";
        let matching: Vec<&str> =
            extractors.iter().filter(|e| e.suggests(url)).map(|e| e.id).collect();
        assert_eq!(matching, vec!["wmpvp.video", "wmpvp.note"]);
        // The URL cannot disambiguate them — that is exactly why both exist.
        assert_eq!(extractors[0].specificity(), extractors[1].specificity());
    }

    #[test]
    fn the_pc_detail_variant_is_also_covered() {
        let extractors = WmPvp.extractors();
        assert!(extractors
            .iter()
            .all(|e| e.suggests("https://news.wmpvp.com/community-pcDetail.html?id=1")));
    }

    #[test]
    fn no_headless_login_is_offered_and_qr_is_reserved() {
        let info = WmPvp.info();
        let qr = info.methods.iter().find(|m| m.id == "qr").unwrap();
        assert!(!qr.implemented);
        assert!(info.methods.iter().filter(|m| m.implemented).count() == 2);
    }
}
