//! Plain GET routes in front of the NJ Transit client.
//!
//! `/healthz` is unauthenticated. Feed routes require
//! [`ACCESS_KEY_HEADER`] when an access key is configured.

use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use njt_client::{constant_time_eq, ApiKind, Client, Error, Feed, Secret};
use serde::Serialize;

use crate::cache::FeedCache;

/// Header clients must send when `NJT_PROXY_ACCESS_KEY` is set.
///
/// gtfs-mcp can send it with `GTFS_RT_MCP_HTTP_HEADERS`.
pub const ACCESS_KEY_HEADER: &str = "x-njt-proxy-key";

/// Router state. Cheap to clone because the clients and cache are reference-counted.
#[derive(Clone)]
pub(crate) struct AppState {
    /// Rail client, when credentials were configured.
    pub(crate) rail: Option<Client>,
    /// Bus client, when credentials were configured.
    pub(crate) bus: Option<Client>,
    /// Shared response cache.
    pub(crate) cache: FeedCache,
    /// Expected access key. `None` leaves feed routes open.
    pub(crate) access_key: Option<Secret>,
}

/// Liveness body.
#[derive(Serialize)]
struct HealthBody {
    /// Always `true` when the process handled the request.
    ok: bool,
    /// Whether `/rail/*` is enabled.
    rail: bool,
    /// Whether `/bus/*` is enabled.
    bus: bool,
}

/// Builds the public router.
///
/// # Arguments
///
/// * `state` — Clients, cache, and optional access key.
///
/// # Returns
///
/// A router with `/healthz` and the eight feed paths.
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
/// let config = Config::from_vars([
///     ("NJT_RAIL_USERNAME", "username"),
///     ("NJT_RAIL_PASSWORD", "password"),
/// ])
/// .expect("config");
/// let _router = config.router().expect("router");
/// ```
pub(crate) fn router_from_state(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/rail/gtfs.zip", get(rail_gtfs))
        .route("/rail/trip-updates.pb", get(rail_trips))
        .route("/rail/vehicle-positions.pb", get(rail_vehicles))
        .route("/rail/alerts.pb", get(rail_alerts))
        .route("/bus/gtfs.zip", get(bus_gtfs))
        .route("/bus/trip-updates.pb", get(bus_trips))
        .route("/bus/vehicle-positions.pb", get(bus_vehicles))
        .route("/bus/alerts.pb", get(bus_alerts))
        .with_state(state)
}

/// Reports which APIs are configured.
///
/// # Arguments
///
/// * `state` — Router state. The access key is not required.
///
/// # Returns
///
/// HTTP 200 and a JSON object `{ok, rail, bus}`.
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
/// let _router = Config::from_vars(None::<(&str, &str)>)
///     .expect("config")
///     .router()
///     .expect("router");
/// ```
async fn healthz(State(state): State<AppState>) -> Json<HealthBody> {
    Json(HealthBody {
        ok: true,
        rail: state.rail.is_some(),
        bus: state.bus.is_some(),
    })
}

/// Serves the rail static zip.
///
/// # Arguments
///
/// * `state` — Router state.
/// * `headers` — Request headers. May include [`ACCESS_KEY_HEADER`].
///
/// # Returns
///
/// The zip response, or a JSON error.
///
/// # Errors
///
/// This function does not return an error. Upstream failures become HTTP responses.
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
/// let _ = Config::from_vars([
///     ("NJT_RAIL_USERNAME", "username"),
///     ("NJT_RAIL_PASSWORD", "password"),
/// ])
/// .expect("config")
/// .router();
/// ```
async fn rail_gtfs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    serve(&state, ApiKind::Rail, Feed::Static, &headers).await
}

/// Serves rail trip updates.
///
/// # Arguments
///
/// * `state` — Router state.
/// * `headers` — Request headers.
///
/// # Returns
///
/// A protobuf response, or a JSON error.
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
/// See [`rail_gtfs`].
///
/// ```
/// use njt_gtfs_proxy::ACCESS_KEY_HEADER;
///
/// assert_eq!(ACCESS_KEY_HEADER, "x-njt-proxy-key");
/// ```
async fn rail_trips(State(state): State<AppState>, headers: HeaderMap) -> Response {
    serve(&state, ApiKind::Rail, Feed::TripUpdates, &headers).await
}

/// Serves rail vehicle positions.
///
/// # Arguments
///
/// * `state` — Router state.
/// * `headers` — Request headers.
///
/// # Returns
///
/// A protobuf response, or a JSON error.
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
/// use njt_gtfs_proxy::ACCESS_KEY_HEADER;
///
/// assert_eq!(ACCESS_KEY_HEADER, "x-njt-proxy-key");
/// ```
async fn rail_vehicles(State(state): State<AppState>, headers: HeaderMap) -> Response {
    serve(&state, ApiKind::Rail, Feed::VehiclePositions, &headers).await
}

