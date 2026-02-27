//! Game result parser for `LogBusinessEvents` with `WinningType`.
//!
//! Detects game completion and emits the event that triggers
//! Class 3 batch finalization in downstream consumers.
//!
//! # Log signatures
//!
//! The game result is identified by `LogBusinessEvents` entries that contain
//! a `WinningType` field. These entries carry:
//!
//! | Field | Purpose |
//! |-------|---------|
//! | `WinningType` | How the game was won (e.g., `"WinLoss"`, `"Draw"`) |
//! | `WinningTeamId` | Which team (seat) won |
//! | `WinningReason` | Why the game ended (e.g., concede, life total) |
//! | `GameNumber` | Game number within the match (1, 2, 3 for Bo3) |
//! | `StartingTeamId` | Which team went first |
//!
//! A separate signal, `GameStage_GameOver`, may appear in `gameInfo.stage`
//! within a GRE `GameStateMessage`. This parser focuses on the
//! `LogBusinessEvents` variant, which carries richer result metadata.
//!
//! **Important**: `LogBusinessEvents` is shared with human draft pick events
//! (`PickGrpId`). This parser only matches entries that contain `WinningType`
//! to avoid false positives on draft pick events.
//!
//! This is a Class 3 (Post-Game Batch) event. When emitted, the desktop
//! app assembles and uploads the accumulated game buffer.

use crate::events::{EventMetadata, GameEvent, GameResultEvent};
use crate::log::entry::LogEntry;

/// Marker that identifies business event entries in the log.
///
/// `LogBusinessEvents` is a shared container used for multiple event types
/// (game results, draft picks, etc.). We further discriminate by checking
/// for the `WinningType` field.
const BUSINESS_EVENTS_MARKER: &str = "LogBusinessEvents";

/// Field that distinguishes game result business events from other
/// `LogBusinessEvents` entries (e.g., draft picks with `PickGrpId`).
const WINNING_TYPE_FIELD: &str = "WinningType";

/// Attempts to parse a [`LogEntry`] as a game result event.
///
/// Returns `Some(GameEvent::GameResult(_))` if the entry is a
/// `LogBusinessEvents` containing a `WinningType` field, or `None`
/// if the entry does not match.
///
/// The `timestamp` is used to construct [`EventMetadata`] for the resulting
/// event. Callers are responsible for parsing the timestamp from the log
/// entry header before invoking this function.
pub fn try_parse(entry: &LogEntry, timestamp: chrono::DateTime<chrono::Utc>) -> Option<GameEvent> {
    let body = &entry.body;

    // Quick check: bail early if the business events marker is not present.
    if !body.contains(BUSINESS_EVENTS_MARKER) {
        return None;
    }

    // Must also contain WinningType to be a game result (not a draft pick).
    if !body.contains(WINNING_TYPE_FIELD) {
        return None;
    }

    // Extract the JSON payload from the body.
    let json_str = extract_json_from_body(body)?;

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            ::log::warn!("LogBusinessEvents (game result): malformed JSON payload: {e}");
            return None;
        }
    };

    // Verify the parsed JSON actually contains WinningType (not just in
    // a surrounding text comment).
    if !has_winning_type(&parsed) {
        return None;
    }

    let payload = build_payload(&parsed);
    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::GameResult(GameResultEvent::new(
        metadata, payload,
    )))
}

/// Checks whether the parsed JSON contains a `WinningType` field.
///
/// Searches the top level and also within a `Params` object, since
/// `LogBusinessEvents` entries may nest their fields under a `Params` key.
fn has_winning_type(parsed: &serde_json::Value) -> bool {
    // Top level.
    if parsed.get(WINNING_TYPE_FIELD).is_some() {
        return true;
    }

    // Inside a `Params` object.
    if let Some(params) = parsed.get("Params") {
        if params.get(WINNING_TYPE_FIELD).is_some() {
            return true;
        }
    }

    // Inside a top-level array of business events.
    if let Some(arr) = parsed.as_array() {
        return arr
            .iter()
            .any(|item| item.get(WINNING_TYPE_FIELD).is_some());
    }

    false
}

