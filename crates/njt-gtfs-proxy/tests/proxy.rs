//! HTTP behavior against a local mock. No NJ Transit credentials or feeds.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeZone, Utc};
use http_body_util::BodyExt;
use njt_client::ManualClock;
use njt_gtfs_proxy::Config;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn clock() -> Arc<ManualClock> {
    Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap(),
    ))
}

fn token() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "Authenticated": "True",
        "UserToken": "fake-token"
    }))
}

async fn app(
    server: &MockServer,
    extra: &[(&str, &str)],
    shared: Arc<ManualClock>,
) -> axum::Router {
    let mut pairs = vec![
        ("NJT_ENV".to_string(), "test".to_string()),
        (
            "NJT_RAIL_USERNAME".to_string(),
            "placeholder-user".to_string(),
        ),
        (
            "NJT_RAIL_PASSWORD".to_string(),
            "placeholder-password".to_string(),
        ),
        ("NJT_RAIL_BASE_URL".to_string(), server.uri()),
        ("NJT_RT_TTL_SECS".to_string(), "30".to_string()),
        ("NJT_STATIC_TTL_SECS".to_string(), "3600".to_string()),
    ];
    for (key, value) in extra {
        pairs.push(((*key).to_string(), (*value).to_string()));
    }
    Config::from_vars(pairs)
        .expect("config")
        .router_with_clock(shared)
        .expect("router")
}

async fn get(
    router: axum::Router,
    uri: &str,
    key: Option<&str>,
) -> (StatusCode, String, axum::http::HeaderMap) {
    let mut builder = Request::builder().uri(uri);
    if let Some(key) = key {
        builder = builder.header("x-njt-proxy-key", key);
    }
    let response = router
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    (status, text, headers)
}

async fn mount_rail(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(token())
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getGTFS"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PK\x03\x04fake-zip".as_slice()))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getTripUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x0afake-trips".as_slice()))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getVehiclePositions"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x0afake-vehicles".as_slice()))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getAlerts"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x0afake-alerts".as_slice()))
        .mount(server)
        .await;
}

#[tokio::test]
async fn healthz_reports_enabled_apis() {
    let server = MockServer::start().await;
    let router = app(&server, &[], clock()).await;
    let (status, body, _) = get(router, "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"ok\":true"));
    assert!(body.contains("\"rail\":true"));
    assert!(body.contains("\"bus\":false"));
}

#[tokio::test]
async fn rail_feeds_use_zip_and_protobuf_types() {
    let server = MockServer::start().await;
    mount_rail(&server).await;
    let shared = clock();
    let router = app(&server, &[], Arc::clone(&shared)).await;
    let (status, body, headers) = get(router.clone(), "/rail/gtfs.zip", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_bytes(), b"PK\x03\x04fake-zip");
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "application/zip"
    );
    assert_eq!(headers.get("x-cache").unwrap(), "miss");

    let (status, body, headers) = get(router.clone(), "/rail/trip-updates.pb", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_bytes(), b"\x0afake-trips");
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "application/x-protobuf"
    );

    let (status, _, headers) = get(router.clone(), "/rail/vehicle-positions.pb", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "application/x-protobuf"
    );
    let (status, _, _) = get(router, "/rail/alerts.pb", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn response_cache_collapses_repeat_and_concurrent_gets() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(token())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getTripUpdates"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"\x0afake-trips".as_slice())
                .set_delay(std::time::Duration::from_millis(40)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let router = app(&server, &[], clock()).await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let router = router.clone();
        tasks.push(tokio::spawn(async move {
            get(router, "/rail/trip-updates.pb", None).await
        }));
    }
    for task in tasks {
        let (status, body, _) = task.await.expect("join");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.as_bytes(), b"\x0afake-trips");
    }
    let (status, _, headers) = get(router, "/rail/trip-updates.pb", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("x-cache").unwrap(), "hit");
    server.verify().await;
}

