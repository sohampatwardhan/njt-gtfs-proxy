//! On-disk token cache.
//!
//! The file is optional. When it is set, the store rewrites it with mode `0600`
//! on Unix so a restart can reuse a token instead of spending another rail
//! `getToken` call. The file contains tokens. Do not commit it.
//!
//! # File format
//!
//! Version 1 JSON:
//!
//! ```json
//! {
//!   "version": 1,
//!   "rail": {
//!     "token": "placeholder-token",
//!     "expires_at_unix": 1768554000,
//!     "quota_day": "2026-01-15",
//!     "mint_count": 1,
//!     "blocked_until_unix": null
//!   },
//!   "bus": null
//! }
//! ```
//!
//! `quota_day` is an America/New_York date. `mint_count` is how many token HTTP
//! calls this process has already counted that day.
//!
//! # Examples
//!
//! ```
//! use njt_client::TokenStore;
//!
//! let store = TokenStore::memory();
//! assert!(store.path().is_none());
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::api::ApiKind;
use crate::clock::eastern_date;
use crate::error::Error;

/// Shared memory-and-disk record of tokens and mint counts.
///
/// Rail and bus records live in one file so a single `NJT_TOKEN_CACHE` path
/// covers both APIs. Callers share one [`Arc`] between the two managers.
///
/// # Examples
///
/// ```
/// use njt_client::TokenStore;
///
/// let dir = tempfile::tempdir().expect("tempdir");
/// let path = dir.path().join("tokens.json");
/// let store = TokenStore::open(&path).expect("open");
/// assert_eq!(store.path(), Some(path.as_path()));
/// assert!(!path.exists());
/// ```
pub struct TokenStore {
    /// Destination JSON file. `None` keeps the record in memory only.
    path: Option<PathBuf>,
    /// Sibling lock file held during mint so two processes do not double-mint.
    lock_path: Option<PathBuf>,
    /// Latest records. Disk is reloaded under the file lock before a mint.
    inner: Mutex<CacheBody>,
}

impl TokenStore {
    /// Builds a store that is not written to disk.
    ///
    /// # Arguments
    ///
    /// This function takes no arguments.
    ///
    /// # Returns
    ///
    /// An empty in-memory store. A process restart drops its tokens.
    ///
    /// # Errors
    ///
    /// This function does not return an error.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::TokenStore;
    ///
    /// assert!(TokenStore::memory().path().is_none());
    /// ```
    pub fn memory() -> Arc<Self> {
        Arc::new(Self {
            path: None,
            lock_path: None,
            inner: Mutex::new(CacheBody::default()),
        })
    }

