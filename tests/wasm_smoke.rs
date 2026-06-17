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

#![cfg(target_arch = "wasm32")]

use manasight_parser::{parse_whole_log, wasm::parse_whole_log_js, GameEvent};
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
