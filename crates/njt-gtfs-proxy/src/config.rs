//! Process configuration from environment variables.
//!
//! Only the variables documented on [`Config`] are read. Values of passwords,
//! usernames, and the access key are not included in error text.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use njt_client::{
    ApiKind, Client, Clock, Secret, SystemClock, TokenStore, BUS_PRODUCTION_BASE, BUS_TEST_BASE,
    RAIL_PRODUCTION_BASE, RAIL_TEST_BASE,
};

use crate::cache::FeedCache;
use crate::http::{router_from_state, AppState};

/// `NJT_ENV`. `production` (default) or `test`.
pub const ENV_NJT_ENV: &str = "NJT_ENV";
/// `NJT_RAIL_USERNAME`. Enables rail when set with the password.
pub const ENV_RAIL_USERNAME: &str = "NJT_RAIL_USERNAME";
/// `NJT_RAIL_PASSWORD`.
pub const ENV_RAIL_PASSWORD: &str = "NJT_RAIL_PASSWORD";
/// `NJT_BUS_USERNAME`. Enables bus when set with the password.
pub const ENV_BUS_USERNAME: &str = "NJT_BUS_USERNAME";
/// `NJT_BUS_PASSWORD`.
pub const ENV_BUS_PASSWORD: &str = "NJT_BUS_PASSWORD";
/// `NJT_PROXY_BIND`. Defaults to `127.0.0.1:8080`.
pub const ENV_BIND: &str = "NJT_PROXY_BIND";
/// `NJT_TOKEN_CACHE`. Optional path of the mode-`0600` token file.
pub const ENV_TOKEN_CACHE: &str = "NJT_TOKEN_CACHE";
/// `NJT_RT_TTL_SECS`. Realtime payload cache. Default 20. `0` disables it.
pub const ENV_RT_TTL: &str = "NJT_RT_TTL_SECS";
/// `NJT_STATIC_TTL_SECS`. Static zip cache. Default 21600 (6 hours).
pub const ENV_STATIC_TTL: &str = "NJT_STATIC_TTL_SECS";
/// `NJT_RAIL_BASE_URL`. Overrides the rail origin selected by [`ENV_NJT_ENV`].
pub const ENV_RAIL_BASE: &str = "NJT_RAIL_BASE_URL";
/// `NJT_BUS_BASE_URL`. Overrides the bus origin selected by [`ENV_NJT_ENV`].
pub const ENV_BUS_BASE: &str = "NJT_BUS_BASE_URL";
/// `NJT_PROXY_ACCESS_KEY`. When set, feed requests must send [`crate::ACCESS_KEY_HEADER`].
pub const ENV_ACCESS_KEY: &str = "NJT_PROXY_ACCESS_KEY";

/// Listen address used when `NJT_PROXY_BIND` is unset.
const DEFAULT_BIND: &str = "127.0.0.1:8080";
/// Default realtime cache TTL in seconds.
const DEFAULT_RT_TTL: u64 = 20;
/// Default static-zip cache TTL in seconds (6 hours).
const DEFAULT_STATIC_TTL: u64 = 6 * 60 * 60;
/// Maximum accepted realtime TTL in seconds.
const MAX_RT_TTL: u64 = 60 * 60;
/// Maximum accepted static TTL in seconds (7 days).
const MAX_STATIC_TTL: u64 = 7 * 24 * 60 * 60;

/// Which NJ Transit deployment the default origins point at.
///
/// # Examples
///
/// ```
/// use njt_gtfs_proxy::Config;
///
/// let config = Config::from_vars([("NJT_ENV", "test")]).expect("env");
/// assert_eq!(config.environment(), "test");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Environment {
    /// `raildata.njtransit.com` and `pcsdata.njtransit.com`.
    Production,
    /// `testraildata.njtransit.com` and `testpcsdata.njtransit.com`.
    Test,
}

