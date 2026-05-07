//! Shared test fixtures for GRE submodule tests.
//!
//! Body-builder helpers shared across multiple GRE test modules live here.
//! Fixtures used by only a single submodule stay local to that submodule's
//! `#[cfg(test)] mod tests` block.

/// Helper: build a realistic `greToClientEvent` JSON body containing
/// a `GREMessageType_ConnectResp` message with a full decklist.
pub(super) fn connect_resp_body() -> String {
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [
                    {
                        "type": "GREMessageType_ConnectResp",
                        "systemSeatIds": [1, 2],
                        "msgId": 1,
                        "gameStateId": 0,
                        "connectResp": {
                            "status": "ConnectionStatus_Success",
                            "deckMessage": {
                                "deckCards": [68398, 68398, 68398, 68398, 70136, 70136, 70136, 70136, 71432, 71432, 71432],
                                "sideboardCards": [73509, 73509, 73510]
                            },
                            "settings": {
                                "stops": [
                                    {"stopType": "StopType_UpkeepStep", "appliesTo": "SettingScope_Team"},
                                    {"stopType": "StopType_DrawStep", "appliesTo": "SettingScope_Team"}
                                ],
                                "autoPassOption": "AutoPassOption_UnresolvedOnly",
                                "graveyardOrder": "OrderType_OrderArbitrary",
                                "manaSelectionType": "ManaSelectionType_Auto",
                                "autoTapStopsSetting": "AutoTapStopsSetting_Enable",
                                "autoOptionalPaymentCancellationSetting": "AutoOptionalPaymentCancellationSetting_AskMe",
                                "transientStops": [],
                                "cosmetics": []
                            }
                        }
                    }
                ]
            }
        })
    )
}

/// Helper: build a `ConnectResp` body without the wrapper
/// `greToClientEvent` key (flat format).
pub(super) fn flat_connect_resp_body() -> String {
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientMessages": [
                {
                    "type": "GREMessageType_ConnectResp",
                    "systemSeatIds": [1, 2],
                    "msgId": 2,
                    "gameStateId": 1,
                    "connectResp": {
                        "deckMessage": {
                            "deckCards": [11111, 22222, 33333],
                            "sideboardCards": [44444]
                        },
                        "settings": {
                            "autoPassOption": "AutoPassOption_ResolveAll"
                        }
                    }
                }
            ]
        })
    )
}

/// Helper: build sample zone fixtures for a full game state snapshot.
pub(super) fn sample_zones() -> serde_json::Value {
    serde_json::json!([
        {"zoneId": 30, "type": "ZoneType_Hand", "visibility": "Visibility_Public",
         "ownerSeatId": 1, "objectInstanceIds": [101, 102, 103]},
        {"zoneId": 31, "type": "ZoneType_Library", "visibility": "Visibility_Hidden",
         "ownerSeatId": 1, "objectInstanceIds": [201, 202, 203, 204, 205]},
        {"zoneId": 32, "type": "ZoneType_Battlefield", "visibility": "Visibility_Public",
         "ownerSeatId": 0, "objectInstanceIds": [301, 302]},
        {"zoneId": 33, "type": "ZoneType_Graveyard", "visibility": "Visibility_Public",
         "ownerSeatId": 1, "objectInstanceIds": [401]},
        {"zoneId": 34, "type": "ZoneType_Exile", "visibility": "Visibility_Public",
         "ownerSeatId": 0, "objectInstanceIds": []},
        {"zoneId": 35, "type": "ZoneType_Stack", "visibility": "Visibility_Public",
         "ownerSeatId": 0, "objectInstanceIds": [501]}
    ])
}

/// Helper: build sample game object fixtures.
pub(super) fn sample_game_objects() -> serde_json::Value {
    serde_json::json!([
        {"instanceId": 101, "grpId": 68398, "type": "GameObjectType_Card",
         "zoneId": 30, "visibility": "Visibility_Public",
         "ownerSeatId": 1, "controllerSeatId": 1,
         "cardTypes": ["CardType_Creature"],
         "subtypes": ["SubType_Human", "SubType_Soldier"],
         "abilities": ["AbilityType_Lifelink"],
         "name": 68398, "power": {"value": 3}, "toughness": {"value": 2}},
        {"instanceId": 301, "grpId": 70136, "type": "GameObjectType_Card",
         "zoneId": 32, "visibility": "Visibility_Public",
         "ownerSeatId": 1, "controllerSeatId": 1,
         "cardTypes": ["CardType_Land"], "subtypes": ["SubType_Plains"],
         "abilities": [], "name": 70136},
        {"instanceId": 501, "grpId": 71432, "type": "GameObjectType_Card",
         "zoneId": 35, "visibility": "Visibility_Public",
         "ownerSeatId": 1, "controllerSeatId": 1,
         "cardTypes": ["CardType_Instant"], "subtypes": [],
         "abilities": [], "name": 71432}
    ])
}