/// Builds a structured payload from the game result business event.
///
/// Extracts key fields into a flat payload for downstream consumers.
/// Falls back to empty/default values for any missing fields.
fn build_payload(parsed: &serde_json::Value) -> serde_json::Value {
    // Business events may wrap fields under a `Params` key.
    let source = parsed
        .get("Params")
        .or(Some(parsed))
        .filter(|v| v.get(WINNING_TYPE_FIELD).is_some())
        .or_else(|| find_winning_entry_in_array(parsed));

    let source = source.unwrap_or(parsed);

    let winning_type = source
        .get("WinningType")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let winning_team_id = source
        .get("WinningTeamId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let winning_reason = source
        .get("WinningReason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let game_number = source
        .get("GameNumber")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    let starting_team_id = source
        .get("StartingTeamId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    // Match ID may be present at various levels.
    let match_id = source
        .get("MatchId")
        .or_else(|| source.get("matchId"))
        .or_else(|| parsed.get("MatchId"))
        .or_else(|| parsed.get("matchId"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    // Event ID (the Arena event, e.g., "CompDraft_MKM_20260201").
    let event_id = source
        .get("EventId")
        .or_else(|| source.get("InternalEventName"))
        .or_else(|| parsed.get("EventId"))
        .or_else(|| parsed.get("InternalEventName"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let mut payload = serde_json::json!({
        "type": "game_result",
        "winning_type": winning_type,
        "winning_team_id": winning_team_id,
        "winning_reason": winning_reason,
        "game_number": game_number,
        "starting_team_id": starting_team_id,
        "match_id": match_id,
        "event_id": event_id,
    });

    // Preserve the full raw business event for consumers that need
    // deeper fields.
    payload["raw_business_event"] = parsed.clone();

    payload
}

/// Searches a top-level JSON array for the first entry with `WinningType`.
///
/// `LogBusinessEvents` can be an array of event objects; this finds the
/// game-result entry within that array.
fn find_winning_entry_in_array(parsed: &serde_json::Value) -> Option<&serde_json::Value> {
    parsed
        .as_array()?
        .iter()
        .find(|item| item.get(WINNING_TYPE_FIELD).is_some())
}

/// Extracts the first JSON object or array from a multi-line log body.
///
/// The log header line may contain brackets (e.g., `[UnityCrossThreadLogger]`)
/// that must not be confused with JSON array delimiters. This function
/// determines a safe search start offset by skipping any `[...]` header
/// prefix, then finds the first `{` or `[` from that offset.
fn extract_json_from_body(body: &str) -> Option<&str> {
    // If the body starts with a `[...]` header prefix, skip past it
    // so we don't match the header bracket as a JSON array start.
    let search_start = if body.starts_with('[') {
        // Find the closing `]` of the header prefix.
        body.find(']').map_or(0, |pos| pos + 1)
    } else {
        0
    };

    let search_region = &body[search_start..];
    let json_start = search_region.find(['{', '['])?;
    let json_start = search_start + json_start;

    let candidate = &body[json_start..];

    let first_byte = candidate.as_bytes().first().copied()?;
    let (open_char, close_char) = if first_byte == b'{' {
        ('{', '}')
    } else {
        ('[', ']')
    };

    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape_next = false;
    let mut end_pos = None;

    for (i, ch) in candidate.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => {
                escape_next = true;
            }
            '"' => {
                in_string = !in_string;
            }
            c if !in_string && c == open_char => {
                depth += 1;
            }
            c if !in_string && c == close_char => {
                depth -= 1;
                if depth == 0 {
                    end_pos = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }

    end_pos.map(|end| &candidate[..end])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::PerformanceClass;
    use crate::log::entry::EntryHeader;
    use chrono::{TimeZone, Utc};

    /// Helper: build a UTC timestamp for tests.
    ///
    /// Uses `unwrap_or_default()` because `clippy::expect_used` is denied
    /// crate-wide. The epoch fallback would visibly fail timestamp assertions.
    fn test_timestamp() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 2, 25, 12, 0, 0)
            .single()
            .unwrap_or_default()
    }

    /// Helper: build a `LogEntry` with `UnityCrossThreadLogger` header.
    fn unity_entry(body: &str) -> LogEntry {
        LogEntry {
            header: EntryHeader::UnityCrossThreadLogger,
            body: body.to_owned(),
        }
    }

    /// Helper: extract the JSON payload from a `GameEvent::GameResult` variant.
    ///
    /// Returns a static null value if the variant is not `GameResult`,
    /// which will cause assertion failures that clearly indicate the wrong
    /// variant was produced.
    fn game_result_payload(event: &GameEvent) -> &serde_json::Value {
        static EMPTY: std::sync::LazyLock<serde_json::Value> =
            std::sync::LazyLock::new(|| serde_json::json!(null));
        match event {
            GameEvent::GameResult(e) => e.payload(),
            _ => &EMPTY,
        }
    }

    // -- Basic game result parsing -------------------------------------------

    mod basic_parsing {
        use super::*;

        #[test]
        fn test_try_parse_game_result_basic_win() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"WinningType\": \"WinLoss\",\n\
                           \"WinningTeamId\": 1,\n\
                           \"WinningReason\": \"ResultReason_Game\",\n\
                           \"GameNumber\": 1,\n\
                           \"StartingTeamId\": 2\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["type"], "game_result");
            assert_eq!(payload["winning_type"], "WinLoss");
            assert_eq!(payload["winning_team_id"], 1);
            assert_eq!(payload["winning_reason"], "ResultReason_Game");
            assert_eq!(payload["game_number"], 1);
            assert_eq!(payload["starting_team_id"], 2);
        }

        #[test]
        fn test_try_parse_game_result_draw() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"WinningType\": \"Draw\",\n\
                           \"WinningTeamId\": 0,\n\
                           \"WinningReason\": \"ResultReason_Draw\",\n\
                           \"GameNumber\": 2,\n\
                           \"StartingTeamId\": 1\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["winning_type"], "Draw");
            assert_eq!(payload["winning_team_id"], 0);
            assert_eq!(payload["winning_reason"], "ResultReason_Draw");
            assert_eq!(payload["game_number"], 2);
        }

        #[test]
        fn test_try_parse_game_result_concession() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"WinningType\": \"WinLoss\",\n\
                           \"WinningTeamId\": 2,\n\
                           \"WinningReason\": \"ResultReason_Concede\",\n\
                           \"GameNumber\": 1,\n\
                           \"StartingTeamId\": 1\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["winning_type"], "WinLoss");
            assert_eq!(payload["winning_team_id"], 2);
            assert_eq!(payload["winning_reason"], "ResultReason_Concede");
        }

        #[test]
        fn test_try_parse_game_result_with_match_and_event_ids() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"WinningType\": \"WinLoss\",\n\
                           \"WinningTeamId\": 1,\n\
                           \"WinningReason\": \"ResultReason_Game\",\n\
                           \"GameNumber\": 1,\n\
                           \"StartingTeamId\": 1,\n\
                           \"MatchId\": \"abc-def-123\",\n\
                           \"InternalEventName\": \"CompDraft_MKM_20260201\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["match_id"], "abc-def-123");
            assert_eq!(payload["event_id"], "CompDraft_MKM_20260201");
        }

        #[test]
        fn test_try_parse_game_result_bo3_game2() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"WinningType\": \"WinLoss\",\n\
                           \"WinningTeamId\": 2,\n\
                           \"WinningReason\": \"ResultReason_Game\",\n\
                           \"GameNumber\": 2,\n\
                           \"StartingTeamId\": 2\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["game_number"], 2);
            assert_eq!(payload["winning_team_id"], 2);
        }

        #[test]
        fn test_try_parse_game_result_bo3_game3() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"WinningType\": \"WinLoss\",\n\
                           \"WinningTeamId\": 1,\n\
                           \"WinningReason\": \"ResultReason_Game\",\n\
                           \"GameNumber\": 3,\n\
                           \"StartingTeamId\": 1\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["game_number"], 3);
        }
    }

    // -- Header-independent parsing ------------------------------------------

    mod header_independent {
        use super::*;

        #[test]
        fn test_try_parse_client_gre_header_with_matching_body_returns_some() {
            let entry = LogEntry {
                header: EntryHeader::ClientGre,
                body: "[Client GRE]LogBusinessEvents {\"WinningType\": \"WinLoss\"}".to_owned(),
            };
            // The parser is text-based, matching on body content regardless
            // of header type. A Client GRE entry whose body contains the
            // business events marker and WinningType should still parse.
            let result = try_parse(&entry, test_timestamp());
            assert!(result.is_some());
        }
    }

    // -- Params-wrapped format -----------------------------------------------

    mod params_wrapped {
        use super::*;

        #[test]
        fn test_try_parse_game_result_with_params_wrapper() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"Params\": {\n\
                             \"WinningType\": \"WinLoss\",\n\
                             \"WinningTeamId\": 1,\n\
                             \"WinningReason\": \"ResultReason_Game\",\n\
                             \"GameNumber\": 1,\n\
                             \"StartingTeamId\": 2\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["winning_type"], "WinLoss");
            assert_eq!(payload["winning_team_id"], 1);
            assert_eq!(payload["game_number"], 1);
        }

        #[test]
        fn test_try_parse_game_result_params_with_event_id() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"EventId\": \"Ladder\",\n\
                           \"Params\": {\n\
                             \"WinningType\": \"WinLoss\",\n\
                             \"WinningTeamId\": 2,\n\
                             \"WinningReason\": \"ResultReason_Concede\",\n\
                             \"GameNumber\": 1,\n\
                             \"StartingTeamId\": 1\n\
                           }\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["event_id"], "Ladder");
            assert_eq!(payload["winning_team_id"], 2);
        }
    }

    // -- Array format --------------------------------------------------------

    mod array_format {
        use super::*;

        #[test]
        fn test_try_parse_game_result_in_array() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         [\n\
                           {\n\
                             \"WinningType\": \"WinLoss\",\n\
                             \"WinningTeamId\": 1,\n\
                             \"WinningReason\": \"ResultReason_Game\",\n\
                             \"GameNumber\": 1,\n\
                             \"StartingTeamId\": 2\n\
                           }\n\
                         ]";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["winning_type"], "WinLoss");
            assert_eq!(payload["winning_team_id"], 1);
        }

        #[test]
        fn test_try_parse_game_result_array_with_mixed_events() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         [\n\
                           {\"SomeOtherField\": \"value\"},\n\
                           {\n\
                             \"WinningType\": \"WinLoss\",\n\
                             \"WinningTeamId\": 2,\n\
                             \"WinningReason\": \"ResultReason_Game\",\n\
                             \"GameNumber\": 1,\n\
                             \"StartingTeamId\": 1\n\
                           }\n\
                         ]";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["winning_team_id"], 2);
        }
    }

    // -- Metadata preservation -----------------------------------------------

    mod metadata {
        use super::*;

        #[test]
        fn test_try_parse_game_result_preserves_raw_bytes() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\"WinningType\": \"WinLoss\", \"WinningTeamId\": 1, \
                          \"WinningReason\": \"\", \"GameNumber\": 1, \
                          \"StartingTeamId\": 1}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }

        #[test]
        fn test_try_parse_game_result_stores_timestamp() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\"WinningType\": \"WinLoss\", \"WinningTeamId\": 1, \
                          \"WinningReason\": \"\", \"GameNumber\": 1, \
                          \"StartingTeamId\": 1}";
            let entry = unity_entry(body);
            let ts = test_timestamp();
            let result = try_parse(&entry, ts);

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.metadata().timestamp(), ts);
        }

        #[test]
        fn test_try_parse_game_result_preserves_raw_business_event() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"WinningType\": \"WinLoss\",\n\
                           \"WinningTeamId\": 1,\n\
                           \"WinningReason\": \"ResultReason_Game\",\n\
                           \"GameNumber\": 1,\n\
                           \"StartingTeamId\": 2,\n\
                           \"ExtraField\": \"preserved\"\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["raw_business_event"]["ExtraField"], "preserved");
        }
    }

    // -- Non-matching entries (should return None) ----------------------------

    mod non_matching {
        use super::*;

        #[test]
        fn test_try_parse_unrelated_entry_returns_none() {
            let body = "[UnityCrossThreadLogger]greToClientEvent\n{\"data\": 1}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_empty_body_returns_none() {
            let body = "[UnityCrossThreadLogger]";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_business_event_without_winning_type_returns_none() {
            // Draft pick business event -- should not match game result parser.
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"PickGrpId\": 12345,\n\
                           \"PackCards\": [100, 200, 300]\n\
                         }";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_business_event_marker_only_returns_none() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_malformed_json_returns_none() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\"WinningType\": broken json!!!}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }

        #[test]
        fn test_try_parse_winning_type_in_text_but_not_json_returns_none() {
            // The text mentions WinningType but the JSON does not contain it.
            let body = "[UnityCrossThreadLogger]LogBusinessEvents WinningType note\n\
                         {\"SomeOtherField\": \"value\"}";
            let entry = unity_entry(body);
            assert!(try_parse(&entry, test_timestamp()).is_none());
        }
    }

    // -- Performance class ---------------------------------------------------

    mod performance_class {
        use super::*;

        #[test]
        fn test_game_result_event_is_post_game_batch() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\"WinningType\": \"WinLoss\", \"WinningTeamId\": 1, \
                          \"WinningReason\": \"\", \"GameNumber\": 1, \
                          \"StartingTeamId\": 1}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            assert_eq!(event.performance_class(), PerformanceClass::PostGameBatch);
        }
    }

    // -- Missing / default fields --------------------------------------------

    mod missing_fields {
        use super::*;

        #[test]
        fn test_try_parse_game_result_minimal_fields() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\"WinningType\": \"WinLoss\"}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["winning_type"], "WinLoss");
            assert_eq!(payload["winning_team_id"], 0);
            assert_eq!(payload["winning_reason"], "");
            assert_eq!(payload["game_number"], 0);
            assert_eq!(payload["starting_team_id"], 0);
            assert_eq!(payload["match_id"], "");
            assert_eq!(payload["event_id"], "");
        }

        #[test]
        fn test_try_parse_game_result_partial_fields() {
            let body = "[UnityCrossThreadLogger]LogBusinessEvents\n\
                         {\n\
                           \"WinningType\": \"WinLoss\",\n\
                           \"WinningTeamId\": 1,\n\
                           \"GameNumber\": 3\n\
                         }";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["winning_type"], "WinLoss");
            assert_eq!(payload["winning_team_id"], 1);
            assert_eq!(payload["winning_reason"], "");
            assert_eq!(payload["game_number"], 3);
        }
    }

    // -- Timestamp in header -------------------------------------------------

    mod timestamp_in_header {
        use super::*;

        #[test]
        fn test_try_parse_game_result_with_timestamp_prefix() {
            let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM \
                         LogBusinessEvents\n\
                         {\"WinningType\": \"WinLoss\", \"WinningTeamId\": 1, \
                          \"WinningReason\": \"ResultReason_Game\", \
                          \"GameNumber\": 1, \"StartingTeamId\": 2}";
            let entry = unity_entry(body);
            let result = try_parse(&entry, test_timestamp());

            assert!(result.is_some());
            let event = result.as_ref().unwrap_or_else(|| unreachable!());
            let payload = game_result_payload(event);

            assert_eq!(payload["winning_type"], "WinLoss");
        }
    }

    // -- Internal helpers ----------------------------------------------------

    mod helpers {
        use super::*;

        #[test]
        fn test_extract_json_from_body_object() {
            let body = "header line\n{\"key\": \"value\"}";
            let json = extract_json_from_body(body);
            assert_eq!(json, Some("{\"key\": \"value\"}"));
        }

        #[test]
        fn test_extract_json_from_body_array() {
            let body = "header line\n[{\"key\": \"value\"}]";
            let json = extract_json_from_body(body);
            assert_eq!(json, Some("[{\"key\": \"value\"}]"));
        }

        #[test]
        fn test_extract_json_from_body_nested() {
            let body = "header\n{\"outer\": {\"inner\": 1}}";
            let json = extract_json_from_body(body);
            assert_eq!(json, Some("{\"outer\": {\"inner\": 1}}"));
        }

        #[test]
        fn test_extract_json_from_body_no_json() {
            let body = "no json here at all";
            assert!(extract_json_from_body(body).is_none());
        }

        #[test]
        fn test_extract_json_from_body_with_string_braces() {
            let body = "header\n{\"msg\": \"hello {world}\"}";
            let json = extract_json_from_body(body);
            assert_eq!(json, Some("{\"msg\": \"hello {world}\"}"));
        }

        #[test]
        fn test_has_winning_type_top_level() {
            let val = serde_json::json!({"WinningType": "WinLoss"});
            assert!(has_winning_type(&val));
        }

        #[test]
        fn test_has_winning_type_in_params() {
            let val = serde_json::json!({"Params": {"WinningType": "Draw"}});
            assert!(has_winning_type(&val));
        }

        #[test]
        fn test_has_winning_type_in_array() {
            let val = serde_json::json!([
                {"SomeField": 1},
                {"WinningType": "WinLoss"}
            ]);
            assert!(has_winning_type(&val));
        }

        #[test]
        fn test_has_winning_type_absent() {
            let val = serde_json::json!({"PickGrpId": 12345});
            assert!(!has_winning_type(&val));
        }

        #[test]
        fn test_find_winning_entry_in_array_present() {
            let val = serde_json::json!([
                {"PickGrpId": 100},
                {"WinningType": "WinLoss", "WinningTeamId": 1}
            ]);
            let entry = find_winning_entry_in_array(&val);
            assert!(entry.is_some());
            let entry = entry.unwrap_or_else(|| unreachable!());
            assert_eq!(entry["WinningType"], "WinLoss");
        }

        #[test]
        fn test_find_winning_entry_in_array_absent() {
            let val = serde_json::json!([
                {"PickGrpId": 100}
            ]);
            assert!(find_winning_entry_in_array(&val).is_none());
        }

        #[test]
        fn test_find_winning_entry_in_non_array() {
            let val = serde_json::json!({"WinningType": "WinLoss"});
            assert!(find_winning_entry_in_array(&val).is_none());
        }
    }
}
