//! Browser-driven cookie extraction — the protocol half.
//!
//! This is the fallback that keeps every channel loggable even when the site
//! has no usable headless login API (YouTube, Youku, and anything parked behind
//! a captcha). Instead of speaking the site's login protocol, we hand the user
//! a real browser, let them sign in the ordinary way, and read the resulting
//! cookies back out over the DevTools Protocol.
//!
//! ## Why CDP rather than reading the cookie database
//!
//! The other way (`yt-dlp --cookies-from-browser`) is to open the browser's
//! `Cookies` SQLite file and decrypt each value with a key pulled from the
//! desktop keyring. That costs three more dependencies (SQLite, D-Bus secret
//! service, AES-CBC) and still fails whenever the profile lives on a different
//! machine from the process doing the reading — the normal case for a service
//! like dsh. CDP against a browser we launch ourselves has neither problem.
//!
//! ## What lives here, and what does not
//!
//! Only the *shape* of the conversation: how the DevTools HTTP URLs are built,
//! how a target list picks a page's WebSocket URL, how one CDP request is
//! framed, how a `Network.getAllCookies` reply becomes Netscape text. Opening a
//! socket, binding a loopback port, spawning a browser and deleting a profile
//! directory are all effects, and they live in `vlogin-abi`; this module gives
//! that side the exact bytes to send and reads back the ones it gets.
//!
//! Secrets boundary: extracted cookies are credentials. They are returned once,
//! handed to the account store, and never written to a log.

use serde_json::{json, Value};

use crate::cookies::{self, Cookie};
use crate::error::{Error, Result};

/// The CDP request id this crate sends. One request per connection, so a fixed
/// id is enough to tell our reply apart from an event.
pub const REQUEST_ID: i64 = 1;

/// URL of the DevTools version endpoint for a given debugging port.
#[must_use]
pub fn version_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/json/version")
}

/// URL of the DevTools target list for a given debugging port.
#[must_use]
pub fn list_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/json/list")
}

/// The `Network.getAllCookies` request body.
#[must_use]
pub fn cookies_request() -> String {
    json!({ "id": REQUEST_ID, "method": "Network.getAllCookies", "params": {} }).to_string()
}

/// Pick the WebSocket URL of the first open page from a `/json/list` body.
///
/// Targets that are not pages (workers, iframes' hosts, the browser target) are
/// skipped: cookies are attached to a page, and `Network.getAllCookies` on a
/// worker target is not the same conversation.
pub fn page_ws_url(body: &str) -> Result<String> {
    let targets: Vec<Value> =
        serde_json::from_str(body).map_err(|e| Error::http(format!("bad target list: {e}")))?;
    for target in targets {
        if target.get("type").and_then(|v| v.as_str()) == Some("page") {
            if let Some(ws) = target.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
                return Ok(ws.to_string());
            }
        }
    }
    Err(Error::http("no browser page is open"))
}

/// Read one CDP reply off the wire.
///
/// Returns `None` for a message that is not this request's reply (a CDP event,
/// or another client's answer), so the caller keeps reading. An `error` member
/// becomes an [`Error::Http`] carrying the DevTools message.
pub fn read_reply(text: &str) -> Result<Option<Value>> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| Error::http(format!("bad DevTools reply: {e}")))?;
    if value.get("id").and_then(|v| v.as_i64()) != Some(REQUEST_ID) {
        return Ok(None);
    }
    if let Some(error) = value.get("error") {
        return Err(Error::http(format!(
            "DevTools error: {}",
            error.get("message").and_then(|v| v.as_str()).unwrap_or("unknown")
        )));
    }
    Ok(Some(value.get("result").cloned().unwrap_or(Value::Null)))
}

/// Pull the browser's cookies for `hosts` out of a `Network.getAllCookies`
/// result, as Netscape text.
///
/// `hosts` are bare suffixes (`douyin.com`), matched exactly or as a dot
/// suffix — the same rule the channel registry uses to route URLs.
pub fn cookies_from_result(payload: &Value, hosts: &[&str]) -> Result<String> {
    let list = payload
        .get("cookies")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::http("DevTools returned no cookie list"))?;

    let mut out = Vec::new();
    for entry in list {
        let domain = entry.get("domain").and_then(|v| v.as_str()).unwrap_or_default();
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        if name.is_empty() || domain.is_empty() {
            continue;
        }
        if !host_matches(domain, hosts) {
            continue;
        }
        out.push(Cookie {
            name: name.to_string(),
            value: entry.get("value").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            domain: domain.to_string(),
            path: entry.get("path").and_then(|v| v.as_str()).unwrap_or("/").to_string(),
            // A negative or absent `expires` means "session cookie", which the
            // Netscape format encodes as 0.
            expires: entry.get("expires").and_then(|v| v.as_f64()).unwrap_or(0.0).max(0.0) as i64,
            secure: entry.get("secure").and_then(|v| v.as_bool()).unwrap_or(false),
            http_only: entry.get("httpOnly").and_then(|v| v.as_bool()).unwrap_or(false),
        });
    }
    Ok(cookies::to_netscape(&out, hosts.first().unwrap_or(&"")))
}