/// Helper: build a GRE event body with a `GameStateMessage` containing
/// zones and game objects.
pub(super) fn game_state_message_body() -> String {
    let zones = sample_zones();
    let objects = sample_game_objects();
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [{
                    "type": "GREMessageType_GameStateMessage",
                    "msgId": 5,
                    "gameStateId": 42,
                    "gameStateMessage": {
                        "zones": zones,
                        "gameObjects": objects,
                        "gameInfo": {
                            "matchID": "match-id-12345",
                            "gameNumber": 1,
                            "stage": "GameStage_Play",
                            "type": "GameType_Standard",
                            "variant": "GameVariant_Normal",
                            "mulliganType": "MulliganType_London"
                        }
                    }
                }]
            }
        })
    )
}

/// Helper: build a `GameStateMessage` body with the inner
/// `gameStateMessage.type` populated as `"GameStateType_Diff"`.
///
/// Mirrors `game_state_message_body` but tags the inner GSM as a Diff
/// (incremental) update — used to exercise the `game_state_type`
/// discriminator extraction.
pub(super) fn diff_game_state_message_body() -> String {
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [{
                    "type": "GREMessageType_GameStateMessage",
                    "msgId": 6,
                    "gameStateId": 43,
                    "gameStateMessage": {
                        "type": "GameStateType_Diff",
                        "zones": [
                            {
                                "zoneId": 31,
                                "type": "ZoneType_Hand",
                                "ownerSeatId": 1,
                                "objectInstanceIds": [201, 202]
                            }
                        ],
                        "gameObjects": []
                    }
                }]
            }
        })
    )
}

/// Helper: build a `QueuedGameStateMessage` body.
pub(super) fn queued_game_state_message_body() -> String {
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [
                    {
                        "type": "GREMessageType_QueuedGameStateMessage",
                        "msgId": 7,
                        "gameStateId": 60,
                        "gameStateMessage": {
                            "zones": [
                                {
                                    "zoneId": 32,
                                    "type": "ZoneType_Battlefield",
                                    "visibility": "Visibility_Public",
                                    "ownerSeatId": 0,
                                    "objectInstanceIds": [301, 302, 303]
                                }
                            ],
                            "gameObjects": [
                                {
                                    "instanceId": 303,
                                    "grpId": 72000,
                                    "type": "GameObjectType_Card",
                                    "zoneId": 32,
                                    "visibility": "Visibility_Public",
                                    "ownerSeatId": 2,
                                    "controllerSeatId": 2,
                                    "cardTypes": ["CardType_Creature"],
                                    "subtypes": [],
                                    "abilities": [],
                                    "name": 72000,
                                    "power": { "value": 5 },
                                    "toughness": { "value": 5 }
                                }
                            ]
                        }
                    }
                ]
            }
        })
    )
}

/// Helper: build a `GameStateMessage` body in flat format (no wrapper).
pub(super) fn flat_game_state_message_body() -> String {
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientMessages": [
                {
                    "type": "GREMessageType_GameStateMessage",
                    "msgId": 3,
                    "gameStateId": 15,
                    "gameStateMessage": {
                        "zones": [
                            {
                                "zoneId": 30,
                                "type": "ZoneType_Hand",
                                "ownerSeatId": 2,
                                "objectInstanceIds": [601, 602]
                            }
                        ],
                        "gameObjects": [
                            {
                                "instanceId": 601,
                                "grpId": 80000,
                                "type": "GameObjectType_Card",
                                "zoneId": 30,
                                "ownerSeatId": 2,
                                "controllerSeatId": 2
                            }
                        ]
                    }
                }
            ]
        })
    )
}

