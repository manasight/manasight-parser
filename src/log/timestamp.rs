//! Locale-dependent timestamp parsing for MTG Arena log entries.
//!
//! MTGA log timestamps vary by system locale. This module handles all known
//! formats (11+ locale-dependent variants, epoch milliseconds, .NET ticks,
//! and ISO 8601) and normalizes them to UTC.
