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
