//! Clocks for token expiry and the America/New_York quota day.
//!
//! Rail's `getToken` budget resets at midnight America/New_York. The rail PDF
//! says to call that method once per day. The developer portal also describes
//! rail tokens as valid for "hours", without a number. This crate treats a rail
//! token as expired at the next Eastern midnight, and earlier when a data call
//! returns an invalid-token error.
//!
//! # Examples
//!
//! ```
//! use chrono::{DateTime, Utc};
//! use njt_client::next_midnight_eastern;
//!
//! let now = DateTime::parse_from_rfc3339("2026-01-15T15:00:00Z")
//!     .expect("timestamp")
//!     .with_timezone(&Utc);
//! let midnight = next_midnight_eastern(now).expect("midnight");
//! assert_eq!(
//!     midnight,
//!     DateTime::parse_from_rfc3339("2026-01-16T05:00:00Z")
//!         .expect("next midnight")
//!         .with_timezone(&Utc)
//! );
//! ```

use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;

use crate::error::Error;

/// A source of "now" so tests can move the quota day without sleeping.
///
/// # Examples
///
/// ```
/// use njt_client::{Clock, SystemClock};
///
/// let now = SystemClock.now();
/// assert!(now.timestamp() > 0);
/// ```
pub trait Clock: Send + Sync {
    /// Returns the current instant in UTC.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// A UTC timestamp. Implementations must not panic.
    ///
    /// # Errors
    ///
    /// This method does not return an error.
    ///
    /// # Panics
    ///
    /// Implementations must not panic. [`SystemClock`] and [`ManualClock`] do not.
    ///
    /// # Examples
    ///
    /// ```
    /// use chrono::{TimeZone, Utc};
    /// use njt_client::{Clock, ManualClock};
    ///
    /// let start = Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap();
    /// let clock = ManualClock::new(start);
    /// assert_eq!(clock.now(), start);
    /// ```
    fn now(&self) -> DateTime<Utc>;
}

/// The process wall clock.
///
/// # Examples
///
/// ```
/// use njt_client::{Clock, SystemClock};
///
/// let _now = SystemClock.now();
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    /// Returns [`Utc::now`].
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The current UTC time from the system clock.
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
    /// use njt_client::{Clock, SystemClock};
    ///
    /// assert!(SystemClock.now().timestamp() > 0);
    /// ```
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock whose time is set by the test or the caller.
///
/// # Examples
///
/// ```
/// use chrono::{TimeZone, Utc};
/// use njt_client::{Clock, ManualClock};
/// use std::time::Duration;
///
/// let start = Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap();
/// let clock = ManualClock::new(start);
/// clock.advance(Duration::from_secs(30));
/// assert_eq!(clock.now(), start + chrono::Duration::seconds(30));
/// ```
#[derive(Clone, Debug)]
pub struct ManualClock {
    /// Interior time. A poisoned lock is recovered so a panic elsewhere cannot
    /// trap the clock.
    now: std::sync::Arc<std::sync::Mutex<DateTime<Utc>>>,
}

impl ManualClock {
    /// Starts the clock at `now`.
    ///
    /// # Arguments
    ///
    /// * `now` — Initial UTC time.
    ///
    /// # Returns
    ///
    /// A clock shared by clones. Advancing one clone advances the others.
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
    /// use chrono::{TimeZone, Utc};
    /// use njt_client::ManualClock;
    ///
    /// let start = Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap();
    /// let _clock = ManualClock::new(start);
    /// ```
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: std::sync::Arc::new(std::sync::Mutex::new(now)),
        }
    }

    /// Replaces the current instant.
    ///
    /// # Arguments
    ///
    /// * `now` — New UTC time.
    ///
    /// # Returns
    ///
    /// Nothing.
    ///
    /// # Errors
    ///
    /// This method does not return an error.
    ///
    /// # Panics
    ///
    /// This method does not panic. A poisoned mutex is recovered.
    ///
    /// # Examples
    ///
    /// ```
    /// use chrono::{TimeZone, Utc};
    /// use njt_client::{Clock, ManualClock};
    ///
    /// let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap());
    /// let later = Utc.with_ymd_and_hms(2026, 1, 16, 5, 0, 0).unwrap();
    /// clock.set(later);
    /// assert_eq!(clock.now(), later);
    /// ```
    pub fn set(&self, now: DateTime<Utc>) {
        *lock_clock(&self.now) = now;
    }

    /// Moves the clock forward by `by`.
    ///
    /// # Arguments
    ///
    /// * `by` — Duration to add. Values chrono cannot represent are ignored.
    ///
    /// # Returns
    ///
    /// Nothing.
    ///
    /// # Errors
    ///
    /// This method does not return an error.
    ///
    /// # Panics
    ///
    /// This method does not panic. Overflowing additions are skipped.
    ///
    /// # Examples
    ///
    /// ```
    /// use chrono::{TimeZone, Utc};
    /// use njt_client::{Clock, ManualClock};
    /// use std::time::Duration;
    ///
    /// let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap());
    /// clock.advance(Duration::from_secs(1));
    /// assert_eq!(clock.now().timestamp() % 60, 1);
    /// ```
    pub fn advance(&self, by: std::time::Duration) {
        let Some(step) = ChronoDuration::from_std(by).ok() else {
            return;
        };
        let mut slot = lock_clock(&self.now);
        if let Some(next) = slot.checked_add_signed(step) {
            *slot = next;
        }
    }
}

