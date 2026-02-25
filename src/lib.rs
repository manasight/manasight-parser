//! MTG Arena log file parser.
//!
//! This library crate reads Arena's `Player.log`, parses raw log entries
//! into typed game events, and distributes them via an async broadcast
//! channel. It is designed to run on the caller's Tokio runtime — it does
//! not initialize its own runtime or logger.
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
//! - **`event_bus`**: `tokio::broadcast` channel for fan-out to subscribers

pub mod event_bus;
pub mod events;
pub mod log;
pub mod parsers;
pub mod router;
