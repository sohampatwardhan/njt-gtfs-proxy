//! Rail and bus API names, default origins, and URL joining.
//!
//! The client calls `getToken` (rail) or `authenticateUser` (bus), then the
//! four data methods. It never builds an `isValidToken` URL. That method shares
//! rail's 10-calls-per-day budget with `getToken`.
//!
//! # Examples
//!
//! ```
//! use njt_client::{endpoint_url, ApiKind, RAIL_PRODUCTION_BASE};
//!
//! let url = endpoint_url(RAIL_PRODUCTION_BASE, ApiKind::Rail, "getToken")
//!     .expect("getToken is allowed");
//! assert_eq!(url, "https://raildata.njtransit.com/api/GTFSRT/getToken");
//! assert!(endpoint_url(RAIL_PRODUCTION_BASE, ApiKind::Rail, "isValidToken").is_err());
//! ```

use crate::error::Error;

/// Production origin for the rail GTFS/GTFSRT API.
///
/// The rail PDF and swagger use this host. The developer portal's sample curls
/// sometimes show `raildata.njt.gov` instead. Pass that host as a base URL if
/// you need it; this crate's default is the `njtransit.com` origin.
pub const RAIL_PRODUCTION_BASE: &str = "https://raildata.njtransit.com";

/// Test origin for the rail GTFS/GTFSRT API.
pub const RAIL_TEST_BASE: &str = "https://testraildata.njtransit.com";

/// Production origin for the bus GTFS/GTFSRT G2 API.
pub const BUS_PRODUCTION_BASE: &str = "https://pcsdata.njtransit.com";

/// Test origin for the bus GTFS/GTFSRT G2 API.
pub const BUS_TEST_BASE: &str = "https://testpcsdata.njtransit.com";

/// Which NJ Transit developer application to call.
///
/// Rail and bus are different hosts. A token from one is not sent to the other.
/// Test and production credentials are issued separately.
///
/// # Examples
///
/// ```
/// use njt_client::ApiKind;
///
/// assert_eq!(ApiKind::Rail.auth_method(), "getToken");
/// assert_eq!(ApiKind::Bus.auth_method(), "authenticateUser");
/// assert_eq!(ApiKind::Bus.path_segment(), "GTFSG2");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApiKind {
    /// Rail feeds under `/api/GTFSRT` on the rail host.
    Rail,
    /// Bus G2 feeds under `/api/GTFSG2` on the bus host.
    Bus,
}

impl ApiKind {
    /// Returns the short name used in logs.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// `rail` or `bus`.
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
    /// use njt_client::ApiKind;
    ///
    /// assert_eq!(ApiKind::Rail.as_str(), "rail");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            ApiKind::Rail => "rail",
            ApiKind::Bus => "bus",
        }
    }

    /// Returns the path segment that follows `/api/`.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// `GTFSRT` for rail and `GTFSG2` for bus.
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
    /// use njt_client::ApiKind;
    ///
    /// assert_eq!(ApiKind::Rail.path_segment(), "GTFSRT");
    /// ```
    pub fn path_segment(self) -> &'static str {
        match self {
            ApiKind::Rail => "GTFSRT",
            ApiKind::Bus => "GTFSG2",
        }
    }

    /// Returns the token-mint method name for this API.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// `getToken` or `authenticateUser`.
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
    /// use njt_client::ApiKind;
    ///
    /// assert_eq!(ApiKind::Bus.auth_method(), "authenticateUser");
    /// ```
    pub fn auth_method(self) -> &'static str {
        match self {
            ApiKind::Rail => "getToken",
            ApiKind::Bus => "authenticateUser",
        }
    }

    /// Reports whether this client is willing to call `method`.
    ///
    /// `isValidToken` is intentionally absent. Rail counts it against the same
    /// 10-calls-per-day budget as `getToken`.
    ///
    /// # Arguments
    ///
    /// * `method` — Final path segment, such as `getGTFS`.
    ///
    /// # Returns
    ///
    /// `true` when [`endpoint_url`] will accept `method` for this API.
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
    /// use njt_client::ApiKind;
    ///
    /// assert!(ApiKind::Rail.allows("getTripUpdates"));
    /// assert!(!ApiKind::Rail.allows("isValidToken"));
    /// assert!(ApiKind::Bus.allows("getAlerts"));
    /// ```
    pub fn allows(self, method: &str) -> bool {
        match self {
            ApiKind::Rail => matches!(
                method,
                "getToken" | "getGTFS" | "getAlerts" | "getTripUpdates" | "getVehiclePositions"
            ),
            ApiKind::Bus => matches!(
                method,
                "authenticateUser"
                    | "getGTFS"
                    | "getAlerts"
                    | "getTripUpdates"
                    | "getVehiclePositions"
            ),
        }
    }
}

