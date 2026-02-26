//! Locale-dependent timestamp parsing for MTG Arena log entries.
//!
//! MTGA log timestamps vary by system locale. This module handles all known
//! formats (11+ locale-dependent variants, epoch milliseconds, .NET ticks,
//! and ISO 8601) and normalizes them to UTC.

use chrono::{DateTime, NaiveDateTime, Utc};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// .NET ticks between 0001-01-01T00:00:00 and 1970-01-01T00:00:00.
const DOTNET_EPOCH_OFFSET_TICKS: i64 = 621_355_968_000_000_000;

/// Number of .NET ticks per second (each tick = 100 nanoseconds).
const TICKS_PER_SECOND: i64 = 10_000_000;

/// Chrono format strings for all known MTGA locale-dependent timestamps.
///
/// Tried in order until one succeeds. Ordering rationale:
/// - Year-first formats first (unambiguous date structure).
/// - US date formats (`M/d/yyyy`) before European (`dd/MM/yyyy`) since
///   they share the `/` separator and are ambiguous when both month and
///   day are <= 12.
/// - ISO 8601 with `T` separator last (11th format).
///
/// Extend this array when new locale variants are discovered.
const LOCALE_FORMATS: &[&str] = &[
    // yyyy-MM-dd (ISO date order)
    "%Y-%-m-%-d %-H:%M:%S",
    "%Y-%-m-%-d %-I:%M:%S %p",
    // yyyy/MM/dd (slash-separated ISO)
    "%Y/%-m/%-d %-H:%M:%S",
    "%Y/%-m/%-d %-I:%M:%S %p",
    // M/d/yyyy (US short date)
    "%-m/%-d/%Y %-H:%M:%S",
    "%-m/%-d/%Y %-I:%M:%S %p",
    // dd/MM/yyyy (European)
    "%-d/%-m/%Y %-H:%M:%S",
    "%-d/%-m/%Y %-I:%M:%S %p",
    // dd.MM.yyyy (German / Central European)
    "%-d.%-m.%Y %-H:%M:%S",
    "%-d.%-m.%Y %-I:%M:%S %p",
    // ISO 8601 with T separator (no timezone suffix)
    "%Y-%-m-%-dT%-H:%M:%S",
];

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error returned when a timestamp cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimestampError {
    /// None of the known locale-dependent formats matched the input.
    #[error("unrecognized timestamp format: {raw:?}")]
    UnrecognizedFormat {
        /// The original timestamp string, preserved for diagnostics.
        raw: String,
    },

    /// The numeric value is out of range for a valid UTC datetime.
    #[error("timestamp value out of range: {value}")]
    OutOfRange {
        /// The numeric value that could not be converted.
        value: i64,
    },
}

// ---------------------------------------------------------------------------
// Public parsing functions
// ---------------------------------------------------------------------------

/// Parses a locale-dependent timestamp from an MTGA log entry header.
///
/// Tries all 11 known locale-dependent formats in sequence until one
/// succeeds. The input should be the timestamp portion extracted from
/// a log entry header line.
///
/// All timestamps are treated as UTC (MTGA does not include timezone
/// information in log entry headers).
///
/// # Errors
///
/// Returns [`TimestampError::UnrecognizedFormat`] if no format matches,
/// preserving the raw string for diagnostics.
pub fn parse_log_timestamp(s: &str) -> Result<DateTime<Utc>, TimestampError> {
    let trimmed = s.trim();

    for fmt in LOCALE_FORMATS {
        if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Ok(naive.and_utc());
        }
    }

    Err(TimestampError::UnrecognizedFormat { raw: s.to_owned() })
}

/// Parses a Unix epoch milliseconds value into a UTC datetime.
///
/// MTGA payloads sometimes express timestamps as milliseconds since
/// 1970-01-01T00:00:00 UTC.
///
/// # Errors
///
/// Returns [`TimestampError::OutOfRange`] if the value cannot be
/// represented as a valid `DateTime<Utc>`.
pub fn parse_epoch_millis(millis: i64) -> Result<DateTime<Utc>, TimestampError> {
    let secs = millis.div_euclid(1000);
    let sub_millis = millis.rem_euclid(1000);
    let nanos = u32::try_from(sub_millis * 1_000_000)
        .map_err(|_| TimestampError::OutOfRange { value: millis })?;
    DateTime::from_timestamp(secs, nanos).ok_or(TimestampError::OutOfRange { value: millis })
}

