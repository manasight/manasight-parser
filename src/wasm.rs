//! wasm-bindgen exports for the `wasm` cargo feature.
//!
//! This module exposes [`parse_whole_log_js`] — a thin wrapper around the
//! native [`crate::parse_whole_log`] — via `wasm-bindgen` so that JS/TS
//! consumers (e.g. a Parse Worker) can call `parseWholeLog(input)` and
//! receive a live JS object graph instead of a JSON string.
//!
//! # Building
//!
//! ```bash
//! wasm-pack build --target web --no-default-features --features wasm
//! ```
//!
//! The `wasm` feature implies `brace_depth_flush` so the artifact always has
//! the same flush semantics as the desktop default build.
//!
//! # Usage from JavaScript / TypeScript
//!
//! ```js
//! import init, { parseWholeLog } from './pkg/manasight_parser.js';
//! await init();
//! const events = parseWholeLog(logText); // returns a JS array of GameEvent objects
//! ```

use wasm_bindgen::prelude::*;

/// Parses an entire MTG Arena `Player.log` string and returns a JS array of
/// `GameEvent` objects.
///
/// This is the wasm-bindgen export of [`crate::parse_whole_log`].  The
/// returned value is a live JS object graph — not a JSON string — so callers
/// can access fields directly without a second `JSON.parse` round-trip.
///
/// All event payloads (and every nested dynamic object) are serialised as
/// plain JS objects (`{}`), not `Map` instances, matching the shape produced
/// by the native `serde_json` path.  Fields are reachable by normal property
/// access (e.g. `event.GameState.payload.greToClientEvent`).
///
/// # Errors
///
/// Returns a `JsValue` error string if serialisation to JS fails (extremely
/// unlikely in practice, but modelled explicitly so callers can handle it).
///
/// # JS name
///
/// The JS-visible name is `parseWholeLog` (camelCase).  The Rust name
/// (`parse_whole_log_js`) is distinct from the native `parse_whole_log` to
/// avoid a symbol clash when the `wasm` feature is compiled on a non-wasm
/// host (e.g. during `cargo clippy --all-features`).
#[wasm_bindgen(js_name = parseWholeLog)]
pub fn parse_whole_log_js(input: &str) -> Result<JsValue, JsValue> {
    use serde::Serialize as _;
    let events = crate::parse_whole_log(input);
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    events
        .serialize(&serializer)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}
