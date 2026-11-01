//! Authenticated downloads of NJ Transit GTFS and GTFS-Realtime bodies.
//!
//! [`Client::fetch`] gets a token from [`crate::TokenManager`], posts it as the
//! `token` form field, and returns bytes only after [`crate::classify`] accepts
//! them. One invalid-token response triggers a single remint and one retry.
//!
//! # Examples
//!
//! ```
//! use njt_client::{ApiKind, Client, RAIL_TEST_BASE};
//!
//! let client = Client::builder(ApiKind::Rail, RAIL_TEST_BASE)
//!     .credentials("username", "password")
//!     .build()
//!     .expect("builder");
//! assert_eq!(client.api(), ApiKind::Rail);
//! ```

use std::sync::Arc;

use bytes::Bytes;

use crate::api::{endpoint_url, ApiKind, Feed};
use crate::clock::Clock;
use crate::error::Error;
use crate::payload::classify;
use crate::store::TokenStore;
use crate::token::{TokenManager, TokenManagerBuilder, TokenPolicy};

/// HTTP client for one NJ Transit GTFS API.
///
/// The client is cheap to clone. Clones share the token manager and its cache.
///
/// # Examples
///
/// ```
/// use njt_client::{ApiKind, Client, BUS_TEST_BASE};
///
/// let client = Client::builder(ApiKind::Bus, BUS_TEST_BASE)
///     .credentials("username", "password")
///     .build()
///     .expect("builder");
/// assert_eq!(client.base_url(), BUS_TEST_BASE);
/// ```
#[derive(Clone)]
pub struct Client {
    /// Which API the data URLs are built for.
    api: ApiKind,
    /// Normalized origin.
    base: String,
    /// HTTP client for data posts. Redirects stay disabled.
    http: reqwest::Client,
    /// Token minting, cap, and cache.
    tokens: TokenManager,
}

impl Client {
    /// Starts a builder that has not contacted NJ Transit.
    ///
    /// # Arguments
    ///
    /// * `api` — Rail or bus.
    /// * `base` — Origin such as [`crate::RAIL_PRODUCTION_BASE`].
    ///
    /// # Returns
    ///
    /// A [`ClientBuilder`].
    ///
    /// # Errors
    ///
    /// This function does not return an error. [`ClientBuilder::build`] reports
    /// configuration problems.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::{ApiKind, Client, RAIL_PRODUCTION_BASE};
    ///
    /// let _client = Client::builder(ApiKind::Rail, RAIL_PRODUCTION_BASE)
    ///     .credentials("username", "password")
    ///     .build()
    ///     .expect("builder");
    /// ```
    pub fn builder(api: ApiKind, base: impl Into<String>) -> ClientBuilder {
        ClientBuilder {
            inner: TokenManager::builder(api, base),
        }
    }

    /// Returns which API this client calls.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The [`ApiKind`] from [`Client::builder`].
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
    /// use njt_client::{ApiKind, Client, BUS_PRODUCTION_BASE};
    ///
    /// let client = Client::builder(ApiKind::Bus, BUS_PRODUCTION_BASE)
    ///     .credentials("username", "password")
    ///     .build()
    ///     .expect("builder");
    /// assert_eq!(client.api(), ApiKind::Bus);
    /// ```
    pub fn api(&self) -> ApiKind {
        self.api
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
    /// use njt_client::{ApiKind, Client, RAIL_TEST_BASE};
    ///
    /// let client = Client::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .build()
    ///     .expect("builder");
    /// assert_eq!(client.base_url(), RAIL_TEST_BASE);
    /// ```
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Returns the token manager used by this client.
    ///
    /// A second binary can mint tokens without downloading a feed.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The shared [`TokenManager`].
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
    /// use njt_client::{ApiKind, Client, RAIL_TEST_BASE};
    ///
    /// let client = Client::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .build()
    ///     .expect("builder");
    /// assert_eq!(client.token_manager().api(), ApiKind::Rail);
    /// ```
    pub fn token_manager(&self) -> &TokenManager {
        &self.tokens
    }

    /// Downloads one feed, refreshing the token at most once if it is rejected.
    ///
    /// # Arguments
    ///
    /// * `feed` — Static zip or a realtime protobuf.
    ///
    /// # Returns
    ///
    /// The raw zip or protobuf bytes. JSON error bodies are not returned.
    ///
    /// # Errors
    ///
    /// * [`Error::DailyLimit`], [`Error::LocalCap`], and [`Error::Backoff`] when
    ///   a token cannot be minted.
    /// * [`Error::InvalidToken`] when the reminted token is also rejected.
    /// * [`Error::NullBody`] when the upstream body is the literal `Null`.
    /// * [`Error::Upstream`] for other JSON `errorMessage` values, including
    ///   `GTFS error`. That case does not spend another token call.
    /// * [`Error::UnexpectedPayload`] when the body is not the requested kind.
    /// * [`Error::Transport`], [`Error::Status`], and [`Error::TooLarge`] for
    ///   HTTP failures.
    ///
    /// # Panics
    ///
    /// This method does not panic. Call it from a Tokio runtime; the token
    /// cache lock uses `spawn_blocking` when a runtime is present and falls
    /// back to the calling thread otherwise.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use njt_client::{ApiKind, Client, Feed, RAIL_PRODUCTION_BASE};
    ///
    /// # async fn demo() -> Result<(), njt_client::Error> {
    /// let client = Client::builder(ApiKind::Rail, RAIL_PRODUCTION_BASE)
    ///     .credentials("username", "password")
    ///     .build()?;
    /// let zip = client.fetch(Feed::Static).await?;
    /// let _ = zip.slice(..2);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn fetch(&self, feed: Feed) -> Result<Bytes, Error> {
        let token = self.tokens.current().await?;
        match self.post_feed(feed, token.expose()).await {
            Err(Error::InvalidToken) => {
                tracing::info!(
                    api = %self.api,
                    feed = %feed,
                    "upstream rejected the token; refreshing once"
                );
                let token = self.tokens.refresh_replacing(token.expose()).await?;
                self.post_feed(feed, token.expose()).await
            }
            other => other,
        }
    }

