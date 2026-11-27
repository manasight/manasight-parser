//! Memory bound for a long run of input with no `'\n'`.
//!
//! A damaged log can contain a long run of bytes with no line terminator
//! (for example a zero-filled gap). [`StreamingParserCore`] must not let the
//! pending partial line grow the heap without bound or re-copy it on every
//! chunk: on a memory-capped host (such as a WASM runtime) that aborts the
//! module.
//!
//! This test installs a counting global allocator, feeds a 64 MiB NUL run in
//! 1 MiB chunks followed by CRLF and a normal log fragment, and asserts that
//! the heap's peak during the run stays well under the run's length and that
//! parsing resumes after the run with exactly the fragment's own events.
//!
//! The file holds a single `#[test]` so no other test thread allocates while
//! the peak is being measured.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use manasight_parser::parse_whole_log;
use manasight_parser::wasm::StreamingParserCore;

/// Global allocator wrapper that tracks live and peak heap bytes.
struct CountingAlloc;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

impl CountingAlloc {
    fn record_alloc(size: usize) {
        let live = LIVE.fetch_add(size, Ordering::SeqCst) + size;
        PEAK.fetch_max(live, Ordering::SeqCst);
    }

    fn record_dealloc(size: usize) {
        LIVE.fetch_sub(size, Ordering::SeqCst);
    }
}

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged and only updates counters on the side, so the `GlobalAlloc`
// contract is upheld by `System`.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            Self::record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc_zeroed`'s contract.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            Self::record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged; the caller upholds `dealloc`'s contract.
        unsafe { System.dealloc(ptr, layout) };
        Self::record_dealloc(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `realloc`'s contract.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            // Count the new block before releasing the old one: a moving
            // realloc holds both while it copies.
            Self::record_alloc(new_size);
            Self::record_dealloc(layout.size());
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

const MIB: usize = 1024 * 1024;

/// Length of the newline-less run.
const RUN_BYTES: usize = 64 * MIB;

/// Size of each chunk fed to the parser.
const CHUNK_BYTES: usize = MIB;

/// A normal log fragment that produces events.
const FIXTURE: &str = include_str!("fixtures/course_deck_v2_multi.log");

#[test]
fn test_push_chunk_newline_less_run_bounded_heap_and_resumes() {
    let expected = parse_whole_log(FIXTURE);
    assert!(
        !expected.is_empty(),
        "fixture must produce events for the resume check to mean anything"
    );

    let chunk = "\0".repeat(CHUNK_BYTES);
    let mut core = StreamingParserCore::new();
    let mut events = Vec::new();

    let baseline = LIVE.load(Ordering::SeqCst);
    PEAK.store(baseline, Ordering::SeqCst);

    for _ in 0..RUN_BYTES / CHUNK_BYTES {
        events.extend(core.push_chunk(&chunk));
    }

    let peak_delta = PEAK.load(Ordering::SeqCst).saturating_sub(baseline);

    events.extend(core.push_chunk("\r\n"));
    events.extend(core.push_chunk(FIXTURE));
    events.extend(core.finish());

    assert!(
        peak_delta < RUN_BYTES / 2,
        "a {} MiB newline-less run used {} MiB of heap",
        RUN_BYTES / MIB,
        peak_delta / MIB,
    );
    assert_eq!(
        events, expected,
        "events after the run must equal the fixture's own events"
    );
}
