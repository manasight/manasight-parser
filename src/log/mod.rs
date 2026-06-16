//! Raw log file reading: discovery, tailing, entry parsing, and timestamps.

#[cfg(feature = "tailer")]
pub mod discovery;
pub mod entry;
// `steam` is consumed only by `discovery` (Linux Steam-library path lookup),
// which is itself `tailer`-gated. Without the `tailer` feature, `discovery` is
// excluded and these helpers would be dead code (denied under `-D warnings` on
// the Linux `--no-default-features` CI build), so gate `steam` the same way.
#[cfg(all(target_os = "linux", feature = "tailer"))]
mod steam;
#[cfg(feature = "tailer")]
pub mod tailer;
pub mod timestamp;
