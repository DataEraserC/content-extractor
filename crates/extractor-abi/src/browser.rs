//! Launching a browser and talking CDP to it — the effects half.
//!
//! `extractor_core::browser` owns what the conversation *looks like*: the DevTools
//! URLs, the request body, how a target list picks a page, how a cookie list
//! becomes Netscape text. This module owns everything that touches the machine:
//! finding a binary on `$PATH`, reserving a loopback port, spawning the child,
//! waiting for DevTools, opening the socket, deleting the throwaway profile.
//!
//! ## Why CDP rather than reading the cookie database
//!
//! `yt-dlp --cookies-from-browser` opens the browser's `Cookies` SQLite file and
//! decrypts each value with a key from the desktop keyring. That costs three
//! more dependencies and still fails when the profile is on another machine
//! from the process reading it — the normal case for a service. CDP against a
//! browser we launch ourselves has neither problem.
//!
//! ## Honest limitation
//!
//! We launch the browser **on the machine running this process**. With no
//! display there — a headless server, a container, a remote box — the window is
//! invisible and this method cannot work; use `manual` (paste a cookie file)
//! there. `--headless=new` is passed only when no `$DISPLAY`/`$WAYLAND_DISPLAY`
//! is set, so an SSH session with X forwarding still gets a real window.
//!
//! Secrets boundary: extracted cookies are credentials. They are returned once,
//! handed to the account store, and never written to a log.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use extractor_core::browser::{self, CHROMIUM_NAMES, EXTRA_DIRS, LAUNCH_POLL_MS, LAUNCH_TIMEOUT_MS};
use extractor_core::{Error, Result};

/// A browser we launched and can still talk to / kill.
pub struct BrowserHandle {
    pub pid: u32,
    pub port: u16,
    profile: PathBuf,
    child: Child,
}

impl Drop for BrowserHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

/// Locate an installed Chromium-family browser.
pub fn find_browser() -> Option<PathBuf> {
    for name in CHROMIUM_NAMES {
        if let Some(path) = which(name) {
            return Some(path);
        }
        for dir in EXTRA_DIRS {
            let candidate = Path::new(dir).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Resolve `name` through `$PATH`.
fn which(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Ask the kernel for a free loopback port.
///
/// The listener is dropped immediately, which is what makes the port free again;
/// a browser that is about to bind it races with anything else that reads the
/// ephemeral range in between. That race is inherent to this trick and is why
/// the caller retries on a failed launch.
fn free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|error| Error::io(format!("cannot reserve a debugging port: {error}")))?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| Error::io(format!("cannot read the reserved port: {error}")))
}

/// True when a graphical session looks available.
fn has_display() -> bool {
    std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
        || cfg!(target_os = "macos")
        || cfg!(target_os = "windows")
}

/// Launch a browser on a fresh profile, pointed at `url`.
///
/// A dedicated `--user-data-dir` is deliberate: it sidesteps Chrome's singleton
/// lock on the real profile (which would either fail outright or hand the
/// navigation to an already-running instance that has no debugging port) and it
/// keeps a login for the agent isolated from the user's browsing state.
pub fn launch(url: &str) -> Result<BrowserHandle> {
    let browser = find_browser().ok_or_else(|| {
        Error::other("no Chromium-based browser found; use manual cookie import")
    })?;
    let port = free_port()?;
    let profile = std::env::temp_dir().join(browser::profile_dir_name(port));
    let args = browser::launch_args(port, &profile.display().to_string(), url, !has_display());

    let child = Command::new(browser)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| Error::other(format!("cannot launch the browser: {error}")))?;
    let pid = child.id();
    let handle = BrowserHandle { pid, port, profile, child };
    wait_for_devtools(port)?;
    Ok(handle)
}

/// Poll the DevTools HTTP endpoint until it answers.
fn wait_for_devtools(port: u16) -> Result<()> {
    let deadline = Instant::now() + Duration::from_millis(LAUNCH_TIMEOUT_MS);
    let poll = Duration::from_millis(LAUNCH_POLL_MS);
    while Instant::now() < deadline {
        if http_get(&browser::version_url(port)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(poll);
    }
    Err(Error::http("the browser did not expose a DevTools endpoint in time"))
}

/// Pull the browser's cookies for `hosts`, as Netscape text.
///
/// `hosts` are bare suffixes (`douyin.com`), matched exactly or as a dot
/// suffix — the same rule the channel registry uses to route URLs.
pub fn cookies(port: u16, hosts: &[&str]) -> Result<String> {
    let body = http_get(&browser::list_url(port))?;
    let ws_url = browser::page_ws_url(&body)?;
    let payload = cdp_call(&ws_url, &browser::cookies_request())?;
    browser::cookies_from_result(&payload, hosts)
}

/// Close a launched browser and clean up its profile.
pub fn close(mut handle: BrowserHandle) -> Result<()> {
    let _ = handle.child.kill();
    let _ = handle.child.wait();
    let _ = std::fs::remove_dir_all(&handle.profile);
    Ok(())
}

/// Blocking HTTP GET against the loopback DevTools endpoint.
fn http_get(url: &str) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| Error::http(format!("cannot build client: {error}")))?;
    client
        .get(url)
        .send()
        .map_err(|error| Error::http(format!("GET {url} failed: {error}")))?
        .text()
        .map_err(|error| Error::http(format!("cannot read response: {error}")))
}

/// One CDP request on a fresh connection, returning its `result`.
///
/// A new connection per call is deliberate: it keeps no connection state that
/// would have to survive across process invocations, and DevTools allows many
/// concurrent clients. The socket is blocking, so the caller's own deadline is
/// what bounds this. The reply *shape* is parsed in `extractor_core::browser`;
/// this only moves bytes.
fn cdp_call(ws_url: &str, request: &str) -> Result<serde_json::Value> {
    use tungstenite::{connect, Message};

    let (mut socket, _) =
        connect(ws_url).map_err(|error| Error::http(format!("cannot reach DevTools: {error}")))?;
    socket
        .send(Message::Text(request.to_string().into()))
        .map_err(|error| Error::http(format!("cannot send DevTools request: {error}")))?;

    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                if let Some(result) = browser::read_reply(&text)? {
                    return Ok(result);
                }
                continue; // a CDP event, not our reply
            }
            Ok(_) => continue,
            Err(error) => {
                return Err(Error::http(format!("DevTools connection failed: {error}")));
            }
        }
    }
}

/// Best-effort process kill by pid, for cancelling a browser session.
///
/// The session file records a pid rather than a handle because the process that
/// launched the browser may be a different invocation from the one cancelling
/// it, so this is the only common ground. A pid that already exited is not an
/// error.
pub fn kill_pid(pid: u64) -> Result<()> {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .arg(pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_free_port_is_reusable_after_the_probe_drops_it() {
        let port = free_port().unwrap();
        assert!(port > 0);
        // The probe must have released it; rebinding proves it did.
        assert!(TcpListener::bind(("127.0.0.1", port)).is_ok());
    }

    #[test]
    fn which_finds_a_binary_that_must_exist() {
        // Every unix has `sh` in $PATH; the point is that the search works
        // rather than that a specific browser is installed in CI.
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-real-binary-extractor").is_none());
    }
}
