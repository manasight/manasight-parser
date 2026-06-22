//! wasm-bindgen exports for the `wasm` cargo feature.
//!
//! This module exposes two APIs:
//!
//! - [`parse_whole_log_js`] — a thin wrapper around [`crate::parse_whole_log`]
//!   for callers that already have the entire log in memory.
//! - [`StreamingParser`] — an incremental parser that accepts the log in
//!   arbitrary chunks (`pushChunk`) and finalises with `finish`, keeping only
//!   a small `tail` buffer in memory. Useful for very large logs or streaming
//!   transports where holding the whole log is not desirable.
//!
//! Both APIs use the same serialiser configuration
//! (`serialize_maps_as_objects(true)`) so their output is identical to the
//! native `parse_whole_log` path.
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
/// - A final line not terminated by `'\n'` is held in `tail` and processed
///   only by [`Self::finish`], matching the behaviour of
///   `for line in input.lines()` followed by a `flush()` call.
///
/// Empty chunks are no-ops (tail is unchanged).
pub struct StreamingParserCore {
    buffer: crate::log::entry::LineBuffer,
    router: crate::router::Router,
    /// Incomplete final line from the last chunk (no trailing `'\n'` yet).
    tail: String,
}

impl StreamingParserCore {
    /// Creates a new, empty streaming core.
    pub fn new() -> Self {
        Self {
            buffer: crate::log::entry::LineBuffer::new(),
            router: crate::router::Router::new(),
            tail: String::new(),
        }
    }

    /// Processes a text chunk, returning any events completed by this chunk.
    ///
    /// The algorithm replicates `str::lines()` semantics:
    ///
    /// 1. Prepend any leftover `tail` from the previous call.
    /// 2. Split on `'\n'`.
    /// 3. For every piece **except the last**: strip a trailing `'\r'` (CRLF
    ///    handling), feed to `LineBuffer::push_line`, collect events.
    /// 4. Retain the last piece as the new `tail` (may be empty if the chunk
    ///    ended with `'\n'`).
    pub fn push_chunk(&mut self, text: &str) -> Vec<crate::events::GameEvent> {
        if text.is_empty() {
            return Vec::new();
        }

        // Build the full text to process by prepending any leftover tail.
        let combined: std::borrow::Cow<str> = if self.tail.is_empty() {
            std::borrow::Cow::Borrowed(text)
        } else {
            let mut s = std::mem::take(&mut self.tail);
            s.push_str(text);
            std::borrow::Cow::Owned(s)
        };

        let mut events = Vec::new();
        let mut pieces = combined.split('\n');

        // The split always produces at least one piece. We need to treat all
        // pieces except the last as complete lines.
        //
        // `split` on `'\n'` yields:
        //   "a\nb\n"  -> ["a", "b", ""]   (last is empty — chunk ended with \n)
        //   "a\nb"    -> ["a", "b"]        (last is partial — no trailing \n)
        //   ""        -> [""]              (empty — handled above, tail unchanged)
        //
        // We use `peekable` to detect the last piece without consuming ahead.
        let mut peekable = pieces.by_ref().peekable();
        while let Some(piece) = peekable.next() {
            if peekable.peek().is_none() {
                // Last piece — retain as tail (may be empty string).
                piece.clone_into(&mut self.tail);
            } else {
                // Complete line — strip trailing `\r` to match `str::lines()` CRLF handling.
                let line = piece.strip_suffix('\r').unwrap_or(piece);
                for entry in self.buffer.push_line(line) {
                    events.extend(self.router.route(&entry));
                }
            }
        }

        events
    }

    /// Finalises the stream, processing any remaining `tail` and flushing the
    /// internal `LineBuffer`.
    ///
    /// Corresponds to the final `buffer.flush()` call in `parse_whole_log`:
    /// drains any entry that was not followed by a new header.
    ///
    /// After `finish`, the core should be considered consumed. Calling
    /// `push_chunk` or `finish` again will process against an empty state.
    pub fn finish(&mut self) -> Vec<crate::events::GameEvent> {
        let mut events = Vec::new();

        // Process the leftover tail (the final line if not terminated by '\n').
        if !self.tail.is_empty() {
            let tail = std::mem::take(&mut self.tail);
            let line = tail.strip_suffix('\r').unwrap_or(&tail);
            for entry in self.buffer.push_line(line) {
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
    /// Processes any partial final line held in the tail buffer and flushes the
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
