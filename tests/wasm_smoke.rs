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

/// The `gsm_with_turn_info.txt` fixture — a `greToClientEvent` log snippet that
/// produces a real `GameState` event through `parse_whole_log`.
///
/// Its header starts with a digit immediately after `]`
/// (`[UnityCrossThreadLogger]3/11/2026 …`), so the entry is classified as
/// `MultiLine` and the JSON body on the next line is accumulated. A timestampless
/// header (`[UnityCrossThreadLogger]greToClientEvent`) would classify as
/// `SingleLine`, orphan the JSON body, and produce zero events.
const FIXTURE: &str = include_str!("fixtures/gsm_with_turn_info.txt");

/// Parses the corpus fixture through both code paths and asserts they agree.
#[wasm_bindgen_test]
fn test_parse_whole_log_js_parity_with_native() {
    // Use an inline input that produces real events so this test is non-vacuous.
    let native_events: Vec<GameEvent> = parse_whole_log(FIXTURE);

    // Guard: if parsing ever regresses to 0 events, the parity assertion
    // becomes vacuously true (0 == 0) and stops catching bugs.
    assert!(
        !native_events.is_empty(),
        "parse_whole_log(FIXTURE) must produce >=1 event; got 0 — \
         check that the header starts with a digit after ']'"
    );

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
/// Calls `parse_whole_log_js` end-to-end with a real log snippet that produces
/// a `GameState` event. Inspects the raw JS output to confirm payloads are plain
/// objects (`{}`) rather than `Map` instances.
///
/// With the default `serde_wasm_bindgen::to_value` serializer, dynamic
/// `serde_json::Value::Object` fields become JS `Map` instances. After the fix,
/// `serialize_maps_as_objects(true)` makes them plain JS objects with enumerable
/// string keys. The round-trip deserialization in `test_parse_whole_log_js_parity_with_native`
/// cannot catch this regression because `from_value` deserialises a `Map` just as
/// happily as a plain object. This test inspects the actual JS structure directly.
///
/// If `src/wasm.rs` is reverted to `serde_wasm_bindgen::to_value(&events)`,
/// `payload_js.is_instance_of::<js_sys::Map>()` becomes `true` and this test
/// fails — proving it actually guards the production serializer.
#[wasm_bindgen_test]
fn test_parse_whole_log_js_payload_is_plain_object_not_map() {
    // Guard: confirm parse_whole_log produces >=1 GameState event so the
    // end-to-end JS inspection below is not vacuous.
    let native_events = parse_whole_log(FIXTURE);
    assert!(
        !native_events.is_empty(),
        "parse_whole_log(FIXTURE) must produce >=1 event before the \
         end-to-end JS inspection can be meaningful"
    );
    assert!(
        matches!(native_events[0], GameEvent::GameState(_)),
        "first event must be GameState; got {:?}",
        native_events[0]
    );

    // Drive parse_whole_log_js end-to-end.
    let js_value = parse_whole_log_js(FIXTURE).expect("parse_whole_log_js must not fail");

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

    // The `payload` field holds the extracted GSM data as a serde_json::Value.
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

    // Assert a known field from build_game_state_message_payload is reachable
    // by property access (not .get()). The extracted payload always has
    // `"type": "game_state_message"` — use that as the stable key.
    let type_field = Reflect::get(&payload_js, &JsValue::from_str("type"))
        .expect("Reflect::get(type) must not throw");
    assert!(
        !type_field.is_undefined(),
        "payload.type must be reachable by property access on a plain object"
    );
    assert_eq!(
        type_field,
        JsValue::from_str("game_state_message"),
        "payload.type must equal \"game_state_message\" for a GameStateMessage"
    );
}