/// Helper: build a GRE event body with **multiple** `GameStateMessage`
/// entries in a single `greToClientMessages` array (batched).
///
/// This simulates the real-world scenario where Arena batches multiple
/// game state updates into one log entry (55% of GRE events in live data).
pub(super) fn batched_game_state_messages_body() -> String {
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [
                    {
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 10,
                        "gameStateId": 100,
                        "gameStateMessage": {
                            "zones": [
                                {"zoneId": 30, "type": "ZoneType_Hand",
                                 "ownerSeatId": 1, "objectInstanceIds": [101, 102]}
                            ],
                            "gameObjects": [],
                            "gameInfo": {"stage": "GameStage_Play"}
                        }
                    },
                    {
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 11,
                        "gameStateId": 101,
                        "gameStateMessage": {
                            "zones": [
                                {"zoneId": 32, "type": "ZoneType_Battlefield",
                                 "ownerSeatId": 0, "objectInstanceIds": [301]}
                            ],
                            "gameObjects": [],
                            "gameInfo": {"stage": "GameStage_Play"}
                        }
                    },
                    {
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 12,
                        "gameStateId": 102,
                        "gameStateMessage": {
                            "zones": [],
                            "gameObjects": [],
                            "gameInfo": {"stage": "GameStage_Play"}
                        }
                    }
                ]
            }
        })
    )
}

/// Helper: build a GRE event body with multiple `QueuedGameStateMessage`
/// entries in a single batch.
pub(super) fn batched_queued_game_state_messages_body() -> String {
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [
                    {
                        "type": "GREMessageType_QueuedGameStateMessage",
                        "msgId": 20,
                        "gameStateId": 200,
                        "gameStateMessage": {
                            "zones": [],
                            "gameObjects": [],
                            "gameInfo": {"stage": "GameStage_Play"}
                        }
                    },
                    {
                        "type": "GREMessageType_QueuedGameStateMessage",
                        "msgId": 21,
                        "gameStateId": 201,
                        "gameStateMessage": {
                            "zones": [],
                            "gameObjects": [],
                            "gameInfo": {"stage": "GameStage_Play"}
                        }
                    }
                ]
            }
        })
    )
}

/// Helper: build a GRE event body with mixed GSM and QGSM in one batch.
pub(super) fn mixed_gsm_qgsm_body() -> String {
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [
                    {
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 30,
                        "gameStateId": 300,
                        "gameStateMessage": {
                            "zones": [],
                            "gameObjects": [],
                            "gameInfo": {"stage": "GameStage_Play"}
                        }
                    },
                    {
                        "type": "GREMessageType_QueuedGameStateMessage",
                        "msgId": 31,
                        "gameStateId": 301,
                        "gameStateMessage": {
                            "zones": [],
                            "gameObjects": [],
                            "gameInfo": {"stage": "GameStage_Play"}
                        }
                    }
                ]
            }
        })
    )
}

/// Helper: build a batched GRE event where one GSM has `GameStage_GameOver`.
pub(super) fn batched_gsm_with_game_over_body() -> String {
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [
                    {
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 40,
                        "gameStateId": 400,
                        "gameStateMessage": {
                            "zones": [],
                            "gameObjects": [],
                            "gameInfo": {"stage": "GameStage_Play"}
                        }
                    },
                    {
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 41,
                        "gameStateId": 401,
                        "gameStateMessage": {
                            "zones": [],
                            "gameObjects": [],
                            "gameInfo": {
                                "stage": "GameStage_GameOver",
                                "matchID": "match-456",
                                "gameNumber": 1,
                                "results": [
                                    {"scope": "MatchScope_Game", "result": "ResultType_Draw",
                                     "winningTeamId": 0}
                                ]
                            }
                        }
                    }
                ]
            }
        })
    )
}

