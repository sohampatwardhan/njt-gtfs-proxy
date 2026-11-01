//! Quota-aware token manager.
//!
//! Tokens are minted lazily, kept in memory, and optionally written by
//! [`crate::TokenStore`]. A mint runs under a single-flight lock. The manager
//! calls the token endpoint again only when the cached token is expired or a
//! data call rejected it. Each attempt counts toward a local daily cap so a
//! retry bug cannot exceed NJ Transit's published rail limit of 10 token calls
//! per America/New_York day. `isValidToken` is never called.
//!
//! # Examples
//!
//! ```no_run
//! use njt_client::{ApiKind, TokenManager, RAIL_PRODUCTION_BASE};
//!
//! # async fn demo() -> Result<(), njt_client::Error> {
//! let manager = TokenManager::builder(ApiKind::Rail, RAIL_PRODUCTION_BASE)
//!     .credentials("username", "password")
//!     .build()?;
//! let _token = manager.current().await?;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use tokio::sync::Mutex;

use crate::api::{endpoint_url, ApiKind};
use crate::clock::{next_midnight_eastern, Clock, SystemClock};
use crate::error::Error;
use crate::payload::parse_token_body;
use crate::secret::{constant_time_eq, Secret};
use crate::store::{MintPermit, TokenStore};

/// Default and maximum token HTTP calls per Eastern day, per API.
///
/// Rail publishes a limit of 10 for `getToken` and `isValidToken` combined.
/// Bus does not publish a number. Both APIs use this cap so a bug stops at 10.
pub const DAILY_MINT_CAP: u32 = 10;

/// Knobs for mint lifetime, cap, and cooldown.
///
/// The proxy uses [`TokenPolicy::default`]. Tests may lower the cap or the bus
/// lifetime. The cap cannot be raised above [`DAILY_MINT_CAP`].
///
/// # Examples
///
/// ```
/// use njt_client::TokenPolicy;
/// use std::time::Duration;
///
/// let policy = TokenPolicy::default();
/// assert_eq!(policy.daily_cap, 10);
/// assert_eq!(policy.bus_token_ttl, Duration::from_secs(24 * 60 * 60));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenPolicy {
    /// Maximum token HTTP calls for one API during one Eastern day.
    ///
    /// Values outside `1..=10` are rejected by [`TokenManagerBuilder::build`].
    pub daily_cap: u32,
    /// First cooldown after a failed mint. Later failures double it.
    pub backoff_initial: Duration,
    /// Upper bound on the cooldown.
    pub backoff_max: Duration,
    /// How long a bus token is reused before it is treated as expired.
    ///
    /// The bus docs say a token typically lasts 24 hours and that the period
    /// can change. An invalid-token response still refreshes earlier.
    pub bus_token_ttl: Duration,
}

impl Default for TokenPolicy {
    /// Returns the production policy: cap 10, 1 second backoff growing to 5 minutes,
    /// bus tokens reused for 24 hours.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// A [`TokenPolicy`] the proxy can use unchanged.
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
    /// use njt_client::TokenPolicy;
    ///
    /// let policy = TokenPolicy::default();
    /// assert_eq!(policy.daily_cap, 10);
    /// ```
    fn default() -> Self {
        Self {
            daily_cap: DAILY_MINT_CAP,
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_secs(5 * 60),
            bus_token_ttl: Duration::from_secs(24 * 60 * 60),
        }
    }
}

/// Mints and caches one API's token.
///
/// Cloning is cheap. Clones share the store, the flight lock, and the cooldown.
///
/// # Examples
///
/// ```
/// use njt_client::{ApiKind, TokenManager, RAIL_TEST_BASE};
///
/// let manager = TokenManager::builder(ApiKind::Rail, RAIL_TEST_BASE)
///     .credentials("username", "password")
///     .build()
///     .expect("builder");
/// assert_eq!(manager.api(), ApiKind::Rail);
/// ```
#[derive(Clone)]
pub struct TokenManager {
    /// Shared state.
    inner: Arc<TokenInner>,
}

