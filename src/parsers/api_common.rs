//! Shared utilities for parsing `==>` / `<==` API request/response pairs.
//!
//! MTG Arena logs API interactions as arrow-delimited entries:
//!
//! | Direction | Format | Example |
//! |-----------|--------|---------|
//! | Request (`==>`) | `==> MethodName {json}` | `==> EventJoin {"request":"..."}` |
//! | Response (`<==`) | `<== MethodName(uuid)\n{json}` | `<== RankGetCombinedRankInfo(a1b2c3d4-...)\n{...}` |
//!
//! These lines appear as continuation lines within a `[UnityCrossThreadLogger]`
//! entry — the `LogEntry.body` will contain the timestamp header line followed
//! by the `==>` or `<==` line and then the JSON payload.

/// Returns `true` if `body` contains a `<== method_name(` response marker.
///
/// Includes the `(` that immediately follows the method name in real log lines
/// (e.g., `<== StartHook(uuid)`), preventing false matches against methods
/// whose names share a common prefix (e.g., `StartHook` vs `StartHookV2`).
pub(crate) fn is_api_response(body: &str, method_name: &str) -> bool {
    let mut marker = String::with_capacity(5 + method_name.len());
    marker.push_str("<== ");
    marker.push_str(method_name);
    marker.push('(');
    body.contains(&marker)
}

/// Returns `true` if `body` contains a `==> method_name ` request marker.
///
/// Includes the space that immediately follows the method name in real log
/// lines (e.g., `==> EventJoin {"id":...}`), preventing false matches against
/// methods whose names share a common prefix (e.g., `EventJoin` vs
/// `EventJoinV2`).
pub(crate) fn is_api_request(body: &str, method_name: &str) -> bool {
    let mut marker = String::with_capacity(5 + method_name.len());
    marker.push_str("==> ");
    marker.push_str(method_name);
    marker.push(' ');
    body.contains(&marker)
}

/// Extracts the first JSON object or array from a multi-line log body.
///
/// Handles `[UnityCrossThreadLogger]` bracket headers by skipping past the
/// first `]` when the body starts with `[`, so header brackets are not
/// confused with JSON array delimiters.
///
/// Uses brace/bracket-depth counting that respects string literals to find
/// the complete JSON boundary.
pub(crate) fn extract_json_from_body(body: &str) -> Option<&str> {
    // If the body starts with a `[...]` header prefix, skip past it
    // so we don't match the header bracket as a JSON array start.
    let search_start = if body.starts_with('[') {
        body.find(']').map_or(0, |pos| pos + 1)
    } else {
        0
    };

    let search_region = &body[search_start..];
    let json_start = search_region.find(['{', '['])?;
    let json_start = search_start + json_start;

    let candidate = &body[json_start..];

    let first_byte = candidate.as_bytes().first().copied()?;
    let (open_char, close_char) = if first_byte == b'{' {
        ('{', '}')
    } else {
        ('[', ']')
    };

    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape_next = false;
    let mut end_pos = None;

    for (i, ch) in candidate.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => {
                escape_next = true;
            }
            '"' => {
                in_string = !in_string;
            }
            c if !in_string && c == open_char => {
                depth += 1;
            }
            c if !in_string && c == close_char => {
                depth -= 1;
                if depth == 0 {
                    end_pos = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }

    end_pos.map(|end| &candidate[..end])
}

/// Extracts and parses JSON from a log body, warning on malformed payloads.
///
/// Combines [`extract_json_from_body`] with `serde_json::from_str`, logging
/// a warning with the given `context` label when JSON parsing fails. Returns
/// `None` if no JSON is found or if parsing fails.
pub(crate) fn parse_json_from_body(body: &str, context: &str) -> Option<serde_json::Value> {
    let json_str = extract_json_from_body(body)?;
    match serde_json::from_str(json_str) {
        Ok(v) => Some(v),
        Err(e) => {
            ::log::warn!("Malformed JSON payload: context={context}, error={e}");
            None
        }
    }
}

/// Extracts and parses a nested JSON string field.
///
/// MTG Arena often escapes JSON payloads inside string fields called
/// `Payload` or `request`. This utility simplifies unescaping and parsing
/// those nested objects.
///
/// Logs a warning when `field` exists as a string but the nested JSON is
/// malformed. Missing fields and non-string fields still return `None`
/// silently so callers can use this as a probe.
pub(crate) fn parse_nested_json(
    v: &serde_json::Value,
    field: &str,
    context: Option<&str>,
) -> Option<serde_json::Value> {
    let nested = v.get(field)?.as_str()?;
    match serde_json::from_str(nested) {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            if let Some(ctx) = context {
                ::log::warn!("Malformed nested JSON: context={ctx}, field={field}, error={e}");
            }
            None
        }
    }
}

