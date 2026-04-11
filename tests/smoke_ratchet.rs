//! Unit tests for the ratchet comparison logic in `smoke_common`.
//!
//! These tests validate that:
//! - Matching baselines produce no diffs
//! - Improvements (increased counts) are detected and pass
//! - Regressions (decreased counts) are detected and fail
//! - New parsers/event types in actual results are treated as improvements
//! - The report formatter produces readable output

mod smoke_common;

use std::collections::BTreeMap;

use smoke_common::{compare_against_baseline, Baseline, BaselineFile, BaselineMeta};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Creates a baseline with a single file entry.
fn make_baseline(filename: &str, parsers: BTreeMap<String, u64>) -> Baseline {
    let event_types = BTreeMap::new();
    Baseline {
        meta: BaselineMeta {
            description: "test baseline".to_string(),
            generated_from_commit: "abc1234".to_string(),
            corpus_tag: "test-v1".to_string(),
        },
        files: BTreeMap::from([(
            filename.to_string(),
            BaselineFile {
                total_entries: 100,
                parsers,
                event_types,
                unclaimed: 10,
                double_claims: 0,
                timestamp_failures: 5,
            },
        )]),
    }
}

/// Creates an actual file entry with parser counts.
fn make_actual_file(parsers: BTreeMap<String, u64>) -> BaselineFile {
    BaselineFile {
        total_entries: 100,
        parsers,
        event_types: BTreeMap::new(),
        unclaimed: 10,
        double_claims: 0,
        timestamp_failures: 5,
    }
}

// ---------------------------------------------------------------------------
// Tests: exact match
// ---------------------------------------------------------------------------

#[test]
fn test_ratchet_exact_match_produces_no_diffs() {
    let parsers = BTreeMap::from([("session".to_string(), 5_u64), ("gre".to_string(), 100)]);
    let baseline = make_baseline("test.log", parsers.clone());
    let actual = BTreeMap::from([("test.log".to_string(), make_actual_file(parsers))]);

    let result = compare_against_baseline(&baseline, &actual);
    assert!(result.diffs.is_empty(), "expected no diffs for exact match");
    assert!(result.is_pass());
}

// ---------------------------------------------------------------------------
// Tests: improvements
// ---------------------------------------------------------------------------

#[test]
fn test_ratchet_improvement_detected_and_passes() {
    let baseline_parsers =
        BTreeMap::from([("session".to_string(), 5_u64), ("gre".to_string(), 100)]);
    let actual_parsers = BTreeMap::from([
        ("session".to_string(), 5_u64),
        ("gre".to_string(), 110), // improved
    ]);
    let baseline = make_baseline("test.log", baseline_parsers);
    let actual = BTreeMap::from([("test.log".to_string(), make_actual_file(actual_parsers))]);

    let result = compare_against_baseline(&baseline, &actual);
    assert!(result.is_pass(), "improvements should pass");
    assert_eq!(result.improvements().len(), 1);
    assert_eq!(result.regressions().len(), 0);

    let improvement = &result.improvements()[0];
    assert_eq!(improvement.metric, "parser/gre");
    assert_eq!(improvement.baseline_value, 100);
    assert_eq!(improvement.actual_value, 110);
}

// ---------------------------------------------------------------------------
// Tests: regressions
// ---------------------------------------------------------------------------

#[test]
fn test_ratchet_regression_detected_and_fails() {
    let baseline_parsers =
        BTreeMap::from([("session".to_string(), 5_u64), ("gre".to_string(), 100)]);
    let actual_parsers = BTreeMap::from([
        ("session".to_string(), 5_u64),
        ("gre".to_string(), 90), // regression
    ]);
    let baseline = make_baseline("test.log", baseline_parsers);
    let actual = BTreeMap::from([("test.log".to_string(), make_actual_file(actual_parsers))]);

    let result = compare_against_baseline(&baseline, &actual);
    assert!(!result.is_pass(), "regressions should fail");
    assert_eq!(result.regressions().len(), 1);

    let regression = &result.regressions()[0];
    assert_eq!(regression.metric, "parser/gre");
    assert_eq!(regression.baseline_value, 100);
    assert_eq!(regression.actual_value, 90);
}

#[test]
fn test_ratchet_mixed_improvement_and_regression() {
    let baseline_parsers = BTreeMap::from([
        ("session".to_string(), 5_u64),
        ("gre".to_string(), 100),
        ("client_actions".to_string(), 50),
    ]);
    let actual_parsers = BTreeMap::from([
        ("session".to_string(), 5_u64),
        ("gre".to_string(), 110),           // improvement
        ("client_actions".to_string(), 40), // regression
    ]);
    let baseline = make_baseline("test.log", baseline_parsers);
    let actual = BTreeMap::from([("test.log".to_string(), make_actual_file(actual_parsers))]);

    let result = compare_against_baseline(&baseline, &actual);
    assert!(!result.is_pass(), "regression should cause failure");
    assert_eq!(result.improvements().len(), 1);
    assert_eq!(result.regressions().len(), 1);
}

