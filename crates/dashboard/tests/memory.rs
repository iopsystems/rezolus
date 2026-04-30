//! Memory regression test for `metriken_query::Tsdb::load_from_bytes` —
//! the eager-decode path the WASM viewer uses on attach.
//!
//! Tracks peak heap allocations via a custom GlobalAlloc wrapper. RSS
//! itself is platform-noisy and doesn't shrink on dealloc; peak heap
//! is deterministic and matches what would be live in the WASM linear
//! memory at the high-water mark.
//!
//! Thresholds are set with headroom above what metriken-query 0.9.6
//! actually delivers; if a future version regresses we want this test
//! to fail on CI before users see OOMs in the WASM viewer.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TrackingAlloc;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for TrackingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding to the system allocator with the same layout.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let new = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(new, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarding to the system allocator with the same layout.
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: TrackingAlloc = TrackingAlloc;

fn reset_peak_to_current() {
    PEAK.store(CURRENT.load(Ordering::Relaxed), Ordering::Relaxed);
}

fn peak_since_reset(baseline: usize) -> usize {
    PEAK.load(Ordering::Relaxed).saturating_sub(baseline)
}

fn data_path(rel: &str) -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .join("../..")
        .join("site/viewer/data")
        .join(rel)
        .canonicalize()
        .unwrap_or_else(|e| panic!("test fixture {rel} unresolved: {e}"))
}

fn measure_load(path: &str) -> usize {
    let full = data_path(path);
    let bytes = std::fs::read(&full).unwrap_or_else(|e| panic!("read {full:?}: {e}"));

    // Reset the high-water mark so prior loads (or test-runner state)
    // don't pollute the measurement; use the current heap level as the
    // baseline so we measure only allocations attributable to this load.
    reset_peak_to_current();
    let baseline = CURRENT.load(Ordering::Relaxed);

    {
        let bytes_field = metriken_query::Bytes::from(bytes);
        let tsdb = metriken_query::Tsdb::load_from_bytes(bytes_field)
            .unwrap_or_else(|e| panic!("load {full:?}: {e}"));
        // Drop happens at end of block; peak captures the high-water mark.
        std::hint::black_box(tsdb);
    }

    let peak = peak_since_reset(baseline);
    eprintln!("{path}: peak heap = {} MiB", peak / (1 << 20));
    peak
}

/// Each entry is (relative path, peak-heap upper bound in bytes).
///
/// Budgets sit ~30–75% above the metriken-query 0.9.6 measurements so
/// allocator-fragmentation noise doesn't flake the test, while still
/// failing on a 0.9.5-shaped regression (where peaks were several
/// times larger).
///
/// Recorded measurements (release build, macOS, M-series):
///
/// | file                          | 0.9.5  | 0.9.6 | budget |
/// |-------------------------------|--------|-------|--------|
/// | demo.parquet                  |  88 MiB|  9 MiB | 16 MiB |
/// | cachecannon.parquet           | 239 MiB| 56 MiB | 80 MiB |
/// | disagg/sglang-nixl-16c.parquet| ~700+† |171 MiB |240 MiB |
///
/// † 0.9.5 nixl was OOM-prone on 4 GB WASM linear memory, which is
/// what motivated the upgrade in the first place.
const FIXTURES: &[(&str, usize)] = &[
    ("demo.parquet", 16 * 1024 * 1024),
    ("cachecannon.parquet", 80 * 1024 * 1024),
    ("disagg/sglang-nixl-16c.parquet", 240 * 1024 * 1024),
];

#[test]
fn metriken_query_load_stays_within_memory_budget() {
    for (path, budget) in FIXTURES {
        let used = measure_load(path);
        assert!(
            used <= *budget,
            "{path}: peak heap {} MiB exceeds budget {} MiB",
            used / (1 << 20),
            budget / (1 << 20),
        );
    }
}
