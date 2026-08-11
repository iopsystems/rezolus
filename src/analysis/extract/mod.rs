//! Extraction: turn a recording into a deterministic [`OverviewRecord`].
//!
//! v1 emits one aggregated entry per metric *name* (counters as
//! `sum(rate(m[1m]))`, gauges as `sum(m)`, histograms as three quantile
//! entries `m:p50`/`m:p90`/`m:p99`), so emitted names are unique and
//! `EvidenceRef.metric` is unambiguous. Per-entry cost is one engine query,
//! plus a second uncertainty query for counters. Correlation evaluates up to
//! C(12,2) pairs, re-querying each candidate per pairing — a per-candidate
//! series cache is a Phase 3 item.
//!
//! v2 schema adds `MetricFeatures.status` (distinguishing query failures from
//! genuinely idle zeros) and per-entry `sampler` labels (subsystem attribution).
//! Labels now carry the sampler; names remain the unique identity.
//!
//! Known v1 limitations: `.rez` recordings carry no `version` metadata
//! (`agent_version: None`); a multi-recording `.rez` opened via the MCP
//! pool path exposes only its first recording's metadata.

pub mod context;
pub mod correlations;
pub mod features;
pub mod rankings;

#[cfg(test)]
mod golden;

use std::collections::{BTreeMap, BTreeSet};

use metriken_query::{MetricsSource, QueryResult};

use crate::analysis::record::{
    Context, CorrelationFeature, DetailTier, MetricFeatures, OverviewRecord, Promotion, Rankings,
    Selection, RECORD_SCHEMA_VERSION,
};
use crate::mcp::anomaly_detection::detect_anomalies;
use correlations::Candidate;

/// The recording's native cadence with the same degenerate-metadata guard
/// the anomaly engine applies: non-finite or non-positive intervals fall
/// back to 1s so all extraction passes agree on step.
pub(crate) fn guarded_step(data: &dyn MetricsSource) -> f64 {
    let interval = data.interval();
    if interval.is_finite() && interval > 0.0 {
        interval
    } else {
        1.0
    }
}

/// One analyzed metric entry, pre-tiering. The record name is unique by
/// construction (one entry per metric name; histogram quantiles suffixed
/// `:p50`/`:p90`/`:p99`).
#[derive(Debug, Clone)]
pub(crate) struct MetricAnalysis {
    pub name: String,
    pub metric_type: String,
    pub query: String,
    pub subsystem: String,
    pub status: crate::analysis::record::AnalysisStatus,
    pub stats: crate::analysis::record::Stats,
    pub noise: crate::analysis::record::NoiseSummary,
    pub anomalies: Vec<crate::analysis::record::AnomalyFeature>,
    pub regime_shifts: Vec<crate::analysis::record::RegimeShiftFeature>,
    pub uncertainty: Option<crate::analysis::record::UncertaintySummary>,
}

