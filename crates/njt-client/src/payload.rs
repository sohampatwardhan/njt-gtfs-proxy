//! Classifies upstream bodies so JSON errors are never served as zip or protobuf.
//!
//! Data methods sometimes answer HTTP 200 with `{"errorMessage":...}` or with
//! the literal body `Null`. Those are errors. A zip must start with a ZIP
//! signature. A protobuf body that is JSON, HTML, or `Null` is rejected.

use serde_json::Value;

use crate::api::PayloadKind;
use crate::error::Error;

/// Turns an upstream `errorMessage` into a typed error.
///
/// # Arguments
///
/// * `message` — Text of the `errorMessage` field. It is sanitized before it is
///   stored on [`Error::Upstream`].
///
/// # Returns
///
/// [`Error::DailyLimit`] when the text mentions `daily usage limit`,
/// [`Error::InvalidToken`] when it mentions `invalid token`, and
/// [`Error::Upstream`] otherwise. `GTFS error` stays an upstream data error and
/// does not refresh the token.
///
/// # Errors
///
/// This function does not return a [`Result`]. The [`Error`] value is the
/// classification.
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
/// assert!(matches!(
///     classify_error_message("GTFS error"),
///     Error::Upstream { .. }
/// ));
/// assert!(matches!(
///     classify_error_message("Daily usage limit:10. Your current daily usage: 11"),
///     Error::DailyLimit
/// ));
/// ```
pub fn classify_error_message(message: &str) -> Error {
    let lower = message.to_ascii_lowercase();
    if lower.contains("daily usage limit") {
        return Error::DailyLimit;
    }
    if lower.contains("invalid token") {
        return Error::InvalidToken;
    }
    Error::Upstream {
        message: sanitize_message(message),
    }
}

/// Parses a token-endpoint body into the `UserToken` string.
///
/// Accepts the rail `getToken` and bus `authenticateUser` shape
/// `{"Authenticated":"True","UserToken":"..."}`. A false flag, an empty token,
/// the literal `Null`, and a daily-limit `errorMessage` are errors.
///
/// # Arguments
///
/// * `bytes` — Raw response body. Leading and trailing ASCII whitespace is ignored.
///
/// # Returns
///
/// The token string. The caller must store it in a [`crate::Secret`] or the
/// token cache and must not log it.
///
/// # Errors
///
/// * [`Error::NullBody`] — body is empty or the literal `Null` / `null`.
/// * [`Error::DailyLimit`] — `errorMessage` reports the daily usage limit.
/// * [`Error::AuthRejected`] — `Authenticated` is not `True`, or `UserToken` is empty.
/// * [`Error::Upstream`] — some other `errorMessage`.
/// * [`Error::UnexpectedPayload`] — the body is not JSON.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::{parse_token_body, Error};
///
/// let token = parse_token_body(
///     br#"{"Authenticated":"True","UserToken":"placeholder-token"}"#,
/// )
/// .expect("token");
/// assert_eq!(token, "placeholder-token");
/// assert!(matches!(parse_token_body(b"Null"), Err(Error::NullBody)));
/// assert!(matches!(
///     parse_token_body(br#"{"Authenticated":"False","UserToken":""}"#),
///     Err(Error::AuthRejected)
/// ));
/// ```
pub fn parse_token_body(bytes: &[u8]) -> Result<String, Error> {
    let trim = trimmed(bytes);
    if trim.is_empty() || is_null_literal(trim) {
        return Err(Error::NullBody);
    }
    let value: Value = serde_json::from_slice(trim).map_err(|_| Error::UnexpectedPayload {
        expected: "token json",
    })?;
    if value.is_null() {
        return Err(Error::NullBody);
    }
    if let Some(message) = value.get("errorMessage").and_then(Value::as_str) {
        return Err(classify_error_message(message));
    }
    let authenticated = value
        .get("Authenticated")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !authenticated.trim().eq_ignore_ascii_case("true") {
        return Err(Error::AuthRejected);
    }
    let token = value
        .get("UserToken")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if token.is_empty() {
        return Err(Error::AuthRejected);
    }
    Ok(token.to_string())
}

/// Accepts a data-call body only when it is a real zip or a non-JSON protobuf.
///
/// # Arguments
///
/// * `bytes` — Raw response body.
/// * `kind` — Whether the caller asked for a zip or a protobuf.
///
/// # Returns
///
/// `Ok(())` when the body may be served to a GTFS client.
///
/// # Errors
///
/// * [`Error::NullBody`] — literal `Null` or JSON `null`.
/// * [`Error::InvalidToken`] — JSON `errorMessage` mentioning an invalid token.
/// * [`Error::DailyLimit`] — JSON daily-usage-limit message.
/// * [`Error::Upstream`] — any other JSON `errorMessage`, including `GTFS error`.
/// * [`Error::UnexpectedPayload`] — empty, HTML, non-zip bytes, or JSON that is
///   not one of the known error objects.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::{classify, Error, PayloadKind};
///
/// classify(b"PK\x03\x04fake-zip", PayloadKind::Zip).expect("zip");
/// let err = classify(br#"{"errorMessage":"Invalid token."}"#, PayloadKind::Protobuf)
///     .expect_err("json is not protobuf");
/// assert!(matches!(err, Error::InvalidToken));
/// assert!(classify(b"not-a-zip", PayloadKind::Zip).is_err());
/// assert!(classify(b"\x0afake-protobuf", PayloadKind::Protobuf).is_ok());
/// ```
pub fn classify(bytes: &[u8], kind: PayloadKind) -> Result<(), Error> {
    let trim = trimmed(bytes);
    if trim.is_empty() {
        return Err(Error::UnexpectedPayload {
            expected: kind.as_str(),
        });
    }
    if is_null_literal(trim) {
        return Err(Error::NullBody);
    }
    if let Some(err) = json_error(trim) {
        return Err(err);
    }
    match kind {
        PayloadKind::Zip => {
            if is_zip(bytes) {
                Ok(())
            } else {
                Err(Error::UnexpectedPayload { expected: "zip" })
            }
        }
        PayloadKind::Protobuf => {
            if trim.first() == Some(&b'<') {
                Err(Error::UnexpectedPayload {
                    expected: "protobuf",
                })
            } else {
                Ok(())
            }
        }
    }
}