impl Environment {
    /// Parses `production` or `test`, ignoring ASCII case.
    ///
    /// # Arguments
    ///
    /// * `raw` — Value of [`ENV_NJT_ENV`].
    ///
    /// # Returns
    ///
    /// The selected deployment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when `raw` is not `production` or `test`.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_gtfs_proxy::Config;
    ///
    /// assert!(Config::from_vars([("NJT_ENV", "staging")]).is_err());
    /// ```
    fn parse(raw: &str) -> Result<Self, ConfigError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "production" => Ok(Self::Production),
            "test" => Ok(Self::Test),
            _ => Err(ConfigError::new("NJT_ENV must be production or test")),
        }
    }

    /// Returns the default origin for `api` before any override.
    ///
    /// # Arguments
    ///
    /// * `api` — Rail or bus.
    ///
    /// # Returns
    ///
    /// An https origin with no path.
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
    /// use njt_gtfs_proxy::Config;
    ///
    /// let config = Config::from_vars(None::<(&str, &str)>).expect("defaults");
    /// assert_eq!(config.environment(), "production");
    /// ```
    fn default_base(self, api: ApiKind) -> &'static str {
        match (self, api) {
            (Self::Production, ApiKind::Rail) => RAIL_PRODUCTION_BASE,
            (Self::Test, ApiKind::Rail) => RAIL_TEST_BASE,
            (Self::Production, ApiKind::Bus) => BUS_PRODUCTION_BASE,
            (Self::Test, ApiKind::Bus) => BUS_TEST_BASE,
        }
    }
}

/// Failure while reading proxy configuration.
///
/// The display text names the variable and the problem. It does not include
/// passwords, tokens, or the access key.
///
/// # Examples
///
/// ```
/// use njt_gtfs_proxy::Config;
///
/// let err = Config::from_vars([("NJT_RAIL_USERNAME", "username")]).expect_err("partial");
/// let text = err.to_string();
/// assert!(text.contains("NJT_RAIL_PASSWORD"));
/// assert!(!text.contains("username"));
/// ```
#[derive(Debug)]
pub struct ConfigError {
    /// Operator-facing explanation.
    message: String,
}

impl ConfigError {
    /// Builds an error from `message`.
    ///
    /// # Arguments
    ///
    /// * `message` — Text that must not contain secrets.
    ///
    /// # Returns
    ///
    /// The error.
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
    /// use njt_gtfs_proxy::Config;
    ///
    /// let err = Config::from_vars([("NJT_PROXY_BIND", "not-an-address")]).expect_err("bind");
    /// assert!(err.to_string().contains("NJT_PROXY_BIND"));
    /// ```
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ConfigError {
    /// Writes the configuration message.
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
    /// use njt_gtfs_proxy::Config;
    ///
    /// let err = Config::from_vars([("NJT_RT_TTL_SECS", "soon")]).expect_err("ttl");
    /// assert!(err.to_string().contains("NJT_RT_TTL_SECS"));
    /// ```
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

/// Credentials for one API. Debug output is redacted.
struct Credentials {
    /// Portal username.
    username: Secret,
    /// Portal password.
    password: Secret,
}

impl std::fmt::Debug for Credentials {
    /// Writes redacted placeholders.
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
    /// use njt_gtfs_proxy::Config;
    ///
    /// let config = Config::from_vars([
    ///     ("NJT_RAIL_USERNAME", "portal-user"),
    ///     ("NJT_RAIL_PASSWORD", "portal-secret-value"),
    /// ])
    /// .expect("config");
    /// let rendered = format!("{config:?}");
    /// assert!(!rendered.contains("portal-secret-value"));
    /// ```
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &self.password)
            .finish()
    }
}