/// Shared token-manager state.
///
/// Clones of [`TokenManager`] point at one `TokenInner`.
struct TokenInner {
    /// Rail or bus. Selects the token URL and the expiry rule.
    api: ApiKind,
    /// Normalized origin, without a path.
    base: String,
    /// Portal username. Never logged.
    username: Secret,
    /// Portal password. Never logged.
    password: Secret,
    /// HTTP client used for the token POST.
    http: reqwest::Client,
    /// Memory and optional disk cache shared with the other API.
    store: Arc<TokenStore>,
    /// Cap, backoff, and bus lifetime.
    policy: TokenPolicy,
    /// Clock that defines "now" for expiry and the quota day.
    clock: Arc<dyn Clock>,
    /// Serializes mints for this API inside the process.
    flight: Mutex<()>,
    /// In-memory cooldown. Not persisted, so a corrected password can be tried
    /// after a restart, still subject to the persisted cap.
    backoff: Mutex<Backoff>,
}

/// Cooldown state after failed mints.
struct Backoff {
    /// When the next mint is allowed. `None` means immediately.
    until: Option<DateTime<Utc>>,
    /// How many failures have been folded into the delay.
    failures: u32,
}

impl TokenManager {
    /// Starts a builder for `api` at `base`.
    ///
    /// # Arguments
    ///
    /// * `api` — Rail or bus. Use a separate manager for each.
    /// * `base` — Origin such as [`crate::RAIL_PRODUCTION_BASE`].
    ///
    /// # Returns
    ///
    /// A builder with [`TokenPolicy::default`], a memory store, and
    /// [`SystemClock`].
    ///
    /// # Errors
    ///
    /// This function does not return an error. [`TokenManagerBuilder::build`]
    /// reports a bad base URL or missing credentials.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::{ApiKind, TokenManager, BUS_PRODUCTION_BASE};
    ///
    /// let manager = TokenManager::builder(ApiKind::Bus, BUS_PRODUCTION_BASE)
    ///     .credentials("username", "password")
    ///     .build()
    ///     .expect("builder");
    /// assert_eq!(manager.api(), ApiKind::Bus);
    /// ```
    pub fn builder(api: ApiKind, base: impl Into<String>) -> TokenManagerBuilder {
        TokenManagerBuilder {
            api,
            base: base.into(),
            username: None,
            password: None,
            store: None,
            policy: TokenPolicy::default(),
            clock: None,
            http: None,
        }
    }

    /// Returns which API this manager mints for.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The [`ApiKind`] passed to [`TokenManager::builder`].
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
    /// use njt_client::{ApiKind, TokenManager, RAIL_PRODUCTION_BASE};
    ///
    /// let manager = TokenManager::builder(ApiKind::Rail, RAIL_PRODUCTION_BASE)
    ///     .credentials("username", "password")
    ///     .build()
    ///     .expect("builder");
    /// assert_eq!(manager.api(), ApiKind::Rail);
    /// ```
    pub fn api(&self) -> ApiKind {
        self.inner.api
    }

    /// Returns the normalized origin.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The base URL, without credentials.
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
    /// use njt_client::{ApiKind, TokenManager, RAIL_PRODUCTION_BASE};
    ///
    /// let manager = TokenManager::builder(ApiKind::Rail, RAIL_PRODUCTION_BASE)
    ///     .credentials("username", "password")
    ///     .build()
    ///     .expect("builder");
    /// assert_eq!(manager.base_url(), RAIL_PRODUCTION_BASE);
    /// ```
    pub fn base_url(&self) -> &str {
        &self.inner.base
    }

