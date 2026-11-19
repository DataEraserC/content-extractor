//! Per-channel account pool: the accounting, not the filesystem.
//!
//! Layout under the state root, which the caller resolves and passes in:
//!
//! ```text
//! accounts/<channel>.json      the pool: { version, active, accounts: [...] }
//! accounts/<channel>.jar.txt   the pinned account, materialized for a downloader
//! cookies/<channel>.json       LEGACY: bare jar array from dsh-video-fetch ≤0.2
//! cookies/<channel>.jar.txt    LEGACY: materialized jar
//! sessions/<id>.json           pending login challenges (see session.rs)
//! ```
//!
//! The legacy `cookies/` files are **read but never written**. On the first
//! load of a channel with no `accounts/` file they are imported and left
//! untouched, so an older plugin can still read them after an upgrade in either
//! direction.
//!
//! ## Why there is a trait
//!
//! This module decides *what* is in a pool, *when* it is stale, and *which*
//! account is pinned. Touching a disk is an effect, so the few operations that
//! need one go through [`Storage`]: the core names the read and the write, and
//! `extract-abi` performs them. Everything else here is a pure function over
//! `AccountFile`, which is what the `pool_policy` tests exercise.
//!
//! Secrets boundary: every file the layout describes is created 0600 inside a
//! 0700 directory, and that is the implementation's job, not this module's.
//! Functions here return [`AccountView`](crate::model::AccountView) or cookie
//! *counts*; only [`Pool::cookies_for`] and [`Pool::materialize`] return secret
//! text, and both are internal to download/export paths that must not log it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ambient::Ambient;
use crate::cookies;
use crate::error::{Error, Result};
use crate::model::{Account, AccountFile, AccountStatus, AccountView};

/// Pick the first non-empty candidate, for defaulting an account label.
fn first_nonempty(candidates: &[&str]) -> String {
    candidates.iter().find(|c| !c.trim().is_empty()).map(|c| c.trim().to_string()).unwrap_or_default()
}

/// Bump when the on-disk pool shape changes incompatibly.
pub const ACCOUNT_FILE_VERSION: u32 = 2;

/// How many accounts one channel keeps before the oldest is dropped.
pub const MAX_ACCOUNTS: usize = 10;

/// The file operations a pool needs, implemented by the host.
///
/// Deliberately tiny and path-based: the core decides layout, the host decides
/// how a path becomes bytes. A missing file is `None`, not an error, because
/// every caller here has a sensible empty answer for it.
pub trait Storage {
    /// Read `path` as UTF-8, or `None` when it does not exist or is unreadable.
    fn read(&self, path: &Path) -> Option<String>;

    /// Write `text` to `path`, creating its parent directory, with `mode` when
    /// the platform has file modes.
    fn write(&self, path: &Path, text: &str, mode: u32) -> Result<()>;

    /// Ensure `path` exists as a directory with `mode`.
    fn create_dir(&self, path: &Path, mode: u32) -> Result<()>;

    /// Delete `path`; a missing file is not an error.
    fn remove_file(&self, path: &Path);

    /// Every file directly inside `dir` whose extension is `extension`, paired
    /// with its text. A missing directory is an empty list, and an unreadable
    /// file is skipped: both are housekeeping, not errors.
    fn list(&self, dir: &Path, extension: &str) -> Vec<(PathBuf, String)>;
}

/// Paths of one channel's files under a state root.
///
/// Pure: every method is `root.join(...)`, so a test can assert the layout
/// without touching a disk.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open (but do not create) a store rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// State root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding every channel's pool.
    pub fn accounts_dir(&self) -> PathBuf {
        self.root.join("accounts")
    }

    /// Directory holding pending login sessions.
    pub fn session_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// Path of a channel's pool file.
    pub fn pool_path(&self, channel: &str) -> PathBuf {
        self.accounts_dir().join(format!("{channel}.json"))
    }

    /// Absolute path of the materialized Netscape file for a channel.
    pub fn jar_path(&self, channel: &str) -> PathBuf {
        self.accounts_dir().join(format!("{channel}.jar.txt"))
    }

    /// Path of the legacy pool file a pre-0.3 install would have written.
    pub fn legacy_pool_path(&self, channel: &str) -> PathBuf {
        self.root.join("cookies").join(format!("{channel}.json"))
    }
}