/// Pure assembly: tiering, ordering, selection accounting. Deterministic for
/// deterministic inputs; fully unit-tested (extract() is the thin IO shell).
pub(crate) fn assemble(
    context: Context,
    analyses: Vec<MetricAnalysis>,
    correlations: Vec<CorrelationFeature>,
    rankings: Rankings,
    ranked_metrics: BTreeSet<String>,
    total_pairs_tested: usize,
) -> OverviewRecord {
    let strong: BTreeSet<&str> = correlations
        .iter()
        .flat_map(|c| [c.metric1.as_str(), c.metric2.as_str()])
        .collect();
    let mut analyses = analyses;
    analyses.sort_by(|a, b| a.name.cmp(&b.name));

    let mut metrics = Vec::with_capacity(analyses.len());
    let mut promotions = Vec::new();
    for a in analyses {
        // Promotion reasons in precedence order; first match wins.
        let reason = if !a.anomalies.is_empty() {
            Some("anomalous")
        } else if !a.regime_shifts.is_empty() {
            Some("regime_shift")
        } else if ranked_metrics.contains(&a.name) {
            Some("top_consumer")
        } else if strong.contains(a.name.as_str()) {
            Some("strong_correlation")
        } else {
            None
        };
        let tier = if reason.is_some() {
            DetailTier::Full
        } else {
            DetailTier::Summary
        };
        if let Some(reason) = reason {
            promotions.push(Promotion {
                metric: a.name.clone(),
                reason: reason.to_string(),
            });
        }
        metrics.push(MetricFeatures {
            name: a.name,
            metric_type: a.metric_type,
            labels: BTreeMap::from([("sampler".to_string(), a.subsystem.clone())]),
            tier,
            status: a.status,
            stats: a.stats,
            noise: a.noise,
            anomalies: if tier == DetailTier::Full {
                a.anomalies
            } else {
                Vec::new()
            },
            regime_shifts: if tier == DetailTier::Full {
                a.regime_shifts
            } else {
                Vec::new()
            },
            // Full tier only, per the schema contract.
            uncertainty: if tier == DetailTier::Full {
                a.uncertainty
            } else {
                None
            },
        });
    }
    promotions.sort_by(|a, b| a.metric.cmp(&b.metric));
    let full_detail_count = metrics
        .iter()
        .filter(|m| m.tier == DetailTier::Full)
        .count();
    let summary_count = metrics.len() - full_detail_count;

    OverviewRecord {
        schema_version: RECORD_SCHEMA_VERSION,
        context,
        metrics,
        correlations,
        rankings,
        selection: Selection {
            full_detail_count,
            summary_count,
            promotions,
            correlation_candidate_set: correlations::CANDIDATE_SET_DESCRIPTION.to_string(),
            total_pairs_tested,
        },
    }
}

/// Finite-float backstop: serialize and reject any JSON null outside the
/// `systeminfo` passthrough. The schema omits every `None`, so a null can
/// only mean a NaN/Inf slipped through a guard.
pub(crate) fn validate_no_nulls(record: &OverviewRecord) -> Result<(), String> {
    let value = serde_json::to_value(record).map_err(|e| e.to_string())?;
    fn walk(v: &serde_json::Value, path: &mut Vec<String>, errs: &mut Vec<String>) {
        match v {
            serde_json::Value::Null => errs.push(format!("null at {}", path.join("."))),
            serde_json::Value::Object(map) => {
                for (k, child) in map {
                    if k == "systeminfo" {
                        continue; // free-form JSON passthrough may contain nulls
                    }
                    path.push(k.clone());
                    walk(child, path, errs);
                    path.pop();
                }
            }
            serde_json::Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    path.push(i.to_string());
                    walk(child, path, errs);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let mut errs = Vec::new();
    walk(&value, &mut Vec::new(), &mut errs);
    if errs.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "record contains {} null(s): {}",
            errs.len(),
            errs.join("; ")
        ))
    }
}