/// Parses a .NET ticks value into a UTC datetime.
///
/// .NET ticks are 100-nanosecond intervals since 0001-01-01T00:00:00.
/// This function subtracts the .NET-to-Unix epoch offset and converts
/// the remainder to a `DateTime<Utc>`.
///
/// # Errors
///
/// Returns [`TimestampError::OutOfRange`] if the ticks value cannot be
/// represented as a valid `DateTime<Utc>`.
pub fn parse_dotnet_ticks(ticks: i64) -> Result<DateTime<Utc>, TimestampError> {
    let unix_ticks = ticks
        .checked_sub(DOTNET_EPOCH_OFFSET_TICKS)
        .ok_or(TimestampError::OutOfRange { value: ticks })?;
    let secs = unix_ticks.div_euclid(TICKS_PER_SECOND);
    let remaining = unix_ticks.rem_euclid(TICKS_PER_SECOND);
    let nanos =
        u32::try_from(remaining * 100).map_err(|_| TimestampError::OutOfRange { value: ticks })?;
    DateTime::from_timestamp(secs, nanos).ok_or(TimestampError::OutOfRange { value: ticks })
}

/// Parses an ISO 8601 datetime string into a UTC datetime.
///
/// Accepts timezone-aware strings like `"2026-02-17T15:30:00Z"` and
/// `"2026-02-17T15:30:00+05:00"`, as well as naive strings like
/// `"2026-02-17T15:30:00"`. Timezone-aware inputs are normalized to
/// UTC; naive inputs are assumed UTC.
///
/// # Errors
///
/// Returns [`TimestampError::UnrecognizedFormat`] if the string is not
/// valid ISO 8601.
pub fn parse_iso8601(s: &str) -> Result<DateTime<Utc>, TimestampError> {
    let trimmed = s.trim();

    // Try RFC 3339 first (handles Z, +00:00, +05:00, fractional seconds).
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }

    // Fall back to naive ISO 8601 (no timezone suffix), treated as UTC.
    NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S%.f")
        .map(|naive| naive.and_utc())
        .map_err(|_| TimestampError::UnrecognizedFormat { raw: s.to_owned() })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // -- parse_log_timestamp: locale formats --------------------------------

    mod log_timestamp {
        use super::*;

        #[test]
        fn test_parse_log_timestamp_iso_date_24h() -> TestResult {
            let dt = parse_log_timestamp("2025-01-15 14:30:45")?;
            assert_eq!(dt.year(), 2025);
            assert_eq!(dt.month(), 1);
            assert_eq!(dt.day(), 15);
            assert_eq!(dt.hour(), 14);
            assert_eq!(dt.minute(), 30);
            assert_eq!(dt.second(), 45);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_iso_date_12h_am() -> TestResult {
            let dt = parse_log_timestamp("2025-01-15 9:30:45 AM")?;
            assert_eq!(dt.hour(), 9);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_iso_date_12h_pm() -> TestResult {
            let dt = parse_log_timestamp("2025-01-15 3:42:17 PM")?;
            assert_eq!(dt.hour(), 15);
            assert_eq!(dt.minute(), 42);
            assert_eq!(dt.second(), 17);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_iso_date_12h_noon() -> TestResult {
            let dt = parse_log_timestamp("2025-06-01 12:00:00 PM")?;
            assert_eq!(dt.hour(), 12);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_iso_date_12h_midnight() -> TestResult {
            let dt = parse_log_timestamp("2025-06-01 12:00:00 AM")?;
            assert_eq!(dt.hour(), 0);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_slash_iso_24h() -> TestResult {
            let dt = parse_log_timestamp("2025/01/15 14:30:45")?;
            assert_eq!(dt.year(), 2025);
            assert_eq!(dt.month(), 1);
            assert_eq!(dt.day(), 15);
            assert_eq!(dt.hour(), 14);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_slash_iso_12h() -> TestResult {
            let dt = parse_log_timestamp("2025/01/15 3:42:17 PM")?;
            assert_eq!(dt.hour(), 15);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_us_date_24h() -> TestResult {
            // M/d/yyyy — day 15 > 12, so only US format matches.
            let dt = parse_log_timestamp("1/15/2025 14:30:45")?;
            assert_eq!(dt.month(), 1);
            assert_eq!(dt.day(), 15);
            assert_eq!(dt.hour(), 14);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_us_date_12h() -> TestResult {
            let dt = parse_log_timestamp("1/15/2025 3:42:17 PM")?;
            assert_eq!(dt.month(), 1);
            assert_eq!(dt.day(), 15);
            assert_eq!(dt.hour(), 15);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_european_date_24h() -> TestResult {
            // dd/MM/yyyy — day 25 > 12, so US format fails and European
            // matches.
            let dt = parse_log_timestamp("25/02/2026 10:15:30")?;
            assert_eq!(dt.day(), 25);
            assert_eq!(dt.month(), 2);
            assert_eq!(dt.hour(), 10);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_european_date_12h() -> TestResult {
            let dt = parse_log_timestamp("25/02/2026 3:15:30 PM")?;
            assert_eq!(dt.day(), 25);
            assert_eq!(dt.month(), 2);
            assert_eq!(dt.hour(), 15);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_german_date_24h() -> TestResult {
            let dt = parse_log_timestamp("25.02.2026 10:15:30")?;
            assert_eq!(dt.day(), 25);
            assert_eq!(dt.month(), 2);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_german_date_12h() -> TestResult {
            let dt = parse_log_timestamp("25.02.2026 3:15:30 PM")?;
            assert_eq!(dt.day(), 25);
            assert_eq!(dt.month(), 2);
            assert_eq!(dt.hour(), 15);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_iso8601_t_separator() -> TestResult {
            let dt = parse_log_timestamp("2025-01-15T14:30:45")?;
            assert_eq!(dt.year(), 2025);
            assert_eq!(dt.month(), 1);
            assert_eq!(dt.day(), 15);
            assert_eq!(dt.hour(), 14);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_trims_whitespace() -> TestResult {
            let dt = parse_log_timestamp("  2025-01-15 14:30:45  ")?;
            assert_eq!(dt.year(), 2025);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_zero_padded_fields() -> TestResult {
            let dt = parse_log_timestamp("01/05/2025 08:05:09")?;
            assert_eq!(dt.month(), 1);
            assert_eq!(dt.day(), 5);
            assert_eq!(dt.hour(), 8);
            assert_eq!(dt.minute(), 5);
            assert_eq!(dt.second(), 9);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_lowercase_am_pm() -> TestResult {
            // chrono's %p is case-insensitive during parsing.
            let dt = parse_log_timestamp("2025-01-15 3:42:17 pm")?;
            assert_eq!(dt.hour(), 15);
            Ok(())
        }

        #[test]
        fn test_parse_log_timestamp_empty_returns_error() {
            assert!(parse_log_timestamp("").is_err());
        }

        #[test]
        fn test_parse_log_timestamp_garbage_returns_error() {
            assert!(parse_log_timestamp("not a timestamp").is_err());
        }

        #[test]
        fn test_parse_log_timestamp_error_preserves_raw_string() {
            let input = "garbage value 123";
            let err = parse_log_timestamp(input);
            assert!(matches!(
                err,
                Err(TimestampError::UnrecognizedFormat { ref raw })
                    if raw == input
            ));
        }
    }

    // -- parse_epoch_millis -------------------------------------------------

    mod epoch_millis {
        use super::*;
        use chrono::TimeZone;

        #[test]
        fn test_parse_epoch_millis_zero_is_unix_epoch() -> TestResult {
            let dt = parse_epoch_millis(0)?;
            assert_eq!(dt.year(), 1970);
            assert_eq!(dt.month(), 1);
            assert_eq!(dt.day(), 1);
            assert_eq!(dt.hour(), 0);
            Ok(())
        }

        #[test]
        fn test_parse_epoch_millis_known_date() -> TestResult {
            let expected = Utc
                .with_ymd_and_hms(2026, 2, 25, 12, 0, 0)
                .single()
                .unwrap_or_default();
            let dt = parse_epoch_millis(expected.timestamp_millis())?;
            assert_eq!(dt, expected);
            Ok(())
        }

        #[test]
        fn test_parse_epoch_millis_sub_second_precision() -> TestResult {
            let dt = parse_epoch_millis(500)?;
            assert_eq!(dt.nanosecond(), 500_000_000);
            Ok(())
        }

        #[test]
        fn test_parse_epoch_millis_negative_before_epoch() -> TestResult {
            // -1000 ms = 1969-12-31T23:59:59 UTC
            let dt = parse_epoch_millis(-1000)?;
            assert_eq!(dt.year(), 1969);
            assert_eq!(dt.month(), 12);
            assert_eq!(dt.day(), 31);
            assert_eq!(dt.hour(), 23);
            assert_eq!(dt.minute(), 59);
            assert_eq!(dt.second(), 59);
            Ok(())
        }
    }

    // -- parse_dotnet_ticks -------------------------------------------------

    mod dotnet_ticks {
        use super::*;
        use chrono::TimeZone;

        #[test]
        fn test_parse_dotnet_ticks_unix_epoch() -> TestResult {
            let dt = parse_dotnet_ticks(DOTNET_EPOCH_OFFSET_TICKS)?;
            assert_eq!(dt.year(), 1970);
            assert_eq!(dt.month(), 1);
            assert_eq!(dt.day(), 1);
            assert_eq!(dt.hour(), 0);
            Ok(())
        }

        #[test]
        fn test_parse_dotnet_ticks_known_date() -> TestResult {
            let expected = Utc
                .with_ymd_and_hms(2026, 2, 25, 12, 0, 0)
                .single()
                .unwrap_or_default();
            let net_ticks = expected.timestamp() * TICKS_PER_SECOND + DOTNET_EPOCH_OFFSET_TICKS;
            let dt = parse_dotnet_ticks(net_ticks)?;
            assert_eq!(dt, expected);
            Ok(())
        }

        #[test]
        fn test_parse_dotnet_ticks_sub_second_precision() -> TestResult {
            // Unix epoch + 5_000_000 ticks = 0.5 seconds
            let ticks = DOTNET_EPOCH_OFFSET_TICKS + 5_000_000;
            let dt = parse_dotnet_ticks(ticks)?;
            assert_eq!(dt.nanosecond(), 500_000_000);
            Ok(())
        }

        #[test]
        fn test_parse_dotnet_ticks_overflow_returns_error() {
            assert!(parse_dotnet_ticks(i64::MIN).is_err());
        }
    }

    // -- parse_iso8601 ------------------------------------------------------

    mod iso8601 {
        use super::*;

        #[test]
        fn test_parse_iso8601_with_z_suffix() -> TestResult {
            let dt = parse_iso8601("2026-02-17T15:30:00Z")?;
            assert_eq!(dt.year(), 2026);
            assert_eq!(dt.month(), 2);
            assert_eq!(dt.day(), 17);
            assert_eq!(dt.hour(), 15);
            assert_eq!(dt.minute(), 30);
            Ok(())
        }

        #[test]
        fn test_parse_iso8601_with_zero_offset() -> TestResult {
            let dt = parse_iso8601("2026-02-17T15:30:00+00:00")?;
            assert_eq!(dt.hour(), 15);
            Ok(())
        }

        #[test]
        fn test_parse_iso8601_positive_offset_normalizes_to_utc() -> TestResult {
            // +05:00 means local 15:30 = UTC 10:30
            let dt = parse_iso8601("2026-02-17T15:30:00+05:00")?;
            assert_eq!(dt.hour(), 10);
            assert_eq!(dt.minute(), 30);
            Ok(())
        }

        #[test]
        fn test_parse_iso8601_negative_offset_normalizes_to_utc() -> TestResult {
            // -08:00 means local 15:30 = UTC 23:30
            let dt = parse_iso8601("2026-02-17T15:30:00-08:00")?;
            assert_eq!(dt.hour(), 23);
            assert_eq!(dt.minute(), 30);
            Ok(())
        }

        #[test]
        fn test_parse_iso8601_naive_treated_as_utc() -> TestResult {
            let dt = parse_iso8601("2026-02-17T15:30:00")?;
            assert_eq!(dt.hour(), 15);
            Ok(())
        }

        #[test]
        fn test_parse_iso8601_with_fractional_seconds() -> TestResult {
            let dt = parse_iso8601("2026-02-17T15:30:00.123Z")?;
            assert_eq!(dt.nanosecond(), 123_000_000);
            Ok(())
        }

        #[test]
        fn test_parse_iso8601_trims_whitespace() -> TestResult {
            let dt = parse_iso8601("  2026-02-17T15:30:00Z  ")?;
            assert_eq!(dt.year(), 2026);
            Ok(())
        }

        #[test]
        fn test_parse_iso8601_invalid_returns_error() {
            assert!(parse_iso8601("not-a-date").is_err());
        }

        #[test]
        fn test_parse_iso8601_error_preserves_raw_string() {
            let input = "bad-iso-input";
            let err = parse_iso8601(input);
            assert!(matches!(
                err,
                Err(TimestampError::UnrecognizedFormat { ref raw })
                    if raw == input
            ));
        }
    }

    // -- TimestampError -----------------------------------------------------

    mod error {
        use super::*;

        #[test]
        fn test_unrecognized_format_display() {
            let err = TimestampError::UnrecognizedFormat {
                raw: "bad".to_owned(),
            };
            let msg = err.to_string();
            assert!(msg.contains("bad"));
            assert!(msg.contains("unrecognized"));
        }

        #[test]
        fn test_out_of_range_display() {
            let err = TimestampError::OutOfRange { value: -999 };
            let msg = err.to_string();
            assert!(msg.contains("-999"));
            assert!(msg.contains("out of range"));
        }

        #[test]
        fn test_error_clone_is_equal() {
            let err = TimestampError::UnrecognizedFormat {
                raw: "test".to_owned(),
            };
            let cloned = err.clone();
            assert_eq!(err, cloned);
        }
    }
}