/// Parsed proxy configuration.
///
/// Build it with [`Config::from_env`] or [`Config::from_vars`]. Feed routes are
/// enabled only when both the username and password for that API are set.
///
/// # Examples
///
/// ```
/// use njt_gtfs_proxy::Config;
///
/// let config = Config::from_vars([
///     ("NJT_ENV", "test"),
///     ("NJT_RAIL_USERNAME", "username"),
///     ("NJT_RAIL_PASSWORD", "password"),
/// ])
/// .expect("config");
/// assert!(config.rail_enabled());
/// assert!(!config.bus_enabled());
/// assert_eq!(config.bind().to_string(), "127.0.0.1:8080");
/// ```
#[derive(Debug)]
pub struct Config {
    /// Production or test, used when a base URL override is absent.
    environment: Environment,
    /// Listen address.
    bind: SocketAddr,
    /// Rail credentials, when both variables were set.
    rail: Option<Credentials>,
    /// Bus credentials, when both variables were set.
    bus: Option<Credentials>,
    /// Optional rail origin override.
    rail_base: Option<String>,
    /// Optional bus origin override.
    bus_base: Option<String>,
    /// Realtime response-cache TTL.
    rt_ttl: Duration,
    /// Static-zip response-cache TTL.
    static_ttl: Duration,
    /// Token file path. `None` keeps tokens in memory only.
    token_cache: Option<PathBuf>,
    /// Required request header value, when set.
    access_key: Option<Secret>,
}

