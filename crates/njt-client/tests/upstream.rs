//! Upstream behavior against a local mock. No NJ Transit credentials or feeds.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use njt_client::{
    next_midnight_eastern, ApiKind, Client, Clock, Error, Feed, ManualClock, TokenPolicy,
    TokenStore, BUS_PRODUCTION_BASE,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn clock() -> Arc<ManualClock> {
    Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap(),
    ))
}

fn client(
    api: ApiKind,
    base: &str,
    store: Arc<TokenStore>,
    clock: Arc<ManualClock>,
    policy: TokenPolicy,
) -> Client {
    Client::builder(api, base)
        .credentials("placeholder-user", "placeholder-password")
        .store(store)
        .clock(clock)
        .policy(policy)
        .build()
        .expect("client")
}

fn token_body(token: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "Authenticated": "True",
        "UserToken": token
    }))
}

async fn count(server: &MockServer, suffix: &str) -> usize {
    server
        .received_requests()
        .await
        .expect("requests")
        .into_iter()
        .filter(|request| request.url.path().ends_with(suffix))
        .count()
}

#[tokio::test]
async fn rail_mints_once_and_reuses_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(token_body("fake-token"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getGTFS"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PK\x03\x04fake-zip".as_slice()))
        .expect(2)
        .mount(&server)
        .await;

    let store = TokenStore::memory();
    let shared = clock();
    let rail = client(
        ApiKind::Rail,
        &server.uri(),
        store,
        shared,
        TokenPolicy::default(),
    );
    let first = rail.fetch(Feed::Static).await.expect("first");
    let second = rail.fetch(Feed::Static).await.expect("second");
    assert_eq!(first.as_ref(), b"PK\x03\x04fake-zip");
    assert_eq!(second, first);
    server.verify().await;
    assert_eq!(count(&server, "isValidToken").await, 0);
}

#[tokio::test]
async fn token_file_is_private_and_survives_restart() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(token_body("fake-token"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getTripUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x0afake-protobuf".as_slice()))
        .expect(2)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tokens.json");
    let shared = clock();
    let first = client(
        ApiKind::Rail,
        &server.uri(),
        TokenStore::open(&path).expect("open"),
        Arc::clone(&shared),
        TokenPolicy::default(),
    );
    first.fetch(Feed::TripUpdates).await.expect("fetch");
    drop(first);

    let raw = std::fs::read_to_string(&path).expect("cache");
    assert!(raw.contains("fake-token"));
    assert!(!raw.contains("placeholder-password"));
    assert!(!raw.contains("placeholder-user"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let lock = dir.path().join("tokens.json.lock");
        let lock_mode = std::fs::metadata(lock).expect("lock").permissions().mode() & 0o777;
        assert_eq!(lock_mode, 0o600);
    }

    let second = client(
        ApiKind::Rail,
        &server.uri(),
        TokenStore::open(&path).expect("reopen"),
        shared,
        TokenPolicy::default(),
    );
    second.fetch(Feed::TripUpdates).await.expect("reuse");
    server.verify().await;
}

#[tokio::test]
async fn invalid_token_remints_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(token_body("fake-token"))
        .expect(2)
        .mount(&server)
        .await;
    let calls = std::sync::atomic::AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getVehiclePositions"))
        .respond_with(move |_request: &wiremock::Request| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"errorMessage": "Invalid token."}))
            } else {
                ResponseTemplate::new(200).set_body_bytes(b"\x0afake-protobuf".as_slice())
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let rail = client(
        ApiKind::Rail,
        &server.uri(),
        TokenStore::memory(),
        clock(),
        TokenPolicy::default(),
    );
    let body = rail.fetch(Feed::VehiclePositions).await.expect("refreshed");
    assert_eq!(body.as_ref(), b"\x0afake-protobuf");
    server.verify().await;
}

#[tokio::test]
async fn daily_limit_stops_further_token_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "errorMessage": "Daily usage limit:10. Your current daily usage: 11"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let rail = client(
        ApiKind::Rail,
        &server.uri(),
        TokenStore::memory(),
        clock(),
        TokenPolicy::default(),
    );
    let first = rail.fetch(Feed::Alerts).await.expect_err("limit");
    let second = rail.fetch(Feed::Alerts).await.expect_err("still limited");
    assert!(matches!(first, Error::DailyLimit));
    assert!(matches!(second, Error::DailyLimit));
    assert!(first.to_string().contains("midnight"));
    server.verify().await;
    assert_eq!(count(&server, "getAlerts").await, 0);
}

#[tokio::test]
async fn local_cap_blocks_the_third_mint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSG2/authenticateUser"))
        .respond_with(token_body("fake-token"))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSG2/getGTFS"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "errorMessage": "Invalid token."
        })))
        .mount(&server)
        .await;

    let policy = TokenPolicy {
        daily_cap: 2,
        ..TokenPolicy::default()
    };
    let bus = client(
        ApiKind::Bus,
        &server.uri(),
        TokenStore::memory(),
        clock(),
        policy,
    );
    let first = bus
        .fetch(Feed::Static)
        .await
        .expect_err("both tokens rejected");
    assert!(matches!(first, Error::InvalidToken));
    let second = bus.fetch(Feed::Static).await.expect_err("cap");
    assert!(matches!(second, Error::LocalCap { cap: 2 }));
    server.verify().await;
}