/// Derive a metric's subsystem. The `sampler` label (stamped by agents >=
/// 5.17.1) is always authoritative when present. Absent a label, *name
/// inference* — everything below — only runs when `infer` is true, which
/// callers must compute from the recording's `source` metadata being
/// exactly `"rezolus"` (see `extract()` and the module doc in
/// `context.rs`): a metric named `cpu_usage` in a Prometheus-scraped or
/// otherwise foreign recording proves nothing, since the name is just a
/// string some unrelated exporter happened to also use. When `infer` is
/// false and no label matched, the result is unconditionally
/// `unattributed`.
///
/// When inference is trusted, in order:
///
/// 1. [`context::AMBIGUOUS_METRICS`]: a name declared identically by more
///    than one sampler resolves to `unattributed` rather than guessing one
///    of its candidates (the name still proves *one of* them ran — see
///    `extract()`, which credits the candidate set to `uncertain_samplers`
///    instead of a domain).
/// 2. An exact lookup in [`context::METRIC_SAMPLERS`] — a static table,
///    harvested from the sampler `stats.rs` declarations, covering metric
///    names whose sampler can't be recovered from the name alone (e.g.
///    `cpu_cycles`, `tcp_bytes`, `cgroup_cpu_usage`).
/// 3. Name-prefix inference: the longest sampler name in
///    [`context::EXPECTED_SUBSYSTEMS`] that is a `_`-boundary prefix of the
///    metric name (an exact match, or `name.starts_with("{sampler}_")`).
///    Longest match wins so e.g. a name starting with `blockio_latency_`
///    prefers `blockio_latency` over a shorter unrelated match.
///
/// Falls back to `unattributed` when nothing disambiguates — a genuinely
/// foreign/future metric name, an ambiguous one, or inference not trusted
/// at all. `extract()` inserts whatever this returns into
/// `present_subsystems`, fulfilling `build_coverage`'s documented caller
/// obligation that `unattributed` be added by the caller, not by
/// `build_coverage` itself.
///
/// `pub(crate)` so `metric_samplers_match_agent_attribution`
/// (`src/agent/samplers/mod.rs`) can drive it directly with the real
/// analysis-side resolution, rather than duplicating tiers 1-3's logic.
pub(crate) fn subsystem_of(
    name: &str,
    label_sets: &[BTreeMap<String, String>],
    infer: bool,
) -> String {
    for labels in label_sets {
        if let Some(s) = labels.get("sampler") {
            return s.clone();
        }
    }
    if !infer {
        return "unattributed".to_string();
    }
    if context::AMBIGUOUS_METRICS.iter().any(|(n, _)| *n == name) {
        return "unattributed".to_string();
    }
    if let Some((_, sampler)) = context::METRIC_SAMPLERS.iter().find(|(n, _)| *n == name) {
        return sampler.to_string();
    }
    context::EXPECTED_SUBSYSTEMS
        .iter()
        .filter(|s| {
            name.strip_prefix(**s)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('_'))
        })
        .max_by_key(|sampler| sampler.len())
        .map(|sampler| sampler.to_string())
        .unwrap_or_else(|| "unattributed".to_string())
}

/// Query templates matching the MCP exhaustive mode.
fn counter_query(name: &str) -> String {
    format!("sum(rate({name}[1m]))")
}
fn gauge_query(name: &str) -> String {
    format!("sum({name})")
}
fn histogram_query(name: &str, quantile: &str) -> String {
    format!("histogram_quantile({quantile}, {name})")
}

/// Analyze one emitted entry. Never errors: a metric whose analysis fails
/// still appears (exhaustive coverage) as a quiet Summary-tier entry with
/// zero stats and Unknown noise. A constant series keeps its stats but
/// drops anomalies/shifts (MAD/CUSUM misfire on zero variance).
///
/// Note: `compute_stats` filters non-finite values but the anomaly engine
/// does not — a NaN-gapped series therefore has clean stats yet
/// NaN-poisoned means/MAD inside the engine's internals. v1 accepts this
/// divergence; revisit if NaN-gapped series appear in practice.
fn analyze_entry(
    data: &dyn MetricsSource,
    name: String,
    metric_type: &str,
    query: String,
    subsystem: String,
    want_uncertainty: bool,
) -> MetricAnalysis {
    let mut analysis = MetricAnalysis {
        name,
        metric_type: metric_type.to_string(),
        query: query.clone(),
        subsystem,
        status: crate::analysis::record::AnalysisStatus::NoData,
        stats: features::compute_stats(&[]),
        noise: crate::analysis::record::NoiseSummary {
            noise_type: "Unknown".to_string(),
            optimal_tau_s: None,
        },
        anomalies: Vec::new(),
        regime_shifts: Vec::new(),
        uncertainty: None,
    };
    let Ok(result) = detect_anomalies(data, &query) else {
        return analysis;
    };
    // The engine currently errors on empty results itself, so these two
    // guards are belt-and-suspenders: an empty or all-non-finite series
    // stays NoData rather than masquerading as a valid Constant entry.
    if result.values.is_empty() || result.values.iter().all(|v| !v.is_finite()) {
        return analysis;
    }
    analysis.stats = features::compute_stats(&result.values);
    analysis.noise = features::noise_summary(&result.allan_analysis);
    let constant = analysis.stats.max == analysis.stats.min;
    if constant {
        analysis.status = crate::analysis::record::AnalysisStatus::Constant;
    } else {
        analysis.status = crate::analysis::record::AnalysisStatus::Analyzed;
        analysis.anomalies = result
            .anomalies
            .iter()
            .map(|a| {
                features::anomaly_feature_from(
                    a.timestamp,
                    a.value,
                    a.index,
                    &format!("{:?}", a.anomaly_type),
                    &format!("{:?}", a.severity),
                    a.confidence,
                    result.mad_analysis.median,
                    result.mad_analysis.mad,
                )
            })
            .collect();
        analysis.regime_shifts =
            features::regime_shift_features(&result.cusum_analysis.window_change_points);
    }
    if want_uncertainty {
        // The engine result carries no acquisition intervals; one extra
        // query reads them. Aggregations that strip intervals -> None.
        if let (Some((start, end)), step) = (data.time_range(), guarded_step(data)) {
            if let Ok(QueryResult::Matrix { result }) = data.query_range(&query, start, end, step) {
                if result.len() == 1 {
                    let sample = &result[0];
                    let values: Vec<f64> = sample.values.iter().map(|(_, v)| *v).collect();
                    analysis.uncertainty =
                        features::uncertainty_summary(&values, sample.intervals.as_deref());
                }
            }
        }
    }
    analysis
}

