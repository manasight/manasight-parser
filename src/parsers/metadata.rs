//! Metadata event parser: `DETAILED LOGS` status detection.
//!
//! Recognizes the `DETAILED LOGS: ENABLED` and `DETAILED LOGS: DISABLED`
//! lines that Arena writes near the top of every session (typically line 24
//! of `Player.log`). These lines have no bracket header prefix and are
//! recognized by [`LineBuffer`] as [`EntryHeader::Metadata`] entries.
//!
//! [`LineBuffer`]: crate::log::entry::LineBuffer
//! [`EntryHeader::Metadata`]: crate::log::entry::EntryHeader::Metadata

use crate::events::{DetailedLoggingStatusEvent, EventMetadata, GameEvent};
use crate::log::entry::{EntryHeader, LogEntry};

/// Marker text for enabled detailed logging.
const DETAILED_LOGS_ENABLED: &str = "DETAILED LOGS: ENABLED";

/// Marker text for disabled detailed logging.
const DETAILED_LOGS_DISABLED: &str = "DETAILED LOGS: DISABLED";

/// Attempts to parse a [`LogEntry`] as a metadata event.
///
/// Returns `Some(GameEvent::DetailedLoggingStatus(_))` if the entry is a
/// `DETAILED LOGS` metadata line, or `None` otherwise.
///
/// The `timestamp` is `None` because metadata lines do not carry a
/// timestamp in the log. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    if entry.header != EntryHeader::Metadata {
        return None;
    }

    let trimmed = entry.body.trim();

    let enabled = if trimmed == DETAILED_LOGS_ENABLED {
        true
    } else if trimmed == DETAILED_LOGS_DISABLED {
        false
    } else {
        return None;
    };

    let metadata = EventMetadata::new(timestamp, entry.body.as_bytes().to_vec());
    Some(GameEvent::DetailedLoggingStatus(
        DetailedLoggingStatusEvent::new(metadata, serde_json::json!({ "enabled": enabled })),
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::test_helpers::test_timestamp;

    /// Helper: build a metadata `LogEntry` from body text.
    fn metadata_entry(body: &str) -> LogEntry {
        LogEntry {
            header: EntryHeader::Metadata,
            body: body.to_owned(),
        }
    }

    // -- Detailed logs detection -----------------------------------------------

    mod detailed_logs {
        use super::*;

        #[test]
        fn test_try_parse_detailed_logs_enabled() {
            let entry = metadata_entry("DETAILED LOGS: ENABLED");
            let result = try_parse(&entry, None);

            assert!(result.is_some());
            assert!(
                matches!(
                    &result,
                    Some(GameEvent::DetailedLoggingStatus(e)) if e.enabled() == Some(true)
                ),
                "expected DetailedLoggingStatus(enabled=true), got {result:?}"
            );
        }

        #[test]
        fn test_try_parse_detailed_logs_disabled() {
            let entry = metadata_entry("DETAILED LOGS: DISABLED");
            let result = try_parse(&entry, None);

            assert!(result.is_some());
            assert!(
                matches!(
                    &result,
                    Some(GameEvent::DetailedLoggingStatus(e)) if e.enabled() == Some(false)
                ),
                "expected DetailedLoggingStatus(enabled=false), got {result:?}"
            );
        }

        #[test]
        fn test_try_parse_preserves_raw_bytes() {
            let entry = metadata_entry("DETAILED LOGS: ENABLED");
            let result = try_parse(&entry, None);

            assert!(result.is_some());
            if let Some(ref event) = result {
                assert_eq!(event.metadata().raw_bytes(), b"DETAILED LOGS: ENABLED");
            }
        }

        #[test]
        fn test_try_parse_passes_through_timestamp() {
            let ts = Some(test_timestamp());
            let entry = metadata_entry("DETAILED LOGS: ENABLED");
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            if let Some(ref event) = result {
                assert_eq!(event.metadata().timestamp(), ts);
            }
        }

        #[test]
        fn test_try_parse_none_timestamp() {
            let entry = metadata_entry("DETAILED LOGS: DISABLED");
            let result = try_parse(&entry, None);

            assert!(result.is_some());
            if let Some(ref event) = result {
                assert!(event.metadata().timestamp().is_none());
            }
        }
    }

    // -- Non-matching entries -------------------------------------------------

    mod non_matching {
        use super::*;

        #[test]
        fn test_try_parse_unrelated_metadata_returns_none() {
            let entry = metadata_entry("some other metadata line");
            assert!(try_parse(&entry, None).is_none());
        }

        #[test]
        fn test_try_parse_unity_header_returns_none() {
            let entry = LogEntry {
                header: EntryHeader::UnityCrossThreadLogger,
                body: "DETAILED LOGS: ENABLED".to_owned(),
            };
            assert!(try_parse(&entry, None).is_none());
        }

        #[test]
        fn test_try_parse_client_gre_header_returns_none() {
            let entry = LogEntry {
                header: EntryHeader::ClientGre,
                body: "DETAILED LOGS: ENABLED".to_owned(),
            };
            assert!(try_parse(&entry, None).is_none());
        }

        #[test]
        fn test_try_parse_similar_text_returns_none() {
            let entry = metadata_entry("DETAILED LOGS: UNKNOWN");
            assert!(try_parse(&entry, None).is_none());
        }

        #[test]
        fn test_try_parse_empty_body_returns_none() {
            let entry = metadata_entry("");
            assert!(try_parse(&entry, None).is_none());
        }
    }
}
