//! Generic platform — cookie import for any host, plus the catch-all extractors.
//!
//! This is the escape hatch. Credential routing falls back to it, so a URL the
//! tool has never heard of is still usable: the agent can import cookies for
//! that host and an extractor will send them.
//!
//! There is deliberately no QR or credential method here. "Log in" to an
//! arbitrary host means either pasting cookies or reading them from a browser —
//! there is no protocol to speak.
//!
//! The generic platform keeps **one** account per host, keyed by the host, so
//! cookies for `example.com` never leak into a request for `other.com`.
//!
//! Its extractors are the two content-agnostic ones: a direct file link, and
//! "any page, via yt-dlp's own 1800-site catalogue". Both are advertised as
//! *candidates* like every other extractor — the caller still chooses.

use std::sync::Arc;

use serde_json::Value;

use extract_core::provider::{require_method, Challenge, CheckOutcome, Provider};
use extract_core::content::{ContentKind, CredentialUse, ExtractorInfo};
use extract_core::ambient::Ambient;
use extract_core::cookies;
use extract_core::error::Result;
use extract_core::model::{ChannelInfo, MethodInfo, MethodKind};

/// Generic provider. `cookie_domain` is filled in per host at call time, so the
/// advertised value is only a placeholder.
pub struct Generic;

impl Generic {
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

impl Default for Generic {
    fn default() -> Self {
        Self
    }
}

impl Provider for Generic {
    fn extractors(&self) -> Vec<ExtractorInfo> {
        vec![
            ExtractorInfo::new(
                "generic.file",
                "generic",
                "直链文件",
                ContentKind::File,
                // Advisory only: the host also checks the extension/MIME, so a
                // bare http prefix is enough to put this on the candidate list.
                vec!["http"],
                CredentialUse::Enhances,
            )
            .with_metadata_keys(vec!["content_type", "bytes"]),
            ExtractorInfo::new(
                "generic.ytdlp",
                "generic",
                "yt-dlp（任意站点视频/音频）",
                ContentKind::Video,
                vec!["http"],
                CredentialUse::Enhances,
            )
            .with_metadata_keys(vec!["extractor_key", "formats", "subtitles"]),
        ]
    }

    fn info(&self) -> ChannelInfo {
        ChannelInfo {
            id: "generic",
            name: "其他站点",
            hosts: vec![],
            cookie_domain: "",
            login_url: "",
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
        Err(extract_core::provider::unsupported(&self.info(), method))
    }

    fn check(&self, cookies_text: &str) -> Result<CheckOutcome> {
        // With no site API to ask, the only defensible answer is "there are
        // cookies here". Callers must not treat this as proof of a live
        // session.
        let logged_in = cookies::count_netscape_cookies(cookies_text) > 0;
        Ok(CheckOutcome { logged_in, identity: String::new() })
    }
}
