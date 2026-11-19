//! The registry: what exists, and which extractors *could* handle an input.
//!
//! ## Selection is the caller's job
//!
//! This module deliberately has **no** "pick the first match" function. A URL
//! on a rich platform is ambiguous — `bilibili.com` serves videos, articles,
//! opus posts, audio and live rooms — so silently choosing one would send an
//! article down the video path and produce a wrong result that looks like
//! success.
//!
//! Instead there are two steps:
//!
//! 1. [`candidates`] answers "who might own this?" with **every** matching
//!    extractor, ranked by how specific its patterns are. It is a suggestion
//!    list for a human or a model to read.
//! 2. [`extractor`] resolves an **explicitly chosen** id, or fails with the
//!    known ids listed. Nothing is inferred.
//!
//! A host that wants one-shot behaviour can present the candidate list and ask;
//! what it must not do is pretend the ambiguity was not there.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

use crate::content::{ContentKind, ExtractorInfo};
use crate::error::{Error, Result};
use crate::model::ChannelInfo;
use crate::provider::Provider;

/// One candidate in a [`candidates`] answer.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    /// The extractor the caller would pass to select this.
    pub extractor: ExtractorInfo,
    /// Total matched pattern length. More means more specific.
    pub score: usize,
    /// Why this extractor is suggested, in one line.
    pub reason: String,
}

/// Every registered platform, plus the extractors they advertise.
///
/// Pure: holds no store, no clock and no storage, so a caller can ask what
/// exists before deciding where its state lives.
pub struct Registry {
    providers: HashMap<String, Arc<dyn Provider>>,
    order: Vec<String>,
    extractors: Vec<ExtractorInfo>,
}