// ---------------------------------------------------------------------------
// Tests: new parsers/event types
// ---------------------------------------------------------------------------

#[test]
fn test_ratchet_new_parser_in_actual_is_improvement() {
    let baseline_parsers = BTreeMap::from([("session".to_string(), 5_u64)]);
    let actual_parsers = BTreeMap::from([
        ("session".to_string(), 5_u64),
        ("draft_bot".to_string(), 10), // new parser not in baseline
    ]);
    let baseline = make_baseline("test.log", baseline_parsers);
    let actual = BTreeMap::from([("test.log".to_string(), make_actual_file(actual_parsers))]);

    let result = compare_against_baseline(&baseline, &actual);
    assert!(result.is_pass(), "new parser should be an improvement");
    assert_eq!(result.improvements().len(), 1);
    assert_eq!(result.improvements()[0].metric, "parser/draft_bot");
}

#[test]
fn test_ratchet_new_event_type_in_actual_is_improvement() {
    let baseline_parsers = BTreeMap::from([("session".to_string(), 5_u64)]);
    let baseline = Baseline {
        meta: BaselineMeta {
            description: "test".to_string(),
            generated_from_commit: "abc1234".to_string(),
            corpus_tag: "test-v1".to_string(),
        },
        files: BTreeMap::from([(
            "test.log".to_string(),
            BaselineFile {
                total_entries: 100,
                parsers: baseline_parsers,
                event_types: BTreeMap::from([("Session".to_string(), 5_u64)]),
                unclaimed: 10,
                double_claims: 0,
                timestamp_failures: 5,
            },
        )]),
    };
    let actual_file = BaselineFile {
        total_entries: 100,
        parsers: BTreeMap::from([("session".to_string(), 5_u64)]),
        event_types: BTreeMap::from([
            ("Session".to_string(), 5_u64),
            ("DraftBot".to_string(), 10), // new event type
        ]),
        unclaimed: 10,
        double_claims: 0,
        timestamp_failures: 5,
    };
    let actual = BTreeMap::from([("test.log".to_string(), actual_file)]);

    let result = compare_against_baseline(&baseline, &actual);
    assert!(result.is_pass());
    assert_eq!(result.improvements().len(), 1);
    assert_eq!(result.improvements()[0].metric, "event_type/DraftBot");
}

// ---------------------------------------------------------------------------
// Tests: missing files
// ---------------------------------------------------------------------------

#[test]
fn test_ratchet_missing_actual_file_is_skipped() {
    let baseline_parsers = BTreeMap::from([("session".to_string(), 5_u64)]);
    let baseline = make_baseline("test.log", baseline_parsers);
    // Actual has no files matching the baseline.
    let actual: BTreeMap<String, BaselineFile> = BTreeMap::new();

    let result = compare_against_baseline(&baseline, &actual);
    assert!(result.diffs.is_empty(), "missing file should be skipped");
    assert!(result.is_pass());
}

// ---------------------------------------------------------------------------
// Tests: new files not in baseline
// ---------------------------------------------------------------------------

#[test]
fn test_ratchet_new_file_in_actual_is_improvement() {
    let baseline_parsers = BTreeMap::from([("session".to_string(), 5_u64)]);
    let baseline = make_baseline("existing.log", baseline_parsers);

    // Actual contains the existing file plus a new file not in baseline.
    let actual = BTreeMap::from([
        (
            "existing.log".to_string(),
            make_actual_file(BTreeMap::from([("session".to_string(), 5_u64)])),
        ),
        (
            "new_session.log".to_string(),
            BaselineFile {
                total_entries: 200,
                parsers: BTreeMap::from([("gre".to_string(), 80_u64)]),
                event_types: BTreeMap::from([("Session".to_string(), 10_u64)]),
                unclaimed: 5,
                double_claims: 0,
                timestamp_failures: 2,
            },
        ),
    ]);

    let result = compare_against_baseline(&baseline, &actual);
    assert!(
        result.is_pass(),
        "new file should be an improvement, not a regression"
    );
    assert_eq!(result.improvements().len(), 2); // parser/gre + event_type/Session
    assert_eq!(result.regressions().len(), 0);

    // Verify diffs reference the new file.
    for diff in result.improvements() {
        assert_eq!(diff.filename, "new_session.log");
        assert_eq!(diff.baseline_value, 0);
    }
}

#[test]
fn test_ratchet_new_file_with_zero_counts_produces_no_diffs() {
    let baseline_parsers = BTreeMap::from([("session".to_string(), 5_u64)]);
    let baseline = make_baseline("existing.log", baseline_parsers);

    let actual = BTreeMap::from([
        (
            "existing.log".to_string(),
            make_actual_file(BTreeMap::from([("session".to_string(), 5_u64)])),
        ),
        (
            "empty.log".to_string(),
            BaselineFile {
                total_entries: 0,
                parsers: BTreeMap::from([("gre".to_string(), 0_u64)]),
                event_types: BTreeMap::new(),
                unclaimed: 0,
                double_claims: 0,
                timestamp_failures: 0,
            },
        ),
    ]);

    let result = compare_against_baseline(&baseline, &actual);
    assert!(
        result.diffs.is_empty(),
        "zero-count new file should produce no diffs"
    );
}

#[test]
fn test_ratchet_new_file_report_shows_improvement_markers() {
    let baseline = Baseline {
        meta: BaselineMeta {
            description: "test".to_string(),
            generated_from_commit: "abc1234".to_string(),
            corpus_tag: "test-v1".to_string(),
        },
        files: BTreeMap::new(), // empty baseline
    };

    let actual = BTreeMap::from([(
        "brand_new.log".to_string(),
        BaselineFile {
            total_entries: 100,
            parsers: BTreeMap::from([("session".to_string(), 5_u64)]),
            event_types: BTreeMap::from([("Session".to_string(), 5_u64)]),
            unclaimed: 10,
            double_claims: 0,
            timestamp_failures: 5,
        },
    )]);

    let result = compare_against_baseline(&baseline, &actual);
    let report = result.format_report();
    assert!(
        report.contains("[+]"),
        "report should show [+] for new file: {report}"
    );
    assert!(
        report.contains("brand_new.log"),
        "report should reference new filename: {report}"
    );
    assert!(
        report.contains("0 -> 5"),
        "report should show 0 -> actual for new file metrics: {report}"
    );
}

// ---------------------------------------------------------------------------
// Tests: report formatting
// ---------------------------------------------------------------------------

#[test]
fn test_ratchet_report_exact_match() {
    let parsers = BTreeMap::from([("session".to_string(), 5_u64)]);
    let baseline = make_baseline("test.log", parsers.clone());
    let actual = BTreeMap::from([("test.log".to_string(), make_actual_file(parsers))]);

    let result = compare_against_baseline(&baseline, &actual);
    let report = result.format_report();
    assert!(
        report.contains("all counts match baseline"),
        "report should indicate match: {report}"
    );
}

#[test]
fn test_ratchet_report_shows_improvements() {
    let baseline_parsers = BTreeMap::from([("gre".to_string(), 100_u64)]);
    let actual_parsers = BTreeMap::from([("gre".to_string(), 110_u64)]);
    let baseline = make_baseline("test.log", baseline_parsers);
    let actual = BTreeMap::from([("test.log".to_string(), make_actual_file(actual_parsers))]);

    let result = compare_against_baseline(&baseline, &actual);
    let report = result.format_report();
    assert!(
        report.contains("[+]"),
        "report should show improvement marker: {report}"
    );
    assert!(
        report.contains("100 -> 110"),
        "report should show value change: {report}"
    );
}

#[test]
fn test_ratchet_report_shows_regressions() {
    let baseline_parsers = BTreeMap::from([("gre".to_string(), 100_u64)]);
    let actual_parsers = BTreeMap::from([("gre".to_string(), 90_u64)]);
    let baseline = make_baseline("test.log", baseline_parsers);
    let actual = BTreeMap::from([("test.log".to_string(), make_actual_file(actual_parsers))]);

    let result = compare_against_baseline(&baseline, &actual);
    let report = result.format_report();
    assert!(
        report.contains("[-]"),
        "report should show regression marker: {report}"
    );
    assert!(
        report.contains("REGRESSION"),
        "report should label regressions: {report}"
    );
}

// ---------------------------------------------------------------------------
// Tests: multiple files
// ---------------------------------------------------------------------------

