//! The facade a host actually calls: core protocol composed with real effects.
//!
//! Every method here maps 1:1 onto an ABI op and a CLI subcommand, so there is a
//! single surface to document and test. The method bodies are the ones that used
//! to live on `vlogin_core::Hub`: what changed is that the state root, the
//! clock, the entropy and the browser are now supplied by this crate instead of
//! discovered inside the protocol layer. The behaviour and the JSON are
//! unchanged.

use serde_json::Value;

use vlogin_core::channels;
use vlogin_core::model::{Challenge, Poll};
use vlogin_core::session::Session;
use vlogin_core::{
    AccountView, Ambient, ChannelStatus, Error, Hub, Pool, PollResponse, Result, StartRequest,
    StartResponse,
};

use crate::browser;
use crate::effect::Machine;

/// A hub bound to one machine's state root, clock and browser.
pub struct Service {
    hub: Hub,
    machine: Machine,
    /// Owned, not borrowed: `Pool` holds a reference to this, so it has to
    /// outlive every pool the facade builds.
    storage: crate::effect::FsStorage,
}

impl Service {
    /// Bind the channel registry to `machine`.
    #[must_use]
    pub fn new(machine: Machine) -> Self {
        Self { hub: Hub::new(), machine, storage: crate::effect::FsStorage }
    }