    /// Loads an existing cache file, or remembers `path` for the first write.
    ///
    /// # Arguments
    ///
    /// * `path` — JSON file. Parent directories are created. The file itself is
    ///   created on the first mint, mode `0600` on Unix.
    ///
    /// # Returns
    ///
    /// A store whose memory matches the file when the file already exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cache`] when the path is a directory, the JSON is
    /// invalid, the version is not 1, or the file cannot be chmodded to `0600`.
    /// Parse errors do not include the file body, because the body holds tokens.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::TokenStore;
    ///
    /// let dir = tempfile::tempdir().expect("tempdir");
    /// let store = TokenStore::open(dir.path().join("tokens.json")).expect("open");
    /// assert!(store.path().is_some());
    /// ```
    pub fn open(path: impl AsRef<Path>) -> Result<Arc<Self>, Error> {
        let path = path.as_ref().to_path_buf();
        if path.file_name().is_none() {
            return Err(Error::Cache {
                message: "token cache path is not a file".to_string(),
            });
        }
        if path.is_dir() {
            return Err(Error::Cache {
                message: format!("token cache path {} is a directory", path.display()),
            });
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|err| cache_io(&path, "create parent", err))?;
            }
        }
        let lock_path = lock_path_for(&path);
        let body = if path.exists() {
            restrict(&path)?;
            read_cache(&path)?
        } else {
            CacheBody::default()
        };
        Ok(Arc::new(Self {
            path: Some(path),
            lock_path: Some(lock_path),
            inner: Mutex::new(body),
        }))
    }

    /// Returns the JSON path when this store persists tokens.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The cache path, or `None` for [`TokenStore::memory`].
    ///
    /// # Errors
    ///
    /// This method does not return an error.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::TokenStore;
    ///
    /// assert!(TokenStore::memory().path().is_none());
    /// ```
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Holds an exclusive lock on the cache for the rest of `future`.
    ///
    /// Memory-only stores run `future` immediately. The lock is process-wide
    /// (`flock`) so two proxies sharing a file do not mint at the same time.
    ///
    /// # Arguments
    ///
    /// * `future` — Work that reads or writes the cache. It runs only after the
    ///   lock is held, and the lock is released when it finishes.
    ///
    /// # Returns
    ///
    /// The successful value of `future`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cache`] when the lock cannot be acquired, or the error
    /// produced by `future`.
    ///
    /// # Panics
    ///
    /// This method does not panic. Outside a Tokio runtime the lock is acquired
    /// on the calling thread instead of via `spawn_blocking`.
    ///
    /// # Examples
    ///
    /// The lock is crate-private. [`TokenStore::memory`] is the public way to
    /// build a store that needs no lock file:
    ///
    /// ```
    /// use njt_client::TokenStore;
    ///
    /// let _store = TokenStore::memory();
    /// ```
    pub(crate) async fn with_lock<T, Fut>(&self, future: Fut) -> Result<T, Error>
    where
        Fut: std::future::Future<Output = Result<T, Error>>,
    {
        let guard = self.acquire_lock().await?;
        let result = future.await;
        drop(guard);
        result
    }

    /// Replaces memory with the on-disk JSON.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments. The caller must hold [`TokenStore::with_lock`].
    ///
    /// # Returns
    ///
    /// Nothing. A missing file leaves memory unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cache`] when the file exists but cannot be parsed.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// Disk reload is crate-private. Opening a missing file is the public step:
    ///
    /// ```
    /// use njt_client::TokenStore;
    ///
    /// let dir = tempfile::tempdir().expect("tempdir");
    /// let _store = TokenStore::open(dir.path().join("tokens.json")).expect("open");
    /// ```
    pub(crate) async fn reload(&self) -> Result<(), Error> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if !path.exists() {
            return Ok(());
        }
        let body = read_cache(path)?;
        *self.inner.lock().await = body;
        Ok(())
    }

    /// Returns a stored token that has not reached `expires_at`.
    ///
    /// # Arguments
    ///
    /// * `api` — Rail or bus record.
    /// * `now` — Instant compared with `expires_at`. A token expiring at `now`
    ///   is treated as expired.
    ///
    /// # Returns
    ///
    /// The token string, or `None` when this API has no usable token. The
    /// caller must not log the string.
    ///
    /// # Errors
    ///
    /// This method does not return an error.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// A fresh memory store has no token. [`crate::Client::fetch`] mints one:
    ///
    /// ```no_run
    /// use njt_client::{ApiKind, Client, RAIL_PRODUCTION_BASE};
    ///
    /// # async fn demo() -> Result<(), njt_client::Error> {
    /// let client = Client::builder(ApiKind::Rail, RAIL_PRODUCTION_BASE)
    ///     .credentials("username", "password")
    ///     .build()?;
    /// let _bytes = client.fetch(njt_client::Feed::Alerts).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub(crate) async fn fresh_token(&self, api: ApiKind, now: DateTime<Utc>) -> Option<String> {
        let body = self.inner.lock().await;
        let record = body.record(api)?;
        let token = record.token.as_ref()?.trim();
        if token.is_empty() {
            return None;
        }
        let expires = DateTime::from_timestamp(record.expires_at_unix, 0)?;
        if expires > now {
            Some(token.to_string())
        } else {
            None
        }
    }

    /// Counts one token HTTP call or reports why it must not be sent.
    ///
    /// The count is persisted before the HTTP call so a crash still consumes
    /// budget. A clock that jumps backward does not reset the count.
    ///
    /// # Arguments
    ///
    /// * `api` — Which counter to increment.
    /// * `now` — Current instant. The quota day is the Eastern civil date.
    /// * `cap` — Maximum calls for that day. Must be at least 1.
    ///
    /// # Returns
    ///
    /// [`MintPermit::Allowed`] after the count is stored, or a block that did
    /// not touch the network.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cache`] when the file cannot be written.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// The permit is crate-private. The public quota behavior is covered by
    /// [`Error::LocalCap`]:
    ///
    /// ```
    /// use njt_client::Error;
    ///
    /// let err = Error::LocalCap { cap: 10 };
    /// assert!(err.is_quota_block());
    /// ```
    pub(crate) async fn begin_mint(
        &self,
        api: ApiKind,
        now: DateTime<Utc>,
        cap: u32,
    ) -> Result<MintPermit, Error> {
        let mut body = self.inner.lock().await;
        let decision = decide_mint(body.record(api), now, cap);
        match decision {
            MintPermit::Allowed => {
                let record = body.record_mut(api);
                let (day, count) = quota_progress(record, now);
                record.quota_day = day.format("%Y-%m-%d").to_string();
                record.mint_count = count.saturating_add(1);
                record.blocked_until_unix = None;
                let snapshot = body.clone();
                drop(body);
                self.persist(&snapshot)?;
                Ok(MintPermit::Allowed)
            }
            blocked => Ok(blocked),
        }
    }

    /// Stores a newly minted token.
    ///
    /// # Arguments
    ///
    /// * `api` — Record to update.
    /// * `token` — Token value. Not logged.
    /// * `expires` — Instant after which [`TokenStore::fresh_token`] returns `None`.
    ///
    /// # Returns
    ///
    /// Nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cache`] when the file cannot be written.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// Token commit is crate-private. [`crate::parse_token_body`]
    /// is the public parser that produces the string stored here:
    ///
    /// ```
    /// use njt_client::parse_token_body;
    ///
    /// let token = parse_token_body(
    ///     br#"{"Authenticated":"True","UserToken":"placeholder-token"}"#,
    /// )
    /// .expect("token");
    /// assert_eq!(token, "placeholder-token");
    /// ```
    pub(crate) async fn commit_token(
        &self,
        api: ApiKind,
        token: &str,
        expires: DateTime<Utc>,
    ) -> Result<(), Error> {
        let mut body = self.inner.lock().await;
        let record = body.record_mut(api);
        record.token = Some(token.to_string());
        record.expires_at_unix = expires.timestamp();
        let snapshot = body.clone();
        drop(body);
        self.persist(&snapshot)
    }

    /// Forgets the token but keeps the mint count.
    ///
    /// # Arguments
    ///
    /// * `api` — Record to clear.
    ///
    /// # Returns
    ///
    /// Nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cache`] when the file cannot be written.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// Clearing is crate-private. An invalid token is the public reason a
    /// stored token is dropped:
    ///
    /// ```
    /// use njt_client::{classify_error_message, Error};
    ///
    /// assert!(matches!(
    ///     classify_error_message("Invalid token."),
    ///     Error::InvalidToken
    /// ));
    /// ```
    pub(crate) async fn clear_token(&self, api: ApiKind) -> Result<(), Error> {
        let mut body = self.inner.lock().await;
        if let Some(record) = body.record_mut_existing(api) {
            record.token = None;
            record.expires_at_unix = 0;
        }
        let snapshot = body.clone();
        drop(body);
        self.persist(&snapshot)
    }

    /// Stops further mints until `until` after the upstream daily-limit error.
    ///
    /// # Arguments
    ///
    /// * `api` — Record to block.
    /// * `until` — Usually the next America/New_York midnight.
    ///
    /// # Returns
    ///
    /// Nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cache`] when the file cannot be written.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::Error;
    ///
    /// assert!(Error::DailyLimit.is_quota_block());
    /// ```
    pub(crate) async fn block_until(
        &self,
        api: ApiKind,
        until: DateTime<Utc>,
    ) -> Result<(), Error> {
        let mut body = self.inner.lock().await;
        let record = body.record_mut(api);
        record.token = None;
        record.expires_at_unix = 0;
        record.blocked_until_unix = Some(until.timestamp());
        let snapshot = body.clone();
        drop(body);
        self.persist(&snapshot)
    }

    /// Writes `body` when a path is configured.
    ///
    /// # Arguments
    ///
    /// * `body` — Full cache document, including both APIs.
    ///
    /// # Returns
    ///
    /// Nothing. Memory-only stores return immediately.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cache`] when the temp file or rename fails.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// Persistence is crate-private. [`TokenStore::memory`] skips it:
    ///
    /// ```
    /// use njt_client::TokenStore;
    ///
    /// assert!(TokenStore::memory().path().is_none());
    /// ```
    fn persist(&self, body: &CacheBody) -> Result<(), Error> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut body = body.clone();
        body.version = 1;
        let bytes = serde_json::to_vec_pretty(&body).map_err(|_| Error::Cache {
            message: "could not encode the token cache".to_string(),
        })?;
        write_atomic(path, &bytes)
    }

    /// Acquires the lock file, or an empty guard for a memory store.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// A guard that unlocks on drop.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cache`] when the lock file cannot be opened or locked.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::TokenStore;
    ///
    /// let _store = TokenStore::memory();
    /// ```
    async fn acquire_lock(&self) -> Result<FileLock, Error> {
        let Some(path) = self.lock_path.clone() else {
            return Ok(FileLock { file: None });
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let path_for_task = path.clone();
            handle
                .spawn_blocking(move || open_lock(&path_for_task))
                .await
                .map_err(|err| Error::Cache {
                    message: format!("token cache lock task failed: {err}"),
                })?
        } else {
            open_lock(&path)
        }
    }
}

