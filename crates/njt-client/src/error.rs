//! Failures returned by the NJ Transit client.
//!
//! Messages are safe to log. They never include usernames, passwords, or tokens.
//!
//! # Examples
//!
//! ```
//! use njt_client::{parse_token_body, Error};
//!
//! let err = parse_token_body(b"Null").expect_err("Null is not a token");
//! assert!(matches!(err, Error::NullBody));
//! assert!(!err.to_string().to_ascii_lowercase().contains("password"));
//! ```

use std::error::Error as StdError;

/// A failure while minting a token or downloading a feed.
///
/// # Examples
///
/// ```
/// use njt_client::{classify_error_message, Error};
///
/// let err = classify_error_message("Daily usage limit:10. Your current daily usage: 11");
/// assert!(matches!(err, Error::DailyLimit));
/// ```
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The upstream token endpoint reported that today's quota is exhausted.
    ///
    /// The client stops calling the token endpoint until the next
    /// America/New_York midnight.
    #[error(
        "NJ Transit token endpoint daily limit reached. No further token requests will be sent until midnight America/New_York"
    )]
    DailyLimit,

    /// This process already used its local mint budget for the quota day.
    ///
    /// `cap` is the configured maximum number of token HTTP calls.
    #[error(
        "local token cap of {cap} calls per America/New_York day reached. No further token requests will be sent until midnight"
    )]
    LocalCap {
        /// Maximum token HTTP calls allowed for this API during the quota day.
        cap: u32,
    },

    /// A recent mint failed and the cooldown has not elapsed.
    ///
    /// No token HTTP call is made while this is in effect.
    #[error("token mint is in backoff; retry after {retry_after:?}")]
    Backoff {
        /// How long the caller should wait before trying again.
        retry_after: std::time::Duration,
    },

    /// Username and password were rejected, or the token field was empty.
    #[error("NJ Transit rejected the username or password")]
    AuthRejected,

    /// A data call returned an invalid-token error.
    #[error("NJ Transit rejected the token")]
    InvalidToken,

    /// The body was the literal `Null` (or JSON `null`).
    #[error("NJ Transit returned a Null body")]
    NullBody,

    /// A JSON `errorMessage` that is neither the daily limit nor an invalid token.
    #[error("NJ Transit error: {message}")]
    Upstream {
        /// Sanitized `errorMessage` text, truncated and without control characters.
        message: String,
    },

    /// The body was not the zip or protobuf the caller asked for.
    #[error("NJ Transit returned a body that is not a {expected} payload")]
    UnexpectedPayload {
        /// Short name of the payload that was required, such as `zip`.
        expected: &'static str,
    },

    /// The on-disk token cache could not be read or written.
    #[error("token cache: {message}")]
    Cache {
        /// Operator-facing reason. Does not include cache contents.
        message: String,
    },

    /// The TCP or TLS request failed before a usable body arrived.
    #[error("could not reach NJ Transit: {0}")]
    Transport(#[source] Box<reqwest::Error>),

    /// The response exceeded the configured byte limit.
    #[error("NJ Transit response exceeded the {limit} byte limit")]
    TooLarge {
        /// Maximum number of bytes the client was willing to buffer.
        limit: usize,
    },

    /// HTTP status was not successful and the body was not a known JSON error.
    #[error("NJ Transit returned HTTP {code}")]
    Status {
        /// HTTP status code from the upstream response.
        code: u16,
    },

    /// A base URL, method name, or policy value was rejected locally.
    #[error("invalid NJ Transit client configuration: {message}")]
    Config {
        /// What was wrong. Credential values are not included.
        message: String,
    },
}

impl Error {
    /// Returns true when the proxy should answer `503` and stop minting for now.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// `true` for [`Error::DailyLimit`], [`Error::LocalCap`], and [`Error::Backoff`].
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
    /// use njt_client::Error;
    ///
    /// assert!(Error::DailyLimit.is_quota_block());
    /// assert!(!Error::InvalidToken.is_quota_block());
    /// ```
    pub fn is_quota_block(&self) -> bool {
        matches!(
            self,
            Error::DailyLimit | Error::LocalCap { .. } | Error::Backoff { .. }
        )
    }
}

/// Formats `err` for logs without walking into secret-bearing debug output.
///
/// # Arguments
///
/// * `err` — Any error. Only its [`std::fmt::Display`] text is used.
///
/// # Returns
///
/// The display text of `err`.
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
/// use njt_client::{safe_error_text, Error};
///
/// let text = safe_error_text(&Error::NullBody);
/// assert!(text.contains("Null"));
/// ```
pub fn safe_error_text(err: &dyn StdError) -> String {
    err.to_string()
}
