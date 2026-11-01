//! Short-lived memory cache of upstream feed bytes.
//!
//! Realtime bodies default to 20 seconds and the static zip defaults to 6 hours.
//! Errors are not cached. One in-flight download is shared by concurrent
//! callers so a burst of GTFS clients does not fan out into NJ Transit.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use njt_client::{ApiKind, Clock, Error, Feed};
use tokio::sync::Mutex;

/// Key for one API's feed body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    /// Rail or bus.
    api: ApiKind,
    /// Which file.
    feed: Feed,
}

/// One successful download.
///
/// The body is a zip or protobuf that already passed classification.
struct Entry {
    /// Zip or protobuf bytes.
    body: Bytes,
    /// When the body was stored, according to the cache clock.
    fetched_at: DateTime<Utc>,
}

/// In-memory TTL cache.
///
/// # Examples
///
/// ```
/// use njt_client::SystemClock;
/// use njt_gtfs_proxy::FeedCache;
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// let cache = FeedCache::new(Duration::from_secs(20), Duration::from_secs(60), Arc::new(SystemClock));
/// assert_eq!(cache.realtime_ttl(), Duration::from_secs(20));
/// ```
#[derive(Clone)]
pub struct FeedCache {
    /// Shared maps and TTLs.
    inner: Arc<Inner>,
}

/// Shared cache maps and TTLs.
struct Inner {
    /// TTL for trip updates, vehicle positions, and alerts.
    rt_ttl: Duration,
    /// TTL for the static zip.
    static_ttl: Duration,
    /// Clock used to decide whether an entry is still fresh.
    clock: Arc<dyn Clock>,
    /// Stored bodies.
    entries: Mutex<HashMap<CacheKey, Entry>>,
    /// One gate per key so concurrent misses share a download.
    gates: Mutex<HashMap<CacheKey, Arc<Mutex<()>>>>,
}