    /// Posts `token` to the data method for `feed`.
    ///
    /// # Arguments
    ///
    /// * `feed` — Which method and payload check to use.
    /// * `token` — Current token. Not logged.
    ///
    /// # Returns
    ///
    /// The response body after [`classify`] accepts it.
    ///
    /// # Errors
    ///
    /// See [`Client::fetch`], except this method does not remint.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::{endpoint_url, ApiKind, Feed, BUS_PRODUCTION_BASE};
    ///
    /// let url = endpoint_url(BUS_PRODUCTION_BASE, ApiKind::Bus, Feed::Alerts.method())
    ///     .expect("alerts");
    /// assert!(url.ends_with("/api/GTFSG2/getAlerts"));
    /// ```
    async fn post_feed(&self, feed: Feed, token: &str) -> Result<Bytes, Error> {
        let url = endpoint_url(&self.base, self.api, feed.method())?;
        let form = reqwest::multipart::Form::new().text("token", token.to_string());
        let response = self
            .http
            .post(&url)
            .header("accept", "*/*")
            .multipart(form)
            .send()
            .await
            .map_err(|err| Error::Transport(Box::new(err)))?;
        let status = response.status();
        let bytes = read_body(response, feed.max_bytes()).await?;
        classify(&bytes, feed.payload_kind())?;
        if !status.is_success() {
            return Err(Error::Status {
                code: status.as_u16(),
            });
        }
        Ok(bytes)
    }
}

impl std::fmt::Debug for Client {
    /// Debug-formats the client without credentials or tokens.
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
    /// use njt_client::{ApiKind, Client, RAIL_TEST_BASE};
    ///
    /// let client = Client::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("portal-user", "portal-secret-value")
    ///     .build()
    ///     .expect("builder");
    /// let rendered = format!("{client:?}");
    /// assert!(rendered.contains("rail"));
    /// assert!(!rendered.contains("portal-secret-value"));
    /// ```
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Client")
            .field("api", &self.api)
            .field("base", &self.base)
            .field("tokens", &self.tokens)
            .finish()
    }
}

/// Builder for [`Client`].
///
/// # Examples
///
/// ```
/// use njt_client::{ApiKind, Client, RAIL_TEST_BASE};
///
/// let _client = Client::builder(ApiKind::Rail, RAIL_TEST_BASE)
///     .credentials("username", "password")
///     .build()
///     .expect("builder");
/// ```
pub struct ClientBuilder {
    /// Token-manager builder that also carries the origin and credentials.
    inner: TokenManagerBuilder,
}

impl ClientBuilder {
    /// Sets the portal username and password.
    ///
    /// # Arguments
    ///
    /// * `username` — Posted as the `username` form field when minting.
    /// * `password` — Posted as the `password` form field when minting.
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
    /// use njt_client::{ApiKind, Client, BUS_TEST_BASE};
    ///
    /// let _client = Client::builder(ApiKind::Bus, BUS_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .build()
    ///     .expect("builder");
    /// ```
    pub fn credentials(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.inner = self.inner.credentials(username, password);
        self
    }

