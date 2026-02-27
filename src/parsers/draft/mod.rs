//! Draft event parsers: bot draft picks, human draft picks, and draft completion.
//!
//! Each sub-module handles one draft event category:
//!
//! | Module | Log Signatures | Event Type |
//! |--------|---------------|------------|
//! | [`bot`] | `DraftStatus: "PickNext"`, `BotDraft_DraftPick` | [`DraftBotEvent`](crate::events::DraftBotEvent) |

pub mod bot;