/// Whether a cookie domain belongs to one of `hosts`.
#[must_use]
fn host_matches(domain: &str, hosts: &[&str]) -> bool {
    let bare = domain.trim_start_matches('.');
    hosts.iter().any(|host| bare == *host || bare.ends_with(&format!(".{host}")))
}

/// The browser command line for a managed profile.
///
/// Pure so the flags that decide where cookies land are testable without
/// launching anything. `profile` is a directory the caller owns and will delete;
/// `headless` is passed when the caller found no graphical session.
#[must_use]
pub fn launch_args(port: u16, profile: &str, url: &str, headless: bool) -> Vec<String> {
    let mut args = vec![
        format!("--remote-debugging-port={port}"),
        format!("--user-data-dir={profile}"),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-gpu".to_string(),
        "--disable-dev-shm-usage".to_string(),
        "--no-sandbox".to_string(),
    ];
    if headless {
        args.push("--headless=new".to_string());
    }
    args.push(url.to_string());
    args
}

/// Chromium-family binaries, in preference order.
pub const CHROMIUM_NAMES: &[&str] = &[
    "google-chrome-stable",
    "google-chrome",
    "chromium",
    "chromium-browser",
    "brave",
    "microsoft-edge-stable",
    "microsoft-edge",
];

/// Directories worth checking beyond `$PATH` (macOS app bundles, Nix).
pub const EXTRA_DIRS: &[&str] = &[
    "/usr/bin",
    "/usr/local/bin",
    "/run/current-system/sw/bin",
    "/nix/var/nix/profiles/default/bin",
    "/Applications/Google Chrome.app/Contents/MacOS",
    "/Applications/Brave Browser.app/Contents/MacOS",
];

/// Name of the throwaway profile directory for a debugging port.
#[must_use]
pub fn profile_dir_name(port: u16) -> String {
    format!("vlogin-profile-{port}")
}

/// How long to wait for a freshly spawned browser to expose DevTools.
pub const LAUNCH_TIMEOUT_MS: u64 = 20_000;

/// How long to sleep between DevTools readiness probes.
pub const LAUNCH_POLL_MS: u64 = 200;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devtools_urls_point_at_loopback() {
        assert_eq!(version_url(9222), "http://127.0.0.1:9222/json/version");
        assert_eq!(list_url(9222), "http://127.0.0.1:9222/json/list");
        assert_eq!(profile_dir_name(9222), "vlogin-profile-9222");
    }

    #[test]
    fn page_ws_url_skips_targets_that_are_not_pages() {
        let body = json!([
            { "type": "service_worker", "webSocketDebuggerUrl": "ws://worker" },
            { "type": "page", "webSocketDebuggerUrl": "ws://page" }
        ])
        .to_string();
        assert_eq!(page_ws_url(&body).unwrap(), "ws://page");
        assert!(page_ws_url("[]").is_err());
        assert!(page_ws_url("not json").is_err());
    }

    #[test]
    fn read_reply_ignores_events_and_raises_devtools_errors() {
        assert!(read_reply(r#"{"method":"Network.loadingFinished"}"#).unwrap().is_none());
        assert!(read_reply(r#"{"id":2,"result":{}}"#).unwrap().is_none());
        let result = read_reply(r#"{"id":1,"result":{"cookies":[]}}"#).unwrap().unwrap();
        assert_eq!(result, json!({"cookies": []}));
        let error = read_reply(r#"{"id":1,"error":{"message":"nope"}}"#).unwrap_err();
        assert!(error.to_string().contains("DevTools error: nope"));
    }

    #[test]
    fn cookies_are_filtered_by_host_suffix_and_default_to_slash() {
        let payload = json!({ "cookies": [
            { "name": "a", "value": "1", "domain": ".bilibili.com", "path": "/", "expires": -1.0, "secure": true, "httpOnly": true },
            { "name": "b", "value": "2", "domain": "evil.example" },
            { "name": "", "value": "3", "domain": ".bilibili.com" }
        ]});
        let text = cookies_from_result(&payload, &["bilibili.com"]).unwrap();
        assert_eq!(cookies::count_netscape_cookies(&text), 1);
        assert!(text.contains("#HttpOnly_.bilibili.com\tTRUE\t/\tTRUE\t0\ta\t1"));
    }

    #[test]
    fn a_cookies_reply_without_a_list_is_an_error() {
        assert!(cookies_from_result(&json!({}), &["bilibili.com"]).is_err());
    }

    #[test]
    fn launch_args_carry_the_profile_and_optional_headless_flag() {
        let args = launch_args(9, "/tmp/p", "https://x/", true);
        assert!(args.contains(&"--remote-debugging-port=9".to_string()));
        assert!(args.contains(&"--user-data-dir=/tmp/p".to_string()));
        assert!(args.contains(&"--headless=new".to_string()));
        assert_eq!(args.last().unwrap(), "https://x/");
        assert!(!launch_args(9, "/tmp/p", "https://x/", false).contains(&"--headless=new".to_string()));
    }
}
