//! Client action parsers for `ClientToGREMessage` and `ClientToGREUIMessage`
//! payloads.
//!
//! Handles `MulliganResp`, `SelectNResp`, and `SubmitDeckResp` — player
//! decisions sent from the client to the game server (GRE). Also claims
//! `ClientToGREUIMessage` entries (hover, chat) as low-value noise.
//!
//! # Message types handled
//!
//! | Client Message Type | Payload Key | Extracted Fields |
//! |---------------------|-------------|------------------|
//! | `ClientMessageType_MulliganResp` | `mulliganResp` | Mulligan decision (keep/mulligan) |
//! | `ClientMessageType_SelectNResp` | `selectNResp` | Selected card instance IDs |
//! | `ClientMessageType_SubmitDeckResp` | `submitDeckResp` | Deck + sideboard card IDs |
//! | `ClientToGREUIMessage` (any) | — | Claimed as noise (hover, chat) |
//!
//! All three decision types are Class 1 (Interactive Dispatch). They share a
//! common client-to-GRE envelope structure:
//!
//! ```json
//! {
//!   "clientToMatchServiceMessageType": "ClientToMatchServiceMessageType_ClientToGREMessage",
//!   "payload": {
//!     "type": "ClientMessageType_...",
//!     "gameStateId": 5,
//!     "respId": 1,
//!     ...
//!   },
//!   "requestId": 12345,
//!   "timestamp": "637..."
//! }
//! ```

use crate::events::{ClientActionEvent, EventMetadata, GameEvent};
use crate::log::entry::LogEntry;
use crate::parsers::api_common;

/// Marker that identifies a client-to-GRE message entry in the log.
///
/// The full string is `ClientToMatchServiceMessageType_ClientToGREMessage`,
/// but the shorter `ClientToGREMessage` suffix is used for matching since
/// some log formats may abbreviate the prefix.
const CLIENT_TO_GRE_MARKER: &str = "ClientToGREMessage";

/// Marker that identifies a client-to-GRE **UI** message entry in the log.
///
/// These are hover notifications (`onHover`), chat messages (`onChat`), etc.
/// `ClientToGREUIMessage` does NOT contain `ClientToGREMessage` as a
/// substring (the `UI` infix breaks the match), so a separate marker is
/// needed.
const CLIENT_TO_GRE_UI_MARKER: &str = "ClientToGREUIMessage";

/// Client message type for mulligan decisions.
const MULLIGAN_RESP_TYPE: &str = "ClientMessageType_MulliganResp";

/// Client message type for card selection responses.
const SELECT_N_RESP_TYPE: &str = "ClientMessageType_SelectNResp";

