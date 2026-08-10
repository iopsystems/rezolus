//! The overview-record data model — a deterministic, versioned summary of a
//! recording's Rezolus-native features. This is the input half of a training
//! example. Fields are a *curated* subset of the internal analysis structs, not
//! the heavy raw analyses.
//!
//! # Invariants (enforced by Phase 2 extraction and validation, not by these types)
//!
//! - **All floats are finite.** serde_json serializes NaN/Infinity as `null`, which
//!   then fails to deserialize into `f64` — a non-finite value produces a record that
//!   writes successfully but can never be read back. Extraction must reject or clamp
//!   non-finite values before a record is persisted.
//! - **Deterministic ordering is two-layered.** These types guarantee same-value →
//!   same-bytes (BTreeMap label maps, declaration-order fields). Same-recording →
//!   same-bytes additionally requires the extractor to order `metrics`, coverage
//!   lists, `correlations`, and `promotions` deterministically.
//! - **Tier↔content coupling.** A `Summary`-tier metric must carry empty
//!   `anomalies`/`regime_shifts` and no `uncertainty`; the extractor upholds this and
//!   validation checks it.
//! - **Readers are lenient.** Unknown fields are ignored on deserialize (serde
//!   default) — a deliberate forward-compatibility choice across schema versions.

/// Schema version for `OverviewRecord`. Bump on any change to extraction logic
/// or record shape so stored examples stay attributable to an extractor version.
pub const RECORD_SCHEMA_VERSION: u32 = 1;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A deterministic, versioned summary of a recording's Rezolus-native features.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverviewRecord {
    pub schema_version: u32,
    pub context: Context,
    /// Every metric appears (exhaustive coverage); detail scales with salience.
    pub metrics: Vec<MetricFeatures>,
    /// Top-N cross-metric relationships from a documented candidate set.
    pub correlations: Vec<CorrelationFeature>,
    pub rankings: Rankings,
    pub selection: Selection,
}

/// Recording-level facts, including the load-bearing coverage map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Context {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    pub duration_s: f64,
    pub sampling_interval_s: f64,
    /// JSON hardware summary passthrough (from agent `systeminfo` metadata).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub systeminfo: Option<serde_json::Value>,
    pub coverage: Coverage,
}

/// Which subsystems are present vs absent — what lets an assessment say
/// `NeedsMetric` instead of hallucinating around a gap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    pub subsystems_present: Vec<String>,
    pub subsystems_absent: Vec<String>,
}

/// Detail level of a per-metric entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DetailTier {
    /// Quiet metric: identity + stats + noise class, no findings.
    Summary,
    /// Salient metric: additionally carries anomalies, regime shifts, uncertainty.
    Full,
}

/// Per-metric features. Exhaustive coverage, tiered detail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricFeatures {
    pub name: String,
    /// "counter" | "gauge" | "histogram".
    pub metric_type: String,
    /// Label set. `BTreeMap` for deterministic serialization order.
    pub labels: BTreeMap<String, String>,
    pub tier: DetailTier,
    pub stats: Stats,
    pub noise: NoiseSummary,
    /// Full tier only; empty (and omitted) for summary-tier metrics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anomalies: Vec<AnomalyFeature>,
    /// Full tier only; empty (and omitted) for summary-tier metrics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regime_shifts: Vec<RegimeShiftFeature>,
    /// Full tier only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<UncertaintySummary>,
}

/// Summary statistics for a metric series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub last: f64,
    pub p50: f64,
    pub p99: f64,
}

/// Curated noise classification: the noise type and the optimal-tau minimum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoiseSummary {
    /// `NoiseType` rendered as a string (e.g. "WhiteFrequency").
    pub noise_type: String,
    /// Tau (seconds) of the strongest deviation minimum, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimal_tau_s: Option<f64>,
}

/// A curated anomaly (mapped down from `mcp::anomaly_detection::Anomaly`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnomalyFeature {
    pub timestamp: f64,
    pub index: usize,
    /// `AnomalyType` rendered as a string.
    pub anomaly_type: String,
    /// `AnomalySeverity` rendered as a string.
    pub severity: String,
    pub confidence: f64,
    /// Deviation magnitude of the point (computed; `Anomaly` carries the raw value, not a magnitude).
    pub magnitude: f64,
}

/// A curated sustained regime shift (mapped down from `WindowChangePoint`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegimeShiftFeature {
    pub index: usize,
    /// "Increase" | "Decrease" — derived from the before/after means (`WindowChangePoint` carries no direction field).
    pub direction: String,
    pub before_mean: f64,
    pub after_mean: f64,
    /// Percent change of the mean, always positive (75.0 = 75%); the sign lives in
    /// `direction`. Mapped from `WindowChangePoint`'s absolute fraction ×100.
    pub mean_change_pct: f64,
    pub confidence: f64,
    /// How many times larger than expected variance (from Allan analysis).
    pub allan_significance: f64,
}