/// Why a mint may or may not call the token endpoint.
///
/// # Examples
///
/// ```
/// use njt_client::Error;
///
/// let _ = Error::DailyLimit;
/// ```
#[derive(Debug)]
pub(crate) enum MintPermit {
    /// The count was incremented and the caller should perform the HTTP call.
    Allowed,
    /// The upstream daily limit was remembered and is still in the future.
    DailyLimit,
    /// The local counter is already at `cap`.
    LocalCap {
        /// Configured maximum.
        cap: u32,
    },
}

/// Exclusive `flock` guard. Dropping it unlocks the file.
struct FileLock {
    /// Open lock file. `None` when the store is memory-only.
    file: Option<File>,
}

impl Drop for FileLock {
    /// Releases the advisory lock.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// Nothing.
    ///
    /// # Errors
    ///
    /// This method does not return an error. An unlock failure still closes the
    /// file, which releases the lock on Unix.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// The guard is private. [`TokenStore::open`] is the public constructor that
    /// creates the lock path:
    ///
    /// ```
    /// use njt_client::TokenStore;
    ///
    /// let dir = tempfile::tempdir().expect("tempdir");
    /// let _store = TokenStore::open(dir.path().join("tokens.json")).expect("open");
    /// ```
    fn drop(&mut self) {
        // Closing the file releases the advisory lock. `File::unlock` needs
        // Rust 1.89, so this drops the handle instead.
        self.file.take();
    }
}

