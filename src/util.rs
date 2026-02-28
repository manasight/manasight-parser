//! Shared utility functions used across the parser crate.

/// Truncates a string for safe inclusion in log messages.
///
/// Returns the original string if its byte length is at most `max_len`.
/// Otherwise, truncates to at most `max_len` bytes, stepping back to the
/// nearest valid UTF-8 character boundary so the returned slice is always
/// valid `&str`.
pub(crate) fn truncate_for_log(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        // Find a valid UTF-8 boundary at or before `max_len`.
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_for_log_short_string_unchanged() {
        assert_eq!(truncate_for_log("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_for_log_exact_length_unchanged() {
        assert_eq!(truncate_for_log("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_for_log_long_string_truncated() {
        assert_eq!(truncate_for_log("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_for_log_multibyte_safe() {
        // "cafe\u{0301}" is 5 bytes (\u{0301} is 2 bytes). Truncating at 4 should
        // not split the \u{0301}.
        let s = "caf\u{00e9}";
        let result = truncate_for_log(s, 4);
        assert_eq!(result, "caf");
    }

    #[test]
    fn test_truncate_for_log_empty_string() {
        assert_eq!(truncate_for_log("", 10), "");
    }

    #[test]
    fn test_truncate_for_log_zero_max_len() {
        assert_eq!(truncate_for_log("hello", 0), "");
    }
}