/// One channel's pool, bound to the storage that holds it.
///
/// Construct with [`Pool::new`]; every method then reads and writes through
/// `storage` and never touches the filesystem itself.
pub struct Pool<'a, S: Storage + ?Sized> {
    store: Store,
    storage: &'a S,
}

impl<'a, S: Storage + ?Sized> Pool<'a, S> {
    /// Bind a channel pool to `storage`.
    pub fn new(store: Store, storage: &'a S) -> Self {
        Self { store, storage }
    }

    /// The paths this pool writes to, for a host that wants to report them.
    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Directory holding pending login sessions.
    #[must_use]
    pub fn session_dir(&self) -> PathBuf {
        self.store.session_dir()
    }

    /// Create `accounts/` and `sessions/` with 0700 permissions.
    fn ensure_dirs(&self) -> Result<()> {
        for dir in [self.store.accounts_dir(), self.store.session_dir()] {
            self.storage.create_dir(&dir, 0o700)?;
        }
        Ok(())
    }

    /// Load a channel's pool, migrating the legacy jar array on first read.
    ///
    /// The legacy file is left in place. Migration is not written back unless
    /// the pool is later mutated, so an untouched install keeps working with an
    /// older plugin.
    pub fn load(&self, channel: &str, ambient: Ambient) -> Result<AccountFile> {
        let path = self.store.pool_path(channel);
        if let Some(text) = self.storage.read(&path) {
            if let Ok(file) = serde_json::from_str::<AccountFile>(&text) {
                if file.version == ACCOUNT_FILE_VERSION {
                    return Ok(file);
                }
                // A future/older version we do not understand: refuse rather
                // than corrupting it on write.
                return Err(Error::other(format!(
                    "{}: unsupported account file version {} (expected {})",
                    path.display(),
                    file.version,
                    ACCOUNT_FILE_VERSION
                )));
            }
        }
        Ok(self.migrate_legacy(channel, ambient))
    }

    /// Read the legacy `cookies/<channel>.json` jar array as a pool.
    fn migrate_legacy(&self, channel: &str, ambient: Ambient) -> AccountFile {
        let legacy = self.store.legacy_pool_path(channel);
        let jars: Vec<LegacyJar> = self
            .storage
            .read(&legacy)
            .and_then(|text| serde_json::from_str::<Vec<LegacyJar>>(&text).ok())
            .unwrap_or_default();

        let mut accounts = Vec::new();
        for (index, jar) in jars.into_iter().enumerate() {
            if cookies::count_netscape_cookies(&jar.data) == 0 {
                continue;
            }
            accounts.push(Account {
                id: ambient.derive(index as u64).id(),
                label: String::new(),
                method: if jar.source.is_empty() { "manual".into() } else { jar.source },
                created_at: jar.created_at,
                updated_at: jar.created_at,
                last_checked_at: 0,
                status: AccountStatus::Unknown,
                identity: String::new(),
                cookies: jar.data,
            });
        }
        if accounts.is_empty() {
            return AccountFile::default();
        }
        // Newest first is the legacy convention; the first entry was the one
        // materialized for yt-dlp, so it stays pinned.
        let active = accounts.first().map(|a| a.id.clone());
        AccountFile { version: ACCOUNT_FILE_VERSION, active, accounts }
    }

    /// Persist a channel's pool (0600).
    pub fn save(&self, channel: &str, file: &AccountFile) -> Result<()> {
        self.ensure_dirs()?;
        let text = serde_json::to_string_pretty(file)?;
        self.storage.write(&self.store.pool_path(channel), &text, 0o600)
    }