    /// Shares `store` with this client's token manager.
    ///
    /// # Arguments
    ///
    /// * `store` — Memory or on-disk cache. Use one store for rail and bus.
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
    /// use njt_client::{ApiKind, Client, TokenStore, RAIL_TEST_BASE};
    ///
    /// let _client = Client::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .store(TokenStore::memory())
    ///     .build()
    ///     .expect("builder");
    /// ```
    pub fn store(mut self, store: Arc<TokenStore>) -> Self {
        self.inner = self.inner.store(store);
        self
    }

    /// Replaces the token policy.
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
    /// [`ClientBuilder::build`].
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::{ApiKind, Client, TokenPolicy, RAIL_TEST_BASE};
    ///
    /// let policy = TokenPolicy {
    ///     daily_cap: 2,
    ///     ..TokenPolicy::default()
    /// };
    /// let _client = Client::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .policy(policy)
    ///     .build()
    ///     .expect("builder");
    /// ```
    pub fn policy(mut self, policy: TokenPolicy) -> Self {
        self.inner = self.inner.policy(policy);
        self
    }

    /// Uses `clock` for token expiry and the quota day.
    ///
    /// # Arguments
    ///
    /// * `clock` — Usually [`crate::SystemClock`] or [`crate::ManualClock`].
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
    /// use njt_client::{ApiKind, Client, ManualClock, RAIL_TEST_BASE};
    /// use std::sync::Arc;
    ///
    /// let clock = Arc::new(ManualClock::new(
    ///     Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap(),
    /// ));
    /// let _client = Client::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .clock(clock)
    ///     .build()
    ///     .expect("builder");
    /// ```
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.inner = self.inner.clock(clock);
        self
    }

    /// Uses `http` for both token and data posts.
    ///
    /// # Arguments
    ///
    /// * `http` — Client. Keep redirects disabled.
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
    /// use njt_client::{ApiKind, Client, RAIL_TEST_BASE};
    ///
    /// let http = reqwest::Client::builder().build().expect("client");
    /// let _client = Client::builder(ApiKind::Rail, RAIL_TEST_BASE)
    ///     .credentials("username", "password")
    ///     .http(http)
    ///     .build()
    ///     .expect("builder");
    /// ```
    pub fn http(mut self, http: reqwest::Client) -> Self {
        self.inner = self.inner.http(http);
        self
    }

    /// Builds the client.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// A client that has not called NJ Transit yet.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] for a bad base URL, missing credentials, or an
    /// invalid [`TokenPolicy`].
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_client::{ApiKind, Client};
    ///
    /// let err = Client::builder(ApiKind::Rail, "https://raildata.njtransit.com")
    ///     .build()
    ///     .expect_err("credentials");
    /// assert!(err.to_string().contains("credential"));
    /// ```
    pub fn build(self) -> Result<Client, Error> {
        let tokens = self.inner.build()?;
        let http = tokens_http(&tokens);
        Ok(Client {
            api: tokens.api(),
            base: tokens.base_url().to_string(),
            http,
            tokens,
        })
    }
}

impl std::fmt::Debug for ClientBuilder {
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
    /// use njt_client::{ApiKind, Client};
    ///
    /// let builder = Client::builder(ApiKind::Bus, "https://pcsdata.njtransit.com")
    ///     .credentials("portal-user", "portal-secret-value");
    /// let rendered = format!("{builder:?}");
    /// assert!(rendered.contains("redacted"));
    /// assert!(!rendered.contains("portal-secret-value"));
    /// ```
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientBuilder")
            .field("inner", &self.inner)
            .finish()
    }
}

/// Reads the reqwest client out of a built token manager.
///
/// The token manager owns the client. The data path needs its own clone, which
/// [`reqwest::Client`] provides cheaply. This helper rebuilds nothing.
///
/// # Arguments
///
/// * `tokens` — Manager whose HTTP client should be cloned.
///
/// # Returns
///
/// A clone of the manager's HTTP client.
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
/// use njt_client::{ApiKind, Client, RAIL_TEST_BASE};
///
/// let client = Client::builder(ApiKind::Rail, RAIL_TEST_BASE)
///     .credentials("username", "password")
///     .build()
///     .expect("builder");
/// assert_eq!(client.api(), ApiKind::Rail);
/// ```
fn tokens_http(tokens: &TokenManager) -> reqwest::Client {
    tokens.http_client()
}

/// Buffers a data response up to `limit` bytes.
///
/// # Arguments
///
/// * `response` — Upstream response. The body is consumed.
/// * `limit` — Maximum size from [`Feed::max_bytes`].
///
/// # Returns
///
/// The full body.
///
/// # Errors
///
/// Returns [`Error::TooLarge`] or [`Error::Transport`].
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::Feed;
///
/// assert_eq!(Feed::Alerts.content_type(), "application/x-protobuf");
/// ```
async fn read_body(mut response: reqwest::Response, limit: usize) -> Result<Bytes, Error> {
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
    Ok(Bytes::from(body))
}
