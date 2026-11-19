//! The core stays a protocol layer.
//!
//! `extract-core` owns wire formats: platform descriptors, HTTP request and
//! response shapes, the Netscape cookie codec, the DevTools conversation, the
//! account-pool layout. It owns no machine-local decision. Reading a file,
//! writing one, resolving a state root out of the environment, spawning a
//! process, binding a listener, reading a clock, drawing entropy, printing to a
//! terminal — every one of those belongs to `extract-abi` (effects behind the
//! frozen C ABI) or `extract-cli` (a human at a terminal). The core takes its
//! inputs as `Ambient` and `&impl Storage` and returns data.
//!
//! This test scans every `.rs` file of the crate and fails when one of those
//! primitives shows up, so the boundary cannot erode silently — the convenient
//! `std::env::var` that "just defaults the state root" is exactly the change it
//! exists to catch.
//!
//! Comments are stripped first: the crate's prose legitimately *names* these
//! primitives while explaining why it does not use them, and a doc comment is
//! history, not a dependency. The stripper is the string-aware one from
//! `bas-core`/`oray-tools`, which matters here because the core carries full
//! URLs (`https://…`) and a naive scan would cut lines at the `//` inside one.

use std::fs;
use std::path::{Path, PathBuf};

/// Primitives that must never appear in `extract-core` code.
///
/// Written as fragments so this file itself does not trip the scan. `std::io`
/// is listed whole rather than as `stdin`/`stdout` separately because every
/// terminal access in Rust reaches through it, and `File::`/`fs::` catch the
/// compact `use std::fs as fs` evasion. `SystemTime`/`Instant` are the two ways
/// to read a clock, and `thread_rng`/`getrandom` the two ways to draw entropy —
/// both are injected instead ([`extract_core::Ambient`]).
const FORBIDDEN: &[&str] = &[
    // Filesystem
    "std::fs",
    "File::open",
    "File::create",
    "fs::",
    // Environment
    "std::env",
    "env::var",
    "var_os",
    // Processes and the network surface the core must not own
    "std::process",
    "Command::new",
    "std::net",
    "TcpListener",
    "TcpStream",
    "bind(",
    // Terminal
    "std::io",
    "read_line",
    "println!",
    "print!",
    "eprintln!",
    "eprint!",
    // Ambient state: a clock or an entropy source
    "SystemTime",
    "Instant",
    "thread_rng",
    "getrandom",
    "rand::",
    // Telling a host where to put its state is an effect, and naming a harness
    // is what `harness_independence.rs` already forbids.
    "default_root",
    "EXTRACT_HOME",
    "XDG_DATA_HOME",
];

/// Tokens allowed in specific files, with the reason they are needed there.
///
/// Empty on purpose: nothing in a protocol layer needs a filesystem, an
/// environment variable, a process, a socket, a terminal, a clock or an entropy
/// source. If an entry is ever added here it is a design change, not a
/// formality, and the reason should say which crate should own the effect
/// instead.
const ALLOWED: &[(&str, &str)] = &[];

/// Drop `//` line comments, ignoring `//` that lives inside a string literal
/// (the core carries full URLs, e.g. `https://…`).
fn strip_comments(line: &str) -> String {
    let mut in_string = false;
    let mut escaped = false;
    let chars: Vec<char> = line.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        } else if c == '/' && chars.get(index + 1) == Some(&'/') {
            return chars[..index].iter().collect();
        }
        index += 1;
    }
    line.to_string()
}

fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sources(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn core_has_no_effects() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_sources(&root, &mut files);
    assert!(
        !files.is_empty(),
        "no sources found under {}",
        root.display()
    );

    let mut violations = Vec::new();
    for file in &files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        let relative = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string();
        // `#[cfg(test)]` modules may print and use the filesystem: they are
        // not part of the library that ships.
        let mut in_tests = false;
        for (index, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("#[cfg(test)]") {
                in_tests = true;
            }
            if in_tests {
                continue;
            }
            let code = strip_comments(line);
            for token in FORBIDDEN {
                if !code.contains(token) {
                    continue;
                }
                if ALLOWED
                    .iter()
                    .any(|(path, allowed)| relative == *path && allowed == token)
                {
                    continue;
                }
                violations.push(format!("{relative}:{}: `{token}`", index + 1));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "extract-core must stay a protocol layer: no filesystem, environment, \
         process, socket, terminal, clock or entropy access. Take these as \
         arguments instead — bytes through `Storage`, time and entropy through \
         `Ambient` — and move the effect to extract-abi or extract-cli:\n  {}",
        violations.join("\n  ")
    );
}

/// The scan must be able to see a violation at all.
///
/// A scanner with a broken stripper (cutting at the `//` in `https://…`, say)
/// or a list of tokens nothing could ever contain would pass forever and prove
/// nothing. This pins the two properties the real scan depends on, so the
/// demonstration in the task report is not the only evidence the test can fail.
#[test]
fn the_scan_recognises_a_violation() {
    // A plain forbidden token is caught.
    assert!(FORBIDDEN.iter().any(|token| "let root = std::env::var_os(\"HOME\");".contains(token)));
    // A URL survives comment stripping, so scanning a line that carries one the
    // core legitimately keeps cannot hide a token after it.
    let line = r#"let url = "https://example.com/x"; let path = std::fs::read_to_string(p);"#;
    let stripped = strip_comments(line);
    assert!(stripped.contains("https://example.com/x"), "the URL must survive intact");
    assert!(stripped.contains("std::fs"), "a token after a URL must still be seen");

    // A real comment is dropped, so prose naming a primitive is not a failure.
    assert!(!strip_comments("// std::fs is an effect").contains("std::fs"));

    // And the scan finds this crate's sources.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_sources(&root, &mut files);
    assert!(files.iter().any(|file| file.ends_with("store.rs")));
}
