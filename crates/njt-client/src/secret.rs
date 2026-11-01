//! Strings that must not appear in logs.
//!
//! [`Secret`] redacts itself in [`std::fmt::Debug`]. The only way to read the
//! value is [`Secret::expose`]. Callers must not log that borrow.

use std::fmt;

/// A credential or token whose debug output is redacted.
///
/// # Examples
///
/// ```
/// use njt_client::Secret;
///
/// let secret = Secret::new("placeholder-token");
/// assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
/// assert_eq!(secret.expose(), "placeholder-token");
/// ```
pub struct Secret {
    /// The raw value. Never pass this field to a logger.
    value: String,
}

impl Secret {
    /// Stores `value` without echoing it.
    ///
    /// # Arguments
    ///
    /// * `value` — Password, username, token, or access key. Placeholder values
    ///   belong in examples; real NJ Transit credentials do not belong in source.
    ///
    /// # Returns
    ///
    /// A secret that debug-formats as `Secret(<redacted>)`.
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
    /// use njt_client::Secret;
    ///
    /// let secret = Secret::new(String::from("placeholder-password"));
    /// assert!(format!("{secret:?}").contains("redacted"));
    /// ```
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }

    /// Borrows the raw value for an upstream form field.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The stored string. Do not log, persist in application logs, or place this
    /// in an [`std::error::Error`] display message.
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
    /// use njt_client::Secret;
    ///
    /// let secret = Secret::new("placeholder-token");
    /// assert_eq!(secret.expose(), "placeholder-token");
    /// ```
    pub fn expose(&self) -> &str {
        &self.value
    }

    /// Reports whether the stored value is empty.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// `true` when the stored string has length zero.
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
    /// use njt_client::Secret;
    ///
    /// assert!(Secret::new("").is_empty());
    /// assert!(!Secret::new("placeholder-token").is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
}

impl Clone for Secret {
    /// Clones the secret without writing it to a log.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// A second handle on the same value.
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
    /// use njt_client::Secret;
    ///
    /// let secret = Secret::new("placeholder-token");
    /// assert_eq!(secret.clone().expose(), secret.expose());
    /// ```
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
        }
    }
}

impl fmt::Debug for Secret {
    /// Writes a redacted placeholder.
    ///
    /// # Arguments
    ///
    /// * `formatter` — Destination that must not receive the raw value.
    ///
    /// # Returns
    ///
    /// A formatting result. The written text is `Secret(<redacted>)`.
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
    /// use njt_client::Secret;
    ///
    /// let secret = Secret::new("placeholder-password");
    /// let rendered = format!("{secret:?}");
    /// assert!(!rendered.contains("placeholder-password"));
    /// ```
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

impl PartialEq for Secret {
    /// Compares two secrets in constant time relative to the longer length.
    ///
    /// # Arguments
    ///
    /// * `other` — Secret to compare with.
    ///
    /// # Returns
    ///
    /// `true` when the raw values are identical.
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
    /// use njt_client::Secret;
    ///
    /// assert_eq!(Secret::new("placeholder-token"), Secret::new("placeholder-token"));
    /// assert_ne!(Secret::new("placeholder-token"), Secret::new("other"));
    /// ```
    fn eq(&self, other: &Self) -> bool {
        constant_time_eq(self.value.as_bytes(), other.value.as_bytes())
    }
}

impl Eq for Secret {}

/// Compares two byte strings without returning early on the first mismatch.
///
/// The running time tracks the longer input. A length difference still affects
/// how many bytes are visited; this is the usual `subtle`-style length mix.
///
/// # Arguments
///
/// * `left` — First byte string. May be a token or access key.
/// * `right` — Second byte string.
///
/// # Returns
///
/// `true` only when `left` and `right` have the same bytes.
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
/// use njt_client::constant_time_eq;
///
/// assert!(constant_time_eq(b"placeholder", b"placeholder"));
/// assert!(!constant_time_eq(b"placeholder", b"other"));
/// assert!(!constant_time_eq(b"placeholder", b"placeholder-x"));
/// ```
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    let len = left.len().max(right.len());
    let mut index = 0;
    while index < len {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
        index += 1;
    }
    diff == 0
}
