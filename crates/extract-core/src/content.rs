//! Platform provider crates — one crate per site.
//!
//! Each `platform-*` crate owns exactly two things for its site:
//!
//! * the **login conversation** that turns a user action into auth cookies
//!   ([`Provider`]), and
//! * the **content descriptors** it can extract ([`ExtractorInfo`]) — which
//!   kinds of content exist on that site and what URL shapes identify them.
//!
//! Everything else (cookie codec, storage, pooling, rotation, HTTP) is generic
//! and lives in `extract-core`. Splitting per platform means adding a site is
//! adding a crate, and a site's quirks cannot leak into another's code.
//!
//! ## Why descriptors are data, not dispatch
//!
//! A descriptor advertises *what could handle this input*; it never selects.
//! The caller picks an extractor id explicitly (see `extract-core::registry`).
//! This is deliberate: bilibili hosts videos, articles, opus posts, audio and
//! live rooms on one domain, so "first match wins" would silently send an
//! article to the video path. Advertising candidates and letting the caller
//! choose is the only shape that stays correct as sites grow content types.

use serde::{Deserialize, Serialize};

/// What kind of content an extractor produces.
///
/// Open-ended by design: `Other` carries a site-specific string so a new
/// content type needs no change here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Video,
    Audio,
    /// An article / column post: long-form text with inline images.
    Article,
    /// A short social post (bilibili opus, a weibo, a note).
    Post,
    /// A gallery or image set.
    Gallery,
    /// A live room's stream.
    Live,
    /// A game replay / match record file.
    Replay,
    /// A plain file behind a direct link.
    File,
    /// Anything else; the payload names it.
    Other(String),
}

impl ContentKind {
    /// Stable machine string, as it appears in JSON.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Article => "article",
            Self::Post => "post",
            Self::Gallery => "gallery",
            Self::Live => "live",
            Self::Replay => "replay",
            Self::File => "file",
            Self::Other(name) => name,
        }
    }
}

/// Whether a credential is required, optional-but-useful, or useless.
///
/// This is deliberately three-valued. A boolean "needs login" conflated two
/// different questions and lost the important middle case: bilibili serves a
/// video anonymously at 360p and the same URL at 1080p+ with cookies, so login
/// is not *required* yet it is exactly what a user wants applied. Collapsing
/// that to `false` silently dropped stored credentials on the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialUse {
    /// Extraction fails without a credential (members-only, paid, private).
    Required,
    /// Works anonymously, but a credential improves the result: higher quality,
    /// full text instead of a teaser, more items, or simply fewer blocks.
    /// **Stored cookies must still be sent.**
    Enhances,
    /// The resource is public and ignores credentials (a public CDN file).
    /// Sending cookies would be pointless — and leaks them to a third party.
    None,
}

impl CredentialUse {
    /// Should a stored credential be attached when one exists?
    ///
    /// True for both `Required` and `Enhances`: the whole point of the account
    /// pool is that a logged-in jar gets used wherever it helps, not only where
    /// it is mandatory.
    #[must_use]
    pub fn uses_credentials(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// One extractor a platform advertises.
///
/// `id` is globally unique and is what a caller passes to select it. `patterns`
/// are URL substrings/regex-free hints used only to *suggest* this extractor;
/// they never decide. `credentials` tells a host whether to attach the
/// platform's stored cookie jar — see [`CredentialUse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractorInfo {
    /// Globally unique extractor id, e.g. `bilibili.video`, `bilibili.article`.
    pub id: &'static str,
    /// Platform this extractor belongs to, e.g. `bilibili`.
    pub platform: &'static str,
    /// Human label for pickers.
    pub label: &'static str,
    /// Content kind produced.
    pub kind: ContentKind,
    /// URL fragments that suggest this extractor (host + path shape).
    /// Advisory only — see the module docs.
    pub patterns: Vec<&'static str>,
    /// How a credential affects this extractor.
    pub credentials: CredentialUse,
    /// Metadata keys this extractor is known to emit, beyond the common set.
    /// Documentation for callers; the actual metadata map stays open.
    pub metadata_keys: Vec<&'static str>,
}

impl ExtractorInfo {
    /// Build a descriptor with no extra metadata keys declared.
    #[must_use]
    pub fn new(
        id: &'static str,
        platform: &'static str,
        label: &'static str,
        kind: ContentKind,
        patterns: Vec<&'static str>,
        credentials: CredentialUse,
    ) -> Self {
        Self { id, platform, label, kind, patterns, credentials, metadata_keys: Vec::new() }
    }

    /// Should the platform's stored cookie jar be attached when one exists?
    #[must_use]
    pub fn uses_credentials(&self) -> bool {
        self.credentials.uses_credentials()
    }

    /// Declare the platform-specific metadata keys this extractor emits.
    #[must_use]
    pub fn with_metadata_keys(mut self, keys: Vec<&'static str>) -> Self {
        self.metadata_keys = keys;
        self
    }

    /// Does `url` look like something this extractor handles?
    ///
    /// Advisory: a `true` here means "candidate", never "chosen". Every
    /// pattern must appear in the URL (case-insensitive).
    #[must_use]
    pub fn suggests(&self, url: &str) -> bool {
        let lower = url.to_ascii_lowercase();
        !self.patterns.is_empty() && self.patterns.iter().all(|p| lower.contains(&p.to_ascii_lowercase()))
    }

    /// How specific this descriptor's patterns are, for ranking candidates.
    ///
    /// Total matched character length, not fragment count: a catch-all whose
    /// one pattern is `http` must rank below a site extractor whose pattern is
    /// `bilibili.com/read/`, even though both match one fragment.
    #[must_use]
    pub fn specificity(&self) -> usize {
        self.patterns.iter().map(|p| p.len()).sum()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enhances_and_required_both_attach_credentials() {
        // The bug this type exists to prevent: a boolean "needs login" made an
        // Enhances extractor look credential-free, so stored cookies were
        // dropped and a bilibili video stayed at 360p.
        assert!(CredentialUse::Required.uses_credentials());
        assert!(CredentialUse::Enhances.uses_credentials());
        assert!(!CredentialUse::None.uses_credentials());
    }

    #[test]
    fn the_wire_shape_is_a_stable_snake_case_string() {
        let info = ExtractorInfo::new(
            "x.video",
            "x",
            "video",
            ContentKind::Video,
            vec!["x.com/v/"],
            CredentialUse::Enhances,
        );
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["credentials"], serde_json::json!("enhances"));
        assert!(json.get("requires_login").is_none(), "the old boolean is gone");
        assert!(info.uses_credentials());
    }

    #[test]
    fn specificity_ranks_by_total_pattern_length() {
        let site = ExtractorInfo::new(
            "x.article", "x", "a", ContentKind::Article,
            vec!["x.com/read/"], CredentialUse::Enhances,
        );
        let catch_all = ExtractorInfo::new(
            "generic.any", "generic", "any", ContentKind::Video,
            vec!["http"], CredentialUse::None,
        );
        assert!(site.specificity() > catch_all.specificity());
        assert!(site.suggests("https://x.com/read/cv1"));
        assert!(!site.suggests("https://x.com/video/BV1"));
    }
}