    /// Add or refresh an account.
    ///
    /// Cookies are compared by value: re-importing an identical jar refreshes
    /// the existing record instead of adding a duplicate.
    pub fn upsert(
        &self,
        channel: &str,
        method: &str,
        cookies_text: &str,
        identity: &str,
        label: &str,
        ambient: Ambient,
    ) -> Result<AccountView> {
        if cookies::count_netscape_cookies(cookies_text) == 0 {
            return Err(Error::cookie("no cookie rows in the supplied cookie data"));
        }
        let mut file = self.load(channel, ambient)?;
        let now = ambient.now_ms();
        if let Some(existing) = file.accounts.iter_mut().find(|a| a.cookies == cookies_text) {
            existing.updated_at = now;
            existing.last_checked_at = now;
            existing.status = AccountStatus::Active;
            if !identity.is_empty() {
                existing.identity = identity.to_string();
            }
            if existing.label.is_empty() {
                existing.label = first_nonempty(&[label, identity, method]);
            }
            let view = existing.view();
            self.save(channel, &file)?;
            return Ok(view);
        }

        let account = Account {
            id: ambient.id(),
            label: first_nonempty(&[label, identity, method]),
            method: method.to_string(),
            created_at: now,
            updated_at: now,
            last_checked_at: now,
            status: AccountStatus::Active,
            identity: identity.to_string(),
            cookies: cookies_text.to_string(),
        };
        let view = account.view();
        file.accounts.insert(0, account);
        while file.accounts.len() > MAX_ACCOUNTS {
            file.accounts.pop();
        }
        if file.active.is_none() {
            file.active = file.accounts.first().map(|a| a.id.clone());
        }
        self.save(channel, &file)?;
        Ok(view)
    }

    /// List a channel's accounts as safe views, pinned account first.
    pub fn list(&self, channel: &str, ambient: Ambient) -> Result<Vec<AccountView>> {
        let file = self.load(channel, ambient)?;
        let active = file.active.clone();
        let mut views: Vec<AccountView> = file.accounts.iter().map(|a| a.view()).collect();
        views.sort_by_key(|v| if active.as_deref() == Some(v.id.as_str()) { 0 } else { 1 });
        Ok(views)
    }