/// Helper: build a batched GRE event with two `GameStage_GameOver` messages:
/// `MatchState_GameComplete` followed by `MatchState_MatchComplete`, matching
/// the real Arena pattern where both are sent in the same batch at game end.
pub(super) fn batched_dual_game_over_body() -> String {
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [
                    {
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 50,
                        "gameStateId": 500,
                        "gameStateMessage": {
                            "gameInfo": {
                                "stage": "GameStage_GameOver",
                                "matchState": "MatchState_GameComplete",
                                "matchID": "match-dual-test",
                                "gameNumber": 1,
                                "results": [
                                    {
                                        "scope": "MatchScope_Game",
                                        "result": "ResultType_WinLoss",
                                        "winningTeamId": 1,
                                        "reason": "ResultReason_Game"
                                    }
                                ]
                            },
                            "zones": [],
                            "gameObjects": [],
                            "players": [{"seatId": 1, "lifeTotal": 20}]
                        }
                    },
                    {
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 51,
                        "gameStateId": 501,
                        "gameStateMessage": {
                            "gameInfo": {
                                "stage": "GameStage_GameOver",
                                "matchState": "MatchState_MatchComplete",
                                "matchID": "match-dual-test",
                                "gameNumber": 1,
                                "results": [
                                    {
                                        "scope": "MatchScope_Game",
                                        "result": "ResultType_WinLoss",
                                        "winningTeamId": 1,
                                        "reason": "ResultReason_Game"
                                    },
                                    {
                                        "scope": "MatchScope_Match",
                                        "result": "ResultType_WinLoss",
                                        "winningTeamId": 1,
                                        "reason": "ResultReason_Game"
                                    }
                                ]
                            }
                        }
                    }
                ]
            }
        })
    )
}

/// Helper: build a GRE event body modeled on the bundled batch shape captured
/// in corpus session `2026-02-22_0000_ecl-premier-bg-elves` line 22433:
/// `[GREMessageType_ConnectResp, GREMessageType_DieRollResultsResp,
/// GREMessageType_GameStateMessage]` in a single `greToClientMessages` array.
///
/// Used to verify the parser emits the `ConnectResp` event followed by the
/// sibling GSM in source-array order, dropping only the unhandled
/// `DieRollResultsResp`. R2 corpus inspection found this shape in 2/7 captured
/// `ConnectResp` entries.
pub(super) fn connect_resp_with_bundled_gsm_body() -> String {
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [
                    {
                        "type": "GREMessageType_ConnectResp",
                        "systemSeatIds": [1, 2],
                        "msgId": 2,
                        "gameStateId": 0,
                        "connectResp": {
                            "status": "ConnectionStatus_Success",
                            "deckMessage": {
                                "deckCards": [
                                    98421, 98502, 98485, 98408, 98408, 98404,
                                    98317, 98514, 98442, 98558, 98579, 98506
                                ],
                                "sideboardCards": [98499, 98439, 98457]
                            },
                            "settings": {
                                "autoPassOption": "AutoPassOption_ResolveMyStackEffects"
                            }
                        }
                    },
                    {
                        "type": "GREMessageType_DieRollResultsResp",
                        "systemSeatIds": [1, 2],
                        "msgId": 3,
                        "dieRollResultsResp": {
                            "playerDieRolls": [
                                {"systemSeatId": 1, "rollValue": 9},
                                {"systemSeatId": 2, "rollValue": 11}
                            ]
                        }
                    },
                    {
                        "type": "GREMessageType_GameStateMessage",
                        "systemSeatIds": [2],
                        "msgId": 4,
                        "gameStateId": 1,
                        "gameStateMessage": {
                            "type": "GameStateType_Full",
                            "gameStateId": 1,
                            "gameInfo": {
                                "matchID": "27011c79-bundled-batch",
                                "gameNumber": 1,
                                "stage": "GameStage_Start",
                                "type": "GameType_Duel",
                                "matchState": "MatchState_GameInProgress",
                                "mulliganType": "MulliganType_London"
                            },
                            "zones": [
                                {
                                    "zoneId": 32,
                                    "type": "ZoneType_Library",
                                    "visibility": "Visibility_Hidden",
                                    "ownerSeatId": 1,
                                    "objectInstanceIds": [120, 121, 122, 123, 124]
                                }
                            ],
                            "gameObjects": []
                        }
                    }
                ]
            }
        })
    )
}

/// Helper: build a `GameStateMessage` with an empty `gameStateMessage`
/// (no zones, no objects, no game info).
pub(super) fn empty_game_state_message_body() -> String {
    format!(
        "[UnityCrossThreadLogger]greToClientEvent\n{}",
        serde_json::json!({
            "greToClientEvent": {
                "greToClientMessages": [
                    {
                        "type": "GREMessageType_GameStateMessage",
                        "msgId": 1,
                        "gameStateMessage": {}
                    }
                ]
            }
        })
    )
}