/// Acquisition-window uncertainty summary: is movement bigger than the error?
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UncertaintySummary {
    /// Ratio of acquisition-window band width to signal magnitude.
    pub band_to_signal_ratio: f64,
    /// True when observed movement sits within the measurement band.
    pub within_band: bool,
}

/// A curated cross-metric correlation (mapped from `DiscoveredCorrelation`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrelationFeature {
    pub metric1: String,
    pub metric2: String,
    pub max_r: f64,
    /// Measurement-uncertainty range of `max_r`, when inputs carry bands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r_band: Option<(f64, f64)>,
    /// Optimal lag in seconds (positive = metric2 lags metric1).
    pub optimal_lag_s: i64,
    /// "CrossSubsystem" | "SameSubsystem".
    pub relationship: String,
}

/// Top resource consumers per domain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rankings {
    pub cpu: Vec<Consumer>,
    pub memory: Vec<Consumer>,
    pub io: Vec<Consumer>,
    pub network: Vec<Consumer>,
}

/// A ranked resource consumer (mapped from `ResourceConsumer`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Consumer {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub avg_usage: f64,
    pub max_usage: f64,
    pub percent_of_total: f64,
}

/// Auditability: how much detail each metric got, and the correlation search space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    pub full_detail_count: usize,
    pub summary_count: usize,
    /// Why each full-tier metric was promoted.
    pub promotions: Vec<Promotion>,
    /// Human description of which pairs entered the top-N correlation search.
    pub correlation_candidate_set: String,
    pub total_pairs_tested: usize,
}