/// Serves rail alerts.
///
/// # Arguments
///
/// * `state` — Router state.
/// * `headers` — Request headers.
///
/// # Returns
///
/// A protobuf response, or a JSON error.
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
/// use njt_gtfs_proxy::ACCESS_KEY_HEADER;
///
/// assert_eq!(ACCESS_KEY_HEADER, "x-njt-proxy-key");
/// ```
async fn rail_alerts(State(state): State<AppState>, headers: HeaderMap) -> Response {
    serve(&state, ApiKind::Rail, Feed::Alerts, &headers).await
}

/// Serves the bus static zip.
///
/// # Arguments
///
/// * `state` — Router state.
/// * `headers` — Request headers.
///
/// # Returns
///
/// The zip response, or a JSON error.
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
/// let config = Config::from_vars([
///     ("NJT_BUS_USERNAME", "username"),
///     ("NJT_BUS_PASSWORD", "password"),
/// ])
/// .expect("config");
/// assert!(config.bus_enabled());
/// ```
async fn bus_gtfs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    serve(&state, ApiKind::Bus, Feed::Static, &headers).await
}

/// Serves bus trip updates.
///
/// # Arguments
///
/// * `state` — Router state.
/// * `headers` — Request headers.
///
/// # Returns
///
/// A protobuf response, or a JSON error.
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
/// use njt_gtfs_proxy::ACCESS_KEY_HEADER;
///
/// assert_eq!(ACCESS_KEY_HEADER, "x-njt-proxy-key");
/// ```
async fn bus_trips(State(state): State<AppState>, headers: HeaderMap) -> Response {
    serve(&state, ApiKind::Bus, Feed::TripUpdates, &headers).await
}

/// Serves bus vehicle positions.
///
/// # Arguments
///
/// * `state` — Router state.
/// * `headers` — Request headers.
///
/// # Returns
///
/// A protobuf response, or a JSON error.
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
/// use njt_gtfs_proxy::ACCESS_KEY_HEADER;
///
/// assert_eq!(ACCESS_KEY_HEADER, "x-njt-proxy-key");
/// ```
async fn bus_vehicles(State(state): State<AppState>, headers: HeaderMap) -> Response {
    serve(&state, ApiKind::Bus, Feed::VehiclePositions, &headers).await
}

/// Serves bus alerts.
///
/// The bus PDF spells one production sample `getALerts`. This route calls the
/// swagger path `getAlerts`.
///
/// # Arguments
///
/// * `state` — Router state.
/// * `headers` — Request headers.
///
/// # Returns
///
/// A protobuf response, or a JSON error.
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
/// use njt_gtfs_proxy::ACCESS_KEY_HEADER;
///
/// assert_eq!(ACCESS_KEY_HEADER, "x-njt-proxy-key");
/// ```
async fn bus_alerts(State(state): State<AppState>, headers: HeaderMap) -> Response {
    serve(&state, ApiKind::Bus, Feed::Alerts, &headers).await
}

/// Authorizes the request, then returns a cached or freshly downloaded feed.
///
/// # Arguments
///
/// * `state` — Router state.
/// * `api` — Rail or bus.
/// * `feed` — Which file.
/// * `headers` — Request headers.
///
/// # Returns
///
/// An HTTP response. Success uses `application/zip` or `application/x-protobuf`.
/// Failures use `application/json`.
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
/// use njt_client::Feed;
///
/// assert_eq!(Feed::Static.content_type(), "application/zip");
/// assert_eq!(Feed::TripUpdates.content_type(), "application/x-protobuf");
/// ```
async fn serve(state: &AppState, api: ApiKind, feed: Feed, headers: &HeaderMap) -> Response {
    if let Some(expected) = &state.access_key {
        let provided = headers
            .get(ACCESS_KEY_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if !constant_time_eq(expected.expose().as_bytes(), provided.as_bytes()) {
            return json_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "send the x-njt-proxy-key header",
                None,
            );
        }
    }
    let Some(client) = client_for(state, api) else {
        return json_error(
            StatusCode::NOT_FOUND,
            "not_configured",
            &format!("{api} credentials are not configured"),
            None,
        );
    };
    let fetched = state
        .cache
        .get_or_fetch(api, feed, || {
            let client = client.clone();
            async move { client.fetch(feed).await }
        })
        .await;
    match fetched {
        Ok((body, hit)) => feed_response(feed, body, hit, ttl_of(state, feed)),
        Err(err) => {
            tracing::warn!(api = %api, feed = %feed, error = %err, "feed request failed");
            error_response(&err)
        }
    }
}

/// Returns the client for `api`.
///
/// # Arguments
///
/// * `state` — Router state.
/// * `api` — Rail or bus.
///
/// # Returns
///
/// The client, or `None` when that API is disabled.
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
/// let config = Config::from_vars(None::<(&str, &str)>).expect("empty");
/// assert!(!config.rail_enabled());
/// assert!(!config.bus_enabled());
/// ```
fn client_for(state: &AppState, api: ApiKind) -> Option<&Client> {
    match api {
        ApiKind::Rail => state.rail.as_ref(),
        ApiKind::Bus => state.bus.as_ref(),
    }
}

