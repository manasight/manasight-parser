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
    }
}
