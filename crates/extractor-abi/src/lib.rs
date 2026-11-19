//! C ABI over `extractor-core`.
//!
//! One entry point, `extractor_call`, plus `extractor_version` for diagnostics. Both
//! sides speak JSON, so the FFI surface stays at two functions and a host binding
//! is a dozen lines of koffi rather than a hand-written struct mirror of the
//! whole model.
//!
//! ## Where the effects live
//!
//! `extractor-core` is a protocol layer: it decides *what* a state root looks like,
//! *which* account is pinned, *what* bytes a CDP request carries. It reads no
//! file, resolves no environment variable, spawns no process and reads no clock.
//! This crate is the other half — [`effect`] owns the filesystem and the
//! environment, [`browser`] owns the child process and the loopback socket,
//! [`service`] composes the two around the core, and the dispatch below is a
//! thin JSON translation of that facade. The contract in `docs/abi.md` is
//! unchanged by the split, and so is every byte of it.
//!
//! ## Memory contract — caller-allocated output
//!
//! ```c
//! char buf[1 << 20];
//! int64_t needed = extractor_call("{\"op\":\"platforms\"}", buf, sizeof buf);
//! ```
//!
//! The caller owns the output buffer; Rust only writes into it. There is
//! deliberately **no** `extract_string_free`. The obvious alternative —
//! returning a Rust-allocated C string for the host to free — demands that the
//! host's allocator and Rust's agree, and every forgetful binding leaks. A
//! caller-owned buffer has neither problem; the only cost is one extra call if
//! the first buffer was too small.
//!
//! Return value is always the size the response occupies **including** the
//! trailing NUL:
//!
//! * `> 0` — response length. It was copied into `out` only if it fit;
//!   otherwise grow the buffer to this size and call again.
//! * `-1` — `request` was null or not valid UTF-8.
//!
//! ## Secrets boundary
//!
//! Every response is shaped so no cookie value appears in it. The only op that
//! returns secret text is `export`, and it exists to be written straight into a
//! downloader's cookie file, not to be displayed.

pub mod browser;
pub mod effect;
pub mod service;

use std::ffi::CStr;
use std::os::raw::c_char;
use std::ptr;

use serde_json::{json, Value};
use extractor_core::{Error, StartRequest};

pub use crate::effect::{resolve_root, FsStorage, Machine};
pub use crate::service::Service;

/// Run one JSON-encoded operation.
///
/// `request` must be a NUL-terminated UTF-8 string; `out`/`out_len` describe a
/// caller-owned buffer the JSON response is written into, NUL included.
///
/// # Safety
///
/// `request` must be a valid pointer to a NUL-terminated C string (or null).
/// `out` must be null or point to at least `out_len` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn extractor_call(
    request: *const c_char,
    out: *mut c_char,
    out_len: u64,
) -> i64 {
    if request.is_null() {
        return -1;
    }
    let text = match CStr::from_ptr(request).to_str() {
        Ok(value) => value,
        Err(_) => return -1,
    };

    let response = match serde_json::from_str::<Value>(text) {
        Ok(parsed) => dispatch(&parsed),
        Err(error) => error_response("bad_request", &error.to_string()),
    };

    let encoded = response.to_string();
    // +1 for the NUL terminator.
    let needed = encoded.len() + 1;
    if !out.is_null() && needed <= out_len as usize {
        let bytes = encoded.as_bytes();
        ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, bytes.len());
        *out.add(bytes.len()) = 0;
    }
    needed as i64
}

/// Library version, for host diagnostics.
///
/// Points at a `'static` array, so the pointer stays valid forever and must
/// **not** be freed.
#[no_mangle]
pub extern "C" fn extractor_version() -> *const c_char {
    c"0.2.0".as_ptr()
}

/// A failed operation, tagged with a stable machine-readable `kind`.
fn error_response(kind: &str, message: &str) -> Value {
    json!({ "ok": false, "error": { "kind": kind, "message": message } })
}

/// A successful operation.
fn ok(result: Value) -> Value {
    json!({ "ok": true, "result": result })
}

