//! Polling-based file tailer with rotation detection.
//!
//! Polls `Player.log` at a configurable interval (default 50 ms) for new
//! data, detecting file rotation (MTGA restart) by monitoring file size
//! and modification time.
