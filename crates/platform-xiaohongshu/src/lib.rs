//! 小红书 — `www.xiaohongshu.com` notes and videos.
//!
//! The crate is named after the host, so "which site is this?" is answerable
//! from the directory name alone. As with every `platform-*` crate it owns two
//! things and nothing else: the **login conversation** ([`Provider`]) and the
//! **content descriptors** ([`ExtractorInfo`]) this site hosts. The bytes are
//! fetched by the content layer in each host, not here.
//!
//! ## What this platform hosts
//!
//! A note page (`/explore/<note_id>` or `/discovery/item/<note_id>`) is either
//! an **image/text note** or a **video note**. The URL alone cannot say which,
//! so both extractors advertise the same pattern and tie on specificity — the
//! caller picks, exactly as with the wmpvp site. Choosing wrong is an
//! actionable error naming the other id, never a silent empty result.
//!
//! ## Why there is no signing code here
//!
//! The site's Web API needs `x-s` / `x-s-common` signatures and a short-lived,
//! source-bound `xsec_token`. Those change with the site's front-end and are a
//! maintenance treadmill, so the content layer reads the server-rendered
//! `window.__INITIAL_STATE__` on the note page instead. This crate therefore
//! needs no HTTP client: it declares hosts and credentials, which the kernel
//! uses to attach the right cookie jar and to route a URL to this platform.
//!
//! ## Login methods
//!
//! 90% of notes require a session, so `manual` (import a cookie file) and
//! `browser` (read one from a signed-in browser over the local debug port) are
//! live. QR / SMS / password are reserved: the app-side confirm loop and the
//! device risk checks make them a poor first implementation. A host renders
//! all five with `implemented` flags and gets a typed refusal for reserved
//! ones, rather than a missing button.

use std::sync::Arc;

use extractor_core::ambient::Ambient;
use extractor_core::content::{ContentKind, CredentialUse, ExtractorInfo};
use extractor_core::cookies;
use extractor_core::error::Result;
use extractor_core::model::{ChannelInfo, MethodInfo, MethodKind};
use extractor_core::provider::{require_method, Challenge, CheckOutcome, Provider};
use serde_json::Value;

/// 小红书 (`www.xiaohongshu.com`) provider.
pub struct Xiaohongshu;

impl Xiaohongshu {
    /// Construct a boxed provider for the registry.
    ///
    /// Returns `Arc<dyn Provider>` rather than `Self` on purpose: the registry
    /// stores trait objects, so every platform hands back the same erased type
    /// and the composition root needs no per-platform generics.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

impl Default for Xiaohongshu {
    fn default() -> Self {
        Self
    }
}

impl Provider for Xiaohongshu {
    fn extractors(&self) -> Vec<ExtractorInfo> {
        vec![
            ExtractorInfo::new(
                "xiaohongshu.note",
                "xiaohongshu",
                "小红书图文笔记",
                ContentKind::Post,
                vec!["xiaohongshu.com"],
                CredentialUse::Enhances,
            )
            .with_metadata_keys(vec![
                "note_id",
                "xsec_token",
                "author",
                "author_id",
                "avatar",
                "text",
                "images",
                "image_count",
                "liked_count",
                "collected_count",
                "comment_count",
                "share_count",
                "tags",
                "ip_location",
                "created_at",
            ]),
            ExtractorInfo::new(
                "xiaohongshu.video",
                "xiaohongshu",
                "小红书视频笔记",
                ContentKind::Video,
                vec!["xiaohongshu.com"],
                CredentialUse::Enhances,
            )
            .with_metadata_keys(vec![
                "note_id",
                "xsec_token",
                "author",
                "author_id",
                "duration",
                "video_url",
                "thumbnail",
                "liked_count",
                "collected_count",
                "comment_count",
                "share_count",
                "tags",
                "created_at",
            ]),
        ]
    }

    fn info(&self) -> ChannelInfo {
        ChannelInfo {
            id: "xiaohongshu",
            name: "小红书",
            hosts: vec!["xiaohongshu.com", "xhslink.com"],
            cookie_domain: "xiaohongshu.com",
            login_url: "https://www.xiaohongshu.com/",
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
        _params: &Value,
        _ambient: Ambient,
    ) -> Result<(Challenge, Value)> {
        require_method(&self.info(), method)?;
        // `manual` / `browser` are completed by the host (cookie import or a
        // local browser read), not by a headless wire conversation here, so
        // there is no challenge this kernel can mint. The typed refusal names
        // the site's login URL for the host to render.
        Err(extractor_core::provider::unsupported(&self.info(), method))
    }

    fn check(&self, cookies_text: &str) -> Result<CheckOutcome> {
        // `web_session` is the long-lived session cookie; without it the jar
        // is anonymous even when device cookies (`a1`) are present. Only the
        // cookie *name* is inspected — a value never leaves this function.
        let logged_in = cookies::parse_netscape(cookies_text)
            .iter()
            .any(|cookie| cookie.name == "web_session");
        Ok(CheckOutcome {
            logged_in,
            identity: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_content_kinds_are_advertised_for_the_same_url_shape() {
        let extractors = Xiaohongshu.extractors();
        let url = "https://www.xiaohongshu.com/explore/69821980000000000e03c95f";
        let matching: Vec<&str> = extractors
            .iter()
            .filter(|e| e.suggests(url))
            .map(|e| e.id)
            .collect();
        assert_eq!(matching, vec!["xiaohongshu.note", "xiaohongshu.video"]);
        // The URL cannot disambiguate them — that is exactly why both exist.
        assert_eq!(extractors[0].specificity(), extractors[1].specificity());
    }

    #[test]
    fn the_discovery_variant_is_also_covered() {
        let extractors = Xiaohongshu.extractors();
        assert!(extractors
            .iter()
            .all(|e| e
                .suggests("https://www.xiaohongshu.com/discovery/item/69821980000000000e03c95f")));
    }

    #[test]
    fn no_headless_login_is_offered_and_qr_is_reserved() {
        let info = Xiaohongshu.info();
        let qr = info.methods.iter().find(|m| m.id == "qr").unwrap();
        assert!(!qr.implemented);
        assert_eq!(info.methods.iter().filter(|m| m.implemented).count(), 2);
        assert!(info.hosts.contains(&"xhslink.com"));
    }

    #[test]
    fn check_needs_the_session_cookie_not_just_device_cookies() {
        let jar = "xiaohongshu.com\tTRUE\t/\tTRUE\t0\ta1\tdevice-only\n";
        assert!(!Xiaohongshu.check(jar).unwrap().logged_in);
        let jar = "xiaohongshu.com\tTRUE\t/\tTRUE\t0\tweb_session\tsession-value\n";
        assert!(Xiaohongshu.check(jar).unwrap().logged_in);
    }
}
