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

/// One extractor a platform advertises.
///
/// `id` is globally unique and is what a caller passes to select it. `patterns`
/// are URL substrings/regex-free hints used only to *suggest* this extractor;
/// they never decide. `requires_login` tells a host whether credentials are
/// needed before extraction will succeed.
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
    /// Whether a logged-in credential is normally required.
    pub requires_login: bool,
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
        requires_login: bool,
    ) -> Self {
        Self { id, platform, label, kind, patterns, requires_login, metadata_keys: Vec::new() }
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