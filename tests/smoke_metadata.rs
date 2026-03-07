//! Tests for smoke test metadata files (corpus manifest and baseline JSON).
//!
//! These tests validate that the committed data files parse correctly and
//! maintain internal consistency. They run without access to the actual
//! corpus — no `MANASIGHT_TEST_LOGS` needed.

use serde::Deserialize;
use std::collections::HashMap;

type TestResult = Result<(), Box<dyn std::error::Error>>;

// ---------------------------------------------------------------------------
// Manifest types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Manifest {
    files: Vec<ManifestEntry>,
}

#[derive(Deserialize)]
struct ManifestEntry {
    filename: String,
    sha256: String,
    size_bytes: u64,
    date_captured: String,
}

// ---------------------------------------------------------------------------
// Baseline types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Baseline {
    #[serde(rename = "_meta")]
    meta: BaselineMeta,
    files: HashMap<String, BaselineFile>,
}

#[derive(Deserialize)]
struct BaselineMeta {
    description: String,
    generated_from_commit: String,
    corpus_tag: String,
}

#[derive(Deserialize)]
struct BaselineFile {
    total_entries: u64,
    parsers: HashMap<String, u64>,
    event_types: HashMap<String, u64>,
    unclaimed: u64,
    double_claims: u64,
    #[allow(dead_code)]
    timestamp_failures: u64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_manifest() -> Result<Manifest, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string("smoke-corpus-manifest.toml")?;
    let manifest: Manifest = toml::from_str(&content)?;
    Ok(manifest)
}

fn read_baseline() -> Result<Baseline, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string("smoke-baseline.json")?;
    let baseline: Baseline = serde_json::from_str(&content)?;
    Ok(baseline)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_manifest_parses_valid_toml() -> TestResult {
    let manifest = read_manifest()?;

    assert!(
        !manifest.files.is_empty(),
        "manifest should contain at least one file entry"
    );
    Ok(())
}

#[test]
fn test_manifest_entries_have_valid_fields() -> TestResult {
    let manifest = read_manifest()?;

    for entry in &manifest.files {
        // Filename must end in .log
        assert!(
            std::path::Path::new(&entry.filename)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("log")),
            "filename '{}' should have a .log extension",
            entry.filename
        );

        // SHA-256 must be 64 hex characters
        assert_eq!(
            entry.sha256.len(),
            64,
            "sha256 for '{}' should be 64 chars, got {}",
            entry.filename,
            entry.sha256.len()
        );
        assert!(
            entry.sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "sha256 for '{}' should be hex-only",
            entry.filename
        );

        // Size must be positive
        assert!(
            entry.size_bytes > 0,
            "size_bytes for '{}' should be positive",
            entry.filename
        );

        // Date must match YYYY-MM-DD format (10 chars)
        assert_eq!(
            entry.date_captured.len(),
            10,
            "date_captured for '{}' should be YYYY-MM-DD format",
            entry.filename
        );
    }
    Ok(())
}

#[test]
fn test_manifest_filenames_are_unique() -> TestResult {
    let manifest = read_manifest()?;

    let mut seen = std::collections::HashSet::new();
    for entry in &manifest.files {
        assert!(
            seen.insert(&entry.filename),
            "duplicate filename in manifest: '{}'",
            entry.filename
        );
    }
    Ok(())
}

#[test]
fn test_baseline_parses_valid_json() -> TestResult {
    let baseline = read_baseline()?;

    assert!(
        !baseline.files.is_empty(),
        "baseline should contain at least one file entry"
    );
    Ok(())
}

#[test]
fn test_baseline_entries_have_consistent_counts() -> TestResult {
    let baseline = read_baseline()?;

    for (filename, file_data) in &baseline.files {
        // total_entries must be positive
        assert!(
            file_data.total_entries > 0,
            "total_entries for '{filename}' should be positive",
        );

        // Sum of parser claims should not exceed total_entries
        let parser_sum: u64 = file_data.parsers.values().sum();
        assert!(
            parser_sum <= file_data.total_entries,
            "parser claim sum ({parser_sum}) for '{filename}' exceeds total_entries ({})",
            file_data.total_entries
        );

        // When double_claims is 0, parser claims + unclaimed should equal total_entries
        if file_data.double_claims == 0 {
            let accounted = parser_sum + file_data.unclaimed;
            assert_eq!(
                accounted, file_data.total_entries,
                "parser claims ({parser_sum}) + unclaimed ({}) should equal total_entries ({}) \
                 for '{filename}' when double_claims is 0",
                file_data.unclaimed, file_data.total_entries
            );
        }

        // Event type counts should be non-empty when any parser claimed entries
        if parser_sum > 0 {
            assert!(
                !file_data.event_types.is_empty(),
                "event_types for '{filename}' should not be empty when parsers claimed entries",
            );
        }
    }
    Ok(())
}

#[test]
fn test_manifest_and_baseline_cover_same_files() -> TestResult {
    let manifest = read_manifest()?;
    let baseline = read_baseline()?;

    let mut manifest_files: Vec<&str> =
        manifest.files.iter().map(|e| e.filename.as_str()).collect();
    manifest_files.sort_unstable();

    let mut baseline_files: Vec<&str> = baseline.files.keys().map(String::as_str).collect();
    baseline_files.sort_unstable();

    assert_eq!(
        manifest_files, baseline_files,
        "manifest and baseline should reference the same set of files"
    );
    Ok(())
}

#[test]
fn test_baseline_meta_fields_present() -> TestResult {
    let baseline = read_baseline()?;

    assert!(
        !baseline.meta.description.is_empty(),
        "_meta.description should not be empty"
    );
    assert!(
        !baseline.meta.generated_from_commit.is_empty(),
        "_meta.generated_from_commit should not be empty"
    );
    assert!(
        !baseline.meta.corpus_tag.is_empty(),
        "_meta.corpus_tag should not be empty"
    );
    Ok(())
}
