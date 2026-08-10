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
//! Known v1 limitations: `.rez` recordings carry no `version` metadata
//! (`agent_version: None`); a multi-recording `.rez` opened via the MCP
//! pool path exposes only its first recording's metadata.

pub mod context;
pub mod correlations;
pub mod features;
pub mod rankings;

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
            labels: BTreeMap::new(),
            tier,
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

/// Derive a metric's subsystem from its label sets (the agent stamps a
/// `sampler` label on every metric); falls back to `unattributed` when
/// absent. `extract()` inserts whatever this returns into
/// `present_subsystems`, fulfilling `build_coverage`'s documented caller
/// obligation that `unattributed` be added by the caller, not by
/// `build_coverage` itself.
fn subsystem_of(label_sets: &[BTreeMap<String, String>]) -> String {
    for labels in label_sets {
        if let Some(s) = labels.get("sampler") {
            return s.clone();
        }
    }
    "unattributed".to_string()
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
    analysis.stats = features::compute_stats(&result.values);
    analysis.noise = features::noise_summary(&result.allan_analysis);
    let constant = analysis.stats.max == analysis.stats.min;
    if !constant {
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

    // --- enumerate entries (exhaustive: every metric appears) ---
    let mut entries: Vec<(String, &'static str, String, String, bool)> = Vec::new();
    let mut present_subsystems = BTreeSet::new();
    for name in data.counter_names() {
        let subsystem = subsystem_of(&data.counter_labels(&name));
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
        let subsystem = subsystem_of(&data.gauge_labels(&name));
        present_subsystems.insert(subsystem.clone());
        entries.push((name.clone(), "gauge", gauge_query(&name), subsystem, false));
    }
    for name in data.histogram_names() {
        let subsystem = subsystem_of(&data.histogram_labels(&name));
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
        data.source(),
        data.version(),
        duration_s,
        data.interval(),
        data.metadata_get("systeminfo"),
        &present_subsystems,
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

    fn quiet(name: &str) -> MetricAnalysis {
        MetricAnalysis {
            name: name.to_string(),
            metric_type: "counter".to_string(),
            query: format!("sum(rate({name}[1m]))"),
            subsystem: "cpu_usage".to_string(),
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
