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
//! ## Note on `test_parse_whole_log_js_empty_input`
//!
//! That test uses a typed round-trip (`serde_wasm_bindgen::from_value::<Vec<GameEvent>>`)
//! and must NOT be extended to non-empty input under lean. The custom
//! `EventMetadata` `Deserialize` impl recomputes `raw_bytes_hash` from the
//! (lean-omitted, therefore empty) `raw_bytes`, producing a hash mismatch
//! against the native struct even though both serialised forms are identical.
//! Empty input is safe because there are no events (and thus no metadata) to
//! round-trip.
//!
//! The test asserts that the wasm-bindgen wrapper [`parse_whole_log_js`]
//! produces a JS object graph whose **serialised JSON** matches the
//! `serde_json` serialisation of the same `Vec<GameEvent>` that the native
//! [`parse_whole_log`] returns on the same input — proving wasm output
//! == native output on identical input (AC-ING-3 parity, no name
//! special-casing required here; redaction is upstream).
//!
//! ## Why serialised JSON and not struct equality?
//!
//! The `wasm` feature implies `lean`.  Under `lean`,
//! `EventMetadata.raw_bytes` is `#[serde(skip_serializing)]`.  A round-trip
//! through the wasm JS object (serialize → deserialize) therefore yields
//! `raw_bytes = []` and recomputes `raw_bytes_hash` from empty bytes, making
//! the deserialized struct unequal to the native in-memory struct.  Both
//! serialised forms are correct: `raw_bytes` is absent and `raw_bytes_hash`
//! holds the real hash (computed at parse time, not during round-trip).
//! Comparing `serde_json` output of native events (where lean also skips
//! `raw_bytes`) to the stringified wasm output avoids this lossiness while
//! still verifying all semantically meaningful fields.

#![cfg(all(target_arch = "wasm32", feature = "wasm"))]

use js_sys::Reflect;
use manasight_parser::{
    parse_whole_log, wasm::parse_whole_log_js, wasm::StreamingParser, GameEvent,
};
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
///
/// Parity is verified by comparing the **serialised JSON structure** of both
/// paths — specifically `serde_json::Value` representations:
/// - Native: `serde_json::to_value(&native_events)` — under `lean`,
///   `raw_bytes` is `skip_serializing`, so the value omits that field and
///   retains the correct `raw_bytes_hash`.
/// - Wasm: `serde_wasm_bindgen::from_value::<serde_json::Value>(js_value)` —
///   converts the JsValue to a generic JSON value without going through
///   `EventMetadata`'s custom `Deserialize`.  The JsValue was produced by the
///   wasm serialiser (lean), so `raw_bytes` is absent and `raw_bytes_hash` is
///   the correct hash.
///
/// A typed struct round-trip (`serde_wasm_bindgen::from_value::<Vec<GameEvent>>`)
/// is intentionally avoided: the custom `Deserialize` for `EventMetadata`
/// recomputes `raw_bytes_hash` from the (now-empty) `raw_bytes`, producing a
/// hash mismatch against the native struct even though both serialised forms
/// are identical.
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

    // Serialise native events to a generic JSON value.  Under `lean`,
    // `raw_bytes` is `skip_serializing`, so the value is structurally
    // identical to what the wasm serialiser emits.
    let native_value: serde_json::Value = serde_json::to_value(&native_events)
        .expect("native events must serialise to serde_json::Value without error");

    // Deserialise the wasm JsValue to a generic JSON value — this bypasses
    // `EventMetadata`'s custom `Deserialize` (which would recompute the hash
    // from empty bytes) and instead gives us the raw serialised shape directly.
    let wasm_value: serde_json::Value =
        serde_wasm_bindgen::from_value(js_value).expect("wasm JsValue must convert to JSON value");

    assert_eq!(
        native_value, wasm_value,
        "serialised JSON structure must be identical between native and wasm paths"
    );
}