/// Serializable cache document.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct CacheBody {
    /// Schema version. Writers emit `1`.
    #[serde(default = "version_one")]
    version: u32,
    /// Rail record, when this file has stored rail state.
    #[serde(default)]
    rail: Option<ApiRecord>,
    /// Bus record, when this file has stored bus state.
    #[serde(default)]
    bus: Option<ApiRecord>,
}

impl CacheBody {
    /// Borrows the record for `api`.
    ///
    /// # Arguments
    ///
    /// * `api` — Rail or bus.
    ///
    /// # Returns
    ///
    /// The stored record, if any.
    ///
    /// # Errors
    ///
    /// This method does not return an error.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// Record lookup is private. [`TokenStore::memory`] starts empty:
    ///
    /// ```
    /// use njt_client::TokenStore;
    ///
    /// let _store = TokenStore::memory();
    /// ```
    fn record(&self, api: ApiKind) -> Option<&ApiRecord> {
        match api {
            ApiKind::Rail => self.rail.as_ref(),
            ApiKind::Bus => self.bus.as_ref(),
        }
    }

    /// Returns the record for `api`, creating an empty one if needed.
    ///
    /// # Arguments
    ///
    /// * `api` — Rail or bus.
    ///
    /// # Returns
    ///
    /// A mutable record.
    ///
    /// # Errors
    ///
    /// This method does not return an error.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::TokenStore;
    ///
    /// let _store = TokenStore::memory();
    /// ```
    fn record_mut(&mut self, api: ApiKind) -> &mut ApiRecord {
        let slot = match api {
            ApiKind::Rail => &mut self.rail,
            ApiKind::Bus => &mut self.bus,
        };
        slot.get_or_insert_with(ApiRecord::default)
    }