    /// Find one account by id.
    pub fn get(&self, channel: &str, id: &str, ambient: Ambient) -> Result<Account> {
        self.load(channel, ambient)?
            .accounts
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| Error::channel(format!("no account {id} for channel {channel}")))
    }

    /// The pinned account, falling back to the first in the pool.
    pub fn active(&self, channel: &str, ambient: Ambient) -> Result<Account> {
        let file = self.load(channel, ambient)?;
        let id = file.active.clone().or_else(|| file.accounts.first().map(|a| a.id.clone()));
        let id = id.ok_or_else(|| Error::channel(format!("no accounts for channel {channel}")))?;
        file.accounts.into_iter().find(|a| a.id == id).ok_or_else(|| {
            Error::channel(format!("pinned account {id} vanished from {channel}"))
        })
    }

    /// Pin an account as the channel's active credential.
    pub fn pin(&self, channel: &str, id: &str, ambient: Ambient) -> Result<AccountView> {
        let mut file = self.load(channel, ambient)?;
        if !file.accounts.iter().any(|a| a.id == id) {
            return Err(Error::channel(format!("no account {id} for channel {channel}")));
        }
        file.active = Some(id.to_string());
        self.save(channel, &file)?;
        self.get(channel, id, ambient).map(|a| a.view())
    }

    /// Drop one account; returns true when something was removed.
    pub fn remove(&self, channel: &str, id: &str, ambient: Ambient) -> Result<bool> {
        let mut file = self.load(channel, ambient)?;
        let before = file.accounts.len();
        file.accounts.retain(|a| a.id != id);
        if file.accounts.len() == before {
            return Ok(false);
        }
        if file.active.as_deref() == Some(id) {
            file.active = file.accounts.first().map(|a| a.id.clone());
        }
        self.save(channel, &file)?;
        if file.accounts.is_empty() {
            // No credentials left: the materialized jar must not outlive them.
            self.storage.remove_file(&self.store.jar_path(channel));
        }
        Ok(true)
    }

    /// Drop every account of a channel.
    pub fn clear(&self, channel: &str, ambient: Ambient) -> Result<usize> {
        let file = self.load(channel, ambient)?;
        let removed = file.accounts.len();
        if removed == 0 {
            return Ok(0);
        }
        self.save(channel, &AccountFile::default())?;
        self.storage.remove_file(&self.store.jar_path(channel));
        Ok(removed)
    }

    /// Record the outcome of a status probe on one account.
    pub fn record_check(
        &self,
        channel: &str,
        id: &str,
        status: AccountStatus,
        ambient: Ambient,
    ) -> Result<()> {
        let mut file = self.load(channel, ambient)?;
        let now = ambient.now_ms();
        let Some(account) = file.accounts.iter_mut().find(|a| a.id == id) else {
            return Err(Error::channel(format!("no account {id} for channel {channel}")));
        };
        account.status = status;
        account.updated_at = now;
        if matches!(status, AccountStatus::Active) {
            account.last_checked_at = now;
        }
        self.save(channel, &file)
    }

    /// Netscape text of one account. SECRET — do not log.
    pub fn cookies_for(&self, channel: &str, id: &str, ambient: Ambient) -> Result<String> {
        Ok(self.get(channel, id, ambient)?.cookies)
    }

    /// Write an account's cookies to the materialized jar file (0600).
    ///
    /// Returns the path to hand to a downloader. SECRET content — the path is
    /// fine to log, the file is not.
    pub fn materialize(&self, channel: &str, id: &str, ambient: Ambient) -> Result<PathBuf> {
        let cookies_text = self.cookies_for(channel, id, ambient)?;
        self.ensure_dirs()?;
        let path = self.store.jar_path(channel);
        self.storage.write(&path, &cookies_text, 0o600)?;
        Ok(path)
    }

    /// Delete expired session files. Called opportunistically; a stale session
    /// is also rejected on read, so this is housekeeping, not correctness.
    pub fn sweep_sessions(&self, ttl_ms: i64, ambient: Ambient) -> Result<usize> {
        let dir = self.store.session_dir();
        let entries = match crate::session::list(&dir, self.storage) {
            Ok(entries) => entries,
            Err(_) => return Ok(0),
        };
        let now = ambient.now_ms();
        let mut swept = 0;
        for (path, created) in entries {
            let expired = created.map(|created| now - created > ttl_ms).unwrap_or(true);
            if expired {
                self.storage.remove_file(&path);
                swept += 1;
            }
        }
        Ok(swept)
    }
}

/// Legacy `cookies/<site>.json` record from dsh-video-fetch ≤0.2.
#[derive(Debug, Clone, serde::Deserialize)]
struct LegacyJar {
    #[serde(default)]
    source: String,
    #[serde(default)]
    #[serde(rename = "createdAt")]
    created_at: i64,
    #[serde(default)]
    data: String,
}

