//! STATE CHANGED parser: match connection state machine transitions.
//!
//! Parses Unity cross-thread logger lines like:
//!   `[UnityCrossThreadLogger]STATE CHANGED {"old":"Playing","new":"Disconnected"}`
//!
//! These entries track the match connection lifecycle and are the
//! definitive signal for local-client disconnect detection. See feature
//! spec `connection-health-indicator.md` **AC-DET-1**.
//!
//! Observed state transitions (deduped across #528/#529 disconnect corpus):
//! - `None → ConnectedToMatchDoor`
//! - `ConnectedToMatchDoor → ConnectedToMatchDoor_ConnectingToGRE`
//! - `ConnectedToMatchDoor_ConnectingToGRE → ConnectedToMatchDoor_ConnectedToGRE_Waiting`
//! - `ConnectedToMatchDoor_ConnectingToGRE → Playing` (fast-path variant)
//! - `ConnectedToMatchDoor_ConnectedToGRE_Waiting → Playing`
//! - `Playing → MatchCompleted`
//! - `Playing → Disconnected`
//! - `MatchCompleted → Disconnected`
//! - `None → Disconnected`
//!
//! macOS recovery sequence: recovery goes through `None`, not directly
//! from `Disconnected`. After a `Playing → Disconnected`, the subsequent
//! recovery transition is `None → ConnectedToMatchDoor`.

use crate::events::{EventMetadata, GameEvent, MatchConnectionStateEvent};
use crate::log::entry::{EntryHeader, LogEntry};
use crate::parsers::api_common;

/// Marker text that identifies a STATE CHANGED entry within the body.
const STATE_CHANGED_MARKER: &str = "STATE CHANGED ";