impl Config {
    /// Reads configuration from the process environment.
    ///
    /// # Arguments
    ///
    /// This function takes no arguments.
    ///
    /// # Returns
    ///
    /// The parsed configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for an unknown `NJT_ENV`, a partial credential
    /// pair, a bad bind address, a TTL out of range, or a base URL the client
    /// rejects. Secret values are not copied into the message.
    ///
    /// # Panics
    ///
    /// This function does not panic.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use njt_gtfs_proxy::Config;
    ///
    /// let _config = Config::from_env()?;
    /// # Ok::<(), njt_gtfs_proxy::ConfigError>(())
    /// ```
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_vars(std::env::vars())
    }

    /// Reads configuration from an explicit variable list.
    ///
    /// Later duplicates win. Unknown variables are ignored so unrelated process
    /// secrets are not retained.
    ///
    /// # Arguments
    ///
    /// * `vars` — Pairs of name and value.
    ///
    /// # Returns
    ///
    /// The parsed configuration.
    ///
    /// # Errors
    ///
    /// See [`Config::from_env`].
    ///
    /// # Panics
    ///
    /// This function does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_gtfs_proxy::Config;
    ///
    /// let config = Config::from_vars([
    ///     ("NJT_BUS_USERNAME", "username"),
    ///     ("NJT_BUS_PASSWORD", "password"),
    ///     ("NJT_BUS_BASE_URL", "https://testpcsdata.njtransit.com"),
    /// ])
    /// .expect("config");
    /// assert!(config.bus_enabled());
    /// assert!(!config.access_key_required());
    /// ```
    pub fn from_vars<I, K, V>(vars: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut values = HashMap::<String, String>::new();
        for (key, value) in vars {
            let key = key.as_ref();
            if is_known(key) {
                values.insert(key.to_string(), value.as_ref().to_string());
            }
        }
        let environment = match values.get(ENV_NJT_ENV) {
            Some(raw) if !raw.trim().is_empty() => Environment::parse(raw)?,
            _ => Environment::Production,
        };
        let bind = parse_bind(values.get(ENV_BIND).map(String::as_str))?;
        let rail = credential_pair(&values, ENV_RAIL_USERNAME, ENV_RAIL_PASSWORD)?;
        let bus = credential_pair(&values, ENV_BUS_USERNAME, ENV_BUS_PASSWORD)?;
        let rail_base =
            optional_base(values.get(ENV_RAIL_BASE).map(String::as_str), ENV_RAIL_BASE)?;
        let bus_base = optional_base(values.get(ENV_BUS_BASE).map(String::as_str), ENV_BUS_BASE)?;
        let rt_ttl = parse_ttl(
            values.get(ENV_RT_TTL).map(String::as_str),
            ENV_RT_TTL,
            DEFAULT_RT_TTL,
            MAX_RT_TTL,
        )?;
        let static_ttl = parse_ttl(
            values.get(ENV_STATIC_TTL).map(String::as_str),
            ENV_STATIC_TTL,
            DEFAULT_STATIC_TTL,
            MAX_STATIC_TTL,
        )?;
        let token_cache = values
            .get(ENV_TOKEN_CACHE)
            .map(String::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let access_key = values
            .get(ENV_ACCESS_KEY)
            .map(String::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(Secret::new);
        Ok(Self {
            environment,
            bind,
            rail,
            bus,
            rail_base,
            bus_base,
            rt_ttl,
            static_ttl,
            token_cache,
            access_key,
        })
    }

    /// Returns `production` or `test`.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The environment name.
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
    /// use njt_gtfs_proxy::Config;
    ///
    /// let config = Config::from_vars(None::<(&str, &str)>).expect("defaults");
    /// assert_eq!(config.environment(), "production");
    /// ```
    pub fn environment(&self) -> &'static str {
        match self.environment {
            Environment::Production => "production",
            Environment::Test => "test",
        }
    }

    /// Returns the listen address.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The socket address. The default is `127.0.0.1:8080`.
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
    /// use njt_gtfs_proxy::Config;
    ///
    /// let config = Config::from_vars([("NJT_PROXY_BIND", "127.0.0.1:9090")]).expect("bind");
    /// assert_eq!(config.bind().port(), 9090);
    /// ```
    pub fn bind(&self) -> SocketAddr {
        self.bind
    }

    /// Reports whether rail routes will be served.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// `true` when both rail credentials were set.
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
    /// use njt_gtfs_proxy::Config;
    ///
    /// let config = Config::from_vars(None::<(&str, &str)>).expect("empty");
    /// assert!(!config.rail_enabled());
    /// ```
    pub fn rail_enabled(&self) -> bool {
        self.rail.is_some()
    }

    /// Reports whether bus routes will be served.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// `true` when both bus credentials were set.
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
    /// use njt_gtfs_proxy::Config;
    ///
    /// let config = Config::from_vars(None::<(&str, &str)>).expect("empty");
    /// assert!(!config.bus_enabled());
    /// ```
    pub fn bus_enabled(&self) -> bool {
        self.bus.is_some()
    }

    /// Reports whether feed requests must present the access key.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// `true` when [`ENV_ACCESS_KEY`] is non-empty. `/healthz` stays open.
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
    /// use njt_gtfs_proxy::Config;
    ///
    /// let config = Config::from_vars([("NJT_PROXY_ACCESS_KEY", "placeholder")]).expect("key");
    /// assert!(config.access_key_required());
    /// ```
    pub fn access_key_required(&self) -> bool {
        self.access_key.is_some()
    }

    /// Builds the HTTP router, opening the token cache if one was configured.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// An axum [`axum::Router`] that does not listen until [`crate::run`].
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the token cache cannot be opened or a
    /// client cannot be built. The message does not include credentials.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_gtfs_proxy::Config;
    ///
    /// let config = Config::from_vars([
    ///     ("NJT_RAIL_USERNAME", "username"),
    ///     ("NJT_RAIL_PASSWORD", "password"),
    /// ])
    /// .expect("config");
    /// let _router = config.router().expect("router");
    /// ```
    pub fn router(&self) -> Result<axum::Router, ConfigError> {
        self.router_with_clock(Arc::new(SystemClock))
    }

    /// Builds the router using `clock` for token expiry and response TTLs.
    ///
    /// # Arguments
    ///
    /// * `clock` — Usually the system clock. Tests pass a manual clock.
    ///
    /// # Returns
    ///
    /// The axum router.
    ///
    /// # Errors
    ///
    /// See [`Config::router`].
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use chrono::{TimeZone, Utc};
    /// use njt_client::ManualClock;
    /// use njt_gtfs_proxy::Config;
    /// use std::sync::Arc;
    ///
    /// let config = Config::from_vars([
    ///     ("NJT_RAIL_USERNAME", "username"),
    ///     ("NJT_RAIL_PASSWORD", "password"),
    /// ])
    /// .expect("config");
    /// let clock = Arc::new(ManualClock::new(
    ///     Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap(),
    /// ));
    /// let _router = config.router_with_clock(clock).expect("router");
    /// ```
    pub fn router_with_clock(&self, clock: Arc<dyn Clock>) -> Result<axum::Router, ConfigError> {
        let store = match &self.token_cache {
            Some(path) => {
                TokenStore::open(path).map_err(|err| ConfigError::new(err.to_string()))?
            }
            None => TokenStore::memory(),
        };
        let rail = self.build_api(
            ApiKind::Rail,
            self.rail.as_ref(),
            store.clone(),
            clock.clone(),
        )?;
        let bus = self.build_api(ApiKind::Bus, self.bus.as_ref(), store, clock.clone())?;
        let state = AppState {
            rail,
            bus,
            cache: FeedCache::new(self.rt_ttl, self.static_ttl, clock),
            access_key: self.access_key.clone(),
        };
        Ok(router_from_state(state))
    }

    /// Builds one API client, or returns `None` when credentials are absent.
    ///
    /// # Arguments
    ///
    /// * `api` — Rail or bus.
    /// * `credentials` — Username and password, if that API is enabled.
    /// * `store` — Shared token cache.
    /// * `clock` — Quota clock.
    ///
    /// # Returns
    ///
    /// `Some` client, or `None` when `credentials` is `None`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the origin or policy is rejected.
    ///
    /// # Panics
    ///
    /// This method does not panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use njt_gtfs_proxy::Config;
    ///
    /// let config = Config::from_vars([
    ///     ("NJT_ENV", "test"),
    ///     ("NJT_BUS_USERNAME", "username"),
    ///     ("NJT_BUS_PASSWORD", "password"),
    /// ])
    /// .expect("config");
    /// assert!(config.bus_enabled());
    /// let _router = config.router().expect("router");
    /// ```
    fn build_api(
        &self,
        api: ApiKind,
        credentials: Option<&Credentials>,
        store: Arc<TokenStore>,
        clock: Arc<dyn Clock>,
    ) -> Result<Option<Client>, ConfigError> {
        let Some(credentials) = credentials else {
            return Ok(None);
        };
        let base = match api {
            ApiKind::Rail => self.rail_base.clone(),
            ApiKind::Bus => self.bus_base.clone(),
        }
        .unwrap_or_else(|| self.environment.default_base(api).to_string());
        let client = Client::builder(api, base)
            .credentials(credentials.username.expose(), credentials.password.expose())
            .store(store)
            .clock(clock)
            .build()
            .map_err(|err| ConfigError::new(err.to_string()))?;
        Ok(Some(client))
    }
}

