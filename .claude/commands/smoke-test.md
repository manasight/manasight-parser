# Multi-Level Smoke Test with Real Logs

Run all three smoke test levels and produce a comparison table.

## Arguments

- `$ARGUMENTS`: Path to directory containing `.log` files (default: `/home/timc/manasight-data/player-log-trial/raw/`)

## Instructions

### 1. Run All Three Smoke Test Levels

Run the full smoke test suite across three files:
- **Level 1 (parser-only)**: `tests/smoke_parsers.rs` -- calls individual `try_parse` functions
- **Level 2 (router)**: `tests/smoke_router.rs` -- feeds entries through `Router::route()`
- **Level 3 (stream)**: `tests/smoke_stream.rs` -- full async pipeline via `MtgaEventStream::start_once()`

```bash
MANASIGHT_TEST_LOGS="${ARGUMENTS:-/home/timc/manasight-data/player-log-trial/raw/}" cargo test smoke -- --nocapture 2>&1
```

Capture the full output.

### 2. Save Results

Save the test output to a timestamped results file:

```
tests/smoke-results/YYYY-MM-DD_HHmmss_<git-short-hash>.txt
```

Include a header block with metadata:

```
# Multi-Level Smoke Test Results
# Date: YYYY-MM-DD HH:MM:SS
# Branch: <current branch>
# Commit: <full commit hash>
# Commit message: <first line of commit message>
# Log directory: <path used>
# Log files: <count of .log files in directory>

<full test output>
```

Create `tests/smoke-results/` if it doesn't exist. Results are tracked in git (~5KB each) for cross-machine comparison.

### 3. Build Multi-Level Comparison Table

Parse the output from all three levels and build a comparison table. Extract from each level's output:

- **Level 1 (parser-only)**: Total claimed events from the `=== Smoke Test Report ===` section. Sum all parser claim counts (but note: inventory+collection overlap means the parser total may exceed the router/stream total by the overlap count).
- **Level 2 (router)**: `routed:` count from the `=== Router-Level Smoke Test Report ===` section. This is the canonical event count since the router handles short-circuit dispatch.
- **Level 3 (stream)**: Total events from the `=== Stream-Level Smoke Test Report ===` section.

Per-event-type breakdown should match between all three levels (modulo the known inventory/collection overlap at Level 1).

### 4. Compare with Previous Results

If prior results exist in `tests/smoke-results/`, compare the latest two files:

- **Per-level event counts**: flag any level whose count changed
- **Per-parser claim counts** (Level 1): flag any parser whose count changed
- **Unclaimed / double_claims / panics**: flag any change
- **New parsers**: note any parsers that appear in the new run but not the old
- **Pass/Fail status**: highlight if it changed

If no prior results exist, skip comparison and note "First run -- no baseline to compare."

### 5. Present Summary

```
## Multi-Level Smoke Test Results

**Status**: PASS / FAIL
**Branch**: <branch> @ <short hash>
**Files processed**: <count> files, <total entries> entries

### Multi-Level Comparison
| File | Level | Events | Delta |
|------|-------|--------|-------|
| Player.log | Parser-only | 8,432 | -- |
| Player.log | Router | 8,432 | 0 |
| Player.log | Stream | 8,432 | 0 |
| Player-prev.log | Parser-only | 4,100 | -- |
| Player-prev.log | Router | 4,100 | 0 |
| Player-prev.log | Stream | 4,100 | 0 |

### Per-Parser Claims (Level 1)
| Parser | Claims | Panics |
|--------|--------|--------|
| ... | ... | ... |

### Event Type Breakdown (all levels should match)
| Type | Parser-only | Router | Stream |
|------|-------------|--------|--------|
| ... | ... | ... | ... |

### Totals
- Claimed (parser-only): <n> / <total entries> (<percent>%)
- Routed (router): <n>
- Events (stream): <n>
- Unclaimed: <n>
- Double claims: <n>
- Timestamp failures (parser-only): <n>
- Timestamp failures (router): <n>

### Changes from Previous Run
<diff summary, or "First run -- no baseline">

Results saved to: <filepath>
```
