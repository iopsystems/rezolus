//! Bridge `metriken_exposition::Snapshot` → `LiveSource::append`.
//!
//! Each agent snapshot becomes one row in `_src`. This module owns the
//! column-name + label-metadata mapping that mirrors what the parquet
//! writer does, so a live `_src` is shaped identically to a parquet
//! `_src` for the same agent's data — that's what makes the cross-engine
//! parity test in `metriken-query-sql` meaningful in production.
//!
//! The conversion lives here (not in `metriken-query-sql`) so the SQL
//! crate stays free of a `metriken-exposition` dep.

use std::collections::BTreeMap;

use metriken_exposition::Snapshot;
use metriken_query_sql::{canonical_column_name, LiveColumn, LiveColumnKind, LiveValue, SqlError};

use super::live_capture::LiveCapture;

/// Metadata keys handled specially by `LiveColumn` construction —
/// `metric_type`, `unit`, `grouping_power`, `max_value_power` describe
/// shape rather than identity and are stripped from the labels map.
/// Mirrors `views::NON_VALUE_METADATA_KEYS` (less `metric`, which we
/// extract separately) so live `_cgroup_index` rows and parquet
/// `_cgroup_index` rows carry the same label keys for the same
/// underlying metric.
const SHAPE_METADATA_KEYS: &[&str] = &["metric_type", "unit", "grouping_power", "max_value_power"];