impl std::fmt::Display for ApiKind {
    /// Writes [`ApiKind::as_str`].
    ///
    /// # Arguments
    ///
    /// * `formatter` — Destination for `rail` or `bus`.
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
    /// use njt_client::ApiKind;
    ///
    /// assert_eq!(ApiKind::Bus.to_string(), "bus");
    /// ```
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One downloadable GTFS or GTFS-Realtime file.
///
/// # Examples
///
/// ```
/// use njt_client::{Feed, PayloadKind};
///
/// assert_eq!(Feed::Alerts.method(), "getAlerts");
/// assert_eq!(Feed::Alerts.content_type(), "application/x-protobuf");
/// assert_eq!(Feed::Static.payload_kind(), PayloadKind::Zip);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feed {
    /// Static GTFS schedule zip from `getGTFS`.
    Static,
    /// GTFS-Realtime `TripUpdates` protobuf from `getTripUpdates`.
    TripUpdates,
    /// GTFS-Realtime `VehiclePositions` protobuf from `getVehiclePositions`.
    VehiclePositions,
    /// GTFS-Realtime service alerts protobuf from `getAlerts`.
    ///
    /// The bus PDF spells one production sample `getALerts`. Swagger lists
    /// `/api/GTFSG2/getAlerts`, and that is the path this client calls.
    Alerts,
}

impl Feed {
    /// Returns the upstream method name.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// `getGTFS`, `getTripUpdates`, `getVehiclePositions`, or `getAlerts`.
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
    /// use njt_client::Feed;
    ///
    /// assert_eq!(Feed::TripUpdates.method(), "getTripUpdates");
    /// ```
    pub fn method(self) -> &'static str {
        match self {
            Feed::Static => "getGTFS",
            Feed::TripUpdates => "getTripUpdates",
            Feed::VehiclePositions => "getVehiclePositions",
            Feed::Alerts => "getAlerts",
        }
    }

    /// Returns the log name for this feed.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// A short stable name such as `trip-updates`.
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
    /// use njt_client::Feed;
    ///
    /// assert_eq!(Feed::VehiclePositions.as_str(), "vehicle-positions");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            Feed::Static => "static",
            Feed::TripUpdates => "trip-updates",
            Feed::VehiclePositions => "vehicle-positions",
            Feed::Alerts => "alerts",
        }
    }

    /// Returns which byte check [`crate::classify`] should apply.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// [`PayloadKind::Zip`] for the static schedule and
    /// [`PayloadKind::Protobuf`] for the realtime feeds.
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
    /// use njt_client::{Feed, PayloadKind};
    ///
    /// assert_eq!(Feed::Static.payload_kind(), PayloadKind::Zip);
    /// ```
    pub fn payload_kind(self) -> PayloadKind {
        match self {
            Feed::Static => PayloadKind::Zip,
            Feed::TripUpdates | Feed::VehiclePositions | Feed::Alerts => PayloadKind::Protobuf,
        }
    }

    /// Returns the Content-Type a GTFS client expects for this feed.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// `application/zip` or `application/x-protobuf`.
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
    /// use njt_client::Feed;
    ///
    /// assert_eq!(Feed::Static.content_type(), "application/zip");
    /// ```
    pub fn content_type(self) -> &'static str {
        match self.payload_kind() {
            PayloadKind::Zip => "application/zip",
            PayloadKind::Protobuf => "application/x-protobuf",
        }
    }

    /// Returns the maximum buffered response size for this feed.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// 250 MiB for the static zip and 80 MiB for a realtime protobuf.
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
    /// use njt_client::Feed;
    ///
    /// assert!(Feed::Static.max_bytes() > Feed::Alerts.max_bytes());
    /// ```
    pub fn max_bytes(self) -> usize {
        match self {
            Feed::Static => 250 * 1024 * 1024,
            Feed::TripUpdates | Feed::VehiclePositions | Feed::Alerts => 80 * 1024 * 1024,
        }
    }
}