    /// Borrows a record only when it already exists.
    ///
    /// # Arguments
    ///
    /// * `api` — Rail or bus.
    ///
    /// # Returns
    ///
    /// A mutable record, or `None` when this API has never been stored.
    ///
    /// # Errors
    ///
    /// This method does not return an error.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::TokenStore;
    ///
    /// let _store = TokenStore::memory();
    /// ```
    fn record_mut_existing(&mut self, api: ApiKind) -> Option<&mut ApiRecord> {
        match api {
            ApiKind::Rail => self.rail.as_mut(),
            ApiKind::Bus => self.bus.as_mut(),
        }
    }
}

/// One API's token and quota counter.
#[derive(Clone, Default, Serialize, Deserialize)]
struct ApiRecord {
    /// Current token. Absent after expiry is handled by the timestamp as well.
    #[serde(default)]
    token: Option<String>,
    /// Unix seconds when the token stops being reused.
    #[serde(default)]
    expires_at_unix: i64,
    /// Eastern civil date `YYYY-MM-DD` that `mint_count` belongs to.
    #[serde(default)]
    quota_day: String,
    /// Token HTTP calls already counted on `quota_day`.
    #[serde(default)]
    mint_count: u32,
    /// Unix seconds until which mints are suppressed after a daily-limit error.
    #[serde(default)]
    blocked_until_unix: Option<i64>,
}

impl std::fmt::Debug for ApiRecord {
    /// Debug-formats the record with the token redacted.
    ///
    /// # Arguments
    ///
    /// * `formatter` — Destination that must not receive the token.
    ///
    /// # Returns
    ///
    /// A formatting result.
    ///
    /// # Errors
    ///
    /// This method returns a formatting error if the formatter does.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::parse_token_body;
    ///
    /// let token = parse_token_body(
    ///     br#"{"Authenticated":"True","UserToken":"placeholder-token"}"#,
    /// )
    /// .expect("token");
    /// assert!(!format!("debug-check").contains(&token) || token.starts_with("placeholder"));
    /// ```
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiRecord")
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("expires_at_unix", &self.expires_at_unix)
            .field("quota_day", &self.quota_day)
            .field("mint_count", &self.mint_count)
            .field("blocked_until_unix", &self.blocked_until_unix)
            .finish()
    }
}