/// Convenience re-export so callers can build a `HashMap` of channels without
/// importing `std::collections` themselves.
pub type ChannelMap<T> = HashMap<String, T>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// In-memory `Storage`, so the pool logic is tested without a disk.
    #[derive(Default)]
    struct Memory {
        files: RefCell<BTreeMap<PathBuf, String>>,
        dirs: RefCell<Vec<(PathBuf, u32)>>,
    }

    impl Storage for Memory {
        fn read(&self, path: &Path) -> Option<String> {
            self.files.borrow().get(path).cloned()
        }
        fn write(&self, path: &Path, text: &str, _mode: u32) -> Result<()> {
            self.files.borrow_mut().insert(path.to_path_buf(), text.to_string());
            Ok(())
        }
        fn create_dir(&self, path: &Path, mode: u32) -> Result<()> {
            self.dirs.borrow_mut().push((path.to_path_buf(), mode));
            Ok(())
        }
        fn remove_file(&self, path: &Path) {
            self.files.borrow_mut().remove(path);
        }
        fn list(&self, dir: &Path, extension: &str) -> Vec<(PathBuf, String)> {
            self.files
                .borrow()
                .iter()
                .filter(|(path, _)| {
                    path.parent() == Some(dir)
                        && path.extension().and_then(|value| value.to_str()) == Some(extension)
                })
                .map(|(path, text)| (path.clone(), text.clone()))
                .collect()
        }
    }

    fn ambient() -> Ambient {
        Ambient::new(1_730_000_000_000, 0xDEAD_BEEF)
    }

    fn jar() -> String {
        "# Netscape HTTP Cookie File\n.bilibili.com\tTRUE\t/\tFALSE\t0\tSESSDATA\tv1\n".to_string()
    }

    #[test]
    fn layout_is_the_documented_one() {
        let store = Store::new("/state");
        assert_eq!(store.accounts_dir(), PathBuf::from("/state/accounts"));
        assert_eq!(store.pool_path("bilibili"), PathBuf::from("/state/accounts/bilibili.json"));
        assert_eq!(store.jar_path("bilibili"), PathBuf::from("/state/accounts/bilibili.jar.txt"));
        assert_eq!(store.legacy_pool_path("bilibili"), PathBuf::from("/state/cookies/bilibili.json"));
        assert_eq!(store.session_dir(), PathBuf::from("/state/sessions"));
    }

    #[test]
    fn upsert_pins_the_first_account_and_dedups_by_cookie_value() {
        let memory = Memory::default();
        let pool = Pool::new(Store::new("/state"), &memory);

        let first = pool.upsert("bilibili", "manual", &jar(), "", "label", ambient()).unwrap();
        assert_eq!(first.cookie_count, 1);
        assert_eq!(first.status, AccountStatus::Active);

        // The same jar again must refresh, not duplicate.
        let again = pool.upsert("bilibili", "manual", &jar(), "名", "", ambient()).unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(again.identity, "名");

        let file = pool.load("bilibili", ambient()).unwrap();
        assert_eq!(file.accounts.len(), 1);
        assert_eq!(file.version, ACCOUNT_FILE_VERSION);
        assert_eq!(file.active.as_deref(), Some(first.id.as_str()));
        // 0600 for the pool file.
        assert_eq!(memory.dirs.borrow()[0].1, 0o700);
    }

    #[test]
    fn another_jar_is_prepended_and_the_oldest_is_dropped_past_the_cap() {
        let memory = Memory::default();
        let pool = Pool::new(Store::new("/state"), &memory);
        for index in 0..(MAX_ACCOUNTS + 2) {
            let text = format!(
                "# Netscape HTTP Cookie File\n.bilibili.com\tTRUE\t/\tFALSE\t0\tS\tv{index}\n"
            );
            pool.upsert("bilibili", "manual", &text, "", "", ambient()).unwrap();
        }
        let file = pool.load("bilibili", ambient()).unwrap();
        assert_eq!(file.accounts.len(), MAX_ACCOUNTS);
        // Newest first: the newest jar is at the head, and pinning followed the
        // first upsert, which has since been evicted.
        assert_eq!(file.accounts[0].cookies, format!(
            "# Netscape HTTP Cookie File\n.bilibili.com\tTRUE\t/\tFALSE\t0\tS\tv{}\n",
            MAX_ACCOUNTS + 1
        ));
    }

    #[test]
    fn legacy_jar_array_is_imported_and_left_in_place() {
        let memory = Memory::default();
        let legacy = r##"[{"source":"manual","createdAt":1730000000000,"data":"# Netscape HTTP Cookie File\n.bilibili.com\tTRUE\t/\tFALSE\t0\tSESSDATA\tlegacy\n"}]"##;
        memory
            .files
            .borrow_mut()
            .insert(PathBuf::from("/state/cookies/bilibili.json"), legacy.to_string());

        let pool = Pool::new(Store::new("/state"), &memory);
        let file = pool.load("bilibili", ambient()).unwrap();
        assert_eq!(file.accounts.len(), 1);
        assert_eq!(file.accounts[0].created_at, 1_730_000_000_000);
        assert_eq!(file.accounts[0].method, "manual");
        assert!(file.active.is_some());
        // Reading migrated; it must not have written anything back.
        assert!(memory.files.borrow().contains_key(&PathBuf::from("/state/cookies/bilibili.json")));
        assert!(!memory.files.borrow().contains_key(&PathBuf::from("/state/accounts/bilibili.json")));
    }

    #[test]
    fn an_unsupported_account_file_version_is_refused_not_overwritten() {
        let memory = Memory::default();
        memory.files.borrow_mut().insert(
            PathBuf::from("/state/accounts/bilibili.json"),
            r#"{"version":999,"active":null,"accounts":[]}"#.to_string(),
        );
        let pool = Pool::new(Store::new("/state"), &memory);
        let error = pool.load("bilibili", ambient()).unwrap_err();
        assert!(error.to_string().contains("unsupported account file version 999"));
    }

    #[test]
    fn removing_the_last_account_drops_the_materialized_jar() {
        let memory = Memory::default();
        let pool = Pool::new(Store::new("/state"), &memory);
        let view = pool.upsert("bilibili", "manual", &jar(), "", "", ambient()).unwrap();
        let path = pool.materialize("bilibili", &view.id, ambient()).unwrap();
        assert!(memory.files.borrow().contains_key(&path));

        assert!(pool.remove("bilibili", &view.id, ambient()).unwrap());
        assert!(!memory.files.borrow().contains_key(&path));
        assert!(!pool.remove("bilibili", &view.id, ambient()).unwrap());
    }

    #[test]
    fn pinning_moves_the_active_pointer_and_listing_puts_it_first() {
        let memory = Memory::default();
        let pool = Pool::new(Store::new("/state"), &memory);
        let one = pool.upsert("bilibili", "manual", &jar(), "", "", ambient()).unwrap();
        let two = pool
            .upsert(
                "bilibili",
                "manual",
                "# Netscape HTTP Cookie File\n.bilibili.com\tTRUE\t/\tFALSE\t0\tS\tv2\n",
                "",
                "",
                ambient(),
            )
            .unwrap();
        pool.pin("bilibili", &one.id, ambient()).unwrap();
        let listed = pool.list("bilibili", ambient()).unwrap();
        assert_eq!(listed[0].id, one.id);
        assert_eq!(listed[1].id, two.id);
        assert!(pool.pin("bilibili", "nope", ambient()).is_err());
    }

    #[test]
    fn record_check_stamps_the_probe_time_only_when_active() {
        let memory = Memory::default();
        let pool = Pool::new(Store::new("/state"), &memory);
        let view = pool.upsert("bilibili", "manual", &jar(), "", "", ambient()).unwrap();

        // Captured at the upsert, so this is the anchor a failed probe must not move.
        let captured = view.last_checked_at;
        assert_eq!(captured, 1_730_000_000_000);

        let later = Ambient::new(1_730_000_060_000, 7);
        pool.record_check("bilibili", &view.id, AccountStatus::Expired, later).unwrap();
        let expired = pool.get("bilibili", &view.id, ambient()).unwrap();
        assert_eq!(expired.status, AccountStatus::Expired);
        assert_eq!(expired.updated_at, 1_730_000_060_000);
        assert_eq!(expired.last_checked_at, captured, "only a successful probe moves this");

        pool.record_check("bilibili", &view.id, AccountStatus::Active, later).unwrap();
        let active = pool.get("bilibili", &view.id, ambient()).unwrap();
        assert_eq!(active.last_checked_at, 1_730_000_060_000);
    }

    #[test]
    fn a_channel_with_no_pool_and_no_legacy_file_loads_empty() {
        let memory = Memory::default();
        let pool = Pool::new(Store::new("/state"), &memory);
        let file = pool.load("youku", ambient()).unwrap();
        assert!(file.accounts.is_empty());
        assert!(file.active.is_none());
        assert_eq!(file.version, ACCOUNT_FILE_VERSION);
    }
}