/// Reports whether `key` is a variable this proxy understands.
///
/// # Arguments
///
/// * `key` — Environment variable name.
///
/// # Returns
///
/// `true` for the `NJT_*` keys listed on [`Config`].
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
/// use njt_gtfs_proxy::Config;
///
/// let config = Config::from_vars([("UNRELATED_SECRET", "do-not-keep")]).expect("ignored");
/// assert!(!config.rail_enabled());
/// ```
fn is_known(key: &str) -> bool {
    matches!(
        key,
        ENV_NJT_ENV
            | ENV_RAIL_USERNAME
            | ENV_RAIL_PASSWORD
            | ENV_BUS_USERNAME
            | ENV_BUS_PASSWORD
            | ENV_BIND
            | ENV_TOKEN_CACHE
            | ENV_RT_TTL
            | ENV_STATIC_TTL
            | ENV_RAIL_BASE
            | ENV_BUS_BASE
            | ENV_ACCESS_KEY
    )
}

/// Parses a socket address, using the localhost default when unset.
///
/// # Arguments
///
/// * `raw` — Optional `NJT_PROXY_BIND` value.
///
/// # Returns
///
/// The listen address.
///
/// # Errors
///
/// Returns [`ConfigError`] when the value is not a socket address.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_gtfs_proxy::Config;
///
/// let config = Config::from_vars(None::<(&str, &str)>).expect("default bind");
/// assert_eq!(config.bind().to_string(), "127.0.0.1:8080");
/// ```
fn parse_bind(raw: Option<&str>) -> Result<SocketAddr, ConfigError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(DEFAULT_BIND);
    text.parse()
        .map_err(|_| ConfigError::new("NJT_PROXY_BIND is not a socket address"))
}