    /// Clones the HTTP client used for token posts.
    ///
    /// Data downloads use the same client so timeouts and the redirect policy
    /// stay aligned. [`reqwest::Client`] clones share one connection pool.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// A clone of the manager's HTTP client.
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
    /// use njt_client::{ApiKind, TokenManager, RAIL_TEST_BASE};
    ///
    /// let manager = TokenManager::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .build()
    ///     .expect("builder");
    /// let _http = manager.http_client();
    /// ```
    pub fn http_client(&self) -> reqwest::Client {
        self.inner.http.clone()
    }

    /// Returns a cached token, minting one when the cache is empty or expired.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The current token. Do not log [`Secret::expose`].
    ///
    /// # Errors
    ///
    /// * [`Error::DailyLimit`] — upstream quota is exhausted until Eastern midnight.
    /// * [`Error::LocalCap`] — this process already counted `daily_cap` mints today.
    /// * [`Error::Backoff`] — the previous failure is still cooling down.
    /// * [`Error::AuthRejected`] — the portal rejected the username or password.
    /// * [`Error::NullBody`] — the token endpoint returned `Null`.
    /// * [`Error::Transport`] — the POST did not complete.
    /// * [`Error::Cache`] — the cache file could not be updated.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use njt_client::{ApiKind, TokenManager, RAIL_PRODUCTION_BASE};
    ///
    /// # async fn demo() -> Result<(), njt_client::Error> {
    /// let manager = TokenManager::builder(ApiKind::Rail, RAIL_PRODUCTION_BASE)
    ///     .credentials("username", "password")
    ///     .build()?;
    /// let token = manager.current().await?;
    /// let _ = token.expose();
    /// # Ok(())
    /// # }
    /// ```
    pub async fn current(&self) -> Result<Secret, Error> {
        let inner = Arc::clone(&self.inner);
        let store = Arc::clone(&inner.store);
        let worker = Arc::clone(&inner);
        let _flight = inner.flight.lock().await;
        store.with_lock(load_or_mint(worker, None)).await
    }

    /// Mints a replacement when `rejected` is still the cached token.
    ///
    /// If another caller already minted a different token, that token is
    /// returned and no extra HTTP call is made.
    ///
    /// # Arguments
    ///
    /// * `rejected` — Token the data call refused. Not logged.
    ///
    /// # Returns
    ///
    /// A token different from `rejected`, when minting succeeds.
    ///
    /// # Errors
    ///
    /// Same as [`TokenManager::current`].
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use njt_client::{ApiKind, TokenManager, RAIL_PRODUCTION_BASE};
    ///
    /// # async fn demo() -> Result<(), njt_client::Error> {
    /// let manager = TokenManager::builder(ApiKind::Rail, RAIL_PRODUCTION_BASE)
    ///     .credentials("username", "password")
    ///     .build()?;
    /// let replacement = manager.refresh_replacing("placeholder-token").await?;
    /// let _ = replacement.expose();
    /// # Ok(())
    /// # }
    /// ```
    pub async fn refresh_replacing(&self, rejected: &str) -> Result<Secret, Error> {
        let inner = Arc::clone(&self.inner);
        let store = Arc::clone(&inner.store);
        let worker = Arc::clone(&inner);
        let rejected = rejected.to_string();
        let _flight = inner.flight.lock().await;
        store.with_lock(load_or_mint(worker, Some(rejected))).await
    }