impl FeedCache {
    /// Creates an empty cache.
    ///
    /// # Arguments
    ///
    /// * `rt_ttl` — Realtime TTL. Zero disables caching for those feeds.
    /// * `static_ttl` — Static zip TTL. Zero disables caching for the zip.
    /// * `clock` — Clock that must match the token manager when tests move time.
    ///
    /// # Returns
    ///
    /// An empty cache.
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
    /// use njt_client::SystemClock;
    /// use njt_gtfs_proxy::FeedCache;
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// let _cache = FeedCache::new(Duration::from_secs(1), Duration::ZERO, Arc::new(SystemClock));
    /// ```
    pub fn new(rt_ttl: Duration, static_ttl: Duration, clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Arc::new(Inner {
                rt_ttl,
                static_ttl,
                clock,
                entries: Mutex::new(HashMap::new()),
                gates: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Returns the realtime TTL.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The TTL passed to [`FeedCache::new`] for realtime feeds.
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
    /// use njt_client::SystemClock;
    /// use njt_gtfs_proxy::FeedCache;
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// let cache = FeedCache::new(Duration::from_secs(15), Duration::from_secs(1), Arc::new(SystemClock));
    /// assert_eq!(cache.realtime_ttl().as_secs(), 15);
    /// ```
    pub fn realtime_ttl(&self) -> Duration {
        self.inner.rt_ttl
    }

    /// Returns the static-zip TTL.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The TTL passed to [`FeedCache::new`] for [`njt_client::Feed::Static`].
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
    /// use njt_client::SystemClock;
    /// use njt_gtfs_proxy::FeedCache;
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// let cache = FeedCache::new(Duration::from_secs(1), Duration::from_secs(60), Arc::new(SystemClock));
    /// assert_eq!(cache.static_ttl().as_secs(), 60);
    /// ```
    pub fn static_ttl(&self) -> Duration {
        self.inner.static_ttl
    }

    /// Returns a fresh body or calls `fetch` once for concurrent waiters.
    ///
    /// # Arguments
    ///
    /// * `api` — Rail or bus.
    /// * `feed` — Which file to cache.
    /// * `fetch` — Upstream download. It runs only on a miss, and its error is
    ///   not stored.
    ///
    /// # Returns
    ///
    /// The body and `true` when it came from the cache.
    ///
    /// # Errors
    ///
    /// Returns the error from `fetch`.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::{ApiKind, Feed, SystemClock};
    /// use njt_gtfs_proxy::FeedCache;
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// # async fn demo() {
    /// let cache = FeedCache::new(Duration::from_secs(30), Duration::from_secs(30), Arc::new(SystemClock));
    /// let (body, hit) = cache
    ///     .get_or_fetch(ApiKind::Rail, Feed::Alerts, || async { Ok(bytes::Bytes::from_static(b"\x0a")) })
    ///     .await
    ///     .expect("fetch");
    /// assert!(!hit);
    /// assert_eq!(body.as_ref(), b"\x0a");
    /// # }
    /// ```
    pub async fn get_or_fetch<F, Fut>(
        &self,
        api: ApiKind,
        feed: Feed,
        fetch: F,
    ) -> Result<(Bytes, bool), Error>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Bytes, Error>>,
    {
        let ttl = self.ttl_for(feed);
        if ttl.is_zero() {
            return Ok((fetch().await?, false));
        }
        let gate = self.gate(CacheKey { api, feed }).await;
        let _hold = gate.lock().await;
        if let Some(hit) = self.lookup(CacheKey { api, feed }, ttl).await {
            return Ok((hit, true));
        }
        let body = fetch().await?;
        self.insert(CacheKey { api, feed }, body.clone()).await;
        Ok((body, false))
    }

    /// Returns the TTL for `feed`.
    ///
    /// # Arguments
    ///
    /// * `feed` — Static or realtime.
    ///
    /// # Returns
    ///
    /// The configured TTL.
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
    /// The TTL split is covered by [`FeedCache::realtime_ttl`]:
    ///
    /// ```
    /// use njt_client::SystemClock;
    /// use njt_gtfs_proxy::FeedCache;
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// let cache = FeedCache::new(Duration::from_secs(20), Duration::from_secs(5), Arc::new(SystemClock));
    /// assert_eq!(cache.realtime_ttl(), Duration::from_secs(20));
    /// ```
    fn ttl_for(&self, feed: Feed) -> Duration {
        match feed {
            Feed::Static => self.inner.static_ttl,
            Feed::TripUpdates | Feed::VehiclePositions | Feed::Alerts => self.inner.rt_ttl,
        }
    }

    /// Returns the single-flight gate for `key`, creating it if needed.
    ///
    /// # Arguments
    ///
    /// * `key` — API and feed.
    ///
    /// # Returns
    ///
    /// A mutex shared by every caller of this key.
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
    /// Gates are private. [`FeedCache::new`] is the public constructor:
    ///
    /// ```
    /// use njt_client::SystemClock;
    /// use njt_gtfs_proxy::FeedCache;
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// let _cache = FeedCache::new(Duration::from_secs(1), Duration::from_secs(1), Arc::new(SystemClock));
    /// ```
    async fn gate(&self, key: CacheKey) -> Arc<Mutex<()>> {
        let mut gates = self.inner.gates.lock().await;
        gates
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Returns a stored body that is still inside its TTL.
    ///
    /// # Arguments
    ///
    /// * `key` — API and feed.
    /// * `ttl` — Freshness window.
    ///
    /// # Returns
    ///
    /// The bytes, or `None` on a miss or expiry.
    ///
    /// # Errors
    ///
    /// This method does not return an error.
    ///
    /// # Panics
    ///
    /// This method does not panic. A TTL chrono cannot represent is a miss.
    ///
    /// # Examples
    ///
    /// Lookup is private. A zero TTL never stores a body:
    ///
    /// ```
    /// use njt_client::{ApiKind, Feed, SystemClock};
    /// use njt_gtfs_proxy::FeedCache;
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// # async fn demo() {
    /// let cache = FeedCache::new(Duration::ZERO, Duration::ZERO, Arc::new(SystemClock));
    /// let (_body, hit) = cache
    ///     .get_or_fetch(ApiKind::Bus, Feed::Static, || async {
    ///         Ok(bytes::Bytes::from_static(b"PK\x03\x04"))
    ///     })
    ///     .await
    ///     .expect("fetch");
    /// assert!(!hit);
    /// # }
    /// ```
    async fn lookup(&self, key: CacheKey, ttl: Duration) -> Option<Bytes> {
        let now = self.inner.clock.now();
        let entries = self.inner.entries.lock().await;
        let entry = entries.get(&key)?;
        if is_fresh(entry, now, ttl) {
            Some(entry.body.clone())
        } else {
            None
        }
    }

    /// Stores `body` at the current clock time.
    ///
    /// # Arguments
    ///
    /// * `key` — API and feed.
    /// * `body` — Successful upstream payload.
    ///
    /// # Returns
    ///
    /// Nothing.
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
    /// Insert is private and runs after a successful fetch in
    /// [`FeedCache::get_or_fetch`].
    ///
    /// ```
    /// use njt_client::SystemClock;
    /// use njt_gtfs_proxy::FeedCache;
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// let _cache = FeedCache::new(Duration::from_secs(20), Duration::from_secs(20), Arc::new(SystemClock));
    /// ```
    async fn insert(&self, key: CacheKey, body: Bytes) {
        let fetched_at = self.inner.clock.now();
        self.inner
            .entries
            .lock()
            .await
            .insert(key, Entry { body, fetched_at });
    }
}

/// Reports whether `entry` is still inside `ttl` at `now`.
///
/// # Arguments
///
/// * `entry` — Stored body.
/// * `now` — Current instant.
/// * `ttl` — Freshness window.
///
/// # Returns
///
/// `true` when `now` is strictly before `fetched_at + ttl`.
///
/// # Errors
///
/// This function does not return an error.
///
/// # Panics
///
/// This function does not panic. Overflowing timestamps count as expired.
///
/// # Examples
///
/// Freshness is private. [`FeedCache::realtime_ttl`] shows the default window
/// the proxy configures:
///
/// ```
/// use njt_client::SystemClock;
/// use njt_gtfs_proxy::FeedCache;
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// assert_eq!(
///     FeedCache::new(Duration::from_secs(20), Duration::from_secs(1), Arc::new(SystemClock))
///         .realtime_ttl(),
///     Duration::from_secs(20)
/// );
/// ```
fn is_fresh(entry: &Entry, now: DateTime<Utc>, ttl: Duration) -> bool {
    let Ok(step) = ChronoDuration::from_std(ttl) else {
        return false;
    };
    match entry.fetched_at.checked_add_signed(step) {
        Some(expiry) => now < expiry,
        None => false,
    }
}
