//! Raw log file reading: discovery, tailing, entry parsing, and timestamps.

#[cfg(feature = "tailer")]
pub mod discovery;
pub mod entry;
#[cfg(target_os = "linux")]
mod steam;
#[cfg(feature = "tailer")]
pub mod tailer;
pub mod timestamp;