    /// Bind the channel registry to the machine this process is running on.
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
        Pool::new(vlogin_core::Store::new(self.machine.root()), &self.storage)
    }

    /// Session directory for this machine.
    fn session_dir(&self) -> std::path::PathBuf {
        self.pool().session_dir()
    }

    /// All channels, in picker order, `generic` last.
    #[must_use]
    pub fn channels(&self) -> Vec<vlogin_core::ChannelInfo> {
        self.hub.channels()
    }

    /// Advertised channels without `generic`.
    #[must_use]
    pub fn site_channels(&self) -> Vec<vlogin_core::ChannelInfo> {
        self.hub.site_channels()
    }

    /// Route a URL to its channel.
    #[must_use]
    pub fn channel_for_url(&self, url: &str) -> Option<String> {
        self.hub.channel_for_url(url)
    }

    /// Begin an interactive login (QR, SMS, password).
    pub fn start(&self, request: &StartRequest) -> Result<StartResponse> {
        let provider = self.hub.provider(&request.channel)?;
        let (challenge, state) =
            provider.start(&request.method, &request.params, self.ambient())?;
        let mut session =
            Session::create(&request.channel, &request.method, state, self.ambient());
        if let Some(label) = &request.label {
            channels::state_set(&mut session, "label", Value::String(label.clone()));
        }
        session.save(&self.session_dir(), &self.storage)?;
        Ok(StartResponse { session_id: session.id.clone(), challenge })
    }

    /// Begin a browser-driven login: open the site in a managed browser and
    /// wait for the user to sign in.
    pub fn browse(&self, channel: &str) -> Result<StartResponse> {
        let provider = self.hub.provider(channel)?;
        let info = provider.info();
        if info.login_url.is_empty() {
            return Err(Error::channel(format!(
                "channel '{channel}' has no login page to open; import cookies instead"
            )));
        }
        let url = info.login_url;
        channels::require_method(&info, "browser")?;
        let handle = browser::launch(url)?;
        let state = channels::state_of(&[
            ("pid", Value::from(handle.pid)),
            ("port", Value::from(handle.port)),
            ("hosts", serde_json::to_value(&info.hosts)?),
            ("jar", Value::String(String::new())),
        ]);
        let session = Session::create(channel, "browser", state, self.ambient());
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

        let provider = self.hub.provider(&session.channel)?;
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
        let provider = self.hub.provider(&session.channel)?;
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
            Ok(cookies) if vlogin_core::cookies::count_netscape_cookies(&cookies) > 0 => {
                let provider = self.hub.provider(&session.channel)?;
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
                let label = channels::state_str(session, "label");
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
        channel: &str,
        method: &str,
        cookies_text: &str,
        label: &str,
    ) -> Result<AccountView> {
        let provider = self.hub.provider(channel)?;
        let info = provider.info();
        if method != "manual" {
            channels::require_method(&info, method)?;
        }
        let normalized = channels::normalize_import(cookies_text, info.cookie_domain)?;
        let identity = provider.check(&normalized).map(|outcome| outcome.identity).unwrap_or_default();
        self.pool().upsert(channel, method, &normalized, &identity, label, self.ambient())
    }

    /// Probe one account and record the result. Omit `account_id` to probe the
    /// pinned account.
    pub fn status(&self, channel: &str, account_id: Option<&str>) -> Result<AccountView> {
        let provider = self.hub.provider(channel)?;
        let pool = self.pool();
        let account = match account_id {
            Some(id) => pool.get(channel, id, self.ambient())?,
            None => pool.active(channel, self.ambient())?,
        };
        let status = match provider.check(&account.cookies) {
            Ok(outcome) if outcome.logged_in => vlogin_core::AccountStatus::Active,
            Ok(_) => vlogin_core::AccountStatus::Expired,
            Err(_) => vlogin_core::AccountStatus::Unreachable,
        };
        let mut view = account.view();
        view.status = status;
        pool.record_check(channel, &account.id, status, self.ambient())?;
        Ok(view)
    }

    /// Every account of a channel, pinned first.
    pub fn accounts(&self, channel: &str) -> Result<Vec<AccountView>> {
        self.hub.provider(channel)?;
        self.pool().list(channel, self.ambient())
    }

    /// Status of every channel, for a full dashboard render.
    #[must_use]
    pub fn statuses(&self) -> Vec<ChannelStatus> {
        let pool = self.pool();
        self.site_channels()
            .into_iter()
            .map(|channel| ChannelStatus {
                active: pool
                    .load(&channel.id, self.ambient())
                    .ok()
                    .and_then(|file| file.active),
                accounts: pool.list(&channel.id, self.ambient()).unwrap_or_default(),
                channel,
            })
            .collect()
    }

    /// Pin an account as the channel's credential.
    pub fn pin(&self, channel: &str, account_id: &str) -> Result<AccountView> {
        self.hub.provider(channel)?;
        self.pool().pin(channel, account_id, self.ambient())
    }

    /// Log out of one account (drop its cookies).
    pub fn logout(&self, channel: &str, account_id: &str) -> Result<bool> {
        self.hub.provider(channel)?;
        self.pool().remove(channel, account_id, self.ambient())
    }

    /// Drop every account of a channel.
    pub fn clear(&self, channel: &str) -> Result<usize> {
        self.hub.provider(channel)?;
        self.pool().clear(channel, self.ambient())
    }

    /// Netscape text for one account, to hand to a downloader. SECRET.
    pub fn export(&self, channel: &str, account_id: Option<&str>) -> Result<String> {
        self.hub.provider(channel)?;
        let pool = self.pool();
        match account_id {
            Some(id) => pool.cookies_for(channel, id, self.ambient()),
            None => Ok(pool.active(channel, self.ambient())?.cookies),
        }
    }

    /// Write one account's cookies to a 0600 file and return its path.
    ///
    /// This is what a downloader's `--cookies` flag points at. The file holds
    /// secrets; the path does not.
    pub fn materialize(
        &self,
        channel: &str,
        account_id: Option<&str>,
    ) -> Result<std::path::PathBuf> {
        self.hub.provider(channel)?;
        let pool = self.pool();
        match account_id {
            Some(id) => pool.materialize(channel, id, self.ambient()),
            None => {
                let account = pool.active(channel, self.ambient())?;
                pool.materialize(channel, &account.id, self.ambient())
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
        self.pool().sweep_sessions(vlogin_core::session::DEFAULT_TTL_MS, self.ambient())
    }
}