/// Client message type for deck submission (sideboarding in Bo3).
const SUBMIT_DECK_RESP_TYPE: &str = "ClientMessageType_SubmitDeckResp";

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Attempts to parse a [`LogEntry`] as a client action event.
///
/// Dispatches to the appropriate sub-parser based on the client message type:
/// - `ClientMessageType_MulliganResp` -> mulligan decision payload
/// - `ClientMessageType_SelectNResp` -> card selection payload
/// - `ClientMessageType_SubmitDeckResp` -> deck submission payload
///
/// Returns `Some(GameEvent::ClientAction(_))` if the entry contains a
/// recognized client-to-GRE message, or `None` if the entry does not match.
///
/// The `timestamp` is `None` when the log entry header did not contain a
/// parseable timestamp. It is passed through to [`EventMetadata`] so
/// downstream consumers can distinguish real vs missing timestamps.
pub fn try_parse(
    entry: &LogEntry,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<GameEvent> {
    let body = &entry.body;

    let is_ui_message = body.contains(CLIENT_TO_GRE_UI_MARKER);

    // Quick check: bail early if neither client-to-GRE marker is present.
    // Note: `ClientToGREUIMessage` does NOT contain `ClientToGREMessage` as
    // a substring, so both markers must be checked independently.
    if !is_ui_message && !body.contains(CLIENT_TO_GRE_MARKER) {
        return None;
    }

    // Extract and parse the JSON payload from the body.
    let context = if is_ui_message {
        "ClientToGREUIMessage"
    } else {
        "ClientToGREMessage"
    };
    let parsed = api_common::parse_json_from_body(body, context)?;

    // UI messages are low-value noise (hover, chat) — claim with a minimal
    // payload. We still parse the JSON above so malformed entries are logged
    // rather than silently swallowed.
    if is_ui_message {
        ::log::trace!("ClientToGREUIMessage: claimed as noise");
        let payload = serde_json::json!({
            "type": "client_ui_message",
            "raw_client_action": parsed,
        });
        let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
        return Some(GameEvent::ClientAction(ClientActionEvent::new(
            metadata, payload,
        )));
    }

    // The inner payload carries the message type and response data.
    let inner_payload = extract_inner_payload(&parsed)?;

    let msg_type = inner_payload
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let payload = match msg_type {
        MULLIGAN_RESP_TYPE => build_mulligan_resp_payload(&inner_payload, &parsed),
        SELECT_N_RESP_TYPE => build_select_n_resp_payload(&inner_payload, &parsed),
        SUBMIT_DECK_RESP_TYPE => build_submit_deck_resp_payload(&inner_payload, &parsed),
        _ => {
            // Unrecognized client message type — still emit as a generic
            // client action so no data is silently lost.
            ::log::debug!("ClientToGREMessage: unrecognized message type: {msg_type}");
            build_generic_client_action_payload(&inner_payload, &parsed)
        }
    };

    let metadata = EventMetadata::new(timestamp, body.as_bytes().to_vec());
    Some(GameEvent::ClientAction(ClientActionEvent::new(
        metadata, payload,
    )))
}

// ---------------------------------------------------------------------------
// Envelope extraction
// ---------------------------------------------------------------------------

/// Extracts the inner `payload` object from the client-to-GRE envelope.
///
/// The envelope structure has either:
/// - `{ "payload": { "type": "ClientMessageType_...", ... } }` (string-encoded
///   payloads are pre-parsed by the log writer)
/// - `{ "payload": "<json-string>" }` (the payload is a JSON-encoded string
///   that needs a second parse)
///
/// Returns `None` if no payload can be extracted.
fn extract_inner_payload(parsed: &serde_json::Value) -> Option<serde_json::Value> {
    let raw_payload = parsed.get("payload")?;

    // Case 1: payload is already a JSON object.
    if raw_payload.is_object() {
        return Some(raw_payload.clone());
    }

    // Case 2: payload is a JSON-encoded string (double-serialized).
    if let Some(payload_str) = raw_payload.as_str() {
        match serde_json::from_str(payload_str) {
            Ok(v) => return Some(v),
            Err(e) => {
                ::log::warn!("ClientToGREMessage: failed to parse string payload: {e}");
                return None;
            }
        }
    }

    ::log::debug!("ClientToGREMessage: payload is neither object nor string");
    None
}

// ---------------------------------------------------------------------------
// Mulligan response payload builder
// ---------------------------------------------------------------------------

/// Builds a structured payload from a `MulliganResp` message.
///
/// The `mulliganResp` sub-object carries the player's mulligan decision:
/// - `"MulliganOption_Mulligan"` — player sends back hand
/// - `"MulliganOption_AcceptHand"` — player keeps hand
///
/// The output payload normalizes this into:
///
/// ```json
/// {
///   "type": "mulligan_resp",
///   "decision": "mulligan" | "keep",
///   "game_state_id": 5,
///   "resp_id": 1,
///   "request_id": 12345,
///   "raw_client_action": { ... }
/// }
/// ```
fn build_mulligan_resp_payload(
    inner: &serde_json::Value,
    envelope: &serde_json::Value,
) -> serde_json::Value {
    let mulligan_resp = inner.get("mulliganResp");

    // Normalize the decision enum to a human-readable string.
    let raw_decision = mulligan_resp
        .and_then(|mr| mr.get("decision"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let decision = normalize_mulligan_decision(raw_decision);

    let game_state_id = extract_game_state_id(inner);
    let resp_id = extract_resp_id(inner);
    let request_id = extract_request_id(envelope);

    serde_json::json!({
        "type": "mulligan_resp",
        "decision": decision,
        "game_state_id": game_state_id,
        "resp_id": resp_id,
        "request_id": request_id,
        "raw_client_action": envelope,
    })
}

/// Normalizes the MTGA mulligan decision enum to a short string.
fn normalize_mulligan_decision(raw: &str) -> &str {
    match raw {
        "MulliganOption_Mulligan" => "mulligan",
        "MulliganOption_AcceptHand" => "keep",
        _ => raw,
    }
}

// ---------------------------------------------------------------------------
// SelectNResp payload builder
// ---------------------------------------------------------------------------

/// Builds a structured payload from a `SelectNResp` message.
///
/// The `selectNResp` sub-object carries the player's card selection:
/// - `selectedOptionIds` — array of selected option IDs
/// - `selectedObjectIds` — array of selected game object instance IDs
///
/// This is used for responses to prompts like "choose a card to discard",
/// "select targets", etc.
///
/// The output payload has the shape:
///
/// ```json
/// {
///   "type": "select_n_resp",
///   "selected_option_ids": [1, 2],
///   "selected_object_ids": [101, 102],
///   "game_state_id": 5,
///   "resp_id": 1,
///   "request_id": 12345,
///   "raw_client_action": { ... }
/// }
/// ```
fn build_select_n_resp_payload(
    inner: &serde_json::Value,
    envelope: &serde_json::Value,
) -> serde_json::Value {
    let select_resp = inner.get("selectNResp");

    let selected_option_ids =
        extract_i64_array(select_resp.and_then(|sr| sr.get("selectedOptionIds")));

    let selected_object_ids =
        extract_i64_array(select_resp.and_then(|sr| sr.get("selectedObjectIds")));

    let game_state_id = extract_game_state_id(inner);
    let resp_id = extract_resp_id(inner);
    let request_id = extract_request_id(envelope);

    serde_json::json!({
        "type": "select_n_resp",
        "selected_option_ids": selected_option_ids,
        "selected_object_ids": selected_object_ids,
        "game_state_id": game_state_id,
        "resp_id": resp_id,
        "request_id": request_id,
        "raw_client_action": envelope,
    })
}

// ---------------------------------------------------------------------------
// SubmitDeckResp payload builder
// ---------------------------------------------------------------------------

/// Builds a structured payload from a `SubmitDeckResp` message.
///
/// The `submitDeckResp` sub-object carries the player's deck submission,
/// typically used for sideboarding between games in a Bo3 match:
/// - `deck.deckCards` — array of card GRP IDs in the main deck
/// - `deck.sideboardCards` — array of card GRP IDs in the sideboard
///
/// The output payload has the shape:
///
/// ```json
/// {
///   "type": "submit_deck_resp",
///   "deck_cards": [68398, 68398, 68398, ...],
///   "sideboard_cards": [70123, ...],
///   "game_state_id": 5,
///   "resp_id": 1,
///   "request_id": 12345,
///   "raw_client_action": { ... }
/// }
/// ```
fn build_submit_deck_resp_payload(
    inner: &serde_json::Value,
    envelope: &serde_json::Value,
) -> serde_json::Value {
    let submit_resp = inner.get("submitDeckResp");
    let deck = submit_resp.and_then(|sr| sr.get("deck"));

    let deck_cards = extract_i64_array(deck.and_then(|d| d.get("deckCards")));
    let sideboard_cards = extract_i64_array(deck.and_then(|d| d.get("sideboardCards")));

    let game_state_id = extract_game_state_id(inner);
    let resp_id = extract_resp_id(inner);
    let request_id = extract_request_id(envelope);

    serde_json::json!({
        "type": "submit_deck_resp",
        "deck_cards": deck_cards,
        "sideboard_cards": sideboard_cards,
        "game_state_id": game_state_id,
        "resp_id": resp_id,
        "request_id": request_id,
        "raw_client_action": envelope,
    })
}

// ---------------------------------------------------------------------------
// Generic client action payload builder
// ---------------------------------------------------------------------------

/// Builds a generic payload for unrecognized client message types.
///
/// Preserves all data so nothing is silently lost — consumers can inspect
/// the `raw_client_action` field for the full envelope.
fn build_generic_client_action_payload(
    inner: &serde_json::Value,
    envelope: &serde_json::Value,
) -> serde_json::Value {
    let msg_type = inner
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");

    let game_state_id = extract_game_state_id(inner);
    let resp_id = extract_resp_id(inner);
    let request_id = extract_request_id(envelope);

    serde_json::json!({
        "type": "client_action",
        "client_message_type": msg_type,
        "game_state_id": game_state_id,
        "resp_id": resp_id,
        "request_id": request_id,
        "raw_client_action": envelope,
    })
}

// ---------------------------------------------------------------------------
// Shared extraction helpers
// ---------------------------------------------------------------------------

/// Extracts `gameStateId` from the inner payload.
fn extract_game_state_id(inner: &serde_json::Value) -> i64 {
    inner
        .get("gameStateId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
}

/// Extracts `respId` from the inner payload.
fn extract_resp_id(inner: &serde_json::Value) -> i64 {
    inner
        .get("respId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
}

/// Extracts `requestId` from the outer envelope.
fn extract_request_id(envelope: &serde_json::Value) -> i64 {
    envelope
        .get("requestId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
}

/// Extracts an array of `i64` values from a JSON value.
///
/// Returns an empty `Vec` if the value is not an array.
fn extract_i64_array(value: Option<&serde_json::Value>) -> Vec<i64> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|arr| arr.iter().filter_map(serde_json::Value::as_i64).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::test_helpers::{test_timestamp, unity_entry};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Helper: build a full client-to-GRE log body from an inner payload JSON.
    fn wrap_client_to_gre(inner_payload: &serde_json::Value) -> String {
        let envelope = serde_json::json!({
            "clientToMatchServiceMessageType": "ClientToMatchServiceMessageType_ClientToGREMessage",
            "payload": inner_payload,
            "requestId": 42,
            "timestamp": "638456789012345678"
        });
        format!(
            "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{}",
            serde_json::to_string_pretty(&envelope).unwrap_or_default()
        )
    }

    // -----------------------------------------------------------------------
    // try_parse: non-matching entries
    // -----------------------------------------------------------------------

    #[test]
    fn test_try_parse_non_matching_entry_returns_none() {
        let entry = unity_entry("[UnityCrossThreadLogger] some other log line");
        assert!(try_parse(&entry, Some(test_timestamp())).is_none());
    }

    #[test]
    fn test_try_parse_empty_body_returns_none() {
        let entry = unity_entry("");
        assert!(try_parse(&entry, Some(test_timestamp())).is_none());
    }

    #[test]
    fn test_try_parse_gre_to_client_event_returns_none() {
        // GRE-to-client events should NOT match the client action parser.
        let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
            {\"greToClientEvent\":{\"greToClientMessages\":[]}}";
        let entry = unity_entry(body);
        assert!(try_parse(&entry, Some(test_timestamp())).is_none());
    }

    #[test]
    fn test_try_parse_marker_present_but_malformed_json_returns_none() {
        let body = "[UnityCrossThreadLogger] ClientToGREMessage\n\
            {invalid json here";
        let entry = unity_entry(body);
        assert!(try_parse(&entry, Some(test_timestamp())).is_none());
    }

    #[test]
    fn test_try_parse_marker_present_but_no_payload_returns_none() {
        let body = format!(
            "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{}",
            serde_json::json!({
                "clientToMatchServiceMessageType": "ClientToMatchServiceMessageType_ClientToGREMessage",
                "requestId": 1
            })
        );
        let entry = unity_entry(&body);
        assert!(try_parse(&entry, Some(test_timestamp())).is_none());
    }

    // -----------------------------------------------------------------------
    // try_parse: MulliganResp
    // -----------------------------------------------------------------------

    #[test]
    fn test_try_parse_mulligan_keep_returns_client_action() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_MulliganResp",
            "mulliganResp": {
                "decision": "MulliganOption_AcceptHand"
            },
            "gameStateId": 5,
            "respId": 1
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "mulligan_resp");
            assert_eq!(payload["decision"], "keep");
            assert_eq!(payload["game_state_id"], 5);
            assert_eq!(payload["resp_id"], 1);
            assert_eq!(payload["request_id"], 42);
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_try_parse_mulligan_send_back_returns_client_action() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_MulliganResp",
            "mulliganResp": {
                "decision": "MulliganOption_Mulligan"
            },
            "gameStateId": 3,
            "respId": 2
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "mulligan_resp");
            assert_eq!(payload["decision"], "mulligan");
            assert_eq!(payload["game_state_id"], 3);
            assert_eq!(payload["resp_id"], 2);
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_try_parse_mulligan_missing_decision_defaults() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_MulliganResp",
            "mulliganResp": {},
            "gameStateId": 1,
            "respId": 1
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "mulligan_resp");
            // Empty decision when missing.
            assert_eq!(payload["decision"], "");
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_try_parse_mulligan_no_mulligan_resp_object() -> TestResult {
        // The mulliganResp sub-object is missing entirely.
        let inner = serde_json::json!({
            "type": "ClientMessageType_MulliganResp",
            "gameStateId": 1,
            "respId": 1
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "mulligan_resp");
            assert_eq!(payload["decision"], "");
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // try_parse: SelectNResp
    // -----------------------------------------------------------------------

    #[test]
    fn test_try_parse_select_n_resp_with_options() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_SelectNResp",
            "selectNResp": {
                "selectedOptionIds": [1, 3],
                "selectedObjectIds": [101, 102, 103]
            },
            "gameStateId": 10,
            "respId": 5
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "select_n_resp");
            assert_eq!(payload["selected_option_ids"], serde_json::json!([1, 3]));
            assert_eq!(
                payload["selected_object_ids"],
                serde_json::json!([101, 102, 103])
            );
            assert_eq!(payload["game_state_id"], 10);
            assert_eq!(payload["resp_id"], 5);
            assert_eq!(payload["request_id"], 42);
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_try_parse_select_n_resp_empty_selections() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_SelectNResp",
            "selectNResp": {
                "selectedOptionIds": [],
                "selectedObjectIds": []
            },
            "gameStateId": 7,
            "respId": 3
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "select_n_resp");
            assert_eq!(payload["selected_option_ids"], serde_json::json!([]));
            assert_eq!(payload["selected_object_ids"], serde_json::json!([]));
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_try_parse_select_n_resp_missing_select_object() -> TestResult {
        // selectNResp sub-object is missing — should default to empty arrays.
        let inner = serde_json::json!({
            "type": "ClientMessageType_SelectNResp",
            "gameStateId": 4,
            "respId": 2
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "select_n_resp");
            assert_eq!(payload["selected_option_ids"], serde_json::json!([]));
            assert_eq!(payload["selected_object_ids"], serde_json::json!([]));
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_try_parse_select_n_resp_only_option_ids() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_SelectNResp",
            "selectNResp": {
                "selectedOptionIds": [5, 6, 7]
            },
            "gameStateId": 8,
            "respId": 4
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "select_n_resp");
            assert_eq!(payload["selected_option_ids"], serde_json::json!([5, 6, 7]));
            assert_eq!(payload["selected_object_ids"], serde_json::json!([]));
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // try_parse: SubmitDeckResp
    // -----------------------------------------------------------------------

    #[test]
    fn test_try_parse_submit_deck_resp_with_deck_and_sideboard() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_SubmitDeckResp",
            "submitDeckResp": {
                "deck": {
                    "deckCards": [68398, 68398, 68398, 68398, 70123, 70123],
                    "sideboardCards": [71000, 71001, 71002]
                }
            },
            "gameStateId": 2,
            "respId": 1
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "submit_deck_resp");
            assert_eq!(
                payload["deck_cards"],
                serde_json::json!([68398, 68398, 68398, 68398, 70123, 70123])
            );
            assert_eq!(
                payload["sideboard_cards"],
                serde_json::json!([71000, 71001, 71002])
            );
            assert_eq!(payload["game_state_id"], 2);
            assert_eq!(payload["resp_id"], 1);
            assert_eq!(payload["request_id"], 42);
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_try_parse_submit_deck_resp_empty_sideboard() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_SubmitDeckResp",
            "submitDeckResp": {
                "deck": {
                    "deckCards": [68398, 70123],
                    "sideboardCards": []
                }
            },
            "gameStateId": 2,
            "respId": 1
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "submit_deck_resp");
            assert_eq!(payload["deck_cards"], serde_json::json!([68398, 70123]));
            assert_eq!(payload["sideboard_cards"], serde_json::json!([]));
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_try_parse_submit_deck_resp_missing_deck_object() -> TestResult {
        // submitDeckResp is missing — should default to empty arrays.
        let inner = serde_json::json!({
            "type": "ClientMessageType_SubmitDeckResp",
            "gameStateId": 2,
            "respId": 1
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "submit_deck_resp");
            assert_eq!(payload["deck_cards"], serde_json::json!([]));
            assert_eq!(payload["sideboard_cards"], serde_json::json!([]));
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_try_parse_submit_deck_resp_missing_sideboard_key() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_SubmitDeckResp",
            "submitDeckResp": {
                "deck": {
                    "deckCards": [68398, 70123]
                }
            },
            "gameStateId": 2,
            "respId": 1
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "submit_deck_resp");
            assert_eq!(payload["deck_cards"], serde_json::json!([68398, 70123]));
            assert_eq!(payload["sideboard_cards"], serde_json::json!([]));
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // try_parse: unrecognized client message types
    // -----------------------------------------------------------------------

    #[test]
    fn test_try_parse_unrecognized_message_type_returns_generic() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_FutureNewType",
            "someData": {"key": "value"},
            "gameStateId": 15,
            "respId": 7
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "client_action");
            assert_eq!(
                payload["client_message_type"],
                "ClientMessageType_FutureNewType"
            );
            assert_eq!(payload["game_state_id"], 15);
            assert_eq!(payload["resp_id"], 7);
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // try_parse: string-encoded (double-serialized) payload
    // -----------------------------------------------------------------------

    #[test]
    fn test_try_parse_string_encoded_payload() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_MulliganResp",
            "mulliganResp": {
                "decision": "MulliganOption_AcceptHand"
            },
            "gameStateId": 5,
            "respId": 1
        });
        let inner_str = serde_json::to_string(&inner).unwrap_or_default();
        let envelope = serde_json::json!({
            "clientToMatchServiceMessageType": "ClientToMatchServiceMessageType_ClientToGREMessage",
            "payload": inner_str,
            "requestId": 99
        });
        let body = format!(
            "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{}",
            serde_json::to_string_pretty(&envelope).unwrap_or_default()
        );
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "mulligan_resp");
            assert_eq!(payload["decision"], "keep");
            assert_eq!(payload["request_id"], 99);
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // try_parse: metadata validation
    // -----------------------------------------------------------------------

    #[test]
    fn test_try_parse_event_metadata_timestamp() {
        let inner = serde_json::json!({
            "type": "ClientMessageType_MulliganResp",
            "mulliganResp": {"decision": "MulliganOption_AcceptHand"},
            "gameStateId": 1,
            "respId": 1
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let ts = Some(test_timestamp());
        let result = try_parse(&entry, ts);

        assert!(result.is_some());
        if let Some(event) = &result {
            assert_eq!(event.metadata().timestamp(), ts);
        }
    }

    #[test]
    fn test_try_parse_event_metadata_raw_bytes() {
        let inner = serde_json::json!({
            "type": "ClientMessageType_SelectNResp",
            "selectNResp": {"selectedOptionIds": [1]},
            "gameStateId": 1,
            "respId": 1
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(event) = &result {
            assert_eq!(event.metadata().raw_bytes(), body.as_bytes());
        }
    }

    #[test]
    fn test_try_parse_returns_correct_performance_class() {
        use crate::events::PerformanceClass;

        let inner = serde_json::json!({
            "type": "ClientMessageType_MulliganResp",
            "mulliganResp": {"decision": "MulliganOption_AcceptHand"},
            "gameStateId": 1,
            "respId": 1
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(event) = &result {
            // Client actions are Class 1 (Interactive Dispatch).
            assert_eq!(
                event.performance_class(),
                PerformanceClass::InteractiveDispatch
            );
        }
    }

    // -----------------------------------------------------------------------
    // extract_inner_payload
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_inner_payload_object() {
        let parsed = serde_json::json!({
            "payload": {"type": "ClientMessageType_MulliganResp"}
        });
        let result = extract_inner_payload(&parsed);
        assert!(result.is_some());
        if let Some(inner) = result {
            assert_eq!(inner["type"], "ClientMessageType_MulliganResp");
        }
    }

    #[test]
    fn test_extract_inner_payload_string() {
        let inner_json = serde_json::json!({"type": "ClientMessageType_SelectNResp"});
        let parsed = serde_json::json!({
            "payload": serde_json::to_string(&inner_json).unwrap_or_default()
        });
        let result = extract_inner_payload(&parsed);
        assert!(result.is_some());
        if let Some(inner) = result {
            assert_eq!(inner["type"], "ClientMessageType_SelectNResp");
        }
    }

    #[test]
    fn test_extract_inner_payload_missing() {
        let parsed = serde_json::json!({"someOtherField": true});
        assert!(extract_inner_payload(&parsed).is_none());
    }

    #[test]
    fn test_extract_inner_payload_invalid_string() {
        let parsed = serde_json::json!({"payload": "not valid json"});
        assert!(extract_inner_payload(&parsed).is_none());
    }

    #[test]
    fn test_extract_inner_payload_number() {
        let parsed = serde_json::json!({"payload": 42});
        assert!(extract_inner_payload(&parsed).is_none());
    }

    // -----------------------------------------------------------------------
    // normalize_mulligan_decision
    // -----------------------------------------------------------------------

    #[test]
    fn test_normalize_mulligan_decision_accept() {
        assert_eq!(
            normalize_mulligan_decision("MulliganOption_AcceptHand"),
            "keep"
        );
    }

    #[test]
    fn test_normalize_mulligan_decision_mulligan() {
        assert_eq!(
            normalize_mulligan_decision("MulliganOption_Mulligan"),
            "mulligan"
        );
    }

    #[test]
    fn test_normalize_mulligan_decision_unknown() {
        assert_eq!(
            normalize_mulligan_decision("MulliganOption_FutureType"),
            "MulliganOption_FutureType"
        );
    }

    #[test]
    fn test_normalize_mulligan_decision_empty() {
        assert_eq!(normalize_mulligan_decision(""), "");
    }

    // -----------------------------------------------------------------------
    // extract_i64_array
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_i64_array_valid() {
        let value = serde_json::json!([1, 2, 3]);
        assert_eq!(extract_i64_array(Some(&value)), vec![1, 2, 3]);
    }

    #[test]
    fn test_extract_i64_array_empty() {
        let value = serde_json::json!([]);
        assert_eq!(extract_i64_array(Some(&value)), Vec::<i64>::new());
    }

    #[test]
    fn test_extract_i64_array_none() {
        assert_eq!(extract_i64_array(None), Vec::<i64>::new());
    }

    #[test]
    fn test_extract_i64_array_mixed_types() {
        // Non-integer values are filtered out.
        let value = serde_json::json!([1, "not_a_number", 3, null, 5]);
        assert_eq!(extract_i64_array(Some(&value)), vec![1, 3, 5]);
    }

    #[test]
    fn test_extract_i64_array_not_array() {
        let value = serde_json::json!("not an array");
        assert_eq!(extract_i64_array(Some(&value)), Vec::<i64>::new());
    }

    // -----------------------------------------------------------------------
    // Shared field extractors
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_game_state_id_present() {
        let inner = serde_json::json!({"gameStateId": 42});
        assert_eq!(extract_game_state_id(&inner), 42);
    }

    #[test]
    fn test_extract_game_state_id_missing() {
        let inner = serde_json::json!({});
        assert_eq!(extract_game_state_id(&inner), 0);
    }

    #[test]
    fn test_extract_resp_id_present() {
        let inner = serde_json::json!({"respId": 7});
        assert_eq!(extract_resp_id(&inner), 7);
    }

    #[test]
    fn test_extract_resp_id_missing() {
        let inner = serde_json::json!({});
        assert_eq!(extract_resp_id(&inner), 0);
    }

    #[test]
    fn test_extract_request_id_present() {
        let envelope = serde_json::json!({"requestId": 12345});
        assert_eq!(extract_request_id(&envelope), 12345);
    }

    #[test]
    fn test_extract_request_id_missing() {
        let envelope = serde_json::json!({});
        assert_eq!(extract_request_id(&envelope), 0);
    }

    // -----------------------------------------------------------------------
    // Realistic log entry integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_realistic_mulligan_keep_entry() -> TestResult {
        // Simulates a realistic log entry for keeping a hand.
        let body = "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n\
            {\n\
              \"clientToMatchServiceMessageType\": \"ClientToMatchServiceMessageType_ClientToGREMessage\",\n\
              \"payload\": {\n\
                \"type\": \"ClientMessageType_MulliganResp\",\n\
                \"mulliganResp\": {\n\
                  \"decision\": \"MulliganOption_AcceptHand\"\n\
                },\n\
                \"gameStateId\": 8,\n\
                \"respId\": 3\n\
              },\n\
              \"requestId\": 54321,\n\
              \"timestamp\": \"638456789012345678\"\n\
            }";
        let entry = unity_entry(body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "mulligan_resp");
            assert_eq!(payload["decision"], "keep");
            assert_eq!(payload["game_state_id"], 8);
            assert_eq!(payload["resp_id"], 3);
            assert_eq!(payload["request_id"], 54321);
            assert!(payload["raw_client_action"].is_object());
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_realistic_sideboard_submission() -> TestResult {
        // Simulates a Bo3 sideboard deck submission.
        let body = "[UnityCrossThreadLogger]2/25/2026 12:05:00 PM\n\
            {\n\
              \"clientToMatchServiceMessageType\": \"ClientToMatchServiceMessageType_ClientToGREMessage\",\n\
              \"payload\": {\n\
                \"type\": \"ClientMessageType_SubmitDeckResp\",\n\
                \"submitDeckResp\": {\n\
                  \"deck\": {\n\
                    \"deckCards\": [68398, 68398, 68398, 68398, 70123, 70123, 70123, 70123, 71500, 71500, 71500],\n\
                    \"sideboardCards\": [72000, 72001, 72002, 72003, 72004]\n\
                  }\n\
                },\n\
                \"gameStateId\": 1,\n\
                \"respId\": 1\n\
              },\n\
              \"requestId\": 67890,\n\
              \"timestamp\": \"638456789999999999\"\n\
            }";
        let entry = unity_entry(body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "submit_deck_resp");
            // Verify deck card count.
            let deck_cards = payload["deck_cards"].as_array();
            assert!(deck_cards.is_some());
            if let Some(cards) = deck_cards {
                assert_eq!(cards.len(), 11);
            }
            // Verify sideboard card count.
            let sideboard = payload["sideboard_cards"].as_array();
            assert!(sideboard.is_some());
            if let Some(cards) = sideboard {
                assert_eq!(cards.len(), 5);
            }
            assert_eq!(payload["request_id"], 67890);
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_realistic_select_targets() -> TestResult {
        // Simulates a card selection (e.g., choosing targets for a spell).
        let body = "[UnityCrossThreadLogger]2/25/2026 12:03:00 PM\n\
            {\n\
              \"clientToMatchServiceMessageType\": \"ClientToMatchServiceMessageType_ClientToGREMessage\",\n\
              \"payload\": {\n\
                \"type\": \"ClientMessageType_SelectNResp\",\n\
                \"selectNResp\": {\n\
                  \"selectedOptionIds\": [2],\n\
                  \"selectedObjectIds\": [456]\n\
                },\n\
                \"gameStateId\": 20,\n\
                \"respId\": 10\n\
              },\n\
              \"requestId\": 11111,\n\
              \"timestamp\": \"638456789555555555\"\n\
            }";
        let entry = unity_entry(body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            let payload = event.payload();
            assert_eq!(payload["type"], "select_n_resp");
            assert_eq!(payload["selected_option_ids"], serde_json::json!([2]));
            assert_eq!(payload["selected_object_ids"], serde_json::json!([456]));
            assert_eq!(payload["game_state_id"], 20);
            assert_eq!(payload["request_id"], 11111);
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_try_parse_missing_game_state_id_defaults_to_zero() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_MulliganResp",
            "mulliganResp": {"decision": "MulliganOption_AcceptHand"},
            "respId": 1
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            assert_eq!(event.payload()["game_state_id"], 0);
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_try_parse_missing_resp_id_defaults_to_zero() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_MulliganResp",
            "mulliganResp": {"decision": "MulliganOption_AcceptHand"},
            "gameStateId": 1
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            assert_eq!(event.payload()["resp_id"], 0);
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_try_parse_missing_request_id_defaults_to_zero() -> TestResult {
        let inner = serde_json::json!({
            "type": "ClientMessageType_MulliganResp",
            "mulliganResp": {"decision": "MulliganOption_AcceptHand"},
            "gameStateId": 1,
            "respId": 1
        });
        // Build envelope without requestId.
        let envelope = serde_json::json!({
            "clientToMatchServiceMessageType": "ClientToMatchServiceMessageType_ClientToGREMessage",
            "payload": inner
        });
        let body = format!(
            "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{}",
            serde_json::to_string_pretty(&envelope).unwrap_or_default()
        );
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));

        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            assert_eq!(event.payload()["request_id"], 0);
        } else {
            return Err("Expected GameEvent::ClientAction".into());
        }
        Ok(())
    }

    #[test]
    fn test_raw_client_action_preserved_in_all_message_types() {
        for msg_type in [
            MULLIGAN_RESP_TYPE,
            SELECT_N_RESP_TYPE,
            SUBMIT_DECK_RESP_TYPE,
        ] {
            let inner = serde_json::json!({
                "type": msg_type,
                "gameStateId": 1,
                "respId": 1
            });
            let body = wrap_client_to_gre(&inner);
            let entry = unity_entry(&body);
            let result = try_parse(&entry, Some(test_timestamp()));

            assert!(result.is_some(), "Expected Some for {msg_type}");
            if let Some(GameEvent::ClientAction(event)) = &result {
                assert!(
                    event.payload()["raw_client_action"].is_object(),
                    "raw_client_action should be present for {msg_type}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // ClientToGREUIMessage (noise recognizer)
    // -----------------------------------------------------------------------

    /// Helper: build a full client-to-GRE **UI** message log body.
    fn wrap_client_to_gre_ui(inner_payload: &serde_json::Value) -> String {
        let envelope = serde_json::json!({
            "clientToMatchServiceMessageType": "ClientToMatchServiceMessageType_ClientToGREUIMessage",
            "payload": inner_payload,
            "requestId": 99,
            "timestamp": "638456789012345678"
        });
        format!(
            "[UnityCrossThreadLogger]2/25/2026 12:00:00 PM\n{}",
            serde_json::to_string_pretty(&envelope).unwrap_or_default()
        )
    }

    #[test]
    fn test_try_parse_ui_message_on_hover_returns_some() {
        let inner = serde_json::json!({
            "type": "ClientMessageType_UIMessage",
            "uiMessage": {
                "onHover": { "objectId": 12345 }
            }
        });
        let body = wrap_client_to_gre_ui(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));
        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            assert_eq!(event.payload()["type"], "client_ui_message");
        }
    }

    #[test]
    fn test_try_parse_ui_message_on_chat_returns_some() {
        let inner = serde_json::json!({
            "type": "ClientMessageType_UIMessage",
            "uiMessage": {
                "onChat": { "text": "Good game" }
            }
        });
        let body = wrap_client_to_gre_ui(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));
        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            assert_eq!(event.payload()["type"], "client_ui_message");
            assert!(event.payload()["raw_client_action"].is_object());
        }
    }

    #[test]
    fn test_try_parse_ui_message_preserves_metadata() {
        let inner = serde_json::json!({
            "type": "ClientMessageType_UIMessage",
            "uiMessage": { "onHover": {} }
        });
        let body = wrap_client_to_gre_ui(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));
        let event = result.as_ref().unwrap_or_else(|| unreachable!());
        assert!(!event.metadata().raw_bytes().is_empty());
        assert_eq!(event.metadata().timestamp(), Some(test_timestamp()));
    }

    #[test]
    fn test_try_parse_ui_message_malformed_json_returns_none() {
        let body = "[UnityCrossThreadLogger]ClientToGREUIMessage\n{invalid json}";
        let entry = unity_entry(body);
        assert!(try_parse(&entry, Some(test_timestamp())).is_none());
    }

    #[test]
    fn test_try_parse_ui_message_no_json_returns_none() {
        let body = "[UnityCrossThreadLogger]ClientToGREUIMessage with no json";
        let entry = unity_entry(body);
        assert!(try_parse(&entry, Some(test_timestamp())).is_none());
    }

    #[test]
    fn test_try_parse_regular_client_message_still_works() {
        // Verify that adding UI message support didn't break regular parsing.
        let inner = serde_json::json!({
            "type": "ClientMessageType_MulliganResp",
            "gameStateId": 5,
            "respId": 1,
            "mulliganResp": {
                "decision": "MulliganOption_AcceptHand"
            }
        });
        let body = wrap_client_to_gre(&inner);
        let entry = unity_entry(&body);
        let result = try_parse(&entry, Some(test_timestamp()));
        assert!(result.is_some());
        if let Some(GameEvent::ClientAction(event)) = &result {
            assert_eq!(event.payload()["type"], "mulligan_resp");
        }
    }
}