/// Sanity-check: empty input produces zero events on both paths.
///
/// **WARNING:** This test uses a typed round-trip (`serde_wasm_bindgen::from_value::<Vec<GameEvent>>`)
/// and must NOT be extended to non-empty input under `lean`. The custom
/// `EventMetadata` `Deserialize` impl recomputes `raw_bytes_hash` from the
/// (lean-omitted, therefore empty) `raw_bytes`, producing a hash mismatch
/// against the native struct even though both serialised forms are identical.
/// Empty input is safe because there are no events — and thus no metadata —
/// to round-trip through the typed deserializer.
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

/// Regression test: `StreamingParser` event payloads must be plain JS objects,
/// not `Map` instances.
///
/// Feeds FIXTURE through `StreamingParser::pushChunk` / `finish` end-to-end and
/// inspects the raw JS output to confirm `serialize_maps_as_objects(true)` is
/// honoured in both methods.
///
/// If `push_chunk` or `finish` in `src/wasm.rs` are reverted to bare
/// `serde_wasm_bindgen::to_value(&events)`, the payload becomes a JS `Map` and
/// this test fails — guarding against that regression just as
/// `test_parse_whole_log_js_payload_is_plain_object_not_map` guards
/// `parse_whole_log_js` (PR #244 precedent).
#[wasm_bindgen_test]
fn test_streaming_parser_payload_is_plain_object_not_map() {
    // Guard: confirm parse_whole_log produces >=1 GameState event so the
    // end-to-end JS inspection below is not vacuous.
    let native_events = parse_whole_log(FIXTURE);
    assert!(
        !native_events.is_empty(),
        "parse_whole_log(FIXTURE) must produce >=1 event before the \
         StreamingParser JS inspection can be meaningful"
    );

    // Feed FIXTURE through StreamingParser in two chunks so both push_chunk
    // and finish are exercised. Split at the midpoint (valid UTF-8 boundary
    // because FIXTURE is ASCII).
    let mid = FIXTURE.len() / 2;
    let (first_half, second_half) = FIXTURE.split_at(mid);

    let mut parser = StreamingParser::new();

    let batch1 = parser
        .push_chunk(first_half)
        .expect("push_chunk (first half) must not fail");
    let batch2 = parser
        .push_chunk(second_half)
        .expect("push_chunk (second half) must not fail");
    let last = parser.finish().expect("finish must not fail");

    // Collect all JS values; at least one batch must be non-empty so the
    // plain-object check is not vacuous.
    let js_arrays = [&batch1, &batch2, &last];

    // Find the first non-empty array.
    let first_event_batch = js_arrays
        .iter()
        .find(|arr| {
            let len = Reflect::get(arr, &JsValue::from_str("length"))
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            len > 0.0
        })
        .expect("at least one batch must contain events from FIXTURE");

    // Get the first event from the batch via index "0".
    let first_event = Reflect::get(first_event_batch, &JsValue::from_str("0"))
        .expect("Reflect::get(\"0\") must not throw");
    assert!(
        !first_event.is_undefined(),
        "first event batch must have an element at index 0"
    );

    // GameEvent is externally-tagged: `{ GameState: { metadata, payload } }`.
    let game_state_inner = Reflect::get(&first_event, &JsValue::from_str("GameState"))
        .expect("Reflect::get(GameState) must not throw");
    assert!(
        !game_state_inner.is_undefined(),
        "first event must be a GameState variant (got something else)"
    );

    let payload_js = Reflect::get(&game_state_inner, &JsValue::from_str("payload"))
        .expect("Reflect::get(payload) must not throw");
    assert!(
        !payload_js.is_undefined(),
        "GameState event must have a payload field"
    );

    // Core assertion: payload must NOT be a JS Map.
    assert!(
        !payload_js.is_instance_of::<js_sys::Map>(),
        "StreamingParser payload must be a plain object, not a JS Map \
         (serialize_maps_as_objects regression in push_chunk/finish)"
    );

    // Confirm the payload is reachable by plain property access.
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
