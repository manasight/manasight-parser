//! Shared utility functions for the parser crate.
//!
//! Contains internal helpers (string truncation) and public pipeline utilities
//! (`compress_log`, `content_hash`) shared by all consumers of parsed log data.

use std::io::Write;

use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};

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

/// Gzip-compress scrubbed log text into a byte buffer.
///
/// Uses [`flate2::write::GzEncoder`] with default compression (level 6),
/// which balances speed and ratio well for the 1-20 MB text files produced
/// by Arena's `Player.log`.
///
/// # Errors
///
/// Returns [`std::io::Error`] if compression fails.
///
/// # Examples
///
/// ```
/// use manasight_parser::util::compress_log;
///
/// let compressed = compress_log("some log data").unwrap();
/// // Output is valid gzip (magic bytes 0x1f 0x8b)
/// assert_eq!(compressed[0], 0x1f);
/// assert_eq!(compressed[1], 0x8b);
/// ```
pub fn compress_log(scrubbed_text: &str) -> Result<Vec<u8>, std::io::Error> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(scrubbed_text.as_bytes())?;
    encoder.finish()
}

/// Compute the SHA-256 hex digest of compressed payload bytes.
///
/// Returns a 64-character lowercase hex string suitable for use as a
/// dedup key or content-addressable identifier.
///
/// # Examples
///
/// ```
/// use manasight_parser::util::content_hash;
///
/// let hash = content_hash(b"hello");
/// assert_eq!(hash.len(), 64);
/// assert_eq!(hash, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
/// ```
pub fn content_hash(compressed_bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(compressed_bytes);
    let digest = hasher.finalize();
    digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write;
        // write! to a String is infallible; the error type exists only for
        // trait uniformity, so ignoring it is safe.
        let _ = write!(acc, "{b:02x}");
        acc
    })
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

    // --- compress_log tests ---

    /// Helper: decompress gzip data for round-trip verification.
    fn gzip_decompress(data: &[u8]) -> Vec<u8> {
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(data);
        let mut result = Vec::new();
        decoder
            .read_to_end(&mut result)
            .unwrap_or_else(|_| unreachable!());
        result
    }

    #[test]
    fn test_compress_log_round_trip() {
        let input = "Line one\nLine two\nLine three\n";
        let compressed = compress_log(input).unwrap_or_else(|_| unreachable!());
        let decompressed = gzip_decompress(&compressed);
        assert_eq!(
            String::from_utf8(decompressed).unwrap_or_else(|_| unreachable!()),
            input
        );
    }

    #[test]
    fn test_compress_log_gzip_magic_bytes() {
        let input = "some log data";
        let compressed = compress_log(input).unwrap_or_else(|_| unreachable!());
        assert!(
            compressed.len() >= 2,
            "compressed output should be at least 2 bytes"
        );
        assert_eq!(compressed[0], 0x1f, "first magic byte should be 0x1f");
        assert_eq!(compressed[1], 0x8b, "second magic byte should be 0x8b");
    }

    #[test]
    fn test_compress_log_empty_input_produces_valid_gzip() {
        let compressed = compress_log("").unwrap_or_else(|_| unreachable!());
        assert!(compressed.len() >= 2);
        assert_eq!(compressed[0], 0x1f);
        assert_eq!(compressed[1], 0x8b);
        let decompressed = gzip_decompress(&compressed);
        assert!(decompressed.is_empty());
    }

    #[test]
    fn test_compress_log_large_input_does_not_panic() {
        let line = "Normal log line without sensitive data, repeating to build volume.\n";
        let large_input: String = line.repeat(75_000);
        let compressed = compress_log(&large_input).unwrap_or_else(|_| unreachable!());
        assert!(
            compressed.len() < large_input.len(),
            "compressed size ({}) should be less than raw size ({})",
            compressed.len(),
            large_input.len()
        );
        let decompressed = gzip_decompress(&compressed);
        assert_eq!(decompressed.len(), large_input.len());
    }

    #[test]
    fn test_compress_log_output_smaller_than_input() {
        let input = "repeated data line\n".repeat(1_000);
        let compressed = compress_log(&input).unwrap_or_else(|_| unreachable!());
        assert!(
            compressed.len() < input.len() / 2,
            "repetitive text should compress to less than half its size"
        );
    }

    // --- content_hash tests ---

    #[test]
    fn test_content_hash_known_vector() {
        let expected = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(content_hash(&[]), expected);
    }

    #[test]
    fn test_content_hash_format_64_lowercase_hex() {
        let hash = content_hash(b"arbitrary payload bytes");
        assert_eq!(hash.len(), 64, "SHA-256 hex digest must be 64 characters");
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hash must be lowercase hex only, got: {hash}"
        );
    }

    #[test]
    fn test_content_hash_deterministic() {
        let data = b"the same input twice";
        assert_eq!(
            content_hash(data),
            content_hash(data),
            "same input must always produce the same hash"
        );
    }

    #[test]
    fn test_content_hash_different_inputs_differ() {
        let hash_a = content_hash(b"payload A");
        let hash_b = content_hash(b"payload B");
        assert_ne!(
            hash_a, hash_b,
            "different inputs should produce different hashes"
        );
    }

    #[test]
    fn test_content_hash_nonempty_known_value() {
        let expected = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert_eq!(content_hash(b"hello"), expected);
    }
}
