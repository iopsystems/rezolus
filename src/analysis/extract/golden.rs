//! End-to-end golden-record, determinism, and structural tests over
//! synthesized `.rez` recordings. The golden's expected document was
//! captured from a reviewed first run (stats depend on PromQL rate() edge
//! behavior and cannot be hand-authored); the invariant tests below hold
//! independently of the capture.
//!
//! NOTE: the coverage map pins the full `EXPECTED_SUBSYSTEMS` universe —
//! adding a sampler to that const will (correctly) break the golden test;
//! update the expected document's `subsystems_absent` when it does.

use std::collections::{BTreeMap, HashMap};
use std::time::SystemTime;

use metriken::Window;
use metriken_exposition::{Counter, Gauge, Snapshot, SnapshotV2};

use crate::analysis::extract::Provenance;
use crate::recorder::rez::RezRecorder;

const NS: u64 = 1_000_000_000;

fn labels(name: &str, sampler: &str) -> HashMap<String, String> {
    [
        ("metric".to_string(), name.to_string()),
        ("sampler".to_string(), sampler.to_string()),
    ]
    .into_iter()
    .collect()
}

/// As `labels`, plus the `grouping_power`/`max_value_power` metadata the
/// reader requires to reconstruct a histogram column (`ColKind::Histogram`
/// in metriken-query's parquet reader returns `None` — silently dropping
/// the column — without both keys parseable as `u8`).
fn histogram_labels(name: &str, sampler: &str) -> HashMap<String, String> {
    let mut m = labels(name, sampler);
    m.insert("grouping_power".to_string(), "7".to_string());
    m.insert("max_value_power".to_string(), "64".to_string());
    m
}

fn snap(ts: u64, counters: Vec<Counter>, gauges: Vec<Gauge>) -> Snapshot {
    snap_with_histograms(ts, counters, gauges, Vec::new())
}

fn snap_with_histograms(
    ts: u64,
    counters: Vec<Counter>,
    gauges: Vec<Gauge>,
    histograms: Vec<metriken_exposition::Histogram>,
) -> Snapshot {
    Snapshot::V2(SnapshotV2 {
        systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
        duration: std::time::Duration::ZERO,
        metadata: HashMap::new(),
        counters,
        gauges,
        histograms,
    })
}

/// 121 snapshots at exactly 1s cadence (120s duration, interval 1.0):
/// - `test_requests` (tcp_traffic): linear counter, +100/s -> constant rate
/// - `test_stepped` (scheduler_runqueue): +10/s until t=60, +1000/s after
/// - `test_depth` (scheduler_runqueue): constant gauge 5
fn build_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let mut recorder = RezRecorder::new(
        [
            ("source".to_string(), "rezolus".to_string()),
            ("sampling_interval_ms".to_string(), "1000".to_string()),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>(),
        [("source".to_string(), "rezolus".to_string())]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
        "rezolus".to_string(),
    );
    let mut stepped: u64 = 0;
    for i in 0..121u64 {
        let ts = NS * (i + 1);
        let window = Some(Window::new(ts - 50_000_000, ts));
        stepped += if i < 60 { 10 } else { 1000 };
        recorder.ingest(
            &snap(
                ts,
                vec![
                    Counter::new(
                        "test_requests".to_string(),
                        i * 100,
                        labels("test_requests", "tcp_traffic"),
                    )
                    .with_window(window),
                    Counter::new(
                        "test_stepped".to_string(),
                        stepped,
                        labels("test_stepped", "scheduler_runqueue"),
                    )
                    .with_window(window),
                ],
                vec![Gauge::new(
                    "test_depth".to_string(),
                    5,
                    labels("test_depth", "scheduler_runqueue"),
                )
                .with_window(window)],
            ),
            ts,
        );
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fixture.rez");
    recorder.finalize(&path).expect("finalize");
    (dir, path)
}

/// As `build_fixture`, but with a fourth metric: a histogram
/// (`test_latency`, subsystem `blockio_latency`) incrementing a handful of
/// values per snapshot so its quantile-over-time series varies. Kept
/// separate from `build_fixture` so the golden/determinism/invariant tests
/// above are unaffected if this attempt is ever pared back.
fn build_fixture_with_histogram() -> (tempfile::TempDir, std::path::PathBuf) {
    let mut recorder = RezRecorder::new(
        [
            ("source".to_string(), "rezolus".to_string()),
            ("sampling_interval_ms".to_string(), "1000".to_string()),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>(),
        [("source".to_string(), "rezolus".to_string())]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
        "rezolus".to_string(),
    );
    for i in 0..121u64 {
        let ts = NS * (i + 1);
        let window = Some(Window::new(ts - 50_000_000, ts));
        let mut h = histogram::Histogram::new(7, 64).expect("histogram");
        // A few increasing values per snapshot so the quantile series varies
        // over time rather than sitting flat.
        for v in [10 + i, 20 + i, 30 + i * 2] {
            h.increment(v).expect("increment");
        }
        recorder.ingest(
            &snap_with_histograms(
                ts,
                vec![],
                vec![],
                vec![metriken_exposition::Histogram::new(
                    "test_latency".to_string(),
                    h,
                    histogram_labels("test_latency", "blockio_latency"),
                )
                .with_window(window)],
            ),
            ts,
        );
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fixture_hist.rez");
    recorder.finalize(&path).expect("finalize");
    (dir, path)
}

/// A recording whose `source` is the endpoint name the USER chose, holding
/// metrics that carry no `sampler` label — the shape `record --endpoint
/// url,source=redis` produces for an agent older than 5.17.1, or any agent
/// metric that ships unlabeled.
///
/// Sampler attribution then has only the metric NAME to go on, which is
/// exactly the inference `Provenance` decides whether to trust.
fn build_unlabeled_fixture(source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let mut recorder = RezRecorder::new(
        [
            ("source".to_string(), source.to_string()),
            ("sampling_interval_ms".to_string(), "1000".to_string()),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>(),
        [("source".to_string(), source.to_string())]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
        source.to_string(),
    );
    for i in 0..121u64 {
        let ts = NS * (i + 1);
        let window = Some(Window::new(ts - 50_000_000, ts));
        recorder.ingest(
            &snap(
                ts,
                vec![Counter::new(
                    "cpu_cycles".to_string(),
                    i * 1_000_000,
                    [("metric".to_string(), "cpu_cycles".to_string())]
                        .into_iter()
                        .collect::<HashMap<_, _>>(),
                )
                .with_window(window)],
                vec![],
            ),
            ts,
        );
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("unlabeled.rez");
    recorder.finalize(&path).expect("finalize");
    (dir, path)
}

fn extract_fixture() -> crate::analysis::record::OverviewRecord {
    let (_dir, path) = build_fixture();
    let reader = crate::mcp::open_source(&path).expect("open");
    crate::analysis::extract::extract(reader.as_ref(), Provenance::FromMetadata).expect("extract")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::record::{AnalysisStatus, DetailTier, RECORD_SCHEMA_VERSION};

    /// A `.rez` arm named by the user must still get sampler inference.
    ///
    /// `extract` decided whether to trust metric names by reading the
    /// recording's `source` metadata, but for a recording inside a `.rez`
    /// that field is the endpoint name the CALLER chose (`--endpoint
    /// url,source=redis`), not a statement about who produced the data — a
    /// `.rez` can only be written from a rezolus/msgpack endpoint. So
    /// selecting such an arm silently attributed every unlabeled metric to
    /// `unattributed`, degrading a path that used to be refused outright.
    #[test]
    fn a_rez_arm_named_by_the_user_still_infers_samplers() {
        let (_dir, path) = build_unlabeled_fixture("redis");
        let reader = crate::mcp::open_source(&path).expect("open");

        let native = crate::analysis::extract::extract(
            reader.as_ref(),
            crate::analysis::extract::Provenance::RezolusAgent,
        )
        .expect("extract");
        assert_eq!(
            native.metrics[0].labels.get("sampler").map(String::as_str),
            Some("cpu_perf"),
            "a .rez is a rezolus recording whatever the arm is called"
        );

        // And the metadata-derived rule really does say otherwise here, so
        // the assertion above is testing the provenance and not the name
        // table: a foreign `source` still blocks inference for a file whose
        // provenance is unknown.
        let by_metadata = crate::analysis::extract::extract(
            reader.as_ref(),
            crate::analysis::extract::Provenance::FromMetadata,
        )
        .expect("extract");
        assert_eq!(
            by_metadata.metrics[0]
                .labels
                .get("sampler")
                .map(String::as_str),
            Some("unattributed")
        );
    }

    #[test]
    fn extraction_invariants_hold() {
        let record = extract_fixture();
        assert_eq!(record.schema_version, RECORD_SCHEMA_VERSION);
        assert_eq!(record.context.source, "rezolus");
        assert!((record.context.duration_s - 120.0).abs() < 1.0);
        assert_eq!(record.context.sampling_interval_s, 1.0);
        // exhaustive coverage: 2 counters + 1 gauge, sorted by name
        let names: Vec<&str> = record.metrics.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["test_depth", "test_requests", "test_stepped"]);
        // sampler labels stamped
        assert_eq!(
            record.metrics[0].labels.get("sampler").map(String::as_str),
            Some("scheduler_runqueue")
        );
        // coverage: both fixture samplers present, e.g. blockio_latency absent
        assert!(record
            .context
            .coverage
            .subsystems_present
            .contains(&"tcp_traffic".to_string()));
        assert!(record
            .context
            .coverage
            .subsystems_absent
            .contains(&"blockio_latency".to_string()));
        // selection accounting is consistent
        assert_eq!(
            record.selection.full_detail_count + record.selection.summary_count,
            record.metrics.len()
        );
    }

    #[test]
    fn stepped_counter_yields_structural_findings() {
        let record = extract_fixture();
        let stepped = record
            .metrics
            .iter()
            .find(|m| m.name == "test_stepped")
            .expect("stepped metric present");
        assert_eq!(stepped.status, AnalysisStatus::Analyzed);
        assert_eq!(stepped.tier, DetailTier::Full);
        // a 100x rate step must register as at least one regime shift or anomaly;
        // pin structure, never engine confidences/magnitudes (drift-prone)
        assert!(
            !stepped.regime_shifts.is_empty() || !stepped.anomalies.is_empty(),
            "100x step produced no findings"
        );
        if let Some(shift) = stepped.regime_shifts.first() {
            assert_eq!(shift.direction, "Increase");
        }
        // the promotion is audited
        assert!(record
            .selection
            .promotions
            .iter()
            .any(|p| p.metric == "test_stepped"));
    }

    #[test]
    fn linear_counter_is_constant_rate() {
        let record = extract_fixture();
        let linear = record
            .metrics
            .iter()
            .find(|m| m.name == "test_requests")
            .expect("linear metric present");
        // steady +100/s -> constant rate series -> findings suppressed
        assert_eq!(linear.status, AnalysisStatus::Constant);
        assert!(linear.anomalies.is_empty());
        assert!(linear.regime_shifts.is_empty());
    }

    #[test]
    fn extraction_is_deterministic_across_runs_and_thread_counts() {
        let (_dir, path) = build_fixture();
        let reader = crate::mcp::open_source(&path).expect("open");
        let a = serde_json::to_string(
            &crate::analysis::extract::extract(reader.as_ref(), Provenance::FromMetadata)
                .expect("run a"),
        )
        .expect("ser a");
        let b = serde_json::to_string(
            &crate::analysis::extract::extract(reader.as_ref(), Provenance::FromMetadata)
                .expect("run b"),
        )
        .expect("ser b");
        assert_eq!(a, b, "same reader, two runs, byte-identical");
        // vary rayon parallelism via scoped pools (the global pool is
        // process-wide; install() scopes the change to this closure)
        let one = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("pool1")
            .install(|| {
                serde_json::to_string(
                    &crate::analysis::extract::extract(reader.as_ref(), Provenance::FromMetadata)
                        .expect("run 1t"),
                )
                .expect("ser 1t")
            });
        let four = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .expect("pool4")
            .install(|| {
                serde_json::to_string(
                    &crate::analysis::extract::extract(reader.as_ref(), Provenance::FromMetadata)
                        .expect("run 4t"),
                )
                .expect("ser 4t")
            });
        assert_eq!(
            one, four,
            "1-thread vs 4-thread extraction must be byte-identical"
        );
        assert_eq!(a, one, "scoped-pool run matches global-pool run");
    }

    #[test]
    fn histogram_metric_yields_three_quantile_entries() {
        let (_dir, path) = build_fixture_with_histogram();
        let reader = crate::mcp::open_source(&path).expect("open");
        let record = crate::analysis::extract::extract(reader.as_ref(), Provenance::FromMetadata)
            .expect("extract");
        for suffix in [":p50", ":p90", ":p99"] {
            let name = format!("test_latency{suffix}");
            let m = record
                .metrics
                .iter()
                .find(|m| m.name == name)
                .unwrap_or_else(|| panic!("{name} present"));
            assert_eq!(m.metric_type, "histogram");
            assert!(m.stats.min.is_finite());
            assert!(m.stats.max.is_finite());
            assert!(m.stats.mean.is_finite());
            assert!(m.stats.last.is_finite());
            assert!(m.stats.p50.is_finite());
            assert!(m.stats.p99.is_finite());
        }
    }

    /// Re-run to re-capture the golden document: `cargo test
    /// analysis::extract::golden::tests::regenerate_golden -- --ignored --nocapture`,
    /// then review the printed JSON and paste it into `GOLDEN` below.
    #[test]
    #[ignore]
    fn regenerate_golden() {
        let record = extract_fixture();
        println!("{}", serde_json::to_string_pretty(&record).unwrap());
    }

    #[test]
    fn golden_record_matches_captured_expectation() {
        // A guard against a syntactically-broken const, independent of the
        // string comparison below.
        let _: serde_json::Value = serde_json::from_str(GOLDEN).expect("golden parses");
        let record = extract_fixture();
        // Text comparison, not `serde_json::Value` comparison: serde_json's
        // default (non-`float_roundtrip`) float parser is not guaranteed to
        // round-trip every decimal back to the exact same f64 bit pattern
        // (observed here — parsing the captured `band_to_signal_ratio` back
        // landed 1 ULP off from the freshly-computed value, failing a
        // `Value`-level `assert_eq!` despite both sides being "the same
        // number" to any practical precision). Comparing the pretty-printed
        // text both sides were produced with sidesteps that reparse entirely.
        let actual = serde_json::to_string_pretty(&record).expect("to_string_pretty");
        assert_eq!(
            actual, GOLDEN,
            "record drifted from the reviewed golden; if the change is intentional \
             (schema/extractor change), re-capture via regenerate_golden and re-review"
        );
    }

    /// Captured via `regenerate_golden` and reviewed. Numeric drift here
    /// means engine behavior changed — re-review the new capture before
    /// re-pasting; don't just make the test pass.
    const GOLDEN: &str = r#"{
  "schema_version": 2,
  "context": {
    "source": "rezolus",
    "duration_s": 120.0,
    "sampling_interval_s": 1.0,
    "coverage": {
      "subsystems_present": [
        "scheduler_runqueue",
        "tcp_traffic"
      ],
      "subsystems_absent": [
        "blockio_latency",
        "blockio_requests",
        "cpu_bandwidth",
        "cpu_branch",
        "cpu_cores",
        "cpu_dtlb",
        "cpu_frequency",
        "cpu_l3",
        "cpu_migrations",
        "cpu_perf",
        "cpu_tlb_flush",
        "cpu_usage",
        "drivehealth",
        "gpu_amd_pmu",
        "gpu_amd_smi",
        "gpu_apple",
        "gpu_nvidia",
        "memory_meminfo",
        "memory_vmstat",
        "network_ethtool",
        "network_interfaces",
        "network_traffic",
        "rezolus_rusage",
        "syscall_counts",
        "syscall_latency",
        "tcp_connect_latency",
        "tcp_packet_latency",
        "tcp_receive",
        "tcp_retransmit"
      ]
    }
  },
  "metrics": [
    {
      "name": "test_depth",
      "metric_type": "gauge",
      "labels": {
        "sampler": "scheduler_runqueue"
      },
      "tier": "Summary",
      "status": "Constant",
      "stats": {
        "min": 5.0,
        "max": 5.0,
        "mean": 5.0,
        "last": 5.0,
        "p50": 5.0,
        "p99": 5.0
      },
      "noise": {
        "noise_type": "Unknown"
      }
    },
    {
      "name": "test_requests",
      "metric_type": "counter",
      "labels": {
        "sampler": "tcp_traffic"
      },
      "tier": "Summary",
      "status": "Constant",
      "stats": {
        "min": 100.0,
        "max": 100.0,
        "mean": 100.0,
        "last": 100.0,
        "p50": 100.0,
        "p99": 100.0
      },
      "noise": {
        "noise_type": "Unknown"
      }
    },
    {
      "name": "test_stepped",
      "metric_type": "counter",
      "labels": {
        "sampler": "scheduler_runqueue"
      },
      "tier": "Full",
      "status": "Analyzed",
      "stats": {
        "min": 10.0,
        "max": 1000.0,
        "mean": 513.25,
        "last": 1000.0,
        "p50": 1000.0,
        "p99": 1000.0
      },
      "noise": {
        "noise_type": "FlickerPhase"
      },
      "anomalies": [
        {
          "timestamp": 47.0,
          "index": 45,
          "anomaly_type": "TrendChange",
          "severity": "High",
          "confidence": 0.95,
          "magnitude": 0.882026377777086
        },
        {
          "timestamp": 51.0,
          "index": 49,
          "anomaly_type": "TrendChange",
          "severity": "Low",
          "confidence": 0.7,
          "magnitude": 0.882026377777086
        },
        {
          "timestamp": 61.0,
          "index": 59,
          "anomaly_type": "TrendChange",
          "severity": "High",
          "confidence": 0.95,
          "magnitude": 0.05188390457512271
        },
        {
          "timestamp": 77.0,
          "index": 75,
          "anomaly_type": "TrendChange",
          "severity": "High",
          "confidence": 0.95,
          "magnitude": 0.6744907594765952
        },
        {
          "timestamp": 82.0,
          "index": 80,
          "anomaly_type": "TrendChange",
          "severity": "Low",
          "confidence": 0.7,
          "magnitude": 0.6744907594765952
        }
      ],
      "regime_shifts": [
        {
          "index": 45,
          "direction": "Increase",
          "before_mean": 10.0,
          "after_mean": 538.0,
          "mean_change_pct": 5280.0,
          "confidence": 0.875662013130736,
          "allan_significance": 16.159207901379325
        },
        {
          "index": 75,
          "direction": "Increase",
          "before_mean": 538.0,
          "after_mean": 1000.0,
          "mean_change_pct": 85.87360594795538,
          "confidence": 0.8537042614893939,
          "allan_significance": 14.139306913706909
        }
      ],
      "uncertainty": {
        "band_to_signal_ratio": 0.051973367762841484,
        "within_band": false
      }
    }
  ],
  "correlations": [],
  "rankings": {
    "cpu": [],
    "memory": [],
    "io": [],
    "network": []
  },
  "selection": {
    "full_detail_count": 1,
    "summary_count": 2,
    "promotions": [
      {
        "metric": "test_stepped",
        "reason": "anomalous"
      }
    ],
    "correlation_candidate_set": "all pairs among salient metrics (anomalous or regime-shifted) and top-consumer base metrics, excluding same-base-metric pairs, capped at 12 by salience",
    "total_pairs_tested": 0
  }
}"#;
}
