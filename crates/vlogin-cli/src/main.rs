//! `vlogin` — multi-channel video-account login from a shell.
//!
//! Deliberately harness-free: everything here works on a plain box with no dsh
//! installed. It is also the easiest way to debug a provider, since it prints
//! the same JSON the ABI returns.
//!
//! ```sh
//! vlogin channels                      # what can I log into, and how?
//! vlogin login start bilibili qr       # -> {"sessionId":…,"challenge":{…}}
//! vlogin login poll <sessionId>        # keep calling until result is done
//! vlogin import bilibili ./cookies.txt # store a finished credential
//! vlogin status                        # is anything still logged in?
//! vlogin export bilibili > jar.txt     # hand cookies to a downloader
//! ```
//!
//! Secrets boundary: `export` writes cookie text to stdout — that is its job.
//! No other subcommand prints a cookie value; `status` prints counts only.

use std::io::Read;
use std::process::ExitCode;

use serde_json::{json, Value};
// `vlogin-abi` exposes the effectful facade as the crate `vlogin`: the library
// it builds is `libvlogin.so` for the C ABI, and the CLI is its second host.
use vlogin::Service;
use vlogin_core::Error;

/// Printed on a bad command line.
const USAGE: &str = "\
vlogin — multi-channel video-account login

Usage:
  vlogin channels
  vlogin status
  vlogin accounts <channel>
  vlogin login start <channel> <method> [key=value ...] [--label NAME]
  vlogin login poll <sessionId>
  vlogin login submit <sessionId> [key=value ...]
  vlogin browse <channel>
  vlogin import <channel> [--method manual] [--label NAME] [--file PATH]
  vlogin check <channel> [account]
  vlogin pin <channel> <account>
  vlogin logout <channel> <account>
  vlogin clear <channel>
  vlogin export <channel> [account]
  vlogin materialize <channel> [account]
  vlogin cancel <sessionId>
  vlogin sweep
";

/// A parsed command line.
enum Command {
    Channels,
    Status,
    Accounts { channel: String },
    Start { channel: String, method: String, params: Value },
    Poll { session_id: String },
    Submit { session_id: String, params: Value },
    Browse { channel: String },
    Import { channel: String, method: String, label: String, file: Option<String> },
    Check { channel: String, account: Option<String> },
    Pin { channel: String, account: String },
    Logout { channel: String, account: String },
    Clear { channel: String },
    Export { channel: String, account: Option<String> },
    Materialize { channel: String, account: Option<String> },
    Cancel { session_id: String },
    Sweep,
}

/// Print usage and exit.
fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

/// Emit one JSON value as the process's only stdout line.
fn emit(value: Value) -> ExitCode {
    println!("{value}");
    ExitCode::SUCCESS
}

/// Emit `{"ok":false,…}` and fail.
fn fail(error: &Error) -> ExitCode {
    let kind = match error {
        Error::Channel(_) => "channel",
        Error::Cookie(_) => "cookie",
        Error::Http(_) => "http",
        Error::Io(_) => "io",
        Error::Json(_) => "json",
        Error::Other(_) => "other",
    };
    println!("{}", json!({ "ok": false, "error": { "kind": kind, "message": error.to_string() } }));
    ExitCode::FAILURE
}

/// Read `--flag value` out of the argument list.
fn flag(arguments: &[String], name: &str) -> Option<String> {
    arguments.windows(2).find(|pair| pair[0] == name).map(|pair| pair[1].clone())
}

/// Parse `key=value` positional arguments into an object.
fn parse_params(arguments: &[String]) -> Value {
    let mut map = serde_json::Map::new();
    for argument in arguments {
        if let Some((key, value)) = argument.split_once('=') {
            map.insert(key.to_string(), Value::String(value.to_string()));
    }
    }
    Value::Object(map)
}

