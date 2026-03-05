//! Shared extraction helpers used across GRE submodules.

/// Extracts an array of strings from a JSON array value.
///
/// Collects all string values, silently skipping non-string entries.
pub(super) fn extract_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Extracts a numeric value from a nested `{ "value": N }` object.
///
/// Power and toughness in MTGA logs are represented as objects with
/// a `value` field (e.g., `{ "value": 3 }`). Returns `null` if the
/// structure is missing or malformed.
pub(super) fn extract_nested_value(obj: Option<&serde_json::Value>) -> serde_json::Value {
    obj.and_then(|o| o.get("value"))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_string_array_normal() {
        let value = serde_json::json!(["CardType_Creature", "CardType_Artifact"]);
        let strings = extract_string_array(Some(&value));
        assert_eq!(strings, vec!["CardType_Creature", "CardType_Artifact"]);
    }

    #[test]
    fn test_extract_string_array_empty() {
        let value = serde_json::json!([]);
        let strings = extract_string_array(Some(&value));
        assert!(strings.is_empty());
    }

    #[test]
    fn test_extract_string_array_none() {
        let strings = extract_string_array(None);
        assert!(strings.is_empty());
    }

    #[test]
    fn test_extract_string_array_mixed_types_skips_non_strings() {
        let value = serde_json::json!(["valid", 42, "also_valid", null]);
        let strings = extract_string_array(Some(&value));
        assert_eq!(strings, vec!["valid", "also_valid"]);
    }

    #[test]
    fn test_extract_nested_value_present() {
        let value = serde_json::json!({"value": 3});
        let result = extract_nested_value(Some(&value));
        assert_eq!(result, serde_json::json!(3));
    }

    #[test]
    fn test_extract_nested_value_missing_value_key() {
        let value = serde_json::json!({"other": 5});
        let result = extract_nested_value(Some(&value));
        assert!(result.is_null());
    }

    #[test]
    fn test_extract_nested_value_none() {
        let result = extract_nested_value(None);
        assert!(result.is_null());
    }
}
