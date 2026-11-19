//! Filesystem and environment effects, the half `extractor-core` refuses to have.
//!
//! Two things live here:
//!
//! * [`FsStorage`] — the concrete [`Storage`] behind the core's account pool and
//!   session files. The core decides *what* and *where*; this performs the
//!   write and sets the mode, which is why the 0600/0700 guarantee is enforced
//!   on this side of the boundary.
//! * [`resolve_root`] — turning `EXTRACTOR_HOME` / `XDG_DATA_HOME` / `HOME` into a
//!   directory. That is a machine-local decision, so it is here and not in the
//!   core, which is the whole point of the split.
//!
//! The on-disk layout itself is unchanged and is documented in
//! `extractor_core::store`: `accounts/<channel>.json`, `accounts/<channel>.jar.txt`,
//! the read-only legacy `cookies/` import, and `sessions/<id>.json`.

use std::fs;
use std::path::{Path, PathBuf};

use extractor_core::{Error, Result, Storage};

/// The real filesystem, with the permission bits the layout promises.
#[derive(Debug, Default, Clone, Copy)]
pub struct FsStorage;

impl Storage for FsStorage {
    fn read(&self, path: &Path) -> Option<String> {
        fs::read_to_string(path).ok()
    }

    fn write(&self, path: &Path, text: &str, mode: u32) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::io(format!("cannot create {}: {error}", parent.display()))
            })?;
        }
        fs::write(path, text)
            .map_err(|error| Error::io(format!("cannot write {}: {error}", path.display())))?;
        set_mode(path, mode)?;
        Ok(())
    }

    fn create_dir(&self, path: &Path, mode: u32) -> Result<()> {
        fs::create_dir_all(path)
            .map_err(|error| Error::io(format!("cannot create {}: {error}", path.display())))?;
        set_mode(path, mode)
    }

    fn remove_file(&self, path: &Path) {
        let _ = fs::remove_file(path);
    }

    fn list(&self, dir: &Path, extension: &str) -> Vec<(PathBuf, String)> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some(extension) {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path) {
                out.push((path, text));
            }
        }
        out
    }
}

/// Apply a unix mode, and do nothing on platforms that have none.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| Error::io(format!("cannot set mode on {}: {error}", path.display())))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

/// Resolve the state root from the environment.
///
/// Precedence: `$EXTRACTOR_HOME` → `$VLOGIN_HOME` (legacy) →
/// `$XDG_DATA_HOME/content-extractor` → `~/.local/share/content-extractor`,
/// falling back to the legacy `video-login` directory when it already exists
/// so an upgrade does not orphan stored credentials.
///
/// The library is harness-independent and stays that way, so it knows nothing
/// about any harness's own home variable. A harness that keeps its state
/// somewhere else exports that into `EXTRACTOR_HOME` before the first call; that
/// is what the host adapter does, and it is why this preference lives in the
/// adapter's side of the boundary rather than in the protocol layer.
#[must_use]
pub fn resolve_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("EXTRACTOR_HOME") {
        return PathBuf::from(dir);
    }
    // Legacy override, still honoured: an existing install sets this.
    if let Some(dir) = std::env::var_os("VLOGIN_HOME") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")));
    match base {
        Some(base) => {
            let legacy = base.join("video-login");
            let current = base.join("content-extractor");
            // Keep using the old directory when it holds state and the new one
            // does not exist yet; otherwise prefer the new name.
            if legacy.is_dir() && !current.exists() {
                legacy
            } else {
                current
            }
        }
        None => PathBuf::from(".content-extractor"),
    }
}

/// Everything one operation needs from the machine: where state lives, the
/// clock, and entropy.
///
/// One value is built per call, so two tokens minted by one `start` are
/// independent while nothing has to persist between calls.
#[derive(Debug, Clone)]
pub struct Machine {
    root: PathBuf,
    ambient: extractor_core::Ambient,
}

impl Machine {
    /// Discover the state root, the clock and a seed from the environment.
    #[must_use]
    pub fn discover() -> Self {
        Self { root: resolve_root(), ambient: fresh_ambient() }
    }

    /// State root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// This operation's clock and entropy.
    #[must_use]
    pub fn ambient(&self) -> extractor_core::Ambient {
        self.ambient
    }

    /// The filesystem every core call writes through.
    #[must_use]
    pub fn storage(&self) -> FsStorage {
        FsStorage
    }
}

/// One operation's clock and pseudo-entropy.
///
/// Entropy here is a hash of the wall clock, the process id and a counter. That
/// is deliberately the same quality the crate used before the split — it seeds
/// device fingerprints and opaque session ids, not secrets — and keeping it
/// identical means the split changes no behaviour. `OsRng` would be the upgrade
/// if a real secret ever needed minting, and it belongs on this side of the
/// boundary either way.
fn fresh_ambient() -> extractor_core::Ambient {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0x243F_6A88_85A3_08D3);

    let now_ms =
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let pid = u64::from(std::process::id());
    let tick = COUNTER.fetch_add(1, Ordering::Relaxed);
    let seed = extractor_core::util::mix(
        (now_ms as u64)
            ^ pid.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ tick.wrapping_mul(0xBF58_476D_1CE4_E5B9),
    );
    extractor_core::Ambient::new(now_ms, seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_sets_0600_and_creating_a_dir_sets_0700() {
        let root = std::env::temp_dir().join(format!(
            "extractor-fsstorage-{}-{}",
            std::process::id(),
            fresh_ambient().seed()
        ));
        let storage = FsStorage;
        let dir = root.join("accounts");
        storage.create_dir(&dir, 0o700).unwrap();
        let file = dir.join("bilibili.json");
        storage.write(&file, "{}", 0o600).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }

        assert_eq!(storage.read(&file).as_deref(), Some("{}"));
        assert_eq!(storage.read(&root.join("missing")).as_deref(), None);
        assert_eq!(storage.list(&dir, "json").len(), 1);
        assert_eq!(storage.list(&dir, "txt").len(), 0);

        storage.remove_file(&file);
        assert!(!file.exists());
        // A missing file is not an error.
        storage.remove_file(&file);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ambient_values_differ_between_calls_and_carry_a_wall_clock() {
        let first = fresh_ambient();
        let second = fresh_ambient();
        assert!(first.now_ms() > 1_700_000_000_000, "sanity: the clock is this century");
        assert_ne!(first.seed(), second.seed(), "a counter keeps two calls distinct");
    }
}
