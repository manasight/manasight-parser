//! Async broadcast channel for distributing parsed events to subscribers.
//!
//! Uses `tokio::sync::broadcast` to fan out events from the parser to
//! multiple consumers (game state engine, game accumulator, etc.).
//! The parser library owns the sender; consumers receive cloned receivers.