impl std::fmt::Display for Feed {
    /// Writes [`Feed::as_str`].
    ///
    /// # Arguments
    ///
    /// * `formatter` — Destination for the short feed name.
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
    /// use njt_client::Feed;
    ///
    /// assert_eq!(Feed::Alerts.to_string(), "alerts");
    /// ```
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// How [`crate::classify`] decides that a body is safe to serve.
///
/// # Examples
///
/// ```
/// use njt_client::PayloadKind;
///
/// assert_eq!(PayloadKind::Zip.as_str(), "zip");
/// assert_eq!(PayloadKind::Protobuf.as_str(), "protobuf");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PayloadKind {
    /// A ZIP archive. The body must start with a ZIP local-file signature.
    Zip,
    /// A GTFS-Realtime protobuf. JSON and the literal `Null` are rejected.
    Protobuf,
}

impl PayloadKind {
    /// Returns the word used in error text.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// `zip` or `protobuf`.
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
    /// use njt_client::PayloadKind;
    ///
    /// assert_eq!(PayloadKind::Protobuf.as_str(), "protobuf");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            PayloadKind::Zip => "zip",
            PayloadKind::Protobuf => "protobuf",
        }
    }
}

/// Checks that `raw` is an origin with an http(s) scheme and no path or userinfo.
///
/// # Arguments
///
/// * `raw` — Caller-supplied base URL. Trailing slashes are removed.
///
/// # Returns
///
/// The normalized origin, without a trailing slash.
///
/// # Errors
///
/// Returns [`Error::Config`] when the value is empty, not absolute http(s),
/// contains a username or password, or has a path other than `/`. The error
/// text does not echo `raw`.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::normalize_base;
///
/// assert_eq!(
///     normalize_base("https://raildata.njtransit.com/").expect("origin"),
///     "https://raildata.njtransit.com"
/// );
/// assert!(normalize_base("https://user:secret@raildata.njtransit.com").is_err());
/// assert!(normalize_base("https://raildata.njtransit.com/api/GTFSRT").is_err());
/// ```
pub fn normalize_base(raw: &str) -> Result<String, Error> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(Error::Config {
            message: "base URL is empty".to_string(),
        });
    }
    let url = reqwest::Url::parse(trimmed).map_err(|_| Error::Config {
        message: "base URL is not an absolute http(s) URL".to_string(),
    })?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(Error::Config {
            message: "base URL scheme must be http or https".to_string(),
        });
    }
    if url.host_str().is_none() {
        return Err(Error::Config {
            message: "base URL is missing a host".to_string(),
        });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Config {
            message: "base URL must not include userinfo".to_string(),
        });
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err(Error::Config {
            message: "base URL must be an origin without a path; the client adds /api".to_string(),
        });
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(Error::Config {
            message: "base URL must not include a query or fragment".to_string(),
        });
    }
    Ok(trimmed.to_string())
}

/// Joins an origin, an API, and an allowed method into a POST URL.
///
/// # Arguments
///
/// * `base` — Origin such as [`RAIL_PRODUCTION_BASE`].
/// * `api` — Rail or bus.
/// * `method` — Final path segment. Must be allowed by [`ApiKind::allows`].
///
/// # Returns
///
/// An absolute URL with no query string.
///
/// # Errors
///
/// Returns [`Error::Config`] when `base` fails [`normalize_base`] or when
/// `method` is not one of the methods this client calls. `isValidToken` is
/// rejected.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::{endpoint_url, ApiKind, BUS_TEST_BASE};
///
/// let url = endpoint_url(BUS_TEST_BASE, ApiKind::Bus, "getAlerts").expect("alerts");
/// assert_eq!(
///     url,
///     "https://testpcsdata.njtransit.com/api/GTFSG2/getAlerts"
/// );
/// assert!(endpoint_url(BUS_TEST_BASE, ApiKind::Bus, "getALerts").is_err());
/// ```
pub fn endpoint_url(base: &str, api: ApiKind, method: &str) -> Result<String, Error> {
    if !api.allows(method) {
        let shown: String = method.chars().take(40).collect();
        return Err(Error::Config {
            message: format!("{shown} is not a method the {api} client calls"),
        });
    }
    let base = normalize_base(base)?;
    Ok(format!("{base}/api/{}/{}", api.path_segment(), method))
}