impl Registry {
    /// Build a registry from an explicit provider list.
    ///
    /// The composition root (`extractor-abi`) passes the platforms it linked;
    /// this crate names none of them, which is what keeps it platform-free.
    #[must_use]
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        let order = providers.iter().map(|p| p.info().id.to_string()).collect();
        let mut extractors = Vec::new();
        for provider in &providers {
            extractors.extend(provider.extractors());
        }
        let map = providers.into_iter().map(|p| (p.info().id.to_string(), p)).collect();
        Self { providers: map, order, extractors }
    }

    /// All platforms, in registration order. `generic` is last so real sites
    /// win URL routing too.
    #[must_use]
    pub fn platforms(&self) -> Vec<ChannelInfo> {
        self.order.iter().filter_map(|id| self.providers.get(id).map(|p| p.info())).collect()
    }

    /// Advertised platforms without `generic`, which is a fallback rather than
    /// a site a user would pick.
    #[must_use]
    pub fn site_platforms(&self) -> Vec<ChannelInfo> {
        self.platforms().into_iter().filter(|c| c.id != "generic").collect()
    }

    /// One platform's provider, or the structured "unknown platform" error.
    pub fn provider(&self, platform: &str) -> Result<&Arc<dyn Provider>> {
        self.providers.get(platform).ok_or_else(|| {
            Error::channel(format!(
                "unknown platform '{platform}'; known platforms: {}",
                self.order.join(", ")
            ))
        })
    }

    /// Every advertised extractor, in registration order.
    #[must_use]
    pub fn extractors(&self) -> &[ExtractorInfo] {
        &self.extractors
    }

    /// Resolve an **explicitly chosen** extractor id.
    ///
    /// There is no fallback: an unknown id is an error listing the known ones.
    pub fn extractor(&self, id: &str) -> Result<&ExtractorInfo> {
        self.extractors.iter().find(|e| e.id == id).ok_or_else(|| {
            Error::channel(format!(
                "unknown extractor '{id}'; known extractors: {}",
                self.extractors.iter().map(|e| e.id).collect::<Vec<_>>().join(", ")
            ))
        })
    }

    /// Which extractors *might* handle this input — plural, ranked, advisory.
    ///
    /// Never selects. An empty result means nothing advertises this shape, and
    /// the caller should say so rather than guess. Ranking uses total pattern
    /// length so a site-specific descriptor outranks a bare-`http` catch-all.
    #[must_use]
    pub fn candidates(&self, url: &str) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = self
            .extractors
            .iter()
            .filter(|e| e.suggests(url))
            .map(|e| Candidate {
                score: e.specificity(),
                reason: format!(
                    "{} handles {} content and its URL patterns match",
                    e.id,
                    e.kind.as_str()
                ),
                extractor: e.clone(),
            })
            .collect();
        // Most specific first; ties keep registration order (stable sort).
        out.sort_by_key(|entry| std::cmp::Reverse(entry.score));
        out
    }

    /// Which platform owns `url` for **credential** purposes.
    ///
    /// This is a different question from extraction: cookies are scoped to a
    /// host, so one platform owns them regardless of content kind. Falls back
    /// to `generic`.
    #[must_use]
    pub fn platform_for_url(&self, url: &str) -> Option<String> {
        let host = crate::util::host_of(url)?;
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

    /// Extractors belonging to one platform.
    #[must_use]
    pub fn extractors_of(&self, platform: &str) -> Vec<ExtractorInfo> {
        self.extractors.iter().filter(|e| e.platform == platform).cloned().collect()
    }

    /// Extractors producing one content kind.
    #[must_use]
    pub fn extractors_of_kind(&self, kind: &ContentKind) -> Vec<ExtractorInfo> {
        self.extractors.iter().filter(|e| &e.kind == kind).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{ContentKind, CredentialUse};
    use crate::model::{MethodInfo, MethodKind};
    use crate::provider::CheckOutcome;

    /// A stand-in platform advertising two content kinds on one host.
    struct TwoKinds;

    impl Provider for TwoKinds {
        fn info(&self) -> ChannelInfo {
            ChannelInfo {
                id: "example",
                name: "Example",
                hosts: vec!["example.com"],
                cookie_domain: "example.com",
                login_url: "https://example.com/login",
                methods: vec![MethodInfo::live("manual", "import", MethodKind::Import)],
            }
        }

        fn extractors(&self) -> Vec<ExtractorInfo> {
            vec![
                ExtractorInfo::new(
                    "example.video",
                    "example",
                    "video",
                    ContentKind::Video,
                    vec!["example.com", "/video/"],
                    CredentialUse::Enhances,
                ),
                ExtractorInfo::new(
                    "example.article",
                    "example",
                    "article",
                    ContentKind::Article,
                    vec!["example.com", "/read/"],
                    CredentialUse::Enhances,
                ),
            ]
        }

        fn start(
            &self,
            _method: &str,
            _params: &serde_json::Value,
            _ambient: crate::ambient::Ambient,
        ) -> Result<(crate::model::Challenge, serde_json::Value)> {
            Err(Error::channel("not used in this test"))
        }

        fn check(&self, _cookies: &str) -> Result<CheckOutcome> {
            Ok(CheckOutcome { logged_in: false, identity: String::new() })
        }
    }

    fn registry() -> Registry {
        Registry::new(vec![Arc::new(TwoKinds)])
    }

    #[test]
    fn candidates_returns_every_match_and_never_chooses() {
        let reg = registry();
        let videos = reg.candidates("https://example.com/video/abc");
        assert_eq!(videos.len(), 1);
        assert_eq!(videos[0].extractor.id, "example.video");
        let articles = reg.candidates("https://example.com/read/cv1");
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].extractor.id, "example.article");
        // A URL matching both patterns of neither yields nothing — not a guess.
        assert!(reg.candidates("https://example.com/other").is_empty());
    }

    #[test]
    fn an_ambiguous_url_lists_both_candidates_ranked_by_specificity() {
        struct Ambiguous;
        impl Provider for Ambiguous {
            fn info(&self) -> ChannelInfo {
                TwoKinds.info()
            }
            fn extractors(&self) -> Vec<ExtractorInfo> {
                vec![
                    ExtractorInfo::new(
                        "example.any",
                        "example",
                        "any",
                        ContentKind::Video,
                        vec!["example.com"],
                        CredentialUse::Enhances,
                    ),
                    ExtractorInfo::new(
                        "example.specific",
                        "example",
                        "specific",
                        ContentKind::Article,
                        vec!["example.com", "/read/", "cv"],
                        CredentialUse::Enhances,
                    ),
                ]
            }
            fn start(
                &self,
                _m: &str,
                _p: &serde_json::Value,
                _a: crate::ambient::Ambient,
            ) -> Result<(crate::model::Challenge, serde_json::Value)> {
                Err(Error::channel("unused"))
            }
            fn check(&self, _c: &str) -> Result<CheckOutcome> {
                Ok(CheckOutcome { logged_in: false, identity: String::new() })
            }
        }
        let reg = Registry::new(vec![Arc::new(Ambiguous)]);
        let found = reg.candidates("https://example.com/read/cv1");
        assert_eq!(found.len(), 2, "both must be offered, not one silently picked");
        assert_eq!(found[0].extractor.id, "example.specific", "most specific first");
        assert!(found[0].score > found[1].score);
    }

    #[test]
    fn explicit_selection_resolves_and_an_unknown_id_is_refused_with_the_list() {
        let reg = registry();
        assert_eq!(reg.extractor("example.article").unwrap().kind, ContentKind::Article);
        let error = reg.extractor("example.nope").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("unknown extractor 'example.nope'"));
        assert!(text.contains("example.video") && text.contains("example.article"));
    }

    #[test]
    fn credential_routing_is_per_host_and_independent_of_content_kind() {
        let reg = registry();
        // Both content kinds share one credential owner.
        assert_eq!(
            reg.platform_for_url("https://example.com/read/cv1").as_deref(),
            Some("example")
        );
        assert_eq!(
            reg.platform_for_url("https://example.com/video/x").as_deref(),
            Some("example")
        );
        assert_eq!(reg.platform_for_url("https://other.test/x").as_deref(), Some("generic"));
        assert_eq!(reg.platform_for_url(""), None);
    }

    #[test]
    fn filtering_by_platform_and_kind_works() {
        let reg = registry();
        assert_eq!(reg.extractors_of("example").len(), 2);
        assert_eq!(reg.extractors_of_kind(&ContentKind::Article).len(), 1);
        assert!(reg.extractors_of("nope").is_empty());
    }
}