/// Route one request to its handler.
///
/// ## Ops
///
/// * `platforms` — every advertised platform, with display name and methods.
///   `channels` is accepted as an alias for older hosts.
/// * `extractors` — every advertised content extractor (id, platform, kind,
///   patterns, whether login is needed, known metadata keys).
/// * `candidates` — `{ url }` → **every** extractor that might handle it,
///   ranked by specificity. Advisory: the caller chooses.
/// * `resolve`    — `{ extractor }` → that extractor's descriptor, or a
///   structured refusal listing the known ids. This is the explicit selection.
/// * `route`    — `{ url }` → `{ platform }`: which platform owns the URL's
///   *credentials* (a different question from extraction).
/// * `status`   — every platform with its account pool, pinned account id and
///   advertised extractors.
/// * `accounts` — `{ platform }` → that platform's accounts, pinned first.
/// * `start`    — `{ platform, method, params?, label? }` → `{ session_id, challenge }`.
/// * `browse`   — `{ platform }` → `{ session_id, challenge }` (real browser).
/// * `poll`     — `{ sessionId }` → `{ result, account? }`.
/// * `submit`   — `{ sessionId, params }` → `{ result, account? }`.
/// * `import`   — `{ platform, cookies, label? }` → `{ account }`.
/// * `check`    — `{ platform, account? }` → `{ account }` (probes the site).
/// * `pin`      — `{ platform, account }` → `{ account }`.
/// * `logout`   — `{ platform, account }` → `{ removed }`.
/// * `clear`    — `{ platform }` → `{ removed }`.
/// * `cancel`   — `{ sessionId }` → `{ cancelled }`.
/// * `export`   — `{ platform, account? }` → `{ cookies }`. SECRET: the only
///   response carrying cookie text.
/// * `materialize` — `{ platform, account? }` → `{ path }` of a 0600 cookie file.
/// * `sweep`  — deletes expired login sessions.
///
/// The closure wrapper is what lets every arm use `?` for both the kernel's own
/// errors and `serde_json`'s while agreeing on one return type.
fn dispatch(request: &Value) -> Value {
    let op = match request.get("op").and_then(|v| v.as_str()) {
        Some(op) => op,
        None => return error_response("bad_request", "missing 'op'"),
    };

    let result = (|| -> Result<Value, Error> {
        match op {
            "platforms" | "channels" => {
                let service = Service::discover();
                Ok(serde_json::to_value(service.site_platforms())?)
            }
            "extractors" => {
                let service = Service::discover();
                Ok(serde_json::to_value(service.extractors())?)
            }
            "candidates" => {
                let url = require_string(request, "url")?;
                let service = Service::discover();
                Ok(serde_json::to_value(service.candidates(&url))?)
            }
            "resolve" => {
                let id = require_string(request, "extractor")?;
                let service = Service::discover();
                Ok(serde_json::to_value(service.resolve_extractor(&id)?)?)
            }
            "route" => {
                let url = require_string(request, "url")?;
                let service = Service::discover();
                let platform = service.platform_for_url(&url);
                // `channel` stays in the payload as an alias so an older host
                // binding keeps working through the rename.
                Ok(json!({ "platform": platform, "channel": platform }))
            }
            "status" => {
                let service = Service::discover();
                Ok(serde_json::to_value(service.statuses())?)
            }
            "accounts" => {
                let channel = require_channel(request)?;
                let service = Service::discover();
                Ok(serde_json::to_value(service.accounts(&channel)?)?)
            }
            "start" => {
                let start: StartRequest = serde_json::from_value(request.clone())?;
                let service = Service::discover();
                Ok(serde_json::to_value(service.start(&start)?)?)
            }
            "browse" => {
                let channel = require_channel(request)?;
                let service = Service::discover();
                Ok(serde_json::to_value(service.browse(&channel)?)?)
            }
            "poll" => {
                let session_id = require_string(request, "sessionId")?;
                let service = Service::discover();
                Ok(serde_json::to_value(service.poll(&session_id)?)?)
            }
            "submit" => {
                let session_id = require_string(request, "sessionId")?;
                let params = request.get("params").cloned().unwrap_or(Value::Null);
                let service = Service::discover();
                Ok(serde_json::to_value(service.submit(&session_id, &params)?)?)
            }
            "import" => {
                let channel = require_channel(request)?;
                let method =
                    require_string_optional(request, "method").unwrap_or_else(|| "manual".into());
                let cookies = require_string(request, "cookies")?;
                let label = require_string_optional(request, "label").unwrap_or_default();
                let service = Service::discover();
                let view = service.import(&channel, &method, &cookies, &label)?;
                Ok(serde_json::to_value(view)?)
            }
            "check" => {
                let channel = require_channel(request)?;
                let account = require_string_optional(request, "account");
                let service = Service::discover();
                let view = service.status(&channel, account.as_deref())?;
                Ok(serde_json::to_value(view)?)
            }
            "pin" => {
                let channel = require_channel(request)?;
                let account = require_string(request, "account")?;
                let service = Service::discover();
                Ok(serde_json::to_value(service.pin(&channel, &account)?)?)
            }
            "logout" => {
                let channel = require_channel(request)?;
                let account = require_string(request, "account")?;
                let service = Service::discover();
                // Bare bool, not `{ removed: bool }`: the ABI envelope already
                // is the wrapper, so a second one would nest and every host
                // binding would have to know to unwrap twice.
                Ok(json!(service.logout(&channel, &account)?))
            }
            "clear" => {
                let channel = require_channel(request)?;
                let service = Service::discover();
                Ok(json!(service.clear(&channel)?))
            }
            "export" => {
                let channel = require_channel(request)?;
                let account = require_string_optional(request, "account");
                let service = Service::discover();
                Ok(json!({ "cookies": service.export(&channel, account.as_deref())? }))
            }
            "materialize" => {
                let channel = require_channel(request)?;
                let account = require_string_optional(request, "account");
                let service = Service::discover();
                Ok(json!({ "path": service.materialize(&channel, account.as_deref())? }))
            }
            "cancel" => {
                let session_id = require_string(request, "sessionId")?;
                let service = Service::discover();
                service.cancel(&session_id)?;
                Ok(json!({ "cancelled": true }))
            }
            "sweep" => {
                let service = Service::discover();
                Ok(json!({ "swept": service.sweep()? }))
            }
            other => Err(Error::channel(format!("unknown op '{other}'"))),
        }
    })();

    match result {
        Ok(value) => ok(value),
        Err(error) => error_response(error_kind(&error), &error.to_string()),
    }
}

