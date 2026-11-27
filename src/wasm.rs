//! wasm-bindgen exports for the `wasm` cargo feature.
//!
//! This module exposes two APIs:
//!
//! - [`parse_whole_log_js`] — a thin wrapper around [`crate::parse_whole_log`]
//!   for callers that already have the entire log in memory.
//! - [`StreamingParser`] — an incremental parser that accepts the log in
//!   arbitrary chunks (`pushChunk`) and finalises with `finish`, keeping only
//!   the pending partial line (capped in length) in memory. Useful for very large logs or streaming
//!   transports where holding the whole log is not desirable.
//!
//! Both APIs use the same serialiser configuration
//! (`serialize_maps_as_objects(true)`), so the serialiser output shape is
//! identical to the native `parse_whole_log` path. However, the `wasm` build
//! enables `lean`, which omits `raw_bytes` and heavy payload fields
//! (annotations, timers, zones, `game_objects`, `raw_start_hook`, decks), so the
//! wasm output is a lean subset of the full native output — not byte-identical.
//!
//! # Building
//!
//! ```bash
//! wasm-pack build --target bundler --no-default-features --features wasm
//! ```
//!
//! The `wasm` feature implies `brace_depth_flush` (consistent flush semantics
//! with the desktop default) and `lean` (omits `raw_bytes` + heavy payload
//! fields to reduce WASM memory pressure).
//!
//! # Usage from JavaScript / TypeScript
//!
//! ```js
//! import init, { parseWholeLog, StreamingParser } from './pkg/manasight_parser.js';
//! await init();
//!
//! // Whole-log path:
//! const events = parseWholeLog(logText);
//!
//! // Streaming path:
//! const parser = new StreamingParser();
//! const batch1 = parser.pushChunk(firstChunk);
//! const batch2 = parser.pushChunk(secondChunk);
//! const last   = parser.finish();
//! parser.free(); // release WASM memory
//! ```

// wasm_bindgen is only available (and needed) under the `wasm` feature.
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

// ---------------------------------------------------------------------------
// Host-testable streaming core (not gated on `cfg(feature="wasm")`)
// ---------------------------------------------------------------------------

/// Incremental streaming parse core.
///
/// Accepts log text in arbitrary chunks and emits batches of [`crate::events::GameEvent`]
/// values. The core is intentionally **not** gated on the `wasm` feature so
/// that host-side tests (e.g. proptest parity checks) can drive it directly
/// without WASM dependencies.
///
/// # Chunking contract
///
/// `push_chunk` replicates [`str::lines`] byte-for-byte:
/// - Lines are separated by `'\n'`.
/// - A trailing `'\r'` before each `'\n'` is stripped (CRLF → LF).
/// - A final line not terminated by `'\n'` is held as a pending partial line
///   and processed only by [`Self::finish`], matching the behaviour of
///   `for line in input.lines()` followed by a `flush()` call.
///
/// Empty chunks are no-ops (the pending line is unchanged).
///
/// One exception keeps memory bounded: a line longer than
/// [`crate::log::lines::MAX_LINE_BYTES`] is dropped whole (see
/// [`crate::log::lines::LineSplitter`]). Output for every shorter line is
/// unchanged.
///
/// # Stability
///
/// This type is `pub` only so that host-side tests can drive it without WASM
/// dependencies. It is **not** part of the stable public API — breaking
/// changes may occur without a semver bump.
#[doc(hidden)]
pub struct StreamingParserCore {
    buffer: crate::log::entry::LineBuffer,
    router: crate::router::Router,
    /// Splits chunks into lines, holding the incomplete final line.
    lines: crate::log::lines::LineSplitter,
}

impl StreamingParserCore {
    /// Creates a new, empty streaming core.
    pub fn new() -> Self {
        Self::with_max_line_bytes(crate::log::lines::MAX_LINE_BYTES)
    }

    /// Creates a new, empty streaming core that drops lines longer than
    /// `max_line_bytes` instead of the default
    /// [`crate::log::lines::MAX_LINE_BYTES`].
    pub fn with_max_line_bytes(max_line_bytes: usize) -> Self {
        Self {
            buffer: crate::log::entry::LineBuffer::new(),
            router: crate::router::Router::new(),
            lines: crate::log::lines::LineSplitter::with_max_line_bytes(max_line_bytes),
        }
    }

