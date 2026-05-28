//! Raw log file reading: discovery, tailing, entry parsing, and timestamps.

pub mod discovery;
pub mod entry;
#[cfg(target_os = "linux")]
mod steam;
pub mod tailer;
pub mod timestamp;