/// Parse a command line. `None` means "print usage".
fn parse(arguments: &[String]) -> Option<Command> {
    match arguments.first().map(|s| s.as_str()) {
        Some("channels") => Some(Command::Channels),
        Some("status") => Some(Command::Status),
        Some("accounts") => arguments.get(1).cloned().map(|channel| Command::Accounts { channel }),
        Some("login") => match arguments.get(1).map(|s| s.as_str()) {
            Some("start") => match (arguments.get(2), arguments.get(3)) {
                (Some(channel), Some(method)) => Some(Command::Start {
                    channel: channel.clone(),
                    method: method.clone(),
                    params: parse_params(&arguments[4..]),
                }),
                _ => None,
            },
            Some("poll") => {
                arguments.get(2).cloned().map(|session_id| Command::Poll { session_id })
            }
            Some("submit") => arguments.get(2).cloned().map(|session_id| Command::Submit {
                session_id,
                params: parse_params(&arguments[3..]),
            }),
            _ => None,
        },
        Some("browse") => arguments.get(1).cloned().map(|channel| Command::Browse { channel }),
        Some("import") => arguments.get(1).cloned().map(|channel| Command::Import {
            channel,
            method: flag(arguments, "--method").unwrap_or_else(|| "manual".to_string()),
            label: flag(arguments, "--label").unwrap_or_default(),
            file: flag(arguments, "--file"),
        }),
        Some("check") => arguments.get(1).cloned().map(|channel| Command::Check {
            channel,
            account: arguments.get(2).cloned(),
        }),
        Some("pin") => match (arguments.get(1), arguments.get(2)) {
            (Some(channel), Some(account)) => {
                Some(Command::Pin { channel: channel.clone(), account: account.clone() })
            }
            _ => None,
        },
        Some("logout") => match (arguments.get(1), arguments.get(2)) {
                (Some(channel), Some(account)) => Some(Command::Logout {
                    channel: channel.clone(),
                    account: account.clone(),
                }),
                _ => None,
            },
        Some("clear") => arguments.get(1).cloned().map(|channel| Command::Clear { channel }),
        Some("export") => arguments.get(1).cloned().map(|channel| Command::Export {
                channel,
                account: arguments.get(2).cloned(),
            }),
        Some("materialize") => arguments.get(1).cloned().map(|channel| Command::Materialize {
                channel,
                account: arguments.get(2).cloned(),
            }),
        Some("cancel") => {
            arguments.get(1).cloned().map(|session_id| Command::Cancel { session_id })
        }
        Some("sweep") => Some(Command::Sweep),
        _ => None,
    }
}

/// Execute a parsed command against `hub`.
fn run(service: &Service, command: Command) -> vlogin_core::Result<Value> {
    match command {
        Command::Channels => {
            serde_json::to_value(service.site_channels()).map_err(Error::from)
        }
        Command::Status => serde_json::to_value(service.statuses()).map_err(Error::from),
        Command::Accounts { channel } => {
            serde_json::to_value(service.accounts(&channel)?).map_err(Error::from)
        }
        Command::Start { channel, method, params } => {
            let request = vlogin_core::StartRequest { channel, method, params, label: None };
            serde_json::to_value(service.start(&request)?).map_err(Error::from)
        }
        Command::Poll { session_id } => {
            serde_json::to_value(service.poll(&session_id)?).map_err(Error::from)
        }
        Command::Submit { session_id, params } => {
            serde_json::to_value(service.submit(&session_id, &params)?).map_err(Error::from)
        }
        Command::Browse { channel } => {
            serde_json::to_value(service.browse(&channel)?).map_err(Error::from)
        }
        Command::Import { channel, method, label, file } => {
            let cookies = match file.as_deref() {
                Some("-") | None => {
                    let mut buffer = String::new();
                    std::io::stdin().read_to_string(&mut buffer).map_err(|error| {
                        Error::io(format!("cannot read the cookie data from stdin: {error}"))
                    })?;
                    buffer
                }
                // Reading a file named on the command line is this crate's
                // effect, not the core's: the core owns no `std::io::Error`.
                Some(path) => std::fs::read_to_string(path).map_err(|error| {
                    Error::io(format!("cannot read {path}: {error}"))
                })?,
            };
            let account = service.import(&channel, &method, &cookies, &label)?;
            serde_json::to_value(account).map_err(Error::from)
        }
        Command::Check { channel, account } => {
            let view = service.status(&channel, account.as_deref())?;
            serde_json::to_value(view).map_err(Error::from)
        }
        Command::Pin { channel, account } => {
            serde_json::to_value(service.pin(&channel, &account)?).map_err(Error::from)
        }
        Command::Logout { channel, account } => {
            Ok(json!({ "removed": service.logout(&channel, &account)? }))
        }
        Command::Clear { channel } => Ok(json!({ "removed": service.clear(&channel)? })),
        Command::Export { channel, account } => {
            Ok(json!({ "cookies": service.export(&channel, account.as_deref())? }))
        }
        Command::Materialize { channel, account } => {
            Ok(json!({ "path": service.materialize(&channel, account.as_deref())? }))
        }
        Command::Cancel { session_id } => {
            service.cancel(&session_id)?;
            Ok(json!({ "cancelled": true }))
        }
        Command::Sweep => Ok(json!({ "swept": service.sweep()? })),
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = parse(&arguments) else {
        return usage();
    };

    // Opening the service cannot fail: the state root is resolved lazily and a
    // first write creates it. Errors surface per command, where they belong.
    let service = Service::discover();

    match run(&service, command) {
        Ok(value) => emit(json!({ "ok": true, "result": value })),
        Err(error) => fail(&error),
    }
}
