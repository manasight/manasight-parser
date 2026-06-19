//! wasm-bindgen parity smoke test.
//!
//! This file is compiled **only** when targeting `wasm32` — it is a no-op on
//! the host (macOS/Linux/Windows) so `make precommit` is unaffected.
//!
//! Run via:
//! ```bash
//! wasm-pack test --node -- --no-default-features --features wasm
//! ```
//!
//! The test asserts that the wasm-bindgen wrapper [`parse_whole_log_js`]
//! produces a JS object graph that round-trips back through
//! `serde_wasm_bindgen::from_value` to the same `Vec<GameEvent>` that the
//! native [`parse_whole_log`] returns on the same input — proving wasm output
//! == native output on identical input (AC-ING-3 parity, no name
//! special-casing required here; redaction is upstream).

#![cfg(all(target_arch = "wasm32", feature = "wasm"))]

use js_sys::Reflect;
use manasight_parser::{parse_whole_log, wasm::parse_whole_log_js, GameEvent};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::wasm_bindgen_test;

// Use the gsm_with_turn_info fixture (inline to avoid I/O in wasm context).
const FIXTURE: &str = include_str!("fixtures/gsm_with_turn_info.txt");

/// Parses the corpus fixture through both code paths and asserts they agree.
#[wasm_bindgen_test]
fn test_parse_whole_log_js_parity_with_native() {
    let native_events: Vec<GameEvent> = parse_whole_log(FIXTURE);

    let js_value = parse_whole_log_js(FIXTURE).expect("wasm wrapper must not fail");

    let wasm_events: Vec<GameEvent> =
        serde_wasm_bindgen::from_value(js_value).expect("round-trip deserialisation must succeed");

    assert_eq!(
        native_events.len(),
        wasm_events.len(),
        "event count must match between native and wasm paths"
    );
    assert_eq!(
        native_events, wasm_events,
        "every event must be identical between native and wasm paths"
    );
}

/// Sanity-check: empty input produces zero events on both paths.
#[wasm_bindgen_test]
fn test_parse_whole_log_js_empty_input() {
    let native_events: Vec<GameEvent> = parse_whole_log("");
    let js_value = parse_whole_log_js("").expect("empty input must not fail");
    let wasm_events: Vec<GameEvent> =
        serde_wasm_bindgen::from_value(js_value).expect("empty round-trip must succeed");

    assert_eq!(
        native_events, wasm_events,
        "empty input must produce identical (empty) results"
    );
}

/// Regression test: event payloads must be plain JS objects, not `Map` instances.
///
/// Constructs a `GameState` event with a known `greToClientEvent` payload
/// (a `serde_json::Value::Object`) and serialises it through the same
/// `serde_wasm_bindgen::Serializer` configuration used by `parse_whole_log_js`.
///
/// With the default serializer, dynamic `serde_json::Value::Object` fields
/// become JS `Map` instances; with `serialize_maps_as_objects(true)` they
/// become plain JS objects with enumerable string keys, matching the native
/// `serde_json` output shape.
///
/// The existing parity test round-trips via `serde_wasm_bindgen::from_value`,
/// which deserialises a `Map` just as happily as a plain object, so it cannot
/// catch this regression.  This test inspects the actual JS structure.
#[wasm_bindgen_test]
fn test_parse_whole_log_js_payload_is_plain_object_not_map() {
    use manasight_parser::{EventMetadata, GameEvent, GameStateEvent};
    use serde::Serialize as _;

    // Construct a minimal GameState event with a known greToClientEvent payload.
    let payload = serde_json::json!({
        "greToClientEvent": {
            "greToClientMessages": [{"type": "GREMessageType_GameStateMessage"}]
        }
    });
    let metadata = EventMetadata::new(None, b"[UnityCrossThreadLogger]greToClientEvent".to_vec());
    let event = GameEvent::GameState(GameStateEvent::new(metadata, payload));
    let events = vec![event];

    // Serialise using the same Serializer configuration as parse_whole_log_js.
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    let js_value = events
        .serialize(&serializer)
        .expect("events.serialize must not fail");

    // The result is a JS array; get the first element via Reflect with key "0".
    let first = Reflect::get(&js_value, &JsValue::from_str("0"))
        .expect("Reflect::get(\"0\") must not throw");
    assert!(
        !first.is_undefined(),
        "serialised array must have an element at index 0"
    );

    // GameEvent is externally-tagged: `{ GameState: { metadata, payload } }`.
    let game_state_inner = Reflect::get(&first, &JsValue::from_str("GameState"))
        .expect("Reflect::get(GameState) must not throw");
    assert!(
        !game_state_inner.is_undefined(),
        "event must be a GameState variant"
    );

    // The `payload` field holds the greToClientEvent JSON as a serde_json::Value.
    // Before the fix it serialised as a JS Map; after the fix it is a plain object.
    let payload_js = Reflect::get(&game_state_inner, &JsValue::from_str("payload"))
        .expect("Reflect::get(payload) must not throw");
    assert!(
        !payload_js.is_undefined(),
        "GameState event must have a payload field"
    );

    // Assert the payload is NOT a JS Map instance.
    assert!(
        !payload_js.is_instance_of::<js_sys::Map>(),
        "payload must be a plain object, not a JS Map (serialize_maps_as_objects regression)"
    );

    // Assert the top-level key is reachable by property access (not .get()).
    let gre_field = Reflect::get(&payload_js, &JsValue::from_str("greToClientEvent"))
        .expect("Reflect::get(greToClientEvent) must not throw");
    assert!(
        !gre_field.is_undefined(),
        "payload.greToClientEvent must be reachable by property access on a plain object"
    );
}