    /// Performs one token POST after the cap and cooldown checks.
    ///
    /// The caller holds the flight lock and the file lock.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The new token, already stored.
    ///
    /// # Errors
    ///
    /// See [`TokenManager::current`].
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use njt_client::{ApiKind, TokenManager, RAIL_PRODUCTION_BASE};
    ///
    /// # async fn demo() -> Result<(), njt_client::Error> {
    /// let manager = TokenManager::builder(ApiKind::Rail, RAIL_PRODUCTION_BASE)
    ///     .credentials("username", "password")
    ///     .build()?;
    /// let _token = manager.current().await?;
    /// # Ok(())
    /// # }
    /// ```
    async fn mint(inner: &TokenInner) -> Result<Secret, Error> {
        let now = inner.clock.now();
        if let Some(retry_after) = backoff_remaining(inner, now).await {
            return Err(Error::Backoff { retry_after });
        }
        match inner
            .store
            .begin_mint(inner.api, now, inner.policy.daily_cap)
            .await?
        {
            MintPermit::Allowed => {}
            MintPermit::DailyLimit => return Err(Error::DailyLimit),
            MintPermit::LocalCap { cap } => return Err(Error::LocalCap { cap }),
        }
        match request_token(inner).await {
            Ok(token) => {
                let expires = expiry_after(inner, now)?;
                inner.store.commit_token(inner.api, &token, expires).await?;
                clear_backoff(inner).await;
                tracing::info!(api = %inner.api, "minted a token");
                Ok(Secret::new(token))
            }
            Err(Error::DailyLimit) => {
                let until = match next_midnight_eastern(now) {
                    Ok(until) => until,
                    Err(_) => now
                        .checked_add_signed(ChronoDuration::hours(24))
                        .unwrap_or(now),
                };
                inner.store.block_until(inner.api, until).await?;
                tracing::warn!(api = %inner.api, "token endpoint reported the daily limit");
                Err(Error::DailyLimit)
            }
            Err(err) => {
                note_failure(inner, now).await;
                tracing::warn!(api = %inner.api, error = %err, "token request failed");
                Err(err)
            }
        }
    }
}

/// Reloads the cache and returns a fresh token, minting when needed.
///
/// # Arguments
///
/// * `inner` — Manager state. The caller holds the flight lock.
/// * `rejected` — When set, a cached token equal to this value is discarded.
///
/// # Returns
///
/// A usable token wrapped so debug output stays redacted.
///
/// # Errors
///
/// See [`TokenManager::current`].
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::{classify_error_message, Error};
///
/// assert!(matches!(
///     classify_error_message("Invalid token."),
///     Error::InvalidToken
/// ));
/// ```
async fn load_or_mint(inner: Arc<TokenInner>, rejected: Option<String>) -> Result<Secret, Error> {
    inner.store.reload().await?;
    let now = inner.clock.now();
    if let Some(token) = inner.store.fresh_token(inner.api, now).await {
        let matches_rejected = rejected
            .as_deref()
            .is_some_and(|rejected| constant_time_eq(token.as_bytes(), rejected.as_bytes()));
        if !matches_rejected {
            return Ok(Secret::new(token));
        }
        inner.store.clear_token(inner.api).await?;
    }
    TokenManager::mint(&inner).await
}

/// Returns the remaining cooldown, if any.
///
/// # Arguments
///
/// * `inner` — Manager state.
/// * `now` — Current instant.
///
/// # Returns
///
/// `Some` when a mint must not be sent yet.
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
/// use std::time::Duration;
///
/// let err = Error::Backoff {
///     retry_after: Duration::from_secs(1),
/// };
/// assert!(err.is_quota_block());
/// ```
async fn backoff_remaining(inner: &TokenInner, now: DateTime<Utc>) -> Option<Duration> {
    let backoff = inner.backoff.lock().await;
    let until = backoff.until?;
    let millis = until
        .timestamp_millis()
        .saturating_sub(now.timestamp_millis());
    if millis <= 0 {
        None
    } else {
        Some(Duration::from_millis(millis as u64))
    }
}