/// Attempts to parse a [`LogEntry`] as a match connection state event.
///
/// Returns `Some(GameEvent::MatchConnectionState(_))` if the entry is a
/// `[UnityCrossThreadLogger]STATE CHANGED {...}` line with a well-formed
/// `{"old": "...", "new": "..."}` JSON payload, or `None` otherwise.
///
/// The payload is emitted as `{"old": "<state>", "new": "<state>"}`.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    if entry.header != EntryHeader::UnityCrossThreadLogger {
        return None;
    }
    if !entry.body.contains(STATE_CHANGED_MARKER) {
        return None;
    }

    let json_str = api_common::extract_json_from_body(&entry.body)?;
    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            ::log::warn!("STATE CHANGED: malformed JSON payload: {e}");
            return None;
        }
    };

    let old_state = parsed.get("old")?.as_str()?.to_owned();
    let new_state = parsed.get("new")?.as_str()?.to_owned();

    let metadata = EventMetadata::new(timestamp, entry.body.as_bytes().to_vec());
    Some(GameEvent::MatchConnectionState(
        MatchConnectionStateEvent::new(
            metadata,
            serde_json::json!({ "old": old_state, "new": new_state }),
        ),
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::test_helpers::{
        match_connection_state_payload, test_timestamp, unity_entry,
    };

    /// Build a STATE CHANGED unity entry body from old/new state strings.
    fn state_body(old: &str, new: &str) -> String {
        format!("[UnityCrossThreadLogger]STATE CHANGED {{\"old\":\"{old}\",\"new\":\"{new}\"}}")
    }

    /// Assert the parser produced a `MatchConnectionState` event whose
    /// payload matches the given `old` and `new` state strings.
    fn assert_transition(entry: &LogEntry, old: &str, new: &str) {
        let result = try_parse(entry, Some(test_timestamp()));
        assert!(
            result.is_some(),
            "expected Some(MatchConnectionState), got None for body {:?}",
            entry.body
        );
        let event = result.as_ref().unwrap_or_else(|| unreachable!());
        assert!(
            matches!(event, GameEvent::MatchConnectionState(_)),
            "expected GameEvent::MatchConnectionState, got {event:?}"
        );
        let payload = match_connection_state_payload(event);
        assert_eq!(payload["old"], old, "old state mismatch");
        assert_eq!(payload["new"], new, "new state mismatch");
    }

    // -- Observed state transitions (one test per transition) ------------------

    mod transitions {
        use super::*;

        #[test]
        fn test_parses_none_to_connected_to_match_door() {
            let body = state_body("None", "ConnectedToMatchDoor");
            let entry = unity_entry(&body);
            assert_transition(&entry, "None", "ConnectedToMatchDoor");
        }

        #[test]
        fn test_parses_match_door_to_connecting_to_gre() {
            let body = state_body(
                "ConnectedToMatchDoor",
                "ConnectedToMatchDoor_ConnectingToGRE",
            );
            let entry = unity_entry(&body);
            assert_transition(
                &entry,
                "ConnectedToMatchDoor",
                "ConnectedToMatchDoor_ConnectingToGRE",
            );
        }

        #[test]
        fn test_parses_connecting_to_gre_to_waiting() {
            let body = state_body(
                "ConnectedToMatchDoor_ConnectingToGRE",
                "ConnectedToMatchDoor_ConnectedToGRE_Waiting",
            );
            let entry = unity_entry(&body);
            assert_transition(
                &entry,
                "ConnectedToMatchDoor_ConnectingToGRE",
                "ConnectedToMatchDoor_ConnectedToGRE_Waiting",
            );
        }

        #[test]
        fn test_parses_fast_path_connecting_to_gre_to_playing() {
            // Fast-path variant: skips the `_Waiting` step.
            let body = state_body("ConnectedToMatchDoor_ConnectingToGRE", "Playing");
            let entry = unity_entry(&body);
            assert_transition(&entry, "ConnectedToMatchDoor_ConnectingToGRE", "Playing");
        }

        #[test]
        fn test_parses_waiting_to_playing() {
            let body = state_body("ConnectedToMatchDoor_ConnectedToGRE_Waiting", "Playing");
            let entry = unity_entry(&body);
            assert_transition(
                &entry,
                "ConnectedToMatchDoor_ConnectedToGRE_Waiting",
                "Playing",
            );
        }

        #[test]
        fn test_parses_playing_to_match_completed() {
            let body = state_body("Playing", "MatchCompleted");
            let entry = unity_entry(&body);
            assert_transition(&entry, "Playing", "MatchCompleted");
        }

        #[test]
        fn test_parses_playing_to_disconnected() {
            let body = state_body("Playing", "Disconnected");
            let entry = unity_entry(&body);
            assert_transition(&entry, "Playing", "Disconnected");
        }

        #[test]
        fn test_parses_match_completed_to_disconnected() {
            let body = state_body("MatchCompleted", "Disconnected");
            let entry = unity_entry(&body);
            assert_transition(&entry, "MatchCompleted", "Disconnected");
        }

        #[test]
        fn test_parses_none_to_disconnected() {
            let body = state_body("None", "Disconnected");
            let entry = unity_entry(&body);
            assert_transition(&entry, "None", "Disconnected");
        }
    }

    // -- macOS recovery pattern -----------------------------------------------

    mod macos_recovery {
        use super::*;

        /// After a `Playing → Disconnected`, the subsequent recovery STATE
        /// CHANGED is `None → ConnectedToMatchDoor`, not a direct
        /// `Disconnected → ConnectedToMatchDoor`. Both entries parse
        /// independently.
        #[test]
        fn test_parses_macos_recovery_sequence() {
            let disconnect_body = state_body("Playing", "Disconnected");
            let disconnect_entry = unity_entry(&disconnect_body);
            assert_transition(&disconnect_entry, "Playing", "Disconnected");

            // Reconnect coroutine executes; old resets to "None" before the
            // next STATE CHANGED is emitted.
            let recovery_body = state_body("None", "ConnectedToMatchDoor");
            let recovery_entry = unity_entry(&recovery_body);
            assert_transition(&recovery_entry, "None", "ConnectedToMatchDoor");
        }
    }

    // -- Non-matching entries (should return None) ----------------------------

    mod non_matching {
        use super::*;

        #[test]
        fn test_non_state_changed_unity_body_returns_none() {
            let entry =
                unity_entry("[UnityCrossThreadLogger]FrontDoorConnection.Close some details");
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_empty_unity_body_returns_none() {
            let entry = unity_entry("[UnityCrossThreadLogger]");
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_client_gre_header_returns_none() {
            let entry = LogEntry {
                header: EntryHeader::ClientGre,
                body: "[Client GRE]STATE CHANGED {\"old\":\"Playing\",\"new\":\"Disconnected\"}"
                    .to_owned(),
            };
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_metadata_header_returns_none() {
            let entry = LogEntry {
                header: EntryHeader::Metadata,
                body: "STATE CHANGED {\"old\":\"Playing\",\"new\":\"Disconnected\"}".to_owned(),
            };
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_malformed_json_returns_none() {
            // Well-formed marker but invalid JSON — should log a warning and
            // return None rather than emitting an event.
            let entry =
                unity_entry("[UnityCrossThreadLogger]STATE CHANGED {\"old\":\"Playing\",new}");
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_missing_old_field_returns_none() {
            let entry =
                unity_entry("[UnityCrossThreadLogger]STATE CHANGED {\"new\":\"Disconnected\"}");
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }

        #[test]
        fn test_missing_new_field_returns_none() {
            let entry = unity_entry("[UnityCrossThreadLogger]STATE CHANGED {\"old\":\"Playing\"}");
            assert!(try_parse(&entry, Some(test_timestamp())).is_none());
        }
    }

    // -- Metadata preservation ------------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_metadata_preserves_raw_bytes() {
            let body = state_body("Playing", "Disconnected");
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_metadata_preserves_timestamp() {
            let body = state_body("Playing", "Disconnected");
            let entry = unity_entry(&body);
            let ts = Some(test_timestamp());
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_metadata_passes_through_none_timestamp() {
            let body = state_body("Playing", "Disconnected");
            let entry = unity_entry(&body);
            let result = try_parse(&entry, None);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert!(event.metadata().timestamp().is_none());
        }
    }
}