impl Clock for ManualClock {
    /// Returns the instant last set or advanced.
    ///
    /// # Arguments
    ///
    /// This method takes no arguments.
    ///
    /// # Returns
    ///
    /// The stored UTC time.
    ///
    /// # Errors
    ///
    /// This method does not return an error.
    ///
    /// # Panics
    ///
    /// This method does not panic. A poisoned mutex is recovered.
    ///
    /// # Examples
    ///
    /// ```
    /// use chrono::{TimeZone, Utc};
    /// use njt_client::{Clock, ManualClock};
    ///
    /// let start = Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap();
    /// assert_eq!(ManualClock::new(start).now(), start);
    /// ```
    fn now(&self) -> DateTime<Utc> {
        *lock_clock(&self.now)
    }
}

/// Locks `slot`, recovering if a previous holder panicked.
///
/// # Arguments
///
/// * `slot` — Mutex that stores the manual clock's instant.
///
/// # Returns
///
/// A guard for the instant.
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
/// The lock is private. [`ManualClock::set`] is the supported way to move a
/// manual clock:
///
/// ```
/// use chrono::{TimeZone, Utc};
/// use njt_client::{Clock, ManualClock};
///
/// let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 1, 15, 15, 0, 0).unwrap());
/// let later = Utc.with_ymd_and_hms(2026, 1, 15, 16, 0, 0).unwrap();
/// clock.set(later);
/// assert_eq!(Clock::now(&clock), later);
/// ```
fn lock_clock(slot: &std::sync::Mutex<DateTime<Utc>>) -> std::sync::MutexGuard<'_, DateTime<Utc>> {
    slot.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Returns the America/New_York calendar date of `now`.
///
/// # Arguments
///
/// * `now` — Instant in UTC.
///
/// # Returns
///
/// The civil date in Eastern Time, including daylight-saving rules.
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
/// use chrono::{Datelike, DateTime, Utc};
/// use njt_client::eastern_date;
///
/// let now = DateTime::parse_from_rfc3339("2026-01-15T04:30:00Z")
///     .expect("timestamp")
///     .with_timezone(&Utc);
/// let date = eastern_date(now);
/// assert_eq!((date.year(), date.month(), date.day()), (2026, 1, 14));
/// ```
pub fn eastern_date(now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&New_York).date_naive()
}

/// Returns the next America/New_York midnight at or after the following civil day.
///
/// The returned instant is the start of the next Eastern calendar day, which is
/// when NJ Transit resets the rail token quota.
///
/// # Arguments
///
/// * `now` — Instant in UTC.
///
/// # Returns
///
/// The UTC instant of the next Eastern midnight.
///
/// # Errors
///
/// Returns [`Error::Config`] when the civil date cannot be advanced or when
/// that local midnight does not exist. Neither happens for dates chrono can
/// represent around a normal Eastern midnight.
///
/// # Panics
///
/// This function does not panic.
///
/// # Examples
///
/// ```
/// use chrono::{DateTime, Utc};
/// use njt_client::next_midnight_eastern;
///
/// let now = DateTime::parse_from_rfc3339("2026-01-15T15:00:00Z")
///     .expect("timestamp")
///     .with_timezone(&Utc);
/// let next = next_midnight_eastern(now).expect("midnight");
/// assert_eq!(next.to_rfc3339(), "2026-01-16T05:00:00+00:00");
/// ```
pub fn next_midnight_eastern(now: DateTime<Utc>) -> Result<DateTime<Utc>, Error> {
    let local = now.with_timezone(&New_York);
    let Some(next_date) = local.date_naive().succ_opt() else {
        return Err(Error::Config {
            message: "could not compute the next Eastern midnight".to_string(),
        });
    };
    let Some(naive) = next_date.and_hms_opt(0, 0, 0) else {
        return Err(Error::Config {
            message: "could not build the next Eastern midnight".to_string(),
        });
    };
    match New_York.from_local_datetime(&naive) {
        chrono::LocalResult::Single(local_midnight) => Ok(local_midnight.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(earliest, _) => Ok(earliest.with_timezone(&Utc)),
        chrono::LocalResult::None => Err(Error::Config {
            message: "the next Eastern midnight does not exist on the civil calendar".to_string(),
        }),
    }
}
