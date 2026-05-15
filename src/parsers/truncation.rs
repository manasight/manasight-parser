//! GSM truncation marker parser.
//!
//! Recognizes Arena's `[Message summarized because one or more
//! GameStateMessages exceeded the 50 GameObject or 50 Annotation limit.]`
//! marker block and emits a [`GameEvent::Truncation`] carrying the parsed
//! `GameObject Count` and `Annotation Count` values.
//!
//! The marker block has the form:
//!
//! ```text
//! [Message summarized because one or more GameStateMessages exceeded the 50 GameObject or 50 Annotation limit.]
//! ::: GameStateMessage
//! :: GameObject Count = 63
//! :: Annotation Count = 4
//! ::: ActionsAvailableReq
//! ```
//!
//! The [`LineBuffer`](crate::log::entry::LineBuffer) classifies the marker
//! line as [`EntryHeader::TruncationMarker`] and accumulates the follow-on
//! lines into the entry body. This parser then extracts the two counts via
//! line-prefix matching.
//!
//! The truncated `GameStateMessage` body is irrecoverable from `Player.log`;
//! the event surfaces the signal so downstream consumers (deck tracker) can
//! mark the next `gsm_id` as crossing a data-loss gap.

use crate::events::{GameEvent, TruncationEvent};
use crate::log::entry::{EntryHeader, LogEntry};

/// Line prefix for the truncated GSM's reported game-object count.
const OBJECT_COUNT_PREFIX: &str = ":: GameObject Count = ";

/// Line prefix for the truncated GSM's reported annotation count.
const ANNOTATION_COUNT_PREFIX: &str = ":: Annotation Count = ";

/// Attempts to parse a [`LogEntry`] as a GSM truncation event.
///
/// Returns `Some(GameEvent::Truncation(_))` when the entry's header is
/// [`EntryHeader::TruncationMarker`] **and** both the `GameObject Count`
/// and `Annotation Count` follow-on lines are present and parseable as
/// `u32`. Returns `None` if the header doesn't match or either count is
/// missing or malformed — partial markers are not surfaced as events
/// because consumers need both counts to assess the size of the lost GSM.
///
/// The `timestamp` is forwarded to [`EventMetadata`](crate::events::EventMetadata)
/// so downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    if entry.header != EntryHeader::TruncationMarker {
        return None;
    }

    let object_count = extract_count(&entry.body, OBJECT_COUNT_PREFIX)?;
    let annotation_count = extract_count(&entry.body, ANNOTATION_COUNT_PREFIX)?;

    Some(GameEvent::Truncation(TruncationEvent::new_truncation(
        timestamp,
        object_count,
        annotation_count,
    )))
}