/// Map an error onto the stable `kind` string hosts switch on.
fn error_kind(error: &Error) -> &'static str {
    match error {
        Error::Channel(_) => "channel",
        Error::Cookie(_) => "cookie",
        Error::Http(_) => "http",
        Error::Io(_) => "io",
        Error::Json(_) => "json",
        Error::Other(_) => "other",
    }
}

/// Read the `platform` field, which most ops carry.
///
/// `channel` is accepted as an alias: it was the field's name before the
/// project widened from video to content, and an older host binding must keep
/// working through the rename.
fn require_channel(request: &Value) -> Result<String, Error> {
    request
        .get("platform")
        .or_else(|| request.get("channel"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::channel("missing 'platform'"))
}

/// Read a required string field.
fn require_string(request: &Value, key: &str) -> Result<String, Error> {
    request
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::channel(format!("missing '{key}'")))
}

/// Read an optional string field.
fn require_string_optional(request: &Value, key: &str) -> Option<String> {
    request
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_request_is_minus_one() {
        let mut buffer = [0i8; 64];
        let result = unsafe { extractor_call(ptr::null(), buffer.as_mut_ptr(), buffer.len() as u64) };
        assert_eq!(result, -1);
    }

    #[test]
    fn a_missing_op_is_a_bad_request_and_a_good_one_round_trips() {
        let response = dispatch(&json!({}));
        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["error"]["kind"], json!("bad_request"));

        let response = dispatch(&json!({ "op": "platforms" }));
        assert_eq!(response["ok"], json!(true));
        let platforms = response["result"].as_array().unwrap();
        assert_eq!(platforms.len(), 8, "generic is not advertised as a site");
        assert_eq!(platforms[0]["id"], json!("bilibili"));
        assert!(platforms.iter().all(|p| p["id"] != json!("generic")));
    }

    #[test]
    fn candidates_is_plural_and_resolve_is_explicit() {
        // An article URL must offer the article extractor, not silently be
        // treated as a video.
        let response = dispatch(&json!({
            "op": "candidates",
            "url": "https://www.bilibili.com/read/cv1/"
        }));
        assert_eq!(response["ok"], json!(true));
        let list = response["result"].as_array().unwrap();
        assert!(list.len() > 1, "candidates must be plural");
        assert_eq!(list[0]["extractor"]["id"], json!("bilibili.article"));
        assert_eq!(list[0]["extractor"]["kind"], json!("article"));

        let video = dispatch(&json!({
            "op": "candidates",
            "url": "https://www.bilibili.com/video/BV1xx"
        }));
        assert_eq!(video["result"][0]["extractor"]["id"], json!("bilibili.video"));

        // Explicit selection resolves; an unknown id is refused with the list.
        let resolved = dispatch(&json!({ "op": "resolve", "extractor": "bilibili.article" }));
        assert_eq!(resolved["result"]["kind"], json!("article"));
        let unknown = dispatch(&json!({ "op": "resolve", "extractor": "bilibili.nope" }));
        assert_eq!(unknown["ok"], json!(false));
        assert!(unknown["error"]["message"].as_str().unwrap().contains("known extractors"));
    }

    #[test]
    fn the_legacy_channel_field_and_op_alias_still_work() {
        // An older host binding sends `channel`; it must keep working.
        let response = dispatch(&json!({ "op": "channels" }));
        assert_eq!(response["ok"], json!(true));
        let route = dispatch(&json!({ "op": "route", "url": "https://www.bilibili.com/read/cv1" }));
        assert_eq!(route["result"]["platform"], json!("bilibili"));
        assert_eq!(route["result"]["channel"], json!("bilibili"));
        // `channel` as the request field is accepted for accounts.
        let accounts = dispatch(&json!({ "op": "accounts", "channel": "bilibili" }));
        assert_eq!(accounts["ok"], json!(true));
    }

    #[test]
    fn an_unknown_op_is_a_channel_error() {
        let response = dispatch(&json!({ "op": "nope" }));
        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["error"]["kind"], json!("channel"));
    }

    #[test]
    fn the_c_buffer_contract_reports_the_needed_size_including_the_nul() {
        let request = std::ffi::CString::new(r#"{"op":"platforms"}"#).unwrap();
        let needed = unsafe { extractor_call(request.as_ptr(), ptr::null_mut(), 0) };
        assert!(needed > 0);

        let mut buffer = vec![0u8; needed as usize];
        let written = unsafe {
            extractor_call(request.as_ptr(), buffer.as_mut_ptr() as *mut c_char, buffer.len() as u64)
        };
        assert_eq!(written, needed);
        assert_eq!(buffer[needed as usize - 1], 0, "the response is NUL-terminated");
        let text = std::str::from_utf8(&buffer[..needed as usize - 1]).unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["ok"], json!(true));

        // Too small: nothing is written, and the size still comes back.
        let mut tiny = vec![0u8; 4];
        let short = unsafe {
            extractor_call(request.as_ptr(), tiny.as_mut_ptr() as *mut c_char, tiny.len() as u64)
        };
        assert_eq!(short, needed);
        assert_eq!(tiny, vec![0u8; 4], "a too-small buffer must be left untouched");
    }

    #[test]
    fn version_is_the_frozen_abi_string() {
        let version = unsafe { CStr::from_ptr(extractor_version()) };
        assert_eq!(version.to_str().unwrap(), "0.2.0");
    }
}