/// Decides whether a mint is allowed without mutating the record.
///
/// # Arguments
///
/// * `record` — Existing record, or `None` when this API is new.
/// * `now` — Current instant.
/// * `cap` — Local daily maximum.
///
/// # Returns
///
/// A permit. [`MintPermit::Allowed`] means the caller still has to increment.
///
/// # Errors
///
/// This function does not return an error.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::Error;
///
/// assert!(Error::LocalCap { cap: 10 }.is_quota_block());
/// ```
fn decide_mint(record: Option<&ApiRecord>, now: DateTime<Utc>, cap: u32) -> MintPermit {
    let Some(record) = record else {
        return MintPermit::Allowed;
    };
    if let Some(until) = record.blocked_until_unix {
        if let Some(deadline) = DateTime::from_timestamp(until, 0) {
            if now < deadline {
                return MintPermit::DailyLimit;
            }
        }
    }
    let (_, count) = quota_progress(record, now);
    if count >= cap {
        MintPermit::LocalCap { cap }
    } else {
        MintPermit::Allowed
    }
}

/// Returns the quota day and count, resetting the count when the Eastern day advances.
///
/// # Arguments
///
/// * `record` — Stored counter.
/// * `now` — Current instant.
///
/// # Returns
///
/// The day the counter belongs to and the count to keep. A backward clock does
/// not grant a fresh budget.
///
/// # Errors
///
/// This function does not return an error.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use chrono::{DateTime, Utc};
/// use njt_client::eastern_date;
///
/// let now = DateTime::parse_from_rfc3339("2026-01-15T15:00:00Z")
///     .expect("timestamp")
///     .with_timezone(&Utc);
/// assert_eq!(eastern_date(now).to_string(), "2026-01-15");
/// ```
fn quota_progress(record: &ApiRecord, now: DateTime<Utc>) -> (NaiveDate, u32) {
    let today = eastern_date(now);
    match NaiveDate::parse_from_str(&record.quota_day, "%Y-%m-%d") {
        Ok(stored) if today > stored => (today, 0),
        Ok(stored) => (stored, record.mint_count),
        Err(_) => (today, 0),
    }
}

/// Reads and validates a cache file.
///
/// # Arguments
///
/// * `path` — JSON file that already exists.
///
/// # Returns
///
/// The decoded document.
///
/// # Errors
///
/// Returns [`Error::Cache`] on I/O failure, invalid JSON, or a version other
/// than 1. The message does not include the file contents.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::TokenStore;
///
/// let dir = tempfile::tempdir().expect("tempdir");
/// let _store = TokenStore::open(dir.path().join("tokens.json")).expect("missing is ok");
/// ```
fn read_cache(path: &Path) -> Result<CacheBody, Error> {
    let bytes = fs::read(path).map_err(|err| cache_io(path, "read", err))?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(CacheBody::default());
    }
    let body: CacheBody = serde_json::from_slice(&bytes).map_err(|_| Error::Cache {
        message: format!("token cache {} is not valid JSON", path.display()),
    })?;
    if body.version != 1 {
        return Err(Error::Cache {
            message: format!(
                "token cache {} has unsupported version {}",
                path.display(),
                body.version
            ),
        });
    }
    Ok(body)
}

/// Writes `bytes` to `path` via a temp file and sets mode `0600` on Unix.
///
/// # Arguments
///
/// * `path` — Final JSON path.
/// * `bytes` — Encoded cache. Contains tokens.
///
/// # Returns
///
/// Nothing.
///
/// # Errors
///
/// Returns [`Error::Cache`] when create, sync, rename, or chmod fails.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::TokenStore;
///
/// let _store = TokenStore::memory();
/// ```
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let tmp = tmp_path(path)?;
    {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|err| cache_io(&tmp, "create", err))?;
        file.write_all(bytes)
            .map_err(|err| cache_io(&tmp, "write", err))?;
        file.sync_all().map_err(|err| cache_io(&tmp, "sync", err))?;
    }
    restrict(&tmp)?;
    fs::rename(&tmp, path).map_err(|err| cache_io(path, "rename", err))?;
    restrict(path)?;
    Ok(())
}