/// Scans the entry body for the first line whose trimmed form starts with
/// `prefix`, parses the trailing value as a `u32`, and returns it.
fn extract_count(body: &str, prefix: &str) -> Option<u32> {
    body.lines()
        .find_map(|line| line.trim_start().strip_prefix(prefix))
        .and_then(|tail| tail.trim().parse::<u32>().ok())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::test_helpers::test_timestamp;

    /// Helper: build a `LogEntry` with the truncation header and the given body.
    fn truncation_entry(body: &str) -> LogEntry {
        LogEntry {
            header: EntryHeader::TruncationMarker,
            body: body.to_owned(),
        }
    }

    /// Fixture: the canonical 5-line marker block (marker + 3 sub-headers +
    /// the next-message header `::: ActionsAvailableReq` that the line
    /// buffer accumulates into the body until a real next header arrives).
    fn marker_block(object_count: u32, annotation_count: u32) -> String {
        format!(
            "[Message summarized because one or more GameStateMessages \
             exceeded the 50 GameObject or 50 Annotation limit.]\n\
             ::: GameStateMessage\n\
             :: GameObject Count = {object_count}\n\
             :: Annotation Count = {annotation_count}\n\
             ::: ActionsAvailableReq"
        )
    }

    #[test]
    fn test_try_parse_emits_truncation_event() {
        let entry = truncation_entry(&marker_block(63, 4));
        let event = try_parse(&entry, Some(test_timestamp()));
        assert!(matches!(event, Some(GameEvent::Truncation(_))));
    }

    #[test]
    fn test_try_parse_extracts_object_count() {
        let entry = truncation_entry(&marker_block(63, 4));
        let Some(GameEvent::Truncation(event)) = try_parse(&entry, Some(test_timestamp())) else {
            unreachable!("expected Truncation event");
        };
        assert_eq!(event.object_count(), Some(63));
    }

    #[test]
    fn test_try_parse_extracts_annotation_count() {
        let entry = truncation_entry(&marker_block(63, 4));
        let Some(GameEvent::Truncation(event)) = try_parse(&entry, Some(test_timestamp())) else {
            unreachable!("expected Truncation event");
        };
        assert_eq!(event.annotation_count(), Some(4));
    }

    #[test]
    fn test_try_parse_passes_through_timestamp() {
        let entry = truncation_entry(&marker_block(63, 4));
        let ts = Some(test_timestamp());
        let event = try_parse(&entry, ts);
        let Some(GameEvent::Truncation(event)) = event else {
            unreachable!("expected Truncation event");
        };
        assert_eq!(event.metadata().timestamp(), ts);
    }

    #[test]
    fn test_try_parse_emits_with_no_timestamp() {
        let entry = truncation_entry(&marker_block(7, 11));
        let event = try_parse(&entry, None);
        let Some(GameEvent::Truncation(event)) = event else {
            unreachable!("expected Truncation event even without timestamp");
        };
        assert!(event.metadata().timestamp().is_none());
        assert_eq!(event.object_count(), Some(7));
        assert_eq!(event.annotation_count(), Some(11));
    }

    #[test]
    fn test_try_parse_wrong_header_returns_none() {
        let entry = LogEntry {
            header: EntryHeader::UnityCrossThreadLogger,
            // Body coincidentally contains the count prefixes — wrong header
            // still bails out.
            body: marker_block(63, 4),
        };
        assert!(try_parse(&entry, Some(test_timestamp())).is_none());
    }

    #[test]
    fn test_try_parse_missing_object_count_returns_none() {
        let body = "[Message summarized because one or more GameStateMessages \
                    exceeded the 50 GameObject or 50 Annotation limit.]\n\
                    ::: GameStateMessage\n\
                    :: Annotation Count = 4\n\
                    ::: ActionsAvailableReq";
        let entry = truncation_entry(body);
        assert!(try_parse(&entry, Some(test_timestamp())).is_none());
    }

    #[test]
    fn test_try_parse_missing_annotation_count_returns_none() {
        let body = "[Message summarized because one or more GameStateMessages \
                    exceeded the 50 GameObject or 50 Annotation limit.]\n\
                    ::: GameStateMessage\n\
                    :: GameObject Count = 63\n\
                    ::: ActionsAvailableReq";
        let entry = truncation_entry(body);
        assert!(try_parse(&entry, Some(test_timestamp())).is_none());
    }

    #[test]
    fn test_try_parse_unparseable_count_returns_none() {
        let body = "[Message summarized because one or more GameStateMessages \
                    exceeded the 50 GameObject or 50 Annotation limit.]\n\
                    :: GameObject Count = NaN\n\
                    :: Annotation Count = 4";
        let entry = truncation_entry(body);
        assert!(try_parse(&entry, Some(test_timestamp())).is_none());
    }

    #[test]
    fn test_try_parse_leading_whitespace_tolerated() {
        // Defensive: line indentation in real logs should not break extraction.
        let body = "[Message summarized because one or more GameStateMessages \
                    exceeded the 50 GameObject or 50 Annotation limit.]\n\
                    \t:: GameObject Count = 51\n\
                    \t:: Annotation Count = 0";
        let entry = truncation_entry(body);
        let Some(GameEvent::Truncation(event)) = try_parse(&entry, Some(test_timestamp())) else {
            unreachable!("expected Truncation event");
        };
        assert_eq!(event.object_count(), Some(51));
        assert_eq!(event.annotation_count(), Some(0));
    }

    #[test]
    fn test_try_parse_marker_only_body_returns_none() {
        // Marker line only, no count lines — not enough info to act on.
        let body = "[Message summarized because one or more GameStateMessages \
                    exceeded the 50 GameObject or 50 Annotation limit.]";
        let entry = truncation_entry(body);
        assert!(try_parse(&entry, Some(test_timestamp())).is_none());
    }
}
