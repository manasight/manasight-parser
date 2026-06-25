//! MTG Arena log file parser.
//!
//! This library crate reads Arena's `Player.log`, parses raw log entries
//! into typed game events, and distributes them via an async broadcast
//! channel. It is designed to run on the caller's Tokio runtime — it does
//! not initialize its own runtime or logger.
//!
//! # Quick start (async, desktop)
//!
//! Requires the `tailer` feature (enabled by default):
//!
//! ```rust,no_run,ignore
//! use std::path::Path;
//! use manasight_parser::MtgaEventStream;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let (stream, mut subscriber) = MtgaEventStream::start(Path::new("Player.log")).await?;
//!
//! while let Some(event) = subscriber.recv().await {
//!     println!("got event: {event:?}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Quick start (sync, WASM)
//!
//! With `--no-default-features --features brace_depth_flush` (no `tailer`),
//! only the pure-sync subset of the crate is available:
//!
//! ```rust
//! use manasight_parser::parse_whole_log;
//!
//! let input = ""; // replace with actual Player.log content
//! let events = parse_whole_log(input);
//! println!("parsed {} events", events.len());
//! ```
//!
//! # Architecture
//!
//! ```text
//! Player.log → File Tailer → Entry Buffer → Router → Parsers → Event Bus
//! ```
//!
//! - **`log`** module: file discovery, polling tailer, entry accumulation, timestamps
//! - **`router`**: dispatches raw entries to the correct category parser
//! - **`parsers`**: one sub-module per event category
//! - **`events`**: public event type enums/structs (the parser's output contract)
//! - **`event_bus`**: `tokio::broadcast` channel for fan-out to subscribers (requires `tailer` feature)
//! - **`stream`**: public entry point ([`MtgaEventStream`]) (requires `tailer` feature)

#[cfg(feature = "tailer")]
pub mod event_bus;
pub mod events;
pub mod log;
pub mod parsers;
pub mod router;
pub mod sanitize;
#[cfg(feature = "tailer")]
pub mod stream;
pub mod util;
// The `wasm` module always exposes `StreamingParserCore` for host-side tests.
// The `#[wasm_bindgen]` exports within are gated on the `wasm` feature.
pub mod wasm;

// ---------------------------------------------------------------------------
// Re-exports — public API surface
// ---------------------------------------------------------------------------

#[cfg(feature = "tailer")]
pub use event_bus::Subscriber;
pub use events::{
    ClientActionEvent, DeckCollectionEvent, DeckSubmissionEvent, DetailedLoggingStatusEvent,
    DraftBotEvent, DraftCompleteEvent, DraftHumanEvent, EventLifecycleEvent, EventMetadata,
    GameEvent, GameResultEvent, GameStateEvent, InventoryEvent, LocalSeatEvent,
    LogFileRotatedEvent, MatchStateEvent, PerformanceClass, RankEvent, SessionEvent,
    TruncationEvent,
};
pub use sanitize::{scrub_raw_log, scrub_raw_log_with, ScrubOptions};
#[cfg(feature = "tailer")]
pub use stream::{MtgaEventStream, StreamError};
pub use util::{compress_log, content_hash};

// ---------------------------------------------------------------------------
// Sync entry point — WASM-compatible
// ---------------------------------------------------------------------------

/// Parses an entire MTG Arena `Player.log` string into a [`Vec`] of [`GameEvent`]s.
///
/// This is a pure, synchronous function with no I/O and no async dependencies.
/// It is suitable for use in WASM environments or any context where the file
/// content has already been loaded into memory.
///
/// # How it works
///
/// Lines are fed through [`log::entry::LineBuffer`] to reassemble log entries,
/// then dispatched via [`router::Router`] — the same core pipeline the async
/// desktop path uses. A final [`LineBuffer::flush`](log::entry::LineBuffer::flush)
/// drains any trailing entry that was not followed by a new header.
///
/// # Example
///
/// ```rust
/// use manasight_parser::parse_whole_log;
///
/// let input = "DETAILED LOGS: ENABLED\n";
/// let events = parse_whole_log(input);
/// assert_eq!(events.len(), 1);
/// ```
pub fn parse_whole_log(input: &str) -> Vec<GameEvent> {
    let mut buffer = log::entry::LineBuffer::new();
    let router = router::Router::new();
    let mut events = Vec::new();

    for line in input.lines() {
        for entry in buffer.push_line(line) {
            events.extend(router.route(&entry));
        }
    }

    // Drain any trailing entry that was not followed by a new header.
    if let Some(entry) = buffer.flush() {
        events.extend(router.route(&entry));
    }

    events
}