/// Picks the TTL to advertise for `feed`.
///
/// # Arguments
///
/// * `state` — Router state. Only the cache TTLs are read.
/// * `feed` — Which file was served.
///
/// # Returns
///
/// The static TTL for the zip and the realtime TTL otherwise.
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
/// use njt_client::Feed;
///
/// assert_eq!(Feed::Alerts.as_str(), "alerts");
/// ```
fn ttl_of(state: &AppState, feed: Feed) -> Duration {
    match feed {
        Feed::Static => state.cache.static_ttl(),
        Feed::TripUpdates | Feed::VehiclePositions | Feed::Alerts => state.cache.realtime_ttl(),
    }
}

/// Builds a successful feed response.
///
/// # Arguments
///
/// * `feed` — Selects the content type.
/// * `body` — Payload bytes.
/// * `hit` — Whether the response cache supplied the body.
/// * `ttl` — Value used for `Cache-Control: max-age`.
///
/// # Returns
///
/// HTTP 200 with the feed bytes.
///
/// # Errors
///
/// This function does not return an error. Header construction uses checked
/// integers and static names.
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
/// assert_eq!(Feed::Static.content_type(), "application/zip");
/// ```
fn feed_response(feed: Feed, body: bytes::Bytes, hit: bool, ttl: Duration) -> Response {
    let cache_control = format!("max-age={}", ttl.as_secs());
    let hit_label = if hit { "hit" } else { "miss" };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, feed.content_type())
        .header(header::CACHE_CONTROL, cache_control)
        .header("x-cache", hit_label)
        .body(Body::from(body))
        .unwrap_or_else(|_| {
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "could not build the feed response",
                None,
            )
        })
}

/// Maps a client error to an HTTP response.
///
/// # Arguments
///
/// * `err` — Failure from [`Client::fetch`](njt_client::Client::fetch). Only its
///   display text is shown.
///
/// # Returns
///
/// HTTP 503 for quota and backoff, otherwise HTTP 502. Daily-limit and local-cap
/// bodies say the quota resets at midnight America/New_York.
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
/// assert!(Error::DailyLimit.is_quota_block());
/// assert!(Error::DailyLimit.to_string().contains("midnight"));
/// ```
fn error_response(err: &Error) -> Response {
    let (status, code, retry_after) = match err {
        Error::DailyLimit | Error::LocalCap { .. } => {
            (StatusCode::SERVICE_UNAVAILABLE, "njt_token_quota", None)
        }
        Error::Backoff { retry_after } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "njt_token_backoff",
            Some(*retry_after),
        ),
        Error::Cache { .. } => (StatusCode::SERVICE_UNAVAILABLE, "token_cache", None),
        Error::AuthRejected => (StatusCode::BAD_GATEWAY, "njt_auth_rejected", None),
        Error::InvalidToken => (StatusCode::BAD_GATEWAY, "njt_invalid_token", None),
        Error::NullBody => (StatusCode::BAD_GATEWAY, "njt_null_body", None),
        Error::Upstream { .. } => (StatusCode::BAD_GATEWAY, "njt_upstream", None),
        Error::UnexpectedPayload { .. } => {
            (StatusCode::BAD_GATEWAY, "njt_unexpected_payload", None)
        }
        Error::Transport(_)
        | Error::Status { .. }
        | Error::TooLarge { .. }
        | Error::Config { .. } => (StatusCode::BAD_GATEWAY, "njt_upstream", None),
    };
    json_error(status, code, &err.to_string(), retry_after)
}

/// Builds a small JSON error response.
///
/// # Arguments
///
/// * `status` — HTTP status.
/// * `code` — Stable machine-readable code.
/// * `message` — Operator-facing text. Must not contain secrets.
/// * `retry_after` — Optional `Retry-After` value.
///
/// # Returns
///
/// The response.
///
/// # Errors
///
/// This function does not return an error.
///
/// # Panics
///
/// This function does not panic. JSON serialization of this fixed shape does
/// not fail; a builder failure becomes an empty 500 body.
///
/// # Examples
///
/// ```
/// use njt_client::Error;
///
/// let err = Error::NullBody;
/// assert!(err.to_string().contains("Null"));
/// ```
fn json_error(
    status: StatusCode,
    code: &str,
    message: &str,
    retry_after: Option<Duration>,
) -> Response {
    let payload = serde_json::json!({
        "error": code,
        "message": message,
    });
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(retry_after) = retry_after {
        let seconds = retry_after.as_secs().max(1);
        builder = builder.header(header::RETRY_AFTER, seconds.to_string());
    }
    builder
        .body(Body::from(payload.to_string()))
        .unwrap_or_else(|_| Response::new(Body::from("{\"error\":\"internal\"}")))
}