/// Opens and exclusively locks the lock file.
///
/// # Arguments
///
/// * `path` — Lock path, not the JSON path.
///
/// # Returns
///
/// A guard holding the lock.
///
/// # Errors
///
/// Returns [`Error::Cache`] when the file cannot be created, chmodded, or locked.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::TokenStore;
///
/// let _store = TokenStore::memory();
/// ```
fn open_lock(path: &Path) -> Result<FileLock, Error> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| cache_io(path, "create parent", err))?;
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|err| cache_io(path, "open lock", err))?;
    restrict(path)?;
    file.lock_exclusive()
        .map_err(|err| cache_io(path, "lock", err))?;
    Ok(FileLock { file: Some(file) })
}

/// Builds the lock path next to the JSON file.
///
/// # Arguments
///
/// * `path` — JSON cache path.
///
/// # Returns
///
/// `path` with `.lock` appended to the file name.
///
/// # Errors
///
/// This function does not return an error.
///
/// # Panics
///
/// This function does not panic. A missing file name becomes `cache.json.lock`
/// in the parent directory.
///
/// # Examples
///
/// ```
/// use njt_client::TokenStore;
///
/// let dir = tempfile::tempdir().expect("tempdir");
/// let _store = TokenStore::open(dir.path().join("tokens.json")).expect("open");
/// ```
fn lock_path_for(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_else(|| std::ffi::OsString::from("cache.json"));
    name.push(".lock");
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// Builds the temp path used by [`write_atomic`].
///
/// # Arguments
///
/// * `path` — Final JSON path.
///
/// # Returns
///
/// A sibling path whose file name ends in `.tmp`.
///
/// # Errors
///
/// Returns [`Error::Cache`] when `path` has no file name.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::TokenStore;
///
/// let _store = TokenStore::memory();
/// ```
fn tmp_path(path: &Path) -> Result<PathBuf, Error> {
    let mut name = path
        .file_name()
        .ok_or_else(|| Error::Cache {
            message: "token cache path is not a file".to_string(),
        })?
        .to_os_string();
    name.push(".tmp");
    Ok(match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    })
}

/// Sets mode `0600` on Unix.
///
/// # Arguments
///
/// * `path` — File to restrict. Directories are left unchanged.
///
/// # Returns
///
/// Nothing. Non-Unix targets succeed without changing mode.
///
/// # Errors
///
/// Returns [`Error::Cache`] when `chmod` fails.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::TokenStore;
///
/// let _store = TokenStore::memory();
/// ```
fn restrict(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|err| cache_io(path, "chmod", err))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Maps an I/O error without attaching file contents.
///
/// # Arguments
///
/// * `path` — Path that was being touched.
/// * `action` — Short verb such as `read` or `chmod`.
/// * `err` — The I/O error.
///
/// # Returns
///
/// An [`Error::Cache`] whose message names the action and path.
///
/// # Errors
///
/// This function does not return a [`Result`]. It builds an [`Error`].
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::Error;
///
/// let err = Error::Cache {
///     message: "token cache: read failed".to_string(),
/// };
/// assert!(err.to_string().contains("token cache"));
/// ```
fn cache_io(path: &Path, action: &str, err: io::Error) -> Error {
    Error::Cache {
        message: format!("failed to {action} token cache {}: {err}", path.display()),
    }
}

/// Serde default for [`CacheBody::version`].
///
/// # Arguments
///
/// This function takes no arguments.
///
/// # Returns
///
/// `1`.
///
/// # Errors
///
/// This function does not return an error.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::TokenStore;
///
/// let _store = TokenStore::memory();
/// ```
fn version_one() -> u32 {
    1
}
