//! Client for NJ Transit's rail and bus GTFS developer APIs.
//!
//! NJ Transit serves GTFS and GTFS-Realtime only as `POST` multipart form
//! calls, and the token methods are quota-limited. This crate hides that behind
//! [`Client::fetch`], which returns zip or protobuf bytes a plain GET client
//! can store. Rail and bus are separate hosts with separate credentials. Test
//! and production credentials are issued separately after registration at
//! <https://developer.njtransit.com/registration>.
//!
//! The rail token methods (`getToken` and `isValidToken`) share a budget of 10
//! calls per America/New_York day. This crate never calls `isValidToken`. It
//! mints lazily, persists the token when a [`TokenStore`] path is set, and
//! stops before the local cap or a daily-limit response can be exceeded.
//!
//! # Module map
//!
//! - [`api`] — [`ApiKind`], [`Feed`], default origins, and [`endpoint_url`].
//! - [`client`] — [`Client`], which posts the `token` field and checks the body.
//! - [`token`] — [`TokenManager`], single-flight minting, cap, and backoff.
//! - [`store`] — [`TokenStore`], the optional mode-`0600` cache file.
//! - [`payload`] — [`parse_token_body`] and [`classify`], which reject `Null`
//!   and JSON error documents.
//! - [`error`] — [`Error`].
//! - [`clock`] — [`Clock`], [`ManualClock`], and the Eastern quota day.
//! - [`secret`] — [`Secret`], which redacts credentials in [`std::fmt::Debug`].
//!
//! # Examples
//!
//! Parsing a token response does not contact NJ Transit. Placeholder
//! credentials in the builder are not sent until [`Client::fetch`].
//!
//! ```
//! use njt_client::{
//!     classify, parse_token_body, ApiKind, Client, Error, PayloadKind, RAIL_TEST_BASE,
//! };
//!
//! let token = parse_token_body(br#"{"Authenticated":"True","UserToken":"placeholder-token"}"#)
//!     .expect("token");
//! assert_eq!(token, "placeholder-token");
//! let err = classify(br#"{"errorMessage":"Invalid token."}"#, PayloadKind::Zip)
//!     .expect_err("json is not a zip");
//! assert!(matches!(err, Error::InvalidToken));
//!
//! let client = Client::builder(ApiKind::Rail, RAIL_TEST_BASE)
//!     .credentials("username", "password")
//!     .build()
//!     .expect("builder");
//! assert_eq!(client.api(), ApiKind::Rail);
//! ```
//!
//! Fetching a feed requires a Tokio runtime and real credentials:
//!
//! ```no_run
//! use njt_client::{ApiKind, Client, Feed, RAIL_PRODUCTION_BASE};
//!
//! # async fn demo() -> Result<(), njt_client::Error> {
//! let client = Client::builder(ApiKind::Rail, RAIL_PRODUCTION_BASE)
//!     .credentials("username", "password")
//!     .build()?;
//! let _zip = client.fetch(Feed::Static).await?;
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]
#![deny(clippy::missing_docs_in_private_items)]
#![deny(clippy::missing_errors_doc)]
#![deny(clippy::missing_panics_doc)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod api;
pub mod client;
pub mod clock;
pub mod error;
pub mod payload;
pub mod secret;
pub mod store;
pub mod token;

pub use api::{
    endpoint_url, normalize_base, ApiKind, Feed, PayloadKind, BUS_PRODUCTION_BASE, BUS_TEST_BASE,
    RAIL_PRODUCTION_BASE, RAIL_TEST_BASE,
};
/// Raw feed bytes returned by [`Client::fetch`].
pub use bytes::Bytes;
pub use client::{Client, ClientBuilder};
pub use clock::{eastern_date, next_midnight_eastern, Clock, ManualClock, SystemClock};
pub use error::{safe_error_text, Error};
pub use payload::{classify, classify_error_message, parse_token_body};
pub use secret::{constant_time_eq, Secret};
pub use store::TokenStore;
pub use token::{TokenManager, TokenManagerBuilder, TokenPolicy, DAILY_MINT_CAP};
