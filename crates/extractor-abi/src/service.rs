//! The facade a host actually calls: core protocol composed with real effects.
//!
//! Every method here maps 1:1 onto an ABI op and a CLI subcommand, so there is a
//! single surface to document and test. The method bodies are the ones that used
//! to live on `extractor_core::Hub`: what changed is that the state root, the
//! clock, the entropy and the browser are now supplied by this crate instead of
//! discovered inside the protocol layer. The behaviour and the JSON are
//! unchanged.

use serde_json::Value;

use extractor_core::provider;
use extractor_core::model::{Challenge, Poll};
use extractor_core::session::Session;
use extractor_core::{
    AccountView, Ambient, Error, PlatformStatus, Pool, PollResponse, Registry, Result,
    StartRequest, StartResponse,
};

use crate::browser;
use crate::effect::Machine;

/// The platforms this build links, in picker order (`generic` last).
///
/// This is the composition root: `extractor-core` names no platform, so the list
/// lives here. Adding a site means adding its crate to `Cargo.toml` and one
/// line below.
fn platforms() -> Vec<std::sync::Arc<dyn extractor_core::Provider>> {
    vec![
        platform_bilibili::Bilibili::new(),
        platform_douyin::Douyin::new(),
        platform_youtube::YouTube::new(),
        platform_tencent::Tencent::new(),
        platform_iqiyi::IQiyi::new(),
        platform_youku::Youku::new(),
        platform_wmpvp::WmPvp::new(),
        platform_astralparty::AstralParty::new(),
        platform_xiaohongshu::Xiaohongshu::new(),
        platform_generic::Generic::new(),
    ]
}

/// A registry bound to one machine's state root, clock and browser.
pub struct Service {
    registry: Registry,
    machine: Machine,
    /// Owned, not borrowed: `Pool` holds a reference to this, so it has to
    /// outlive every pool the facade builds.
    storage: crate::effect::FsStorage,
}

impl Service {
    /// Bind the platform registry to `machine`.
    #[must_use]
    pub fn new(machine: Machine) -> Self {
        Self {
            registry: Registry::new(platforms()),
            machine,
            storage: crate::effect::FsStorage,
        }
    }

    /// Bind the platform registry to the machine this process is running on.
    #[must_use]
    pub fn discover() -> Self {
        Self::new(Machine::discover())
    }

