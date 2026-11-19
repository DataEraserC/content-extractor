//! Every platform crate stays a protocol layer too.
//!
//! The kernel's purity rule (`extractor-core/tests/purity.rs`) applies with the
//! same force to each `platform-*` crate: a site's login conversation and its
//! content descriptors are wire formats, not machine-local decisions. A
//! convenient `std::env::var` inside a provider is exactly the erosion this
//! catches.
//!
//! The scan walks every sibling `platform-*` directory from this crate's
//! manifest, so adding a platform crate is covered automatically — no list to
//! forget to update.

use std::fs;
use std::path::{Path, PathBuf};

/// Primitives that must never appear in platform code. Mirrors the kernel's
/// list; written as fragments so this file does not trip its own scan.
const FORBIDDEN: &[&str] = &[
    "std::fs",
    "File::open",
    "File::create",
    "fs::",
    "std::env",
    "env::var",
    "var_os",
    "std::process",
    "Command::new",
    "std::net",
    "TcpListener",
    "TcpStream",
    "bind(",
    "std::io",
    "read_line",
    "println!",
    "print!",
    "eprintln!",
    "eprint!",
    "SystemTime",
    "Instant",
    "thread_rng",
    "getrandom",
    "rand::",
    "default_root",
    "EXTRACTOR_HOME",
    "XDG_DATA_HOME",
];

/// Drop `//` line comments, ignoring `//` inside a string literal (providers
/// carry full URLs).
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

/// The workspace's `crates/` directory, derived from this crate's manifest.
fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("extractor-core lives under crates/")
        .to_path_buf()
}

#[test]
fn every_platform_crate_is_effect_free() {
    let root = crates_dir();
    let mut platform_dirs = Vec::new();
    for entry in fs::read_dir(&root).expect("crates dir readable").flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("platform-") && entry.path().join("src").is_dir() {
            platform_dirs.push(entry.path().join("src"));
        }
    }
    assert!(
        platform_dirs.len() >= 9,
        "expected every linked platform crate to be scanned, found {}",
        platform_dirs.len()
    );

    let mut violations = Vec::new();
    let mut files_seen = 0;
    for dir in &platform_dirs {
        let mut files = Vec::new();
        collect_sources(dir, &mut files);
        for file in &files {
            files_seen += 1;
            let Ok(text) = fs::read_to_string(file) else {
                continue;
            };
            let relative = file.strip_prefix(&root).unwrap_or(file).display().to_string();
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
                    if code.contains(token) {
                        violations.push(format!("{relative}:{}: `{token}`", index + 1));
                    }
                }
            }
        }
    }
    assert!(files_seen > 0, "the scan must actually see platform sources");
    assert!(
        violations.is_empty(),
        "platform crates must stay protocol layers: no filesystem, environment, \
         process, socket, terminal, clock or entropy access. Take these as \
         arguments (Storage / Ambient) and move the effect to extractor-abi:\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn every_platform_crate_names_no_harness() {
    const HARNESS_TOKENS: &[&str] = &["DSH_HOME", "dsh-", "dsh_", "deepseek", "cordis"];
    let root = crates_dir();
    let mut violations = Vec::new();
    for entry in fs::read_dir(&root).expect("crates dir readable").flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("platform-") {
            continue;
        }
        let mut files = Vec::new();
        collect_sources(&entry.path().join("src"), &mut files);
        for file in files {
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            let relative = file.strip_prefix(&root).unwrap_or(&file).display().to_string();
            let mut in_tests = false;
            for (index, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("#[cfg(test)]") {
                    in_tests = true;
                }
                if in_tests {
                    continue;
                }
                let code = strip_comments(line);
                for token in HARNESS_TOKENS {
                    if code.contains(token) {
                        violations.push(format!("{relative}:{}: `{token}`", index + 1));
                    }
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "a platform crate is reached through a C ABI and used by a CLI as well as \
         by hosts; it must name no harness:\n  {}",
        violations.join("\n  ")
    );
}
