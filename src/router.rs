//! Raw log entry to parser dispatch routing.
//!
//! Examines the header prefix and payload of each raw log entry to
//! determine which category-specific parser should handle it. Unrecognized
//! entries are counted and logged at debug level.
