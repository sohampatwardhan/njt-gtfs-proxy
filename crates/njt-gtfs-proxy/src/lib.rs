//! Local HTTP proxy that turns NJ Transit token-authenticated POSTs into plain GETs.
//!
//! GTFS clients, including [gtfs-mcp](https://github.com/sohampatwardhan/gtfs-mcp),
//! fetch a zip and three protobuf URLs. NJ Transit's rail and bus developer APIs
//! instead require `POST` multipart form calls and a token. This process holds
//! the credentials, caches the token, and serves:
//!
//! - `GET /rail/gtfs.zip`, `/rail/trip-updates.pb`, `/rail/vehicle-positions.pb`, `/rail/alerts.pb`
//! - the same paths under `/bus/`
//! - `GET /healthz`
//!
//! An API is enabled only when both its username and password are set. The
//! listen address defaults to `127.0.0.1:8080`. Publishing the port shares the
//! owner's NJ Transit quota with anyone who can reach it. Set
//! `NJT_PROXY_ACCESS_KEY` and send it as `x-njt-proxy-key` when the proxy is
//! reachable beyond the machine.
//!
//! # Module map
//!
//! - [`config`] — environment variables and [`Config`].
//! - [`http`] — axum routes and [`ACCESS_KEY_HEADER`].
//! - [`cache`] — [`FeedCache`], the short TTL in front of NJ Transit.
//!
//! The token manager and upstream client live in [`njt_client`] so another
//! binary can reuse them without this HTTP server.
//!
//! # Examples
//!
//! ```
//! use njt_gtfs_proxy::Config;
//!
//! let config = Config::from_vars([
//!     ("NJT_ENV", "test"),
//!     ("NJT_RAIL_USERNAME", "username"),
//!     ("NJT_RAIL_PASSWORD", "password"),
//! ])
//! .expect("config");
//! let _router = config.router().expect("router");
//! assert!(config.rail_enabled());
//! ```
//!
//! Listening on the configured address reads the process environment:
//!
//! ```no_run
//! # async fn demo() -> Result<(), njt_gtfs_proxy::ConfigError> {
//! njt_gtfs_proxy::run_from_env().await?;
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]
#![deny(clippy::missing_docs_in_private_items)]
#![deny(clippy::missing_errors_doc)]
#![deny(clippy::missing_panics_doc)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod cache;
pub mod config;
pub mod http;

pub use cache::FeedCache;
pub use config::{Config, ConfigError};
pub use http::ACCESS_KEY_HEADER;

use std::net::SocketAddr;

/// Reads the environment, binds [`Config::bind`], and serves until SIGINT or SIGTERM.
///
/// # Arguments
///
/// This function takes no arguments.
///
/// # Returns
///
/// `Ok(())` after a graceful shutdown. The function waits until the process is
/// signalled.
///
/// # Errors
///
/// Returns [`ConfigError`] when configuration is invalid, the token cache
/// cannot be opened, or the socket cannot be bound. The message does not
/// include credentials or tokens.
///
/// # Panics
///
/// This function does not panic. A second tracing subscriber is left in place
/// when one is already installed.
///
/// # Examples
///
/// ```no_run
/// # async fn demo() -> Result<(), njt_gtfs_proxy::ConfigError> {
/// njt_gtfs_proxy::run_from_env().await?;
/// # Ok(())
/// # }
/// ```
pub async fn run_from_env() -> Result<(), ConfigError> {
    init_tracing();
    let config = Config::from_env()?;
    run(config).await
}

/// Serves `config` until the process is asked to stop.
///
/// # Arguments
///
/// * `config` — Parsed environment. Rail and bus clients are built from it.
///
/// # Returns
///
/// `Ok(())` after graceful shutdown.
///
/// # Errors
///
/// Returns [`ConfigError`] when the router cannot be built or the bind fails.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```no_run
/// use njt_gtfs_proxy::{run, Config};
///
/// # async fn demo() -> Result<(), njt_gtfs_proxy::ConfigError> {
/// let config = Config::from_vars([
///     ("NJT_RAIL_USERNAME", "username"),
///     ("NJT_RAIL_PASSWORD", "password"),
///     ("NJT_PROXY_BIND", "127.0.0.1:8080"),
/// ])?;
/// run(config).await?;
/// # Ok(())
/// # }
/// ```
pub async fn run(config: Config) -> Result<(), ConfigError> {
    let addr = config.bind();
    tracing::info!(
        %addr,
        environment = config.environment(),
        rail = config.rail_enabled(),
        bus = config.bus_enabled(),
        access_key = config.access_key_required(),
        "listening"
    );
    let app = config.router()?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| ConfigError::bind(addr, err))?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ConfigError::serve)?;
    Ok(())
}

/// Installs a stderr tracing subscriber when one is not already set.
///
/// # Arguments
///
/// This function takes no arguments.
///
/// # Returns
///
/// Nothing. `RUST_LOG` selects the filter, otherwise `info`.
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
/// Tracing is installed by [`run_from_env`]. Building a router does not require it:
///
/// ```
/// use njt_gtfs_proxy::Config;
///
/// let _ = Config::from_vars(None::<(&str, &str)>).expect("config").router();
/// ```
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

/// Waits for Ctrl-C or, on Unix, SIGTERM.
///
/// # Arguments
///
/// This function takes no arguments.
///
/// # Returns
///
/// Nothing, once a shutdown signal arrives. If the signal handler cannot be
/// installed, the function returns immediately so the process can exit.
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
/// Shutdown is private. [`run`] is the public server entry point:
///
/// ```no_run
/// # async fn demo() -> Result<(), njt_gtfs_proxy::ConfigError> {
/// njt_gtfs_proxy::run_from_env().await
/// # }
/// ```
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "could not install the SIGTERM handler");
                }
            }
        };
        tokio::select! {
            () = ctrl_c => {}
            () = terminate => {}
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await;
    }
}

impl ConfigError {
    /// Describes a bind failure without including credentials.
    ///
    /// # Arguments
    ///
    /// * `addr` — Address that could not be bound.
    /// * `err` — The I/O error.
    ///
    /// # Returns
    ///
    /// A configuration error.
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
    /// Bind failures are reported by [`run`]. An invalid address fails earlier:
    ///
    /// ```
    /// use njt_gtfs_proxy::Config;
    ///
    /// assert!(Config::from_vars([("NJT_PROXY_BIND", "nope")]).is_err());
    /// ```
    pub(crate) fn bind(addr: SocketAddr, err: std::io::Error) -> Self {
        Self::new(format!("could not bind {addr}: {err}"))
    }

    /// Describes a server failure.
    ///
    /// # Arguments
    ///
    /// * `err` — Error from the HTTP server.
    ///
    /// # Returns
    ///
    /// A configuration error whose text is the server error's display.
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
    /// let _config = Config::from_vars(None::<(&str, &str)>).expect("config");
    /// ```
    pub(crate) fn serve(err: std::io::Error) -> Self {
        Self::new(format!("HTTP server stopped: {err}"))
    }
}