#[tokio::test]
async fn realtime_cache_expires_on_the_shared_clock() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(token())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getAlerts"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x0afake-alerts".as_slice()))
        .expect(2)
        .mount(&server)
        .await;
    let shared = clock();
    let router = app(&server, &[], Arc::clone(&shared)).await;
    let (status, _, headers) = get(router.clone(), "/rail/alerts.pb", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("x-cache").unwrap(), "miss");
    shared.advance(std::time::Duration::from_secs(31));
    let (_, _, headers) = get(router, "/rail/alerts.pb", None).await;
    assert_eq!(headers.get("x-cache").unwrap(), "miss");
    server.verify().await;
}

#[tokio::test]
async fn json_and_null_bodies_are_not_served_as_feeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(token())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getGTFS"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "errorMessage": "GTFS error"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getAlerts"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Null"))
        .mount(&server)
        .await;
    let router = app(&server, &[], clock()).await;
    let (status, body, headers) = get(router.clone(), "/rail/gtfs.zip", None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "application/json"
    );
    assert!(body.contains("GTFS error"));
    assert!(!body.as_bytes().starts_with(b"PK"));
    let (status, body, headers) = get(router, "/rail/alerts.pb", None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "application/json"
    );
    assert!(body.contains("Null"));
}

#[tokio::test]
async fn daily_limit_is_a_clear_503() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "errorMessage": "Daily usage limit:10. Your current daily usage: 11"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let router = app(&server, &[], clock()).await;
    let (status, body, _) = get(router.clone(), "/rail/gtfs.zip", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("njt_token_quota"));
    assert!(body.contains("midnight"));
    assert!(body.contains("America/New_York"));
    let (status, _, _) = get(router, "/rail/trip-updates.pb", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    server.verify().await;
}

#[tokio::test]
async fn access_key_gates_feeds_but_not_healthz() {
    let server = MockServer::start().await;
    mount_rail(&server).await;
    let router = app(
        &server,
        &[("NJT_PROXY_ACCESS_KEY", "placeholder-key")],
        clock(),
    )
    .await;
    let (status, _, _) = get(router.clone(), "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = get(router.clone(), "/rail/gtfs.zip", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = get(router.clone(), "/rail/gtfs.zip", Some("nope")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body, _) = get(router, "/rail/gtfs.zip", Some("placeholder-key")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_bytes(), b"PK\x03\x04fake-zip");
}

#[tokio::test]
async fn unconfigured_bus_is_404_and_bus_routes_work_when_configured() {
    let server = MockServer::start().await;
    mount_rail(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSG2/authenticateUser"))
        .respond_with(token())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSG2/getGTFS"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PK\x03\x04bus-zip".as_slice()))
        .mount(&server)
        .await;
    let router = app(&server, &[], clock()).await;
    let (status, body, _) = get(router, "/bus/gtfs.zip", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("not_configured"));

    let bus_base = server.uri();
    let router = app(
        &server,
        &[
            ("NJT_BUS_USERNAME", "placeholder-user"),
            ("NJT_BUS_PASSWORD", "placeholder-password"),
            ("NJT_BUS_BASE_URL", bus_base.as_str()),
        ],
        clock(),
    )
    .await;
    let (status, body, headers) = get(router, "/bus/gtfs.zip", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_bytes(), b"PK\x03\x04bus-zip");
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "application/zip"
    );
}

#[tokio::test]
async fn partial_credentials_fail_closed() {
    let err = Config::from_vars([("NJT_RAIL_USERNAME", "placeholder-user")]).expect_err("partial");
    assert!(err.to_string().contains("NJT_RAIL_PASSWORD"));
    assert!(!err.to_string().contains("placeholder-user"));
}

#[tokio::test]
async fn proxy_token_cache_is_mode_600() {
    let server = MockServer::start().await;
    mount_rail(&server).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tokens.json");
    let router = app(
        &server,
        &[("NJT_TOKEN_CACHE", path.to_str().unwrap())],
        clock(),
    )
    .await;
    let (status, _, _) = get(router, "/rail/alerts.pb", None).await;
    assert_eq!(status, StatusCode::OK);
    let raw = std::fs::read_to_string(&path).expect("cache");
    assert!(raw.contains("fake-token"));
    assert!(!raw.contains("placeholder-password"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