#[tokio::test]
async fn failed_mint_backs_off_without_another_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSG2/authenticateUser"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Authenticated": "False",
            "UserToken": ""
        })))
        .expect(2)
        .mount(&server)
        .await;

    let shared = clock();
    let bus = client(
        ApiKind::Bus,
        &server.uri(),
        TokenStore::memory(),
        Arc::clone(&shared),
        TokenPolicy::default(),
    );
    assert!(matches!(
        bus.fetch(Feed::Alerts).await.expect_err("auth"),
        Error::AuthRejected
    ));
    assert!(matches!(
        bus.fetch(Feed::Alerts).await.expect_err("backoff"),
        Error::Backoff { .. }
    ));
    assert_eq!(count(&server, "authenticateUser").await, 1);
    shared.advance(std::time::Duration::from_secs(2));
    assert!(matches!(
        bus.fetch(Feed::Alerts).await.expect_err("retry"),
        Error::AuthRejected
    ));
    server.verify().await;
}

#[tokio::test]
async fn null_token_body_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Null"))
        .expect(1)
        .mount(&server)
        .await;
    let rail = client(
        ApiKind::Rail,
        &server.uri(),
        TokenStore::memory(),
        clock(),
        TokenPolicy::default(),
    );
    assert!(matches!(
        rail.fetch(Feed::Static).await.expect_err("null"),
        Error::NullBody
    ));
    server.verify().await;
}

#[tokio::test]
async fn gtfs_error_is_not_served_and_does_not_remint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSG2/authenticateUser"))
        .respond_with(token_body("fake-token"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSG2/getGTFS"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "errorMessage": "GTFS error"
        })))
        .expect(2)
        .mount(&server)
        .await;
    let bus = client(
        ApiKind::Bus,
        &server.uri(),
        TokenStore::memory(),
        clock(),
        TokenPolicy::default(),
    );
    let err = bus.fetch(Feed::Static).await.expect_err("gtfs");
    assert!(matches!(err, Error::Upstream { .. }));
    assert!(err.to_string().contains("GTFS error"));
    let again = bus.fetch(Feed::Static).await.expect_err("again");
    assert!(matches!(again, Error::Upstream { .. }));
    server.verify().await;
    let _ = BUS_PRODUCTION_BASE;
}

#[tokio::test]
async fn concurrent_fetches_share_one_mint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(token_body("fake-token").set_delay(std::time::Duration::from_millis(50)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getAlerts"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x0afake-protobuf".as_slice()))
        .mount(&server)
        .await;
    let rail = Arc::new(client(
        ApiKind::Rail,
        &server.uri(),
        TokenStore::memory(),
        clock(),
        TokenPolicy::default(),
    ));
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let rail = Arc::clone(&rail);
        tasks.push(tokio::spawn(async move { rail.fetch(Feed::Alerts).await }));
    }
    for task in tasks {
        let body = task.await.expect("join").expect("fetch");
        assert_eq!(body.as_ref(), b"\x0afake-protobuf");
    }
    server.verify().await;
}

#[tokio::test]
async fn bus_uses_authenticate_user_and_get_alerts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSG2/authenticateUser"))
        .respond_with(token_body("fake-token"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSG2/getAlerts"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x0afake-protobuf".as_slice()))
        .expect(1)
        .mount(&server)
        .await;
    let bus = client(
        ApiKind::Bus,
        &server.uri(),
        TokenStore::memory(),
        clock(),
        TokenPolicy::default(),
    );
    bus.fetch(Feed::Alerts).await.expect("alerts");
    server.verify().await;
    assert_eq!(count(&server, "getALerts").await, 0);
}

#[tokio::test]
async fn rail_token_is_refreshed_at_the_next_eastern_midnight() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(token_body("fake-token"))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getAlerts"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x0afake-protobuf".as_slice()))
        .expect(2)
        .mount(&server)
        .await;
    let shared = clock();
    let rail = client(
        ApiKind::Rail,
        &server.uri(),
        TokenStore::memory(),
        Arc::clone(&shared),
        TokenPolicy::default(),
    );
    rail.fetch(Feed::Alerts).await.expect("first day");
    let midnight = next_midnight_eastern(shared.now()).expect("midnight");
    shared.set(midnight);
    rail.fetch(Feed::Alerts).await.expect("next day");
    server.verify().await;
}

#[tokio::test]
async fn form_fields_are_username_password_and_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getToken"))
        .respond_with(token_body("fake-token"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/GTFSRT/getGTFS"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PK\x03\x04fake-zip".as_slice()))
        .mount(&server)
        .await;
    let rail = client(
        ApiKind::Rail,
        &server.uri(),
        TokenStore::memory(),
        clock(),
        TokenPolicy::default(),
    );
    rail.fetch(Feed::Static).await.expect("zip");
    let requests = server.received_requests().await.expect("requests");
    let auth = requests
        .iter()
        .find(|request| request.url.path().ends_with("getToken"))
        .expect("auth");
    let auth_body = String::from_utf8_lossy(&auth.body);
    assert!(auth_body.contains("name=\"username\""));
    assert!(auth_body.contains("placeholder-user"));
    assert!(auth_body.contains("name=\"password\""));
    assert!(auth_body.contains("placeholder-password"));
    let data = requests
        .iter()
        .find(|request| request.url.path().ends_with("getGTFS"))
        .expect("data");
    let data_body = String::from_utf8_lossy(&data.body);
    assert!(data_body.contains("name=\"token\""));
    assert!(data_body.contains("fake-token"));
    assert!(!data_body.contains("placeholder-password"));
}