/// Records a failed mint and arms the cooldown.
///
/// # Arguments
///
/// * `now` — Current instant.
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
/// This method does not panic. A delay chrono cannot add is skipped.
///
/// # Examples
///
/// ```
/// use njt_client::TokenPolicy;
///
/// assert!(TokenPolicy::default().backoff_initial.as_secs() >= 1);
/// ```
async fn note_failure(inner: &TokenInner, now: DateTime<Utc>) {
    let mut backoff = inner.backoff.lock().await;
    let shift = backoff.failures.min(16);
    let multiplier = 2u32.saturating_pow(shift);
    let mut delay = inner.policy.backoff_initial.saturating_mul(multiplier);
    if delay > inner.policy.backoff_max {
        delay = inner.policy.backoff_max;
    }
    if delay.is_zero() {
        delay = Duration::from_millis(1);
    }
    backoff.failures = backoff.failures.saturating_add(1);
    if let Ok(step) = ChronoDuration::from_std(delay) {
        backoff.until = now.checked_add_signed(step);
    }
}

/// Clears the cooldown after a successful mint.
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
/// This method does not return an error.
///
/// # Panics
///
/// This method does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::TokenPolicy;
///
/// let _policy = TokenPolicy::default();
/// ```
async fn clear_backoff(inner: &TokenInner) {
    let mut backoff = inner.backoff.lock().await;
    backoff.until = None;
    backoff.failures = 0;
}

/// Computes when a token minted at `now` should stop being reused.
///
/// Rail tokens last until the next Eastern midnight. Bus tokens last for
/// [`TokenPolicy::bus_token_ttl`].
///
/// # Arguments
///
/// * `now` — Mint time.
///
/// # Returns
///
/// The expiry instant stored with the token.
///
/// # Errors
///
/// Returns [`Error::Config`] when the bus lifetime cannot be represented or
/// the Eastern midnight cannot be computed.
///
/// # Panics
///
/// This method does not panic.
///
/// # Examples
///
/// ```
/// use chrono::{DateTime, Utc};
/// use njt_client::next_midnight_eastern;
///
/// let now = DateTime::parse_from_rfc3339("2026-01-15T15:00:00Z")
///     .expect("timestamp")
///     .with_timezone(&Utc);
/// assert!(next_midnight_eastern(now).expect("midnight") > now);
/// ```
fn expiry_after(inner: &TokenInner, now: DateTime<Utc>) -> Result<DateTime<Utc>, Error> {
    match inner.api {
        ApiKind::Rail => next_midnight_eastern(now),
        ApiKind::Bus => {
            let step = ChronoDuration::from_std(inner.policy.bus_token_ttl).map_err(|_| {
                Error::Config {
                    message: "bus token lifetime is too large".to_string(),
                }
            })?;
            now.checked_add_signed(step).ok_or_else(|| Error::Config {
                message: "bus token expiry overflowed".to_string(),
            })
        }
    }
}

/// Posts username and password to the token endpoint.
///
/// # Arguments
///
/// This method takes no arguments.
///
/// # Returns
///
/// The raw token string. The caller stores it and must not log it.
///
/// # Errors
///
/// See [`crate::parse_token_body`] plus [`Error::Transport`], [`Error::Status`],
/// and [`Error::TooLarge`].
///
/// # Panics
///
/// This method does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::{endpoint_url, ApiKind, RAIL_PRODUCTION_BASE};
///
/// let url = endpoint_url(RAIL_PRODUCTION_BASE, ApiKind::Rail, "getToken").expect("url");
/// assert!(url.ends_with("/api/GTFSRT/getToken"));
/// ```
async fn request_token(inner: &TokenInner) -> Result<String, Error> {
    let url = endpoint_url(&inner.base, inner.api, inner.api.auth_method())?;
    let form = reqwest::multipart::Form::new()
        .text("username", inner.username.expose().to_string())
        .text("password", inner.password.expose().to_string());
    let response = inner
        .http
        .post(&url)
        .header("accept", "application/json, text/plain, */*")
        .multipart(form)
        .send()
        .await
        .map_err(|err| Error::Transport(Box::new(err)))?;
    let status = response.status();
    let bytes = read_limited(response, 64 * 1024).await?;
    match parse_token_body(&bytes) {
        Ok(token) if status.is_success() => Ok(token),
        Ok(_) => Err(Error::Status {
            code: status.as_u16(),
        }),
        Err(Error::UnexpectedPayload { .. }) if !status.is_success() => Err(Error::Status {
            code: status.as_u16(),
        }),
        Err(err) => Err(err),
    }
}