/// Requires both credential variables, or neither.
///
/// # Arguments
///
/// * `values` — Known environment values.
/// * `username_key` — Username variable name.
/// * `password_key` — Password variable name.
///
/// # Returns
///
/// Credentials, or `None` when both are unset.
///
/// # Errors
///
/// Returns [`ConfigError`] when only one of the pair is set, or when either
/// value is empty. The message names the missing variable and does not echo
/// the one that was set.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_gtfs_proxy::Config;
///
/// let err = Config::from_vars([("NJT_BUS_PASSWORD", "password")]).expect_err("partial");
/// assert!(err.to_string().contains("NJT_BUS_USERNAME"));
/// ```
fn credential_pair(
    values: &HashMap<String, String>,
    username_key: &str,
    password_key: &str,
) -> Result<Option<Credentials>, ConfigError> {
    let username = values
        .get(username_key)
        .map(String::as_str)
        .filter(|value| !value.is_empty());
    let password = values
        .get(password_key)
        .map(String::as_str)
        .filter(|value| !value.is_empty());
    match (username, password) {
        (None, None) => Ok(None),
        (Some(username), Some(password)) => Ok(Some(Credentials {
            username: Secret::new(username),
            password: Secret::new(password),
        })),
        (None, Some(_)) => Err(ConfigError::new(format!(
            "{username_key} is required when {password_key} is set"
        ))),
        (Some(_), None) => Err(ConfigError::new(format!(
            "{password_key} is required when {username_key} is set"
        ))),
    }
}

/// Validates an optional origin override.
///
/// # Arguments
///
/// * `raw` — The variable's value, if present.
/// * `name` — Variable name used in the error.
///
/// # Returns
///
/// The normalized origin, or `None` when unset.
///
/// # Errors
///
/// Returns [`ConfigError`] when the value is not an http(s) origin. The value
/// itself is not included.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_gtfs_proxy::Config;
///
/// let err = Config::from_vars([("NJT_RAIL_BASE_URL", "ftp://example.com")]).expect_err("scheme");
/// assert!(err.to_string().contains("NJT_RAIL_BASE_URL"));
/// ```
fn optional_base(raw: Option<&str>, name: &str) -> Result<Option<String>, ConfigError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    njt_client::normalize_base(raw)
        .map(Some)
        .map_err(|_| ConfigError::new(format!("{name} must be an http(s) origin without a path")))
}

/// Parses a TTL in seconds.
///
/// # Arguments
///
/// * `raw` — Variable value.
/// * `name` — Variable name used in errors.
/// * `default` — Value used when the variable is unset.
/// * `max` — Inclusive upper bound.
///
/// # Returns
///
/// The TTL. Zero disables that cache.
///
/// # Errors
///
/// Returns [`ConfigError`] when the value is not an integer in `0..=max`.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_gtfs_proxy::Config;
///
/// let config = Config::from_vars([("NJT_RT_TTL_SECS", "0")]).expect("ttl");
/// let _router = config.router().expect("router");
/// ```
fn parse_ttl(
    raw: Option<&str>,
    name: &str,
    default: u64,
    max: u64,
) -> Result<Duration, ConfigError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Duration::from_secs(default));
    };
    let seconds: u64 = raw
        .parse()
        .map_err(|_| ConfigError::new(format!("{name} must be a whole number of seconds")))?;
    if seconds > max {
        return Err(ConfigError::new(format!(
            "{name} must be between 0 and {max} seconds"
        )));
    }
    Ok(Duration::from_secs(seconds))
}