/// Apply a snapshot to the live capture: build column descriptors,
/// run `LiveSource::append` to grow `_src`, and update the capture's
/// `DashboardData` schema cache (`counter_metrics` / `gauge_metrics`
/// / `histogram_metrics`) so subsequent metadata queries reflect the
/// newly-seen metrics.
pub fn ingest_snapshot(live: &mut LiveCapture, snapshot: Snapshot) -> Result<(), SqlError> {
    let mut snap = snapshot;

    let timestamp_ns: u64 = snap
        .systemtime()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let duration_ns: Option<u64> = snap.duration().map(|d| d.as_nanos() as u64);

    let counters = snap.counters();
    let gauges = snap.gauges();
    let histograms = snap.histograms();

    // Histogram bucket arrays are borrowed by `LiveValue::Histogram(&[u64])`,
    // so their owners must outlive the `append` call.
    let hist_buckets: Vec<Vec<u64>> = histograms
        .iter()
        .map(|h| h.value.as_slice().to_vec())
        .collect();

    let mut columns: Vec<(LiveColumn, LiveValue<'_>)> =
        Vec::with_capacity(counters.len() + gauges.len() + histograms.len());

    for c in &counters {
        let col = make_column(&c.name, &c.metadata, LiveColumnKind::Counter);
        columns.push((col, LiveValue::Counter(c.value)));
    }
    for g in &gauges {
        let col = make_column(&g.name, &g.metadata, LiveColumnKind::Gauge);
        columns.push((col, LiveValue::Gauge(g.value)));
    }
    for (i, h) in histograms.iter().enumerate() {
        // Parquet convention: histogram column name is "<metric>:buckets".
        let physical = format!("{}:buckets", h.name);
        let grouping_power = h
            .metadata
            .get("grouping_power")
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or_else(|| h.value.config().grouping_power());
        let col = make_column_with_physical(
            &physical,
            &h.name,
            &h.metadata,
            LiveColumnKind::Histogram { grouping_power },
        );
        columns.push((col, LiveValue::Histogram(&hist_buckets[i])));
    }

    live.live().append(timestamp_ns, duration_ns, &columns)?;

    // Update the schema cache so `DashboardData::*_names` and
    // `*_label_count` reflect the newly-observed columns. Borrows
    // a `LiveColumn` slice without copying.
    let descriptors: Vec<LiveColumn> = columns.into_iter().map(|(c, _)| c).collect();
    live.observe_columns(&descriptors);
    Ok(())
}

/// Build a `LiveColumn` whose raw incoming name is the metric's `name`
/// field (counters and gauges).
fn make_column(
    name: &str,
    metadata: &std::collections::HashMap<String, String>,
    kind: LiveColumnKind,
) -> LiveColumn {
    make_column_with_physical(name, name, metadata, kind)
}

/// Build a `LiveColumn` from the agent's emitted shape.
///
/// **Canonical-name derivation.** Rezolus agents emit per-metric
/// metric NAMEs that may be numeric IDs (`"49"`, `"24x0"`) rather
/// than the canonical dashboard names (`"cpu_usage/user/0"`). The
/// parquet path applies `views::canonical_alias` at load time to
/// rebuild the canonical name from `metric + labels`. The live path
/// has to do the same so dashboard SQL targeting `_src` finds the
/// columns it expects — otherwise everything binds to numeric column
/// names that don't appear in any `COLUMNS('^cpu_usage…$')` regex.
fn make_column_with_physical(
    raw_physical: &str,
    name: &str,
    metadata: &std::collections::HashMap<String, String>,
    kind: LiveColumnKind,
) -> LiveColumn {
    // Canonical metric name: prefer the `metric` metadata key (mirrors
    // `views::classify`); fall back to the name with any `:buckets`
    // suffix stripped (histograms whose metadata omits `metric`).
    let metric = metadata
        .get("metric")
        .cloned()
        .unwrap_or_else(|| name.strip_suffix(":buckets").unwrap_or(name).to_string());
    // Strip shape keys + the `metric` key (it lives in its own slot).
    let labels: BTreeMap<String, String> = metadata
        .iter()
        .filter(|(k, _)| k.as_str() != "metric")
        .filter(|(k, _)| !SHAPE_METADATA_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    // Compute the canonical column name. For agents that already emit
    // canonical-shape names this is a passthrough.
    let physical = canonical_column_name(raw_physical, &metric, &labels, kind);
    LiveColumn {
        physical,
        metric,
        kind,
        labels,
    }
}

#[cfg(test)]
mod tests {
    //! L4-substitute: end-to-end through the Snapshot → LiveSource
    //! bridge. Catches:
    //!
    //! - Snapshot-format mismatches (the metric/labels metadata keys
    //!   we look for must match what the agent emits).
    //! - Round-trip failures from msgpack-shaped data through the
    //!   bridge to DuckDB SQL queries.
    //! - Histogram bucket extraction from `histogram::Histogram::as_slice`.
    //!
    //! What this does *not* cover (deferred follow-up): driving an
    //! actual `rezolus view http://stub` subprocess through Chromium
    //! to assert frontend rendering. The frontend is untouched by
    //! this work — the wire format (Prometheus matrix JSON) is
    //! identical between live and parquet paths — and the existing
    //! `scripts/viewer_chromium_smoke.sh` already exercises frontend
    //! rendering for the file mode. So the marginal value of
    //! chromium-against-live is low until the next change to the
    //! frontend shape.

    use super::*;
    use histogram::Histogram as RawHistogram;
    use metriken_exposition::{Counter, Gauge, Histogram, SnapshotV2};
    use std::collections::HashMap;
    use std::time::{Duration, UNIX_EPOCH};

    fn synthetic_snapshot(ts_secs: u64) -> Snapshot {
        // One counter, one gauge, one histogram. Same shape an agent
        // emits, including the metadata keys (`metric`, `metric_type`,
        // optional `grouping_power` etc.).
        let mut counter_meta = HashMap::new();
        counter_meta.insert("metric".to_string(), "cpu_usage".to_string());
        counter_meta.insert("metric_type".to_string(), "counter".to_string());
        counter_meta.insert("state".to_string(), "user".to_string());
        counter_meta.insert("id".to_string(), "0".to_string());
        let counter = Counter {
            name: "cpu_usage/user/0".to_string(),
            value: ts_secs * 100,
            metadata: counter_meta,
        };

        let mut gauge_meta = HashMap::new();
        gauge_meta.insert("metric".to_string(), "memory_total".to_string());
        gauge_meta.insert("metric_type".to_string(), "gauge".to_string());
        let gauge = Gauge {
            name: "memory_total".to_string(),
            value: 1_000_000,
            metadata: gauge_meta,
        };

        // A small H2 histogram: grouping_power=4, max_value_power=10
        // → manageable bucket count. Increment a few values to make
        // it non-trivial.
        let mut hist_value = RawHistogram::new(4, 10).expect("histogram");
        for v in [1, 2, 5, 10, 20, 50, 100, 200].iter() {
            hist_value.increment(*v).expect("increment");
        }
        let mut hist_meta = HashMap::new();
        hist_meta.insert("metric".to_string(), "request_latency".to_string());
        hist_meta.insert("metric_type".to_string(), "histogram".to_string());
        hist_meta.insert("grouping_power".to_string(), "4".to_string());
        hist_meta.insert("max_value_power".to_string(), "10".to_string());
        let histogram = Histogram {
            name: "request_latency".to_string(),
            value: hist_value,
            metadata: hist_meta,
        };

        Snapshot::V2(SnapshotV2 {
            systemtime: UNIX_EPOCH + Duration::from_secs(ts_secs),
            duration: Duration::from_millis(50),
            metadata: HashMap::new(),
            counters: vec![counter],
            gauges: vec![gauge],
            histograms: vec![histogram],
        })
    }

    fn fresh_capture() -> LiveCapture {
        let live = metriken_query_sql::LiveSource::new("rezolus", 1000).expect("LiveSource::new");
        LiveCapture::new(live, 1000, "rezolus", "test", "http://test")
    }

    #[test]
    fn snapshot_to_live_source_round_trip() {
        let mut cap = fresh_capture();
        for ts in [1u64, 2, 3] {
            ingest_snapshot(&mut cap, synthetic_snapshot(ts)).expect("ingest");
        }
        let live = cap.live().clone();

        // Query the data back.
        let batches = live
            .run_sql(
                "SELECT timestamp, \"cpu_usage/user/0\", memory_total, \
                        h2_total(\"request_latency:buckets\") AS hist_total \
                 FROM _src ORDER BY timestamp",
            )
            .expect("run_sql");
        let b = &batches[0];
        assert_eq!(b.num_rows(), 3, "three appended snapshots");

        use arrow::array::{Int64Array, UInt64Array};
        let ts = b
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("timestamp BIGINT");
        let cpu = b
            .column(1)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .expect("cpu UBIGINT");
        let mem = b
            .column(2)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("memory BIGINT");
        let hist = b
            .column(3)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .expect("h2_total UBIGINT");

        // Timestamps are nanoseconds since epoch; ingest_snapshot
        // converts via duration_since(UNIX_EPOCH).as_nanos(), then
        // snap to the 1s grid leaves them at 1e9, 2e9, 3e9.
        assert_eq!(ts.value(0), 1_000_000_000);
        assert_eq!(ts.value(1), 2_000_000_000);
        assert_eq!(ts.value(2), 3_000_000_000);

        // Counter follows ts_secs * 100.
        assert_eq!(cpu.value(0), 100);
        assert_eq!(cpu.value(1), 200);
        assert_eq!(cpu.value(2), 300);

        // Gauge is constant 1_000_000.
        assert_eq!(mem.value(0), 1_000_000);
        assert_eq!(mem.value(1), 1_000_000);
        assert_eq!(mem.value(2), 1_000_000);

        // Histogram had 8 increments — h2_total returns total bucket
        // count, which equals the number of increments.
        for i in 0..3 {
            assert_eq!(hist.value(i), 8, "row {i} h2_total");
        }
    }

    #[test]
    fn label_metadata_propagates_into_cgroup_index_for_cgroup_metrics() {
        // Confirm a snapshot whose counter is a `cgroup_*` metric
        // populates `_cgroup_index` correctly via the bridge. This
        // also pins that the `name` / `id` labels survive
        // `make_column`'s metadata filtering (they're not in
        // SHAPE_METADATA_KEYS so they should pass through).
        let mut cap = fresh_capture();

        let mut meta = HashMap::new();
        meta.insert("metric".to_string(), "cgroup_cpu_usage".to_string());
        meta.insert("metric_type".to_string(), "counter".to_string());
        meta.insert("name".to_string(), "system.slice/foo".to_string());
        meta.insert("id".to_string(), "1234".to_string());
        let counter = Counter {
            name: "cgroup_cpu_usage/foo".to_string(),
            value: 42,
            metadata: meta,
        };
        let snap = Snapshot::V2(SnapshotV2 {
            systemtime: UNIX_EPOCH + Duration::from_secs(1),
            duration: Duration::from_millis(50),
            metadata: HashMap::new(),
            counters: vec![counter],
            gauges: vec![],
            histograms: vec![],
        });
        ingest_snapshot(&mut cap, snap).expect("ingest");

        use arrow::array::StringArray;
        let batches = cap
            .live()
            .run_sql("SELECT metric, name, id FROM _cgroup_index")
            .expect("cgroup index");
        let b = &batches[0];
        assert_eq!(b.num_rows(), 1);
        let metric = b
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0);
        let name = b
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0);
        let id = b
            .column(2)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0);
        assert_eq!(metric, "cgroup_cpu_usage");
        assert_eq!(name, "system.slice/foo");
        assert_eq!(id, "1234");
    }
}
