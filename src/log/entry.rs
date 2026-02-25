//! Log entry prefix identification and multi-line JSON accumulation.
//!
//! Detects log entry boundaries using the `[UnityCrossThreadLogger]` and
//! `[Client GRE]` header patterns, then accumulates subsequent lines until
//! the next header boundary to form complete raw entries.