/// Extracts an event name from a parsed JSON value.
///
/// MTG Arena is inconsistent about where it stores event names. This helper
/// checks the following locations in order:
/// 1.  Top-level `EventName` or `InternalEventName`.
/// 2.  Common nested objects:
///     a. `Course.InternalEventName` or `Course.EventName` (common in responses).
///     b. `PickInfo.EventName` (common in bot draft requests).
/// 3.  A nested string-escaped `request` field containing any of the above
///     (common in outbound requests).
pub(crate) fn extract_event_name(parsed: &serde_json::Value) -> String {
    // 1. Try direct top-level fields.
    if let Some(name) = parsed
        .get("EventName")
        .or_else(|| parsed.get("InternalEventName"))
        .and_then(serde_json::Value::as_str)
    {
        return name.to_owned();
    }

    // 2. Try common nested objects.
    for field in ["Course", "PickInfo"] {
        if let Some(name) = parsed.get(field).and_then(|obj| {
            obj.get("InternalEventName")
                .or_else(|| obj.get("EventName"))
                .and_then(serde_json::Value::as_str)
        }) {
            return name.to_owned();
        }
    }

    // 3. Try nested string-escaped request field (requests).
    if let Some(request_json) = parse_nested_json(parsed, "request", None) {
        // Recursion is safe here as MTGA logs have shallow request nesting.
        let name = extract_event_name(&request_json);
        if !name.is_empty() {
            return name;
        }
    }

    String::new()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- is_api_response -------------------------------------------------------

    mod api_response {
        use super::*;

        #[test]
        fn test_is_api_response_matches_method() {
            let body = "[UnityCrossThreadLogger]2/22/2026 11:59:51 AM\n\
                         <== StartHook(e3f1a2b4-5678-9abc-def0-123456789abc)\n\
                         {\"InventoryInfo\": {}}";
            assert!(is_api_response(body, "StartHook"));
        }

        #[test]
        fn test_is_api_response_no_match_wrong_method() {
            let body = "<== RankGetCombinedRankInfo(uuid)\n{}";
            assert!(!is_api_response(body, "StartHook"));
        }

        #[test]
        fn test_is_api_response_no_match_request_arrow() {
            let body = "==> StartHook {\"data\": 1}";
            assert!(!is_api_response(body, "StartHook"));
        }

        #[test]
        fn test_is_api_response_no_match_empty() {
            assert!(!is_api_response("", "StartHook"));
        }

        #[test]
        fn test_is_api_response_no_match_prefix_method() {
            // "StartHook" must not match a hypothetical "StartHookV2" response.
            let body = "<== StartHookV2(uuid)\n{}";
            assert!(!is_api_response(body, "StartHook"));
        }
    }

    // -- is_api_request --------------------------------------------------------

    mod api_request {
        use super::*;

        #[test]
        fn test_is_api_request_matches_method() {
            let body = "[UnityCrossThreadLogger]==> EventJoin {\"request\": \"{}\"}";
            assert!(is_api_request(body, "EventJoin"));
        }

        #[test]
        fn test_is_api_request_no_match_wrong_method() {
            let body = "==> EventClaimPrize {}";
            assert!(!is_api_request(body, "EventJoin"));
        }

        #[test]
        fn test_is_api_request_no_match_response_arrow() {
            let body = "<== EventJoin(uuid)\n{}";
            assert!(!is_api_request(body, "EventJoin"));
        }

        #[test]
        fn test_is_api_request_no_match_empty() {
            assert!(!is_api_request("", "EventJoin"));
        }

        #[test]
        fn test_is_api_request_no_match_prefix_method() {
            // "EventJoin" must not match a hypothetical "EventJoinV2" request.
            let body = "==> EventJoinV2 {\"data\": 1}";
            assert!(!is_api_request(body, "EventJoin"));
        }
    }

    // -- extract_json_from_body ------------------------------------------------

    mod json_extraction {
        use super::*;

        #[test]
        fn test_extract_json_object() {
            let body = "header line\n{\"key\": \"value\"}";
            assert_eq!(extract_json_from_body(body), Some("{\"key\": \"value\"}"));
        }

        #[test]
        fn test_extract_json_array() {
            let body = "header line\n[1, 2, 3]";
            assert_eq!(extract_json_from_body(body), Some("[1, 2, 3]"));
        }

        #[test]
        fn test_extract_json_with_bracket_header() {
            let body = "[UnityCrossThreadLogger]some text\n{\"data\": 1}";
            assert_eq!(extract_json_from_body(body), Some("{\"data\": 1}"));
        }

        #[test]
        fn test_extract_json_nested_objects() {
            let body = "header\n{\"outer\": {\"inner\": 1}}";
            assert_eq!(
                extract_json_from_body(body),
                Some("{\"outer\": {\"inner\": 1}}")
            );
        }

        #[test]
        fn test_extract_json_with_string_braces() {
            let body = "header\n{\"msg\": \"hello {world}\"}";
            assert_eq!(
                extract_json_from_body(body),
                Some("{\"msg\": \"hello {world}\"}")
            );
        }

        #[test]
        fn test_extract_json_no_json() {
            assert!(extract_json_from_body("no json here").is_none());
        }

        #[test]
        fn test_extract_json_multiline() {
            let body = "[UnityCrossThreadLogger]2/22/2026 11:59:51 AM\n\
                         <== StartHook(uuid)\n\
                         {\n\
                           \"InventoryInfo\": {\"Gems\": 1234}\n\
                         }";
            let json = extract_json_from_body(body);
            assert!(json.is_some());
            let parsed: serde_json::Value =
                serde_json::from_str(json.unwrap_or("{}")).unwrap_or_default();
            assert_eq!(parsed["InventoryInfo"]["Gems"], 1234);
        }

        #[test]
        fn test_extract_json_unclosed_brace() {
            let body = "header {\"key\": \"value\"";
            assert!(extract_json_from_body(body).is_none());
        }

        #[test]
        fn test_extract_json_brace_in_string() {
            let body = r#"text {"key": "value with { braces }"}"#;
            assert_eq!(
                extract_json_from_body(body),
                Some(r#"{"key": "value with { braces }"}"#)
            );
        }

        #[test]
        fn test_extract_json_escaped_quote_in_string() {
            let body = r#"prefix {"key": "val\"ue"}"#;
            assert_eq!(extract_json_from_body(body), Some(r#"{"key": "val\"ue"}"#));
        }
    }

    // -- parse_json_from_body --------------------------------------------------

    mod parse_json {
        use super::*;

        #[test]
        fn test_parse_json_from_body_valid_object() {
            let body = "header\n{\"key\": 42}";
            let result = parse_json_from_body(body, "test");
            assert_eq!(result, Some(serde_json::json!({"key": 42})));
        }

        #[test]
        fn test_parse_json_from_body_no_json_returns_none() {
            assert!(parse_json_from_body("no json", "test").is_none());
        }

        #[test]
        fn test_parse_json_from_body_malformed_json_returns_none() {
            let body = "header\n{invalid}";
            assert!(parse_json_from_body(body, "test").is_none());
        }

        #[test]
        fn test_parse_json_from_body_valid_array() {
            let body = "header\n[1, 2, 3]";
            let result = parse_json_from_body(body, "test");
            assert_eq!(result, Some(serde_json::json!([1, 2, 3])));
        }
    }

    // -- parse_nested_json -----------------------------------------------------
    mod nested_json {
        use super::*;

        #[test]
        fn test_parse_nested_json_valid_string_returns_json() {
            let v = serde_json::json!({"Payload": "{\"key\":\"value\"}"});
            let result = parse_nested_json(&v, "Payload", Some("test"));
            assert_eq!(result, Some(serde_json::json!({"key": "value"})));
        }

        #[test]
        fn test_parse_nested_json_missing_field_returns_none() {
            let v = serde_json::json!({"Other": "data"});
            assert!(parse_nested_json(&v, "Payload", Some("test")).is_none());
        }

        #[test]
        fn test_parse_nested_json_non_string_returns_none() {
            let v = serde_json::json!({"Payload": {"key": "value"}});
            assert!(parse_nested_json(&v, "Payload", Some("test")).is_none());
        }

        #[test]
        fn test_parse_nested_json_invalid_json_returns_none() {
            let v = serde_json::json!({"Payload": "not json"});
            assert!(parse_nested_json(&v, "Payload", Some("test")).is_none());
        }
    }

    // -- extract_event_name ----------------------------------------------------
    mod event_name {
        use super::*;

        #[test]
        fn test_extract_event_name_top_level_event_name_returns_name() {
            let parsed = serde_json::json!({"EventName": "DirectEvent"});
            assert_eq!(extract_event_name(&parsed), "DirectEvent");
        }

        #[test]
        fn test_extract_event_name_top_level_internal_name_returns_name() {
            let parsed = serde_json::json!({"InternalEventName": "InternalTest"});
            assert_eq!(extract_event_name(&parsed), "InternalTest");
        }

        #[test]
        fn test_extract_event_name_course_nested_returns_name() {
            let parsed = serde_json::json!({
                "Course": {"InternalEventName": "CourseInternal"}
            });
            assert_eq!(extract_event_name(&parsed), "CourseInternal");
        }

        #[test]
        fn test_extract_event_name_string_escaped_request_returns_name() {
            let parsed = serde_json::json!({
                "id": "test",
                "request": "{\"EventName\":\"NestedRequest\"}"
            });
            assert_eq!(extract_event_name(&parsed), "NestedRequest");
        }

        #[test]
        fn test_extract_event_name_top_level_wins_over_course_and_request_returns_top_level() {
            // Top-level should win over Course, which should win over request.
            let parsed = serde_json::json!({
                "EventName": "TopLevel",
                "Course": {"EventName": "CourseLevel"},
                "request": "{\"EventName\":\"RequestLevel\"}"
            });
            assert_eq!(extract_event_name(&parsed), "TopLevel");
        }

        #[test]
        fn test_extract_event_name_pick_info_nested_returns_name() {
            let parsed = serde_json::json!({
                "PickInfo": {"EventName": "PickInfoTest"}
            });
            assert_eq!(extract_event_name(&parsed), "PickInfoTest");
        }

        #[test]
        fn test_extract_event_name_nested_pick_info_in_request_returns_name() {
            let parsed = serde_json::json!({
                "request": "{\"PickInfo\":{\"EventName\":\"NestedPickInfo\"}}"
            });
            assert_eq!(extract_event_name(&parsed), "NestedPickInfo");
        }

        #[test]
        fn test_extract_event_name_no_matching_field_returns_empty() {
            let parsed = serde_json::json!({"id": "test"});
            assert_eq!(extract_event_name(&parsed), "");
        }

        #[test]
        fn test_extract_event_name_malformed_request_json_returns_empty() {
            let parsed = serde_json::json!({"request": "not json"});
            assert_eq!(extract_event_name(&parsed), "");
        }
    }
}