/// Strips control characters, collapses whitespace, and caps the length.
///
/// # Arguments
///
/// * `message` — Raw `errorMessage` text.
///
/// # Returns
///
/// A single-line string of at most 180 characters. Empty input becomes
/// `empty errorMessage`.
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
/// [`classify_error_message`] uses this for the `GTFS error` text:
///
/// ```
/// use njt_client::{classify_error_message, Error};
///
/// let Error::Upstream { message } = classify_error_message("GTFS error") else {
///     panic!("expected upstream");
/// };
/// assert_eq!(message, "GTFS error");
/// ```
fn sanitize_message(message: &str) -> String {
    let mut out = String::new();
    for ch in message.chars() {
        if out.chars().count() >= 180 {
            break;
        }
        if ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "empty errorMessage".to_string()
    } else {
        collapsed
    }
}

/// Returns the subslice with ASCII whitespace removed from both ends.
///
/// # Arguments
///
/// * `bytes` — Body to trim.
///
/// # Returns
///
/// A subslice of `bytes`. It is empty when `bytes` is empty or all whitespace.
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
/// [`parse_token_body`] trims before it looks for `Null`:
///
/// ```
/// use njt_client::{parse_token_body, Error};
///
/// assert!(matches!(parse_token_body(b"  Null\n"), Err(Error::NullBody)));
/// ```
fn trimmed(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(0, |index| index.saturating_add(1));
    if start >= end {
        &[]
    } else {
        bytes.get(start..end).unwrap_or(&[])
    }
}

/// Reports whether `bytes` is the literal null body, ignoring ASCII case.
///
/// # Arguments
///
/// * `bytes` — Already trimmed body.
///
/// # Returns
///
/// `true` for `Null`, `null`, and `NULL`.
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
/// use njt_client::{parse_token_body, Error};
///
/// assert!(matches!(parse_token_body(b"null"), Err(Error::NullBody)));
/// ```
fn is_null_literal(bytes: &[u8]) -> bool {
    bytes.eq_ignore_ascii_case(b"null")
}

/// Reports whether `bytes` starts with a ZIP local, empty, or spanned signature.
///
/// # Arguments
///
/// * `bytes` — Original body, including any leading bytes.
///
/// # Returns
///
/// `true` for `PK\x03\x04`, `PK\x05\x06`, and `PK\x07\x08`.
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
/// use njt_client::{classify, PayloadKind};
///
/// assert!(classify(b"PK\x03\x04fake-zip", PayloadKind::Zip).is_ok());
/// assert!(classify(b"PK\x05\x06", PayloadKind::Zip).is_ok());
/// ```
fn is_zip(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some(b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x07\x08")
    )
}

/// Parses a JSON object or array into a feed error.
///
/// # Arguments
///
/// * `trim` — Trimmed body.
///
/// # Returns
///
/// `None` when the body does not start like JSON. `Some` when it does, even if
/// the JSON is malformed, because a JSON-looking feed must not be served.
///
/// # Errors
///
/// This function does not return a [`Result`]. The optional [`Error`] is the
/// classification.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::{classify, Error, PayloadKind};
///
/// let err = classify(br#"{"errorMessage":"GTFS error"}"#, PayloadKind::Zip)
///     .expect_err("json");
/// assert!(matches!(err, Error::Upstream { .. }));
/// ```
fn json_error(trim: &[u8]) -> Option<Error> {
    let first = trim.first().copied()?;
    if first != b'{' && first != b'[' {
        return None;
    }
    let value: Value = match serde_json::from_slice(trim) {
        Ok(value) => value,
        Err(_) => {
            return Some(Error::UnexpectedPayload {
                expected: "feed bytes",
            });
        }
    };
    Some(error_from_json(&value))
}

/// Maps a JSON value that arrived where a feed was expected.
///
/// # Arguments
///
/// * `value` — Parsed JSON body.
///
/// # Returns
///
/// A typed error. Successful `Authenticated` objects are still rejected here
/// because a data call must not return a token document.
///
/// # Errors
///
/// This function does not return a [`Result`].
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use njt_client::{classify, Error, PayloadKind};
///
/// let err = classify(b"null", PayloadKind::Protobuf).expect_err("null");
/// assert!(matches!(err, Error::NullBody));
/// ```
fn error_from_json(value: &Value) -> Error {
    if value.is_null() {
        return Error::NullBody;
    }
    if let Some(message) = value.get("errorMessage").and_then(Value::as_str) {
        return classify_error_message(message);
    }
    if let Some(flag) = value.get("Authenticated").and_then(Value::as_str) {
        if !flag.trim().eq_ignore_ascii_case("true") {
            return Error::AuthRejected;
        }
    }
    Error::UnexpectedPayload {
        expected: "feed bytes",
    }
}