    /// Processes a text chunk, returning any events completed by this chunk.
    ///
    /// Every line the chunk completes (with `'\n'` and a preceding `'\r'`
    /// removed, as `str::lines()` does) is fed to `LineBuffer::push_line`.
    /// Text after the chunk's last `'\n'` is held for the next call.
    pub fn push_chunk(&mut self, text: &str) -> Vec<crate::events::GameEvent> {
        let mut events = Vec::new();
        let buffer = &mut self.buffer;
        let router = &self.router;
        self.lines.push(text, |line| {
            for entry in buffer.push_line(line) {
                events.extend(router.route(&entry));
            }
        });
        events
    }

    /// Finalises the stream, processing any pending partial line and flushing
    /// the internal `LineBuffer`.
    ///
    /// Corresponds to the final `buffer.flush()` call in `parse_whole_log`:
    /// drains any entry that was not followed by a new header.
    ///
    /// After `finish`, the core should be considered consumed. Calling
    /// `push_chunk` or `finish` again will process against an empty state.
    pub fn finish(&mut self) -> Vec<crate::events::GameEvent> {
        let mut events = Vec::new();

        // Process the final line if it was not terminated by '\n'.
        //
        // `LineSplitter::finish` does NOT strip a lone trailing '\r'.
        // `str::lines()` only removes '\r' as part of '\r\n'; the last element
        // returned for a final line without a trailing '\n' retains any lone
        // '\r'. Stripping it would diverge from `parse_whole_log`'s
        // `input.lines()` last element.
        if let Some(tail) = self.lines.finish() {
            for entry in self.buffer.push_line(&tail) {
                events.extend(self.router.route(&entry));
            }
        }

        // Flush any trailing entry that was not followed by a new header.
        if let Some(entry) = self.buffer.flush() {
            events.extend(self.router.route(&entry));
        }

        events
    }
}

impl Default for StreamingParserCore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// WASM-bindgen wrapper (gated on `cfg(feature="wasm")`)
// ---------------------------------------------------------------------------

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
/// access (e.g. `event.GameState.payload.type`).
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
#[cfg(feature = "wasm")]
#[wasm_bindgen(js_name = parseWholeLog)]
pub fn parse_whole_log_js(input: &str) -> Result<JsValue, JsValue> {
    use serde::Serialize as _;
    let events = crate::parse_whole_log(input);
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    events
        .serialize(&serializer)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Incremental / streaming parser for MTG Arena log files, exposed as a
/// `wasm-bindgen` export.
///
/// Accepts log text in arbitrary chunks via [`StreamingParser::push_chunk`]
/// and finalises with [`StreamingParser::finish`].  Callers that have the
/// whole log in memory should prefer [`parse_whole_log_js`] instead.
///
/// # Memory management
///
/// The handle allocates state in WASM linear memory. Callers **must** call
/// `parser.free()` when done to release it. wasm-bindgen emits `free` as an
/// ordinary export; usable via manual `WebAssembly.Instance` instantiation
/// without a bundler.
///
/// # Example (JavaScript)
///
/// ```js
/// const parser = new StreamingParser();
/// let events = [];
/// for (const chunk of logChunks) {
///     events = events.concat(parser.pushChunk(chunk));
/// }
/// events = events.concat(parser.finish());
/// parser.free();
/// ```
#[cfg(feature = "wasm")]
#[wasm_bindgen]
pub struct StreamingParser {
    core: StreamingParserCore,
}

#[cfg(feature = "wasm")]
impl Default for StreamingParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "wasm")]
#[wasm_bindgen]
impl StreamingParser {
    /// Creates a new, empty `StreamingParser`.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            core: StreamingParserCore::new(),
        }
    }

    /// Processes a text chunk and returns a JS array of `GameEvent` objects
    /// completed by this chunk.
    ///
    /// The returned array may be empty if the chunk ends mid-line. Events are
    /// emitted eagerly as soon as log entries are fully buffered.
    ///
    /// # Errors
    ///
    /// Returns a `JsValue` error string if serialisation to JS fails.
    #[wasm_bindgen(js_name = pushChunk)]
    pub fn push_chunk(&mut self, text: &str) -> Result<JsValue, JsValue> {
        use serde::Serialize as _;
        let events = self.core.push_chunk(text);
        let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
        events
            .serialize(&serializer)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Finalises the stream and returns a JS array of any remaining `GameEvent`
    /// objects.
    ///
    /// Processes any pending partial final line and flushes the
    /// internal `LineBuffer`, mirroring the trailing-entry flush in
    /// `parse_whole_log`. Call `parser.free()` after this.
    ///
    /// # Errors
    ///
    /// Returns a `JsValue` error string if serialisation to JS fails.
    #[wasm_bindgen]
    pub fn finish(&mut self) -> Result<JsValue, JsValue> {
        use serde::Serialize as _;
        let events = self.core.finish();
        let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
        events
            .serialize(&serializer)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}
