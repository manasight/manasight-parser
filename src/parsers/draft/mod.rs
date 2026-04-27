//! Draft event parsers: bot draft picks, human draft picks, and draft completion.
//!
//! Each sub-module handles one draft event category:
//!
//! | Module | Log Signatures | Event Type |
//! |--------|---------------|------------|
//! | [`bot`] | `BotDraftDraftStatus`, `BotDraftDraftPick` | [`DraftBotEvent`](crate::events::DraftBotEvent) |
//! | [`human`] | `Draft.Notify`, `EventPlayerDraftMakePick`, `LogBusinessEvents` with `PickGrpId` | [`DraftHumanEvent`](crate::events::DraftHumanEvent) |
//! | [`complete`] | `DraftCompleteDraft` | [`DraftCompleteEvent`](crate::events::DraftCompleteEvent) |

pub mod bot;
pub mod complete;
pub mod human;
