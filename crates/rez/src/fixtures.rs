//! Archives of several recordings of one run, for the viewers' tests of the
//! combined view: the server viewer's (in the binary crate) and the WASM
//! viewer's (`crates/viewer`), which build the same archive so they can be
//! compared.

use std::collections::HashMap;
use std::time::SystemTime;

use metriken_exposition::{Counter, Gauge, Snapshot, SnapshotV2};

use crate::rez_v3_writer::{ManifestSeed, RezArchive, StreamRecorderV3};
use crate::window::Window;

/// The first row's time, in ns since the epoch.
pub const ANCHOR: u64 = 1_700_000_000_000_000_000;

/// How many one-second rows each recording holds.
pub const TICKS: u64 = 10;

/// One recording of [`one_run`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Recording<'a> {
    pub labels: &'a [(&'a str, &'a str)],
    pub metadata: &'a [(&'a str, &'a str)],
    /// How many CPUs the recording's `cpu_usage` has (`id` 0 to n - 1),
    /// with `cpu_cores` the count. None when 0.
    pub cpus: u64,
    /// Each CPU's busy nanoseconds per second, so `sum(irate(cpu_usage[5m]))
    /// / cpu_cores / 1e9` is `busy_ns / 1e9`.
    pub busy_ns: u64,
    /// A counter of this name rising `rate` per second, for a service.
    pub counter: Option<(&'a str, u64)>,
}

fn meta(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// A finalized v3 `.rez` at `path` holding `recordings`, each written over
/// the same [`TICKS`] seconds from [`ANCHOR`]: one run.
pub fn one_run(path: &std::path::Path, recordings: &[Recording<'_>]) {
    let mut archive = RezArchive::create(path).unwrap();
    let mut recs: Vec<StreamRecorderV3> = recordings
        .iter()
        .map(|r| {
            let seed = ManifestSeed {
                labels: r
                    .labels
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                metadata: r
                    .metadata
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                clock_anchor_wall_ns: ANCHOR,
            };
            StreamRecorderV3::new(archive.add_recording(seed).unwrap())
        })
        .collect();
    for (rec, r) in recs.iter_mut().zip(recordings) {
        for t in 0..TICKS {
            let ts = ANCHOR + t * 1_000_000_000;
            let window = Some(Window::new(ts - 1_000_000, ts));
            let mut counters: Vec<Counter> = (0..r.cpus)
                .map(|id| {
                    Counter::new(
                        format!("cpu_usage/user/{id}"),
                        r.busy_ns * t,
                        meta(&[
                            ("metric", "cpu_usage"),
                            ("sampler", "cpu_usage"),
                            ("id", &id.to_string()),
                            ("state", "user"),
                        ]),
                    )
                    .with_window(window)
                })
                .collect();
            let mut gauges: Vec<Gauge> = Vec::new();
            if r.cpus > 0 {
                gauges.push(
                    Gauge::new(
                        "cpu_cores".to_string(),
                        r.cpus as i64,
                        meta(&[("metric", "cpu_cores"), ("sampler", "cpu_usage")]),
                    )
                    .with_window(window),
                );
            }
            if let Some((name, rate)) = r.counter {
                counters.push(
                    Counter::new(
                        name.to_string(),
                        rate * t,
                        meta(&[("metric", name), ("sampler", "service")]),
                    )
                    .with_window(window),
                );
            }
            let snapshot = Snapshot::V2(SnapshotV2 {
                systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
                duration: std::time::Duration::ZERO,
                metadata: HashMap::new(),
                counters,
                gauges,
                histograms: Vec::new(),
            });
            rec.ingest(&snapshot, ts, 0).unwrap();
        }
    }
    for rec in recs {
        rec.finalize((ANCHOR + TICKS * 1_000_000_000, 0)).unwrap();
    }
    archive.join().unwrap();
}
