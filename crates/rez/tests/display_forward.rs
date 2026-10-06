//! A display query through [`RezReader`] is reduced as the archive reader
//! computes it: its peak allocation stays well below that of evaluating the
//! query in full and reducing the result, which is what the trait's default
//! does.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use metriken_exposition::{Counter, Snapshot, SnapshotV2};
use metriken_query::{BufferPool, DisplayOptions, MetricsSource, QueryOptions, QueryResult};
use rez::reader::RezReader;
use rez::rez::RezRecorder;

/// The system allocator, tracking bytes held and their peak.
struct Counting;

static HELD: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let held = HELD.fetch_add(layout.size(), Relaxed) + layout.size();
            PEAK.fetch_max(held, Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        HELD.fetch_sub(layout.size(), Relaxed);
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Bytes allocated by `f` beyond what was held when it started, at most.
fn peak_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = HELD.load(Relaxed);
    PEAK.store(base, Relaxed);
    let out = f();
    (out, PEAK.load(Relaxed) - base)
}

const SERIES: u64 = 200;
const ROWS: u64 = 3000;

fn archive(path: &std::path::Path) {
    let labels: BTreeMap<String, String> = [("source".to_string(), "rezolus".to_string())]
        .into_iter()
        .collect();
    let mut r = RezRecorder::new(labels.clone(), labels, "rezolus".to_string());
    for i in 0..ROWS {
        let ts = 1_000_000_000 * (i + 1);
        let counters = (0..SERIES)
            .map(|id| {
                // A unique name per series; `metric` is the name queries use.
                Counter::new(
                    format!("cycles/{id}"),
                    i * (id + 1) * 7 + (i * 7919 * id) % 5,
                    [
                        ("metric".to_string(), "cycles".to_string()),
                        ("sampler".to_string(), "cpu".to_string()),
                        ("id".to_string(), id.to_string()),
                    ]
                    .into_iter()
                    .collect(),
                )
            })
            .collect();
        let snapshot = Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH + Duration::from_nanos(ts),
            duration: Duration::ZERO,
            metadata: HashMap::new(),
            counters,
            gauges: Vec::new(),
            histograms: Vec::new(),
        });
        r.ingest(&snapshot, ts);
    }
    r.finalize(path).unwrap();
}

#[test]
fn a_display_query_is_reduced_as_it_is_computed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("display.rez");
    archive(&path);
    let reader = RezReader::open_with_pool(&path, BufferPool::new(64 << 20)).unwrap();
    let (start, end) = reader.time_range().unwrap();
    let query = "irate(cycles[5s])";
    let opts = DisplayOptions {
        budget: 10,
        ..Default::default()
    };
    let qopts = QueryOptions::default();
    // Warm the reader so neither measurement pays for opening its tables.
    reader
        .query_range_opts(query, start, end, 1.0, &qopts)
        .unwrap();

    let (displayed, display_peak) = peak_of(|| {
        reader
            .query_range_display_opts(query, start, end, 1.0, &opts, &qopts)
            .unwrap()
    });
    let (full, full_peak) = peak_of(|| {
        let QueryResult::Matrix { result } = reader
            .query_range_opts(query, start, end, 1.0, &qopts)
            .unwrap()
        else {
            panic!("a matrix");
        };
        let reduced: Vec<usize> = result
            .iter()
            .map(|s| {
                opts.reducer
                    .reduce(
                        &s.values,
                        s.bands.as_deref(),
                        s.interpolated.as_deref(),
                        opts.budget,
                        opts.band,
                    )
                    .len()
            })
            .collect();
        Arc::new(reduced)
    });
    let metriken_query::DisplayResult::Series { result, .. } = displayed else {
        panic!("series");
    };
    assert_eq!(result.len(), full.len());
    eprintln!("display peak {display_peak} B, full evaluation peak {full_peak} B");
    assert!(
        display_peak * 2 < full_peak,
        "display peak {display_peak} B is not well below the full evaluation's {full_peak} B"
    );
}