impl std::fmt::Debug for TokenManager {
    /// Debug-formats the manager without credentials or tokens.
    ///
    /// # Arguments
    ///
    /// * `formatter` — Destination.
    ///
    /// # Returns
    ///
    /// A formatting result. Username and password are shown as redacted.
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
    /// use njt_client::{ApiKind, TokenManager, RAIL_PRODUCTION_BASE};
    ///
    /// let manager = TokenManager::builder(ApiKind::Rail, RAIL_PRODUCTION_BASE)
    ///     .credentials("portal-user", "portal-secret-value")
    ///     .build()
    ///     .expect("builder");
    /// let rendered = format!("{manager:?}");
    /// assert!(rendered.contains("redacted"));
    /// assert!(!rendered.contains("portal-secret-value"));
    /// ```
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenManager")
            .field("api", &self.inner.api)
            .field("base", &self.inner.base)
            .field("username", &self.inner.username)
            .field("password", &self.inner.password)
            .field("daily_cap", &self.inner.policy.daily_cap)
            .finish()
    }
}

/// Builder for [`TokenManager`].
///
/// # Examples
///
/// ```
/// use njt_client::{ApiKind, TokenManager, BUS_TEST_BASE};
///
/// let _manager = TokenManager::builder(ApiKind::Bus, BUS_TEST_BASE)
///     .credentials("username", "password")
///     .build()
///     .expect("builder");
/// ```
pub struct TokenManagerBuilder {
    /// Target API.
    api: ApiKind,
    /// Caller-supplied origin, not yet normalized.
    base: String,
    /// Username, once [`TokenManagerBuilder::credentials`] is called.
    username: Option<Secret>,
    /// Password, once [`TokenManagerBuilder::credentials`] is called.
    password: Option<Secret>,
    /// Shared cache. Defaults to memory.
    store: Option<Arc<TokenStore>>,
    /// Cap and lifetimes.
    policy: TokenPolicy,
    /// Clock. Defaults to the system clock.
    clock: Option<Arc<dyn Clock>>,
    /// HTTP client. Defaults to a rustls client with redirects disabled.
    http: Option<reqwest::Client>,
}

impl TokenManagerBuilder {
    /// Sets the portal username and password.
    ///
    /// # Arguments
    ///
    /// * `username` — Value posted as the `username` form field.
    /// * `password` — Value posted as the `password` form field.
    ///
    /// # Returns
    ///
    /// The builder.
    ///
    /// # Errors
    ///
    /// This method does not return an error. Empty values fail in
    /// [`TokenManagerBuilder::build`].
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::{ApiKind, TokenManager, RAIL_TEST_BASE};
    ///
    /// let _manager = TokenManager::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .build()
    ///     .expect("builder");
    /// ```
    pub fn credentials(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = Some(Secret::new(username));
        self.password = Some(Secret::new(password));
        self
    }