#[test]
fn test_ratchet_multiple_files_tracked_independently() {
    let baseline = Baseline {
        meta: BaselineMeta {
            description: "test".to_string(),
            generated_from_commit: "abc1234".to_string(),
            corpus_tag: "test-v1".to_string(),
        },
        files: BTreeMap::from([
            (
                "file_a.log".to_string(),
                BaselineFile {
                    total_entries: 100,
                    parsers: BTreeMap::from([("gre".to_string(), 50_u64)]),
                    event_types: BTreeMap::new(),
                    unclaimed: 10,
                    double_claims: 0,
                    timestamp_failures: 5,
                },
            ),
            (
                "file_b.log".to_string(),
                BaselineFile {
                    total_entries: 200,
                    parsers: BTreeMap::from([("gre".to_string(), 80_u64)]),
                    event_types: BTreeMap::new(),
                    unclaimed: 20,
                    double_claims: 0,
                    timestamp_failures: 10,
                },
            ),
        ]),
    };

    let actual = BTreeMap::from([
        (
            "file_a.log".to_string(),
            BaselineFile {
                total_entries: 100,
                parsers: BTreeMap::from([("gre".to_string(), 55_u64)]), // improvement
                event_types: BTreeMap::new(),
                unclaimed: 10,
                double_claims: 0,
                timestamp_failures: 5,
            },
        ),
        (
            "file_b.log".to_string(),
            BaselineFile {
                total_entries: 200,
                parsers: BTreeMap::from([("gre".to_string(), 70_u64)]), // regression
                event_types: BTreeMap::new(),
                unclaimed: 20,
                double_claims: 0,
                timestamp_failures: 10,
            },
        ),
    ]);

    let result = compare_against_baseline(&baseline, &actual);
    assert!(
        !result.is_pass(),
        "regression in file_b should cause failure"
    );
    assert_eq!(result.improvements().len(), 1);
    assert_eq!(result.regressions().len(), 1);

    // Verify the correct file is flagged for each.
    let improvement = result.improvements()[0];
    assert_eq!(improvement.filename, "file_a.log");

    let regression = result.regressions()[0];
    assert_eq!(regression.filename, "file_b.log");
}

// ---------------------------------------------------------------------------
// Tests: parser count going to zero is regression
// ---------------------------------------------------------------------------

#[test]
fn test_ratchet_parser_count_to_zero_is_regression() {
    let baseline_parsers = BTreeMap::from([("gre".to_string(), 100_u64)]);
    let actual_parsers = BTreeMap::from([("gre".to_string(), 0_u64)]);
    let baseline = make_baseline("test.log", baseline_parsers);
    let actual = BTreeMap::from([("test.log".to_string(), make_actual_file(actual_parsers))]);

    let result = compare_against_baseline(&baseline, &actual);
    assert!(!result.is_pass());
    assert_eq!(result.regressions().len(), 1);
    assert_eq!(result.regressions()[0].actual_value, 0);
}

// ---------------------------------------------------------------------------
// Tests: diff sorting
// ---------------------------------------------------------------------------

#[test]
fn test_ratchet_diffs_sorted_by_filename_then_metric() {
    let baseline = Baseline {
        meta: BaselineMeta {
            description: "test".to_string(),
            generated_from_commit: "abc1234".to_string(),
            corpus_tag: "test-v1".to_string(),
        },
        files: BTreeMap::from([
            (
                "b_file.log".to_string(),
                BaselineFile {
                    total_entries: 100,
                    parsers: BTreeMap::from([("gre".to_string(), 10_u64)]),
                    event_types: BTreeMap::new(),
                    unclaimed: 10,
                    double_claims: 0,
                    timestamp_failures: 5,
                },
            ),
            (
                "a_file.log".to_string(),
                BaselineFile {
                    total_entries: 100,
                    parsers: BTreeMap::from([("session".to_string(), 5_u64)]),
                    event_types: BTreeMap::new(),
                    unclaimed: 10,
                    double_claims: 0,
                    timestamp_failures: 5,
                },
            ),
        ]),
    };

    let actual = BTreeMap::from([
        (
            "b_file.log".to_string(),
            BaselineFile {
                total_entries: 100,
                parsers: BTreeMap::from([("gre".to_string(), 15_u64)]),
                event_types: BTreeMap::new(),
                unclaimed: 10,
                double_claims: 0,
                timestamp_failures: 5,
            },
        ),
        (
            "a_file.log".to_string(),
            BaselineFile {
                total_entries: 100,
                parsers: BTreeMap::from([("session".to_string(), 8_u64)]),
                event_types: BTreeMap::new(),
                unclaimed: 10,
                double_claims: 0,
                timestamp_failures: 5,
            },
        ),
    ]);

    let result = compare_against_baseline(&baseline, &actual);
    assert_eq!(result.diffs.len(), 2);
    assert_eq!(result.diffs[0].filename, "a_file.log");
    assert_eq!(result.diffs[1].filename, "b_file.log");
}
