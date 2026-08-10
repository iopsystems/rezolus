//! The assessment data model — the structured, actionable conclusion an agent
//! (or fine-tuned model) emits over an `OverviewRecord`.
//!
//! The whole assessment is `Finding`s in three buckets (`overall`, `findings`,
//! `ruled_out`) — one type, three roles, uniform grounding and validation.

/// Schema version for `Assessment`. Bump on any change to the assessment shape.
pub const ASSESSMENT_SCHEMA_VERSION: u32 = 1;

use serde::{Deserialize, Serialize};

/// A reference from a `Finding` into an element of the `OverviewRecord` that
/// supports (or, in `ruled_out`, dismisses) the claim. Exactly one of the index
/// fields is normally set alongside `metric`; all optional so an evidence item
/// can point at a metric, an anomaly, a regime shift, or a correlation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Metric this evidence points at, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    /// Index into that metric's `anomalies`, if the evidence is an anomaly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anomaly_index: Option<usize>,
    /// Index into that metric's `regime_shifts`, if the evidence is a regime shift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regime_shift_index: Option<usize>,
    /// Index into the record's `correlations`, if the evidence is a correlation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_index: Option<usize>,
    /// One-line rationale tying this record element to the claim.
    pub rationale: String,
}

/// Categorical confidence. Must track the uncertainty signals: a finding whose
/// magnitude sits inside its acquisition-window band cannot be `High`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// The shared unit of an assessment: one grounded claim. Reused in every bucket.
///
/// NOTE: `Finding`'s field names share a JSON namespace with the sibling fields of any struct that `#[serde(flatten)]`s it (`Overall`, `TieredFinding`) — adding a field named `status`, `data_quality`, `priority`, or `kind` here would silently corrupt the wire format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// One-line claim.
    pub summary: String,
    pub confidence: Confidence,
    /// References into the overview record + rationale.
    pub evidence: Vec<EvidenceRef>,
    /// Concrete next step. Absent on `ruled_out` items (nothing to do).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_action: Option<String>,
}

/// Whole-recording judgment. `Inconclusive`/`NoLimitingFactor` live here rather
/// than as findings because they are judgments about the whole recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverallStatus {
    Actionable,
    Inconclusive,
    NoLimitingFactor,
}

/// Explicit fitness-for-conclusion statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataQuality {
    /// Subsystems absent from the recording (from the record's coverage map).
    pub coverage_gaps: Vec<String>,
    /// True if any finding was limited by acquisition-window uncertainty.
    pub uncertainty_limited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The headline conclusion for the whole recording: a `Finding` plus the
/// whole-recording status and data-quality statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overall {
    #[serde(flatten)]
    pub finding: Finding,
    pub status: OverallStatus,
    pub data_quality: DataQuality,
}

/// Priority tier for an itemized finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    High,
    Medium,
    Low,
}

/// What kind of itemized finding this is. Internally tagged on `type` (nested under the `kind` field, so the wire shape is `"kind": {"type": "Bottleneck", ...}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FindingKind {
    /// A resource/subsystem limiting the workload.
    Bottleneck {
        subsystem: String,
        mechanism: String,
        direction: String,
    },
    /// A data gap that must be filled to conclude. Specific and actionable.
    NeedsMetric {
        missing: String,
        would_resolve: String,
    },
}

/// An itemized, prioritized finding: a `Finding` plus its tier and kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TieredFinding {
    #[serde(flatten)]
    pub finding: Finding,
    pub priority: Priority,
    pub kind: FindingKind,
}