/// Extract a deterministic overview record from a recording.
///
/// Errors only on structural problems (no time range, sub-10s recording —
/// the anomaly engine's floor — or the finite-float backstop tripping).
/// Per-metric analysis failures degrade to quiet entries instead.
pub fn extract(data: &dyn MetricsSource) -> Result<OverviewRecord, Box<dyn std::error::Error>> {
    let (start, end) = data
        .time_range()
        .ok_or("recording has no time range (empty?)")?;
    let duration_s = end - start;
    if duration_s < 10.0 {
        return Err(
            format!("recording too short for analysis: {duration_s:.1}s (minimum 10s)").into(),
        );
    }

    // Name inference (METRIC_SAMPLERS/prefix/ambiguity tiers) is trusted
    // only for genuine rezolus recordings (parquet and .rez, every agent
    // version) -- a metric coincidentally named e.g. `cpu_usage` in a
    // Prometheus-scraped or otherwise foreign recording proves nothing. A
    // missing/empty source is treated as unknown, i.e. conservative (no
    // inference). See the module doc in context.rs.
    let source = data.source();
    let infer = source == "rezolus";

    // --- enumerate entries (exhaustive: every metric appears) ---
    let mut entries: Vec<(String, &'static str, String, String, bool)> = Vec::new();
    let mut present_subsystems = BTreeSet::new();
    // Domains that still have at least one unattributed metric that isn't
    // covered by a tighter AMBIGUOUS_METRICS candidate set: their true
    // absence is unknowable, so build_coverage must exclude them from
    // subsystems_absent.
    let mut uncertain_domains = BTreeSet::new();
    // Samplers named directly as an AMBIGUOUS_METRICS candidate for some
    // unattributed metric: pruned from subsystems_absent individually
    // (tighter than uncertain_domains -- see AMBIGUOUS_METRICS's doc for
    // why, e.g. rezolus_bpf_run_count must not also prune the unrelated
    // rezolus_rusage sampler).
    let mut uncertain_samplers = BTreeSet::new();
    // Record that `name` resolved to "unattributed": credit its exact
    // AMBIGUOUS_METRICS candidate set when inference is trusted and the
    // name is one of them, else fall back to its whole domain.
    let mut note_unattributed = |name: &str| {
        if infer {
            if let Some((_, candidates)) =
                context::AMBIGUOUS_METRICS.iter().find(|(n, _)| *n == name)
            {
                uncertain_samplers.extend(candidates.iter().map(|s| s.to_string()));
                return;
            }
        }
        uncertain_domains.insert(context::domain_of(name).to_string());
    };
    for name in data.counter_names() {
        let subsystem = subsystem_of(&name, &data.counter_labels(&name), infer);
        if subsystem == "unattributed" {
            note_unattributed(&name);
        }
        present_subsystems.insert(subsystem.clone());
        entries.push((
            name.clone(),
            "counter",
            counter_query(&name),
            subsystem,
            true,
        ));
    }
    for name in data.gauge_names() {
        let subsystem = subsystem_of(&name, &data.gauge_labels(&name), infer);
        if subsystem == "unattributed" {
            note_unattributed(&name);
        }
        present_subsystems.insert(subsystem.clone());
        entries.push((name.clone(), "gauge", gauge_query(&name), subsystem, false));
    }
    for name in data.histogram_names() {
        let subsystem = subsystem_of(&name, &data.histogram_labels(&name), infer);
        if subsystem == "unattributed" {
            note_unattributed(&name);
        }
        present_subsystems.insert(subsystem.clone());
        // The `m:pNN` entries' stats describe the quantile-over-time series
        // (e.g. `p99` of `blockio_latency:p50` is the p99-across-time of the
        // p50 quantile), not a per-observation distribution.
        //
        // Correctness here leans on metriken-query merging multi-series
        // histograms inside histogram_quantile (one merged-distribution
        // quantile series). If that ever changed to per-series output, the
        // engine's extract_time_series would silently SUM per-series
        // quantiles — nonsense values with no error.
        for (suffix, q) in [(":p50", "0.50"), (":p90", "0.90"), (":p99", "0.99")] {
            entries.push((
                format!("{name}{suffix}"),
                "histogram",
                histogram_query(&name, q),
                subsystem.clone(),
                false,
            ));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // --- per-metric analysis (sequential; determinism over speed in v1) ---
    let mut analyses = Vec::with_capacity(entries.len());
    for (name, metric_type, query, subsystem, want_uncertainty) in entries {
        analyses.push(analyze_entry(
            data,
            name,
            metric_type,
            query,
            subsystem,
            want_uncertainty,
        ));
    }

    // --- rankings (must run before candidate construction: top-consumer
    // base metrics feed in as salience-0 correlation candidates below) ---
    let (rankings, ranked_metrics) = rankings::build_rankings(data);

    // --- correlation candidates: salient metrics, plus ranked top-consumer
    // base metrics not already present, as salience-0 candidates ---
    let mut candidates: Vec<Candidate> = analyses
        .iter()
        .filter(|a| !a.anomalies.is_empty() || !a.regime_shifts.is_empty())
        .map(|a| Candidate {
            name: a.name.clone(),
            query: a.query.clone(),
            subsystem: a.subsystem.clone(),
            salience: a.anomalies.len() + a.regime_shifts.len(),
        })
        .collect();
    let already_candidates: BTreeSet<String> = candidates.iter().map(|c| c.name.clone()).collect();
    for name in &ranked_metrics {
        if already_candidates.contains(name) {
            continue;
        }
        if let Some(a) = analyses.iter().find(|a| &a.name == name) {
            candidates.push(Candidate {
                name: a.name.clone(),
                query: a.query.clone(),
                subsystem: a.subsystem.clone(),
                salience: 0,
            });
        }
    }
    let (correlation_features, total_pairs_tested) = correlations::discover(data, &candidates);

    // --- context ---
    // Deliberately the RAW interval, not guarded_step(): this is a
    // provenance field describing the recording's metadata. A non-finite
    // interval would trip the no-null backstop below — a loud failure is
    // preferable to silently substituting 1s into a provenance field.
    // (All current MetricsSource impls return finite intervals.)
    let context = context::build_context(
        source,
        data.version(),
        duration_s,
        data.interval(),
        data.metadata_get("systeminfo"),
        &present_subsystems,
        &context::Uncertainty {
            domains: &uncertain_domains,
            samplers: &uncertain_samplers,
        },
    );

    let record = assemble(
        context,
        analyses,
        correlation_features,
        rankings,
        ranked_metrics,
        total_pairs_tested,
    );
    validate_no_nulls(&record)?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::record::*;
    use std::collections::BTreeSet;

    #[test]
    fn subsystem_of_prefers_the_sampler_label() {
        let labels = vec![BTreeMap::from([(
            "sampler".to_string(),
            "cpu_usage".to_string(),
        )])];
        // Name alone would infer nothing useful; the label wins regardless,
        // and regardless of `infer` too (a label is always authoritative).
        assert_eq!(
            subsystem_of("some_unrelated_name", &labels, true),
            "cpu_usage"
        );
        assert_eq!(
            subsystem_of("some_unrelated_name", &labels, false),
            "cpu_usage"
        );
    }

    #[test]
    fn subsystem_of_infers_from_name_prefix_when_unlabeled() {
        assert_eq!(
            subsystem_of("scheduler_runqueue_latency", &[], true),
            "scheduler_runqueue"
        );
        assert_eq!(subsystem_of("cpu_usage", &[], true), "cpu_usage");
        assert_eq!(
            subsystem_of("tcp_connect_latency", &[], true),
            "tcp_connect_latency"
        );
    }

    #[test]
    fn subsystem_of_longest_prefix_wins() {
        // blockio_latency is a real sampler name; a metric name extending it
        // with a further `_`-boundary suffix must still resolve to it.
        assert_eq!(
            subsystem_of("blockio_latency_p50", &[], true),
            "blockio_latency"
        );
    }

    #[test]
    fn subsystem_of_resolves_non_prefixing_names_via_metric_samplers_table() {
        // cpu_cycles comes from cpu_perf, but no sampler name is a
        // `_`-boundary prefix of "cpu_cycles" itself — resolved via the
        // METRIC_SAMPLERS table instead of falling to unattributed.
        assert_eq!(subsystem_of("cpu_cycles", &[], true), "cpu_perf");
        // tcp_bytes comes from tcp_traffic, same story.
        assert_eq!(subsystem_of("tcp_bytes", &[], true), "tcp_traffic");
        // memory_free is declared in memory/linux/meminfo/stats.rs.
        assert_eq!(subsystem_of("memory_free", &[], true), "memory_meminfo");
        // a cgroup_* metric: declared inside its owning sampler's own
        // module (there is no separate cgroup sampler).
        assert_eq!(subsystem_of("cgroup_syscall", &[], true), "syscall_counts");
    }

    #[test]
    fn subsystem_of_stays_unattributed_for_genuinely_ambiguous_or_foreign_names() {
        // gpu_memory is declared identically by both gpu_amd_smi and
        // gpu_nvidia; a flat table can't pick one, so it's excluded from
        // METRIC_SAMPLERS (see AMBIGUOUS_METRICS) and stays unattributed
        // absent a label, even with inference trusted.
        assert_eq!(subsystem_of("gpu_memory", &[], true), "unattributed");
        // A metric name with no relationship to any known sampler at all
        // (e.g. from a foreign/future source) stays unattributed too.
        assert_eq!(
            subsystem_of("totally_unknown_metric", &[], true),
            "unattributed"
        );
    }

    #[test]
    fn subsystem_of_cpu_cores_is_ambiguous_not_a_linux_exact_match() {
        // CRITICAL regression: "cpu_cores" exactly matches the Linux
        // cpu_cores sampler's own name via tier-3 prefix inference, but the
        // same name is also emitted by macOS's cpu_usage sampler. Before
        // AMBIGUOUS_METRICS covered it, an unlabeled macOS recording would
        // fabricate presence of the Linux-only cpu_cores sampler. It must
        // resolve to unattributed instead (and extract() credits exactly
        // {cpu_cores, cpu_usage} to uncertain_samplers -- see context.rs
        // tests).
        assert_eq!(subsystem_of("cpu_cores", &[], true), "unattributed");
    }

    #[test]
    fn subsystem_of_precedence_label_beats_table_beats_prefix() {
        // Table would resolve cpu_cycles -> cpu_perf, but an explicit label
        // wins regardless of what the name implies.
        let labeled = vec![BTreeMap::from([(
            "sampler".to_string(),
            "some_override".to_string(),
        )])];
        assert_eq!(subsystem_of("cpu_cycles", &labeled, true), "some_override");

        // No label: table lookup wins over prefix inference. "tcp_bytes"
        // would not prefix-match any EXPECTED_SUBSYSTEMS entry, so without
        // the table it would fall to unattributed; the table resolves it.
        assert_eq!(subsystem_of("tcp_bytes", &[], true), "tcp_traffic");

        // No label, no table entry: prefix inference is the last resort.
        assert_eq!(subsystem_of("cpu_usage", &[], true), "cpu_usage");
    }

    #[test]
    fn subsystem_of_infer_false_blocks_table_and_prefix_tiers() {
        // CRITICAL regression: without a `sampler` label, and with
        // inference untrusted (a non-rezolus source), a metric coincidentally
        // named `cpu_usage`/`memory_free`/`tcp_bytes` must NOT fabricate
        // subsystem presence -- neither the METRIC_SAMPLERS table nor
        // name-prefix inference may run.
        assert_eq!(subsystem_of("cpu_usage", &[], false), "unattributed");
        assert_eq!(subsystem_of("memory_free", &[], false), "unattributed");
        assert_eq!(subsystem_of("tcp_bytes", &[], false), "unattributed");
        assert_eq!(subsystem_of("cpu_cycles", &[], false), "unattributed");

        // Same names resolve normally when inference IS trusted -- proves
        // the difference is solely the `infer` flag, not the names.
        assert_eq!(subsystem_of("cpu_usage", &[], true), "cpu_usage");
        assert_eq!(subsystem_of("memory_free", &[], true), "memory_meminfo");
        assert_eq!(subsystem_of("tcp_bytes", &[], true), "tcp_traffic");
        assert_eq!(subsystem_of("cpu_cycles", &[], true), "cpu_perf");
    }

    #[test]
    fn subsystem_of_label_wins_even_when_infer_is_false() {
        // A `sampler` label is agent-stamped ground truth, independent of
        // whether the *recording's* source is trusted for name inference.
        let labels = vec![BTreeMap::from([(
            "sampler".to_string(),
            "cpu_usage".to_string(),
        )])];
        assert_eq!(subsystem_of("cpu_usage", &labels, false), "cpu_usage");
    }

    fn quiet(name: &str) -> MetricAnalysis {
        MetricAnalysis {
            name: name.to_string(),
            metric_type: "counter".to_string(),
            query: format!("sum(rate({name}[1m]))"),
            subsystem: "cpu_usage".to_string(),
            status: crate::analysis::record::AnalysisStatus::Analyzed,
            stats: Stats {
                min: 0.0,
                max: 1.0,
                mean: 0.5,
                last: 0.5,
                p50: 0.5,
                p99: 0.9,
            },
            noise: NoiseSummary {
                noise_type: "WhitePhase".to_string(),
                optimal_tau_s: None,
            },
            anomalies: vec![],
            regime_shifts: vec![],
            uncertainty: None,
        }
    }

    fn salient(name: &str) -> MetricAnalysis {
        let mut m = quiet(name);
        m.anomalies.push(AnomalyFeature {
            timestamp: 1.0,
            index: 1,
            anomaly_type: "PointOutlier".to_string(),
            severity: "High".to_string(),
            confidence: 0.9,
            magnitude: 3.0,
        });
        m
    }

    fn ctx() -> Context {
        Context {
            source: "rezolus".to_string(),
            agent_version: None,
            duration_s: 60.0,
            sampling_interval_s: 1.0,
            systeminfo: None,
            coverage: Coverage {
                subsystems_present: vec![],
                subsystems_absent: vec![],
            },
        }
    }

    fn empty_rankings() -> Rankings {
        Rankings {
            cpu: vec![],
            memory: vec![],
            io: vec![],
            network: vec![],
        }
    }

    #[test]
    fn assemble_sorts_metrics_and_tiers_by_salience() {
        let record = assemble(
            ctx(),
            vec![quiet("zzz"), salient("aaa"), quiet("mmm")],
            vec![],
            empty_rankings(),
            BTreeSet::new(),
            0,
        );
        let names: Vec<&str> = record.metrics.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["aaa", "mmm", "zzz"]);
        assert_eq!(record.metrics[0].tier, DetailTier::Full);
        assert_eq!(record.metrics[1].tier, DetailTier::Summary);
        assert_eq!(record.selection.full_detail_count, 1);
        assert_eq!(record.selection.summary_count, 2);
        assert_eq!(record.selection.promotions.len(), 1);
        assert_eq!(record.selection.promotions[0].metric, "aaa");
        assert_eq!(record.selection.promotions[0].reason, "anomalous");
        assert_eq!(record.schema_version, RECORD_SCHEMA_VERSION);
        assert_eq!(
            record.metrics[0].labels.get("sampler").map(String::as_str),
            Some("cpu_usage")
        );
        assert_eq!(
            record.metrics[0].status,
            crate::analysis::record::AnalysisStatus::Analyzed
        );
    }

    #[test]
    fn summary_tier_strips_uncertainty() {
        let mut m = quiet("aaa");
        m.uncertainty = Some(UncertaintySummary {
            band_to_signal_ratio: 0.5,
            within_band: false,
        });
        let record = assemble(ctx(), vec![m], vec![], empty_rankings(), BTreeSet::new(), 0);
        assert_eq!(record.metrics[0].tier, DetailTier::Summary);
        assert!(record.metrics[0].uncertainty.is_none());
    }

    #[test]
    fn promotion_reasons_ranked() {
        // top_consumer promotion for a ranked base metric
        let mut ranked = BTreeSet::new();
        ranked.insert("mmm".to_string());
        // strong_correlation promotion via a kept correlation
        let corr = vec![CorrelationFeature {
            metric1: "zzz".to_string(),
            metric2: "aaa".to_string(),
            max_r: 0.9,
            r_band: None,
            optimal_lag_s: 0,
            relationship: "CrossSubsystem".to_string(),
        }];
        let record = assemble(
            ctx(),
            vec![quiet("zzz"), salient("aaa"), quiet("mmm")],
            corr,
            empty_rankings(),
            ranked,
            3,
        );
        // aaa: anomalous outranks strong_correlation; mmm: top_consumer; zzz: strong_correlation
        let reasons: Vec<(&str, &str)> = record
            .selection
            .promotions
            .iter()
            .map(|p| (p.metric.as_str(), p.reason.as_str()))
            .collect();
        assert_eq!(
            reasons,
            vec![
                ("aaa", "anomalous"),
                ("mmm", "top_consumer"),
                ("zzz", "strong_correlation")
            ]
        );
        assert_eq!(record.selection.total_pairs_tested, 3);
        assert_eq!(record.selection.full_detail_count, 3);
    }

    #[test]
    fn no_null_backstop_catches_nan() {
        let mut m = quiet("aaa");
        m.stats.mean = f64::NAN;
        let record = assemble(ctx(), vec![m], vec![], empty_rankings(), BTreeSet::new(), 0);
        let err = validate_no_nulls(&record).unwrap_err();
        assert!(err.contains("null"), "should report the NaN-as-null: {err}");
    }

    #[test]
    fn no_null_backstop_allows_systeminfo_nulls() {
        let mut c = ctx();
        c.systeminfo = Some(serde_json::json!({"gpu": null}));
        let record = assemble(
            c,
            vec![quiet("aaa")],
            vec![],
            empty_rankings(),
            BTreeSet::new(),
            0,
        );
        assert!(validate_no_nulls(&record).is_ok());
    }
}
