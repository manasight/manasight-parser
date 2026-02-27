//! Category-specific parsers — one module per event category.

pub(crate) mod api_common;
pub mod client_actions;
pub mod collection;
pub mod draft;
pub mod event_lifecycle;
pub mod gre;
pub mod inventory;
pub mod match_state;
pub mod rank;
pub mod session;