/// A complete assessment of one recording: `Finding`s in three buckets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assessment {
    /// Schema version for this assessment. Readers are expected to check this; enforcement lands with the Phase 3 front door.
    pub schema_version: u32,
    /// Headline conclusion for the whole recording.
    pub overall: Overall,
    /// Priority-tiered itemized findings.
    pub findings: Vec<TieredFinding>,
    /// Hypotheses actively dismissed; `evidence` is the dismissing signal.
    pub ruled_out: Vec<Finding>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_assessment() -> Assessment {
        Assessment {
            schema_version: ASSESSMENT_SCHEMA_VERSION,
            overall: Overall {
                finding: Finding {
                    summary: "Workload is scheduler-bound".to_string(),
                    confidence: Confidence::High,
                    evidence: vec![EvidenceRef {
                        metric: Some("scheduler_runqueue_latency".to_string()),
                        anomaly_index: None,
                        regime_shift_index: Some(0),
                        correlation_index: None,
                        rationale: "sustained runqueue-latency regime shift".to_string(),
                    }],
                    recommended_action: Some("reduce runnable-task oversubscription".to_string()),
                },
                status: OverallStatus::Actionable,
                data_quality: DataQuality {
                    coverage_gaps: vec!["blockio".to_string()],
                    uncertainty_limited: false,
                    note: None,
                },
            },
            findings: vec![TieredFinding {
                finding: Finding {
                    summary: "Runqueue latency p99 elevated".to_string(),
                    confidence: Confidence::High,
                    evidence: vec![EvidenceRef {
                        metric: None,
                        anomaly_index: None,
                        regime_shift_index: None,
                        correlation_index: Some(0),
                        rationale: "runqueue latency co-moves with cpu saturation".to_string(),
                    }],
                    recommended_action: Some("pin latency-sensitive threads".to_string()),
                },
                priority: Priority::High,
                kind: FindingKind::Bottleneck {
                    subsystem: "scheduler".to_string(),
                    mechanism: "runqueue_latency".to_string(),
                    direction: "tasks waiting to run".to_string(),
                },
            }],
            ruled_out: vec![Finding {
                summary: "Thermal throttling".to_string(),
                confidence: Confidence::High,
                evidence: vec![EvidenceRef {
                    metric: Some("cpu_frequency_ratio".to_string()),
                    anomaly_index: None,
                    regime_shift_index: None,
                    correlation_index: None,
                    rationale: "frequency ratio stable, no Allan noise transition".to_string(),
                }],
                recommended_action: None,
            }],
        }
    }

    #[test]
    fn assessment_wire_shape_is_pinned() {
        let v = serde_json::to_value(sample_assessment()).unwrap();
        let expected = serde_json::json!({
            "schema_version": 1,
            "overall": {
                "summary": "Workload is scheduler-bound",
                "confidence": "High",
                "evidence": [
                    {
                        "metric": "scheduler_runqueue_latency",
                        "regime_shift_index": 0,
                        "rationale": "sustained runqueue-latency regime shift"
                    }
                ],
                "recommended_action": "reduce runnable-task oversubscription",
                "status": "Actionable",
                "data_quality": {
                    "coverage_gaps": ["blockio"],
                    "uncertainty_limited": false
                }
            },
            "findings": [
                {
                    "summary": "Runqueue latency p99 elevated",
                    "confidence": "High",
                    "evidence": [
                        {
                            "correlation_index": 0,
                            "rationale": "runqueue latency co-moves with cpu saturation"
                        }
                    ],
                    "recommended_action": "pin latency-sensitive threads",
                    "priority": "High",
                    "kind": {
                        "type": "Bottleneck",
                        "subsystem": "scheduler",
                        "mechanism": "runqueue_latency",
                        "direction": "tasks waiting to run"
                    }
                }
            ],
            "ruled_out": [
                {
                    "summary": "Thermal throttling",
                    "confidence": "High",
                    "evidence": [
                        {
                            "metric": "cpu_frequency_ratio",
                            "rationale": "frequency ratio stable, no Allan noise transition"
                        }
                    ]
                }
            ]
        });
        assert_eq!(
            v,
            expected,
            "wire shape mismatch:\nactual:\n{}\n",
            serde_json::to_string_pretty(&v).unwrap()
        );
    }

    #[test]
    fn assessment_round_trips_through_json() {
        let a = sample_assessment();
        let json = serde_json::to_string(&a).unwrap();
        let back: Assessment = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn tiered_finding_wire_shape_nests_kind() {
        let a = sample_assessment();
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"kind\":{\"type\":\"Bottleneck\""));
        assert!(json.contains("\"subsystem\":\"scheduler\""));
    }

    #[test]
    fn needs_metric_kind_round_trips() {
        let f = TieredFinding {
            finding: Finding {
                summary: "Cannot assess block IO".to_string(),
                confidence: Confidence::Medium,
                evidence: vec![],
                recommended_action: Some("enable blockio latency sampler".to_string()),
            },
            priority: Priority::High,
            kind: FindingKind::NeedsMetric {
                missing: "blockio_latency".to_string(),
                would_resolve: "whether stalls are storage-bound".to_string(),
            },
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: TieredFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
        assert!(json.contains("\"kind\":{\"type\":\"NeedsMetric\""));
    }

    #[test]
    fn recommended_action_omitted_when_none() {
        let ruled = &sample_assessment().ruled_out[0];
        let json = serde_json::to_string(ruled).unwrap();
        assert!(!json.contains("recommended_action"));
    }
}