    /// State root.
    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        self.machine.root()
    }

    /// This operation's clock and entropy.
    fn ambient(&self) -> Ambient {
        self.machine.ambient()
    }

    /// The account pool, bound to this machine's storage.
    fn pool(&self) -> Pool<'_, crate::effect::FsStorage> {
        Pool::new(extractor_core::Store::new(self.machine.root()), &self.storage)
    }

    /// Session directory for this machine.
    fn session_dir(&self) -> std::path::PathBuf {
        self.pool().session_dir()
    }

    /// All platforms, in picker order, `generic` last.
    #[must_use]
    pub fn platforms(&self) -> Vec<extractor_core::ChannelInfo> {
        self.registry.platforms()
    }

    /// Advertised platforms without `generic`.
    #[must_use]
    pub fn site_platforms(&self) -> Vec<extractor_core::ChannelInfo> {
        self.registry.site_platforms()
    }

    /// Route a URL to its credential-owning platform.
    #[must_use]
    pub fn platform_for_url(&self, url: &str) -> Option<String> {
        self.registry.platform_for_url(url)
    }

    /// Begin an interactive login (QR, SMS, password).
    pub fn start(&self, request: &StartRequest) -> Result<StartResponse> {
        let provider = self.registry.provider(&request.platform)?;
        let (challenge, state) =
            provider.start(&request.method, &request.params, self.ambient())?;
        let mut session =
            Session::create(&request.platform, &request.method, state, self.ambient());
        if let Some(label) = &request.label {
            provider::state_set(&mut session, "label", Value::String(label.clone()));
        }
        session.save(&self.session_dir(), &self.storage)?;
        Ok(StartResponse { session_id: session.id.clone(), challenge })
    }

    /// Begin a browser-driven login: open the site in a managed browser and
    /// wait for the user to sign in.
    pub fn browse(&self, platform: &str) -> Result<StartResponse> {
        let provider = self.registry.provider(platform)?;
        let info = provider.info();
        if info.login_url.is_empty() {
            return Err(Error::channel(format!(
                "platform '{platform}' has no login page to open; import cookies instead"
            )));
        }
        let url = info.login_url;
        provider::require_method(&info, "browser")?;
        let handle = browser::launch(url)?;
        let state = provider::state_of(&[
            ("pid", Value::from(handle.pid)),
            ("port", Value::from(handle.port)),
            ("hosts", serde_json::to_value(&info.hosts)?),
            ("jar", Value::String(String::new())),
        ]);
        let session = Session::create(platform, "browser", state, self.ambient());
        session.save(&self.session_dir(), &self.storage)?;
        // Keep the child process alive for the lifetime of the session: the
        // handle is dropped here, which would kill it, so leak it deliberately
        // and let the pid/port be the only record. The browser exits when the
        // user closes the window; `cancel` kills it by pid.
        std::mem::forget(handle);
        Ok(StartResponse {
            session_id: session.id.clone(),
            challenge: Challenge::Browse {
                url: url.to_string(),
                prompt: "已打开浏览器，请在其中完成登录后回到这里".into(),
            },
        })
    }

    /// Advance a login that needs no further input.
    pub fn poll(&self, session_id: &str) -> Result<PollResponse> {
        let dir = self.session_dir();
        let storage = self.machine.storage();
        let mut session = Session::load(&dir, session_id, self.ambient().now_ms(), &storage)?
            .ok_or_else(|| Error::channel("that login session has expired; start again"))?;

        if session.method == "browser" {
            return self.poll_browser(&mut session);
        }

        let provider = self.registry.provider(&session.channel)?;
        let result = provider.poll(&mut session, self.ambient())?;
        session.save(&dir, &storage)?;
        self.finish(&mut session, result)
    }

    /// Advance a login with user-supplied input.
    pub fn submit(&self, session_id: &str, params: &Value) -> Result<PollResponse> {
        let dir = self.session_dir();
        let storage = self.machine.storage();
        let mut session = Session::load(&dir, session_id, self.ambient().now_ms(), &storage)?
            .ok_or_else(|| Error::channel("that login session has expired; start again"))?;
        let provider = self.registry.provider(&session.channel)?;
        let result = provider.submit(&mut session, params, self.ambient())?;
        session.save(&dir, &storage)?;
        self.finish(&mut session, result)
    }

    /// Pull cookies out of the browser we launched for `session`.
    fn poll_browser(&self, session: &mut Session) -> Result<PollResponse> {
        let dir = self.session_dir();
        let storage = self.machine.storage();
        let port = session
            .state
            .get("port")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| Error::channel("this browser session has no debugging port"))?
            as u16;
        let hosts: Vec<String> = session
            .state
            .get("hosts")
            .and_then(|v| v.as_array())
            .map(|list| list.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let host_refs: Vec<&str> = hosts.iter().map(|s| s.as_str()).collect();

        match browser::cookies(port, &host_refs) {
            Ok(cookies) if extractor_core::cookies::count_netscape_cookies(&cookies) > 0 => {
                let provider = self.registry.provider(&session.channel)?;
                let identity =
                    provider.check(&cookies).map(|outcome| outcome.identity).unwrap_or_default();
                session.save(&dir, &storage)?;
                self.finish(session, Poll::Done { cookies, identity })
            }
            Ok(_) => Ok(PollResponse {
                result: Poll::Pending { message: "等待在浏览器中完成登录…".into() },
                account: None,
            }),
            Err(error) => Ok(PollResponse {
                // A browser that has not finished starting (or that the user
                // closed) is a normal interim state, not a failure.
                result: Poll::Pending { message: error.to_string() },
                account: None,
            }),
        }
    }

    /// Store a completed login, or hand back the in-flight session untouched.
    pub fn finish(&self, session: &mut Session, result: Poll) -> Result<PollResponse> {
        match result {
            Poll::Done { cookies, identity } => {
                let label = provider::state_str(session, "label");
                let pool = self.pool();
                let view = pool.upsert(
                    &session.channel,
                    &session.method,
                    &cookies,
                    &identity,
                    &label,
                    self.ambient(),
                )?;
                Session::remove(&self.session_dir(), &session.id, &self.storage)?;
                Ok(PollResponse { result: Poll::Done { cookies, identity }, account: Some(view) })
            }
            other => Ok(PollResponse { result: other, account: None }),
        }
    }

    /// Import a finished credential (a pasted cookie file or header).
    pub fn import(
        &self,
        platform: &str,
        method: &str,
        cookies_text: &str,
        label: &str,
    ) -> Result<AccountView> {
        let provider = self.registry.provider(platform)?;
        let info = provider.info();
        if method != "manual" {
            provider::require_method(&info, method)?;
        }
        let normalized = provider::normalize_import(cookies_text, info.cookie_domain)?;
        let identity = provider.check(&normalized).map(|outcome| outcome.identity).unwrap_or_default();
        self.pool().upsert(platform, method, &normalized, &identity, label, self.ambient())
    }

    /// Probe one account and record the result. Omit `account_id` to probe the
    /// pinned account.
    pub fn status(&self, platform: &str, account_id: Option<&str>) -> Result<AccountView> {
        let provider = self.registry.provider(platform)?;
        let pool = self.pool();
        let account = match account_id {
            Some(id) => pool.get(platform, id, self.ambient())?,
            None => pool.active(platform, self.ambient())?,
        };
        let status = match provider.check(&account.cookies) {
            Ok(outcome) if outcome.logged_in => extractor_core::AccountStatus::Active,
            Ok(_) => extractor_core::AccountStatus::Expired,
            Err(_) => extractor_core::AccountStatus::Unreachable,
        };
        let mut view = account.view();
        view.status = status;
        pool.record_check(platform, &account.id, status, self.ambient())?;
        Ok(view)
    }

    /// Every account of a platform, pinned first.
    pub fn accounts(&self, platform: &str) -> Result<Vec<AccountView>> {
        self.registry.provider(platform)?;
        self.pool().list(platform, self.ambient())
    }

    /// Status of every platform, for a full dashboard render.
    #[must_use]
    pub fn statuses(&self) -> Vec<PlatformStatus> {
        let pool = self.pool();
        self.site_platforms()
            .into_iter()
            .map(|platform| PlatformStatus {
                active: pool
                    .load(platform.id, self.ambient())
                    .ok()
                    .and_then(|file| file.active),
                accounts: pool.list(platform.id, self.ambient()).unwrap_or_default(),
                extractors: self.registry.extractors_of(platform.id),
                platform,
            })
            .collect()
    }

    /// Every advertised content extractor.
    #[must_use]
    pub fn extractors(&self) -> Vec<extractor_core::ExtractorInfo> {
        self.registry.extractors().to_vec()
    }

    /// Which extractors *might* handle this URL — plural, ranked, advisory.
    ///
    /// Never selects; the caller reads the list and passes an explicit id to
    /// [`Self::resolve_extractor`].
    #[must_use]
    pub fn candidates(&self, url: &str) -> Vec<extractor_core::Candidate> {
        self.registry.candidates(url)
    }

    /// Resolve an explicitly chosen extractor id, or refuse with the known list.
    pub fn resolve_extractor(&self, id: &str) -> Result<extractor_core::ExtractorInfo> {
        self.registry.extractor(id).cloned()
    }

    /// Pin an account as the platform's credential.
    pub fn pin(&self, platform: &str, account_id: &str) -> Result<AccountView> {
        self.registry.provider(platform)?;
        self.pool().pin(platform, account_id, self.ambient())
    }

    /// Log out of one account (drop its cookies).
    pub fn logout(&self, platform: &str, account_id: &str) -> Result<bool> {
        self.registry.provider(platform)?;
        self.pool().remove(platform, account_id, self.ambient())
    }

    /// Drop every account of a platform.
    pub fn clear(&self, platform: &str) -> Result<usize> {
        self.registry.provider(platform)?;
        self.pool().clear(platform, self.ambient())
    }

    /// Netscape text for one account, to hand to a downloader. SECRET.
    pub fn export(&self, platform: &str, account_id: Option<&str>) -> Result<String> {
        self.registry.provider(platform)?;
        let pool = self.pool();
        match account_id {
            Some(id) => pool.cookies_for(platform, id, self.ambient()),
            None => Ok(pool.active(platform, self.ambient())?.cookies),
        }
    }

    /// Write one account's cookies to a 0600 file and return its path.
    ///
    /// This is what a downloader's `--cookies` flag points at. The file holds
    /// secrets; the path does not.
    pub fn materialize(
        &self,
        platform: &str,
        account_id: Option<&str>,
    ) -> Result<std::path::PathBuf> {
        self.registry.provider(platform)?;
        let pool = self.pool();
        match account_id {
            Some(id) => pool.materialize(platform, id, self.ambient()),
            None => {
                let account = pool.active(platform, self.ambient())?;
                pool.materialize(platform, &account.id, self.ambient())
            }
        }
    }

    /// Abandon a login in flight, killing its browser if it owns one.
    pub fn cancel(&self, session_id: &str) -> Result<()> {
        let dir = self.session_dir();
        let storage = self.machine.storage();
        if let Some(session) =
            Session::load(&dir, session_id, self.ambient().now_ms(), &storage)?
        {
            if let Some(pid) = session.state.get("pid").and_then(|v| v.as_u64()) {
                let _ = browser::kill_pid(pid);
            }
        }
        Session::remove(&dir, session_id, &storage)
    }

    /// Delete expired session files.
    pub fn sweep(&self) -> Result<usize> {
        self.pool().sweep_sessions(extractor_core::session::DEFAULT_TTL_MS, self.ambient())
    }
}