    /// Uses `store` instead of a private memory store.
    ///
    /// Pass the same [`Arc`] to the rail and bus managers so they share one file.
    ///
    /// # Arguments
    ///
    /// * `store` — Cache created by [`TokenStore::open`] or [`TokenStore::memory`].
    ///
    /// # Returns
    ///
    /// The builder.
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
    /// use njt_client::{ApiKind, TokenManager, TokenStore, RAIL_TEST_BASE};
    ///
    /// let store = TokenStore::memory();
    /// let _manager = TokenManager::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .store(store)
    ///     .build()
    ///     .expect("builder");
    /// ```
    pub fn store(mut self, store: Arc<TokenStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Replaces the default policy.
    ///
    /// # Arguments
    ///
    /// * `policy` — Cap, backoff, and bus lifetime. The cap must be `1..=10`.
    ///
    /// # Returns
    ///
    /// The builder.
    ///
    /// # Errors
    ///
    /// This method does not return an error. Invalid policies fail in
    /// [`TokenManagerBuilder::build`].
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::{ApiKind, TokenManager, TokenPolicy, RAIL_TEST_BASE};
    ///
    /// let policy = TokenPolicy {
    ///     daily_cap: 2,
    ///     ..TokenPolicy::default()
    /// };
    /// let _manager = TokenManager::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .policy(policy)
    ///     .build()
    ///     .expect("builder");
    /// ```
    pub fn policy(mut self, policy: TokenPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Uses `clock` instead of the wall clock.
    ///
    /// # Arguments
    ///
    /// * `clock` — Clock used for expiry and the quota day.
    ///
    /// # Returns
    ///
    /// The builder.
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
    /// use chrono::{TimeZone, Utc};
    /// use njt_client::{ApiKind, ManualClock, TokenManager, RAIL_TEST_BASE};
    /// use std::sync::Arc;
    ///
    /// let clock = Arc::new(ManualClock::new(
    ///     Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap(),
    /// ));
    /// let _manager = TokenManager::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .clock(clock)
    ///     .build()
    ///     .expect("builder");
    /// ```
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Uses `http` instead of the default rustls client.
    ///
    /// # Arguments
    ///
    /// * `http` — Client used for the token POST. Redirects should stay disabled
    ///   so a token POST cannot be bounced to another host.
    ///
    /// # Returns
    ///
    /// The builder.
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
    /// use njt_client::{ApiKind, TokenManager, RAIL_TEST_BASE};
    ///
    /// let http = reqwest::Client::builder()
    ///     .build()
    ///     .expect("client");
    /// let _manager = TokenManager::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .http(http)
    ///     .build()
    ///     .expect("builder");
    /// ```
    pub fn http(mut self, http: reqwest::Client) -> Self {
        self.http = Some(http);
        self
    }

    /// Builds the manager.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// A manager that has not contacted NJ Transit yet.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when the base URL is not an origin, credentials
    /// are missing or empty, the daily cap is outside `1..=10`, or the bus
    /// lifetime is zero or longer than 48 hours.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::{ApiKind, TokenManager};
    ///
    /// let err = TokenManager::builder(ApiKind::Rail, "https://raildata.njtransit.com")
    ///     .build()
    ///     .expect_err("credentials are required");
    /// assert!(err.to_string().contains("credential"));
    /// ```
    pub fn build(self) -> Result<TokenManager, Error> {
        let base = crate::api::normalize_base(&self.base)?;
        let username = self.username.ok_or_else(|| Error::Config {
            message: "credentials are required".to_string(),
        })?;
        let password = self.password.ok_or_else(|| Error::Config {
            message: "credentials are required".to_string(),
        })?;
        if username.is_empty() || password.is_empty() {
            return Err(Error::Config {
                message: "credentials must not be empty".to_string(),
            });
        }
        validate_policy(&self.policy)?;
        let http = match self.http {
            Some(http) => http,
            None => default_http()?,
        };
        Ok(TokenManager {
            inner: Arc::new(TokenInner {
                api: self.api,
                base,
                username,
                password,
                http,
                store: self.store.unwrap_or_else(TokenStore::memory),
                policy: self.policy,
                clock: self.clock.unwrap_or_else(|| Arc::new(SystemClock)),
                flight: Mutex::new(()),
                backoff: Mutex::new(Backoff {
                    until: None,
                    failures: 0,
                }),
            }),
        })
    }
}

impl std::fmt::Debug for TokenManagerBuilder {
    /// Debug-formats the builder with credentials redacted.
    ///
    /// # Arguments
    ///
    /// * `formatter` — Destination.
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
    /// use njt_client::{ApiKind, TokenManager};
    ///
    /// let builder = TokenManager::builder(ApiKind::Rail, "https://raildata.njtransit.com")
    ///     .credentials("portal-user", "portal-secret-value");
    /// let rendered = format!("{builder:?}");
    /// assert!(rendered.contains("redacted"));
    /// assert!(!rendered.contains("portal-secret-value"));
    /// ```
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenManagerBuilder")
            .field("api", &self.api)
            .field("base", &self.base)
            .field("username", &self.username)
            .field("password", &self.password)
            .field("daily_cap", &self.policy.daily_cap)
            .finish()
    }
}

/// Rejects a policy that could exceed the published rail budget or busy-loop.
///
/// # Arguments
///
/// * `policy` — Policy about to be stored.
///
/// # Returns
///
/// Nothing when the policy is usable.
///
/// # Errors
///
/// Returns [`Error::Config`] when the cap, backoff, or bus lifetime is out of range.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::{ApiKind, TokenManager, TokenPolicy, RAIL_TEST_BASE};
///
/// let policy = TokenPolicy {
///     daily_cap: 11,
///     ..TokenPolicy::default()
/// };
/// let err = TokenManager::builder(ApiKind::Rail, RAIL_TEST_BASE)
///     .credentials("username", "password")
///     .policy(policy)
///     .build()
///     .expect_err("cap");
/// assert!(err.to_string().contains("1..=10"));
/// ```
fn validate_policy(policy: &TokenPolicy) -> Result<(), Error> {
    if policy.daily_cap == 0 || policy.daily_cap > DAILY_MINT_CAP {
        return Err(Error::Config {
            message: "daily_cap must be 1..=10".to_string(),
        });
    }
    if policy.backoff_initial.is_zero() {
        return Err(Error::Config {
            message: "backoff_initial must be at least 1ms".to_string(),
        });
    }
    if policy.backoff_max < policy.backoff_initial {
        return Err(Error::Config {
            message: "backoff_max must be at least backoff_initial".to_string(),
        });
    }
    let bus = policy.bus_token_ttl.as_secs();
    if bus == 0 || bus > 48 * 60 * 60 {
        return Err(Error::Config {
            message: "bus_token_ttl must be between 1 second and 48 hours".to_string(),
        });
    }
    Ok(())
}

/// Builds the default HTTP client.
///
/// # Arguments
///
/// This function takes no arguments.
///
/// # Returns
///
/// A client with a 120 second timeout, rustls, and redirects disabled.
///
/// # Errors
///
/// Returns [`Error::Config`] when the TLS stack cannot be initialized. The
/// message does not include credentials.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::{ApiKind, TokenManager, RAIL_TEST_BASE};
///
/// let _manager = TokenManager::builder(ApiKind::Rail, RAIL_TEST_BASE)
///     .credentials("username", "password")
///     .build()
///     .expect("default http client");
/// ```
fn default_http() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .user_agent(concat!("njt-client/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| Error::Config {
            message: format!("could not build the HTTP client: {err}"),
        })
}

/// Reads a response body up to `limit` bytes.
///
/// # Arguments
///
/// * `response` — Upstream response. The body is consumed.
/// * `limit` — Maximum buffered size.
///
/// # Returns
///
/// The full body.
///
/// # Errors
///
/// Returns [`Error::TooLarge`] when `Content-Length` or the streamed body
/// exceeds `limit`, or [`Error::Transport`] when a chunk fails.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// The reader is private. [`crate::Feed::max_bytes`] publishes the data-call caps:
///
/// ```
/// use njt_client::Feed;
///
/// assert!(Feed::Static.max_bytes() > 64 * 1024);
/// ```
async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<bytes::Bytes, Error> {
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return Err(Error::TooLarge { limit });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| Error::Transport(Box::new(err)))?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(Error::TooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(bytes::Bytes::from(body))
}