/// Why a metric was promoted to full detail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Promotion {
    pub metric: String,
    /// "anomalous" | "regime_shift" | "top_consumer" | "strong_correlation".
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn sample_record() -> OverviewRecord {
        let mut labels = BTreeMap::new();
        labels.insert("id".to_string(), "0".to_string());
        labels.insert("state".to_string(), "user".to_string());
        OverviewRecord {
            schema_version: RECORD_SCHEMA_VERSION,
            context: Context {
                source: "rezolus".to_string(),
                agent_version: Some("1.2.3".to_string()),
                duration_s: 120.0,
                sampling_interval_s: 1.0,
                systeminfo: Some(serde_json::json!({
                    "os": "linux",
                    "cores": 8,
                })),
                coverage: Coverage {
                    subsystems_present: vec!["cpu".to_string(), "scheduler".to_string()],
                    subsystems_absent: vec!["blockio".to_string()],
                },
            },
            metrics: vec![
                MetricFeatures {
                    name: "cpu_usage".to_string(),
                    metric_type: "counter".to_string(),
                    labels: labels.clone(),
                    tier: DetailTier::Full,
                    stats: Stats {
                        min: 0.0,
                        max: 1.0,
                        mean: 0.5,
                        last: 0.6,
                        p50: 0.5,
                        p99: 0.95,
                    },
                    noise: NoiseSummary {
                        noise_type: "WhiteFrequency".to_string(),
                        optimal_tau_s: Some(4.0),
                    },
                    anomalies: vec![AnomalyFeature {
                        timestamp: 42.0,
                        index: 42,
                        anomaly_type: "LevelShift".to_string(),
                        severity: "High".to_string(),
                        confidence: 0.9,
                        magnitude: 0.3,
                    }],
                    regime_shifts: vec![RegimeShiftFeature {
                        index: 40,
                        direction: "Increase".to_string(),
                        before_mean: 0.4,
                        after_mean: 0.7,
                        mean_change_pct: 75.0,
                        confidence: 0.95,
                        allan_significance: 3.2,
                    }],
                    uncertainty: Some(UncertaintySummary {
                        band_to_signal_ratio: 0.05,
                        within_band: false,
                    }),
                },
                MetricFeatures {
                    name: "network_bytes".to_string(),
                    metric_type: "counter".to_string(),
                    labels: BTreeMap::new(),
                    tier: DetailTier::Summary,
                    stats: Stats {
                        min: 0.0,
                        max: 10.0,
                        mean: 5.0,
                        last: 5.0,
                        p50: 5.0,
                        p99: 9.0,
                    },
                    noise: NoiseSummary {
                        noise_type: "WhitePhase".to_string(),
                        optimal_tau_s: None,
                    },
                    anomalies: vec![],
                    regime_shifts: vec![],
                    uncertainty: None,
                },
            ],
            correlations: vec![CorrelationFeature {
                metric1: "cpu_usage".to_string(),
                metric2: "scheduler_runqueue_latency".to_string(),
                max_r: 0.85,
                r_band: Some((0.80, 0.88)),
                optimal_lag_s: 2,
                relationship: "CrossSubsystem".to_string(),
            }],
            rankings: Rankings {
                cpu: vec![Consumer {
                    name: "cpu0".to_string(),
                    labels: labels.clone(),
                    avg_usage: 0.5,
                    max_usage: 1.0,
                    percent_of_total: 40.0,
                }],
                memory: vec![],
                io: vec![],
                network: vec![],
            },
            selection: Selection {
                full_detail_count: 1,
                summary_count: 1,
                promotions: vec![Promotion {
                    metric: "cpu_usage".to_string(),
                    reason: "anomalous".to_string(),
                }],
                correlation_candidate_set: "pairs where at least one side is anomalous".to_string(),
                total_pairs_tested: 3,
            },
        }
    }

    #[test]
    fn record_round_trips_through_json() {
        let r = sample_record();
        let json = serde_json::to_string(&r).unwrap();
        let back: OverviewRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn serialization_is_deterministic() {
        // Same record serialized twice is byte-identical, and label maps sort.
        let r = sample_record();
        let a = serde_json::to_string(&r).unwrap();
        let b = serde_json::to_string(&r).unwrap();
        assert_eq!(a, b);
        // BTreeMap orders keys: "id" before "state".
        let idx_id = a.find("\"id\"").unwrap();
        let idx_state = a.find("\"state\"").unwrap();
        assert!(
            idx_id < idx_state,
            "label keys must serialize in sorted order"
        );
    }

    #[test]
    fn summary_tier_metric_omits_empty_detail() {
        let r = sample_record();
        let summary_metric = serde_json::to_string(&r.metrics[1]).unwrap();
        assert!(!summary_metric.contains("anomalies"));
        assert!(!summary_metric.contains("regime_shifts"));
        assert!(!summary_metric.contains("uncertainty"));
    }

    #[test]
    fn record_wire_shape_is_pinned() {
        let v = serde_json::to_value(sample_record()).unwrap();
        let expected = serde_json::json!({
            "schema_version": 1,
            "context": {
                "source": "rezolus",
                "agent_version": "1.2.3",
                "duration_s": 120.0,
                "sampling_interval_s": 1.0,
                "systeminfo": {
                    "cores": 8,
                    "os": "linux"
                },
                "coverage": {
                    "subsystems_present": ["cpu", "scheduler"],
                    "subsystems_absent": ["blockio"]
                }
            },
            "metrics": [
                {
                    "name": "cpu_usage",
                    "metric_type": "counter",
                    "labels": {
                        "id": "0",
                        "state": "user"
                    },
                    "tier": "Full",
                    "stats": {
                        "min": 0.0,
                        "max": 1.0,
                        "mean": 0.5,
                        "last": 0.6,
                        "p50": 0.5,
                        "p99": 0.95
                    },
                    "noise": {
                        "noise_type": "WhiteFrequency",
                        "optimal_tau_s": 4.0
                    },
                    "anomalies": [
                        {
                            "timestamp": 42.0,
                            "index": 42,
                            "anomaly_type": "LevelShift",
                            "severity": "High",
                            "confidence": 0.9,
                            "magnitude": 0.3
                        }
                    ],
                    "regime_shifts": [
                        {
                            "index": 40,
                            "direction": "Increase",
                            "before_mean": 0.4,
                            "after_mean": 0.7,
                            "mean_change_pct": 75.0,
                            "confidence": 0.95,
                            "allan_significance": 3.2
                        }
                    ],
                    "uncertainty": {
                        "band_to_signal_ratio": 0.05,
                        "within_band": false
                    }
                },
                {
                    "name": "network_bytes",
                    "metric_type": "counter",
                    "labels": {},
                    "tier": "Summary",
                    "stats": {
                        "min": 0.0,
                        "max": 10.0,
                        "mean": 5.0,
                        "last": 5.0,
                        "p50": 5.0,
                        "p99": 9.0
                    },
                    "noise": {
                        "noise_type": "WhitePhase"
                    }
                }
            ],
            "correlations": [
                {
                    "metric1": "cpu_usage",
                    "metric2": "scheduler_runqueue_latency",
                    "max_r": 0.85,
                    "r_band": [0.8, 0.88],
                    "optimal_lag_s": 2,
                    "relationship": "CrossSubsystem"
                }
            ],
            "rankings": {
                "cpu": [
                    {
                        "name": "cpu0",
                        "labels": {
                            "id": "0",
                            "state": "user"
                        },
                        "avg_usage": 0.5,
                        "max_usage": 1.0,
                        "percent_of_total": 40.0
                    }
                ],
                "memory": [],
                "io": [],
                "network": []
            },
            "selection": {
                "full_detail_count": 1,
                "summary_count": 1,
                "promotions": [
                    {
                        "metric": "cpu_usage",
                        "reason": "anomalous"
                    }
                ],
                "correlation_candidate_set": "pairs where at least one side is anomalous",
                "total_pairs_tested": 3
            }
        });
        assert_eq!(v, expected);
    }
}
