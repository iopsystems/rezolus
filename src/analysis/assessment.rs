//! The assessment data model — the structured, actionable conclusion an agent
//! (or fine-tuned model) emits over an `OverviewRecord`.
//!
//! The whole assessment is `Finding`s in three buckets (`overall`, `findings`,
//! `ruled_out`) — one type, three roles, uniform grounding and validation.

/// Schema version for `Assessment`. Bump on any change to the assessment shape.
pub const ASSESSMENT_SCHEMA_VERSION: u32 = 1;

use serde::{Deserialize, Serialize};

use crate::analysis::record::OverviewRecord;

/// A reference from a `Finding` into an element of the `OverviewRecord` that
/// supports (or, in `ruled_out`, dismisses) the claim. Exactly one of the index
/// fields is normally set alongside `metric`; all optional so an evidence item
/// can point at a metric, an anomaly, a regime shift, or a correlation.
///
/// Metric identity (v1, resolved): extraction emits one aggregated entry
/// per metric name (histogram quantiles suffixed `:p50`/`:p90`/`:p99`), so
/// `metric` names are unique within a record. Revisit if per-series
/// features ever land.
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

impl EvidenceRef {
    /// True when this evidence points at something checkable in a record.
    fn has_pointer(&self) -> bool {
        self.metric.is_some() || self.correlation_index.is_some()
    }

    /// Resolve this reference against a record, or say precisely what failed.
    fn resolve_in(&self, record: &OverviewRecord, role: &str) -> Result<(), String> {
        if (self.anomaly_index.is_some() || self.regime_shift_index.is_some())
            && self.metric.is_none()
        {
            return Err(format!("{role}: evidence has an index but no metric"));
        }
        if let Some(name) = &self.metric {
            let Some(m) = record.metrics.iter().find(|m| &m.name == name) else {
                return Err(format!("{role}: evidence cites unknown metric {name}"));
            };
            if let Some(i) = self.anomaly_index {
                if i >= m.anomalies.len() {
                    return Err(format!(
                        "{role}: anomaly_index {i} out of range for {name} ({} anomalies)",
                        m.anomalies.len()
                    ));
                }
            }
            if let Some(i) = self.regime_shift_index {
                if i >= m.regime_shifts.len() {
                    return Err(format!(
                        "{role}: regime_shift_index {i} out of range for {name} ({} shifts)",
                        m.regime_shifts.len()
                    ));
                }
            }
        }
        if let Some(i) = self.correlation_index {
            if i >= record.correlations.len() {
                return Err(format!(
                    "{role}: correlation_index {i} out of range ({} correlations)",
                    record.correlations.len()
                ));
            }
        }
        Ok(())
    }
}

/// Categorical confidence. Must track the uncertainty signals: a finding whose
/// magnitude sits inside its acquisition-window band cannot be `High`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

impl Finding {
    /// Structural invariants a single finding must satisfy, regardless of role.
    fn validate_self(&self, role: &str) -> Result<(), String> {
        if self.summary.trim().is_empty() {
            return Err(format!("{role} has an empty summary"));
        }
        // A High-confidence claim must be grounded in evidence.
        if self.confidence == Confidence::High && self.evidence.is_empty() {
            return Err(format!("{role} is High confidence but cites no evidence"));
        }
        // A High-confidence claim must cite at least one evidence item with a
        // resolvable pointer — rationale-only evidence (all pointer fields
        // None) cannot ground High.
        if self.confidence == Confidence::High
            && !self.evidence.iter().any(EvidenceRef::has_pointer)
        {
            return Err(format!(
                "{role} is High confidence but no evidence item has a resolvable pointer"
            ));
        }
        Ok(())
    }

    /// Cross-record checks: every evidence ref resolves, and a High-confidence
    /// claim may not rest on a metric whose movement sits inside its
    /// acquisition-window band.
    fn validate_against_record(&self, record: &OverviewRecord, role: &str) -> Result<(), String> {
        for e in &self.evidence {
            e.resolve_in(record, role)?;
        }
        if self.confidence >= Confidence::High {
            for e in &self.evidence {
                if let Some(name) = &e.metric {
                    if let Some(m) = record.metrics.iter().find(|m| &m.name == name) {
                        if m.uncertainty.as_ref().is_some_and(|u| u.within_band) {
                            return Err(format!(
                                "{role}: High confidence cites {name}, whose movement is within its measurement band"
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl TieredFinding {
    /// Structural invariants specific to the itemized-finding shape.
    fn validate_self(&self, role: &str) -> Result<(), String> {
        self.finding.validate_self(role)?;
        if let FindingKind::NeedsMetric { missing, .. } = &self.kind {
            if missing.trim().is_empty() {
                return Err(format!("{role} is NeedsMetric but `missing` is empty"));
            }
        }
        Ok(())
    }
}

impl Assessment {
    /// Structural validation that needs no `OverviewRecord`. Cross-record checks
    /// (evidence indices resolving, uncertainty forcing confidence down) are
    /// applied by `validate_against`.
    pub fn validate(&self) -> Result<(), String> {
        self.overall.finding.validate_self("overall finding")?;
        for (i, f) in self.findings.iter().enumerate() {
            f.validate_self(&format!("findings[{i}]"))?;
        }
        for (i, f) in self.ruled_out.iter().enumerate() {
            f.validate_self(&format!("ruled_out[{i}]"))?;
        }
        Ok(())
    }

    /// Full validation against the record this assessment claims to describe:
    /// structural invariants plus evidence resolution and the uncertainty cap.
    pub fn validate_against(&self, record: &OverviewRecord) -> Result<(), String> {
        self.validate()?;
        self.overall
            .finding
            .validate_against_record(record, "overall finding")?;
        for (i, f) in self.findings.iter().enumerate() {
            f.finding
                .validate_against_record(record, &format!("findings[{i}]"))?;
        }
        for (i, f) in self.ruled_out.iter().enumerate() {
            f.validate_against_record(record, &format!("ruled_out[{i}]"))?;
        }
        Ok(())
    }
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
            findings: vec![
                TieredFinding {
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
                },
                TieredFinding {
                    finding: Finding {
                        summary: "Cannot rule out storage stalls".to_string(),
                        confidence: Confidence::Medium,
                        evidence: vec![],
                        recommended_action: Some("enable blockio latency sampler".to_string()),
                    },
                    priority: Priority::Medium,
                    kind: FindingKind::NeedsMetric {
                        missing: "blockio_latency".to_string(),
                        would_resolve: "whether stalls are storage-bound".to_string(),
                    },
                },
            ],
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
                },
                {
                    "summary": "Cannot rule out storage stalls",
                    "confidence": "Medium",
                    "evidence": [],
                    "recommended_action": "enable blockio latency sampler",
                    "priority": "Medium",
                    "kind": {
                        "type": "NeedsMetric",
                        "missing": "blockio_latency",
                        "would_resolve": "whether stalls are storage-bound"
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

    #[test]
    fn valid_assessment_passes_validation() {
        assert!(sample_assessment().validate().is_ok());
    }

    #[test]
    fn high_confidence_without_evidence_is_rejected() {
        let mut a = sample_assessment();
        a.findings[0].finding.evidence.clear();
        a.findings[0].finding.confidence = Confidence::High;
        let err = a.validate().unwrap_err();
        assert!(
            err.contains("High"),
            "error should mention the High-confidence rule: {err}"
        );
        assert!(
            err.contains("findings[0]"),
            "error should name the finding: {err}"
        );
    }

    #[test]
    fn ruled_out_high_without_evidence_is_rejected() {
        let mut a = sample_assessment();
        a.ruled_out[0].evidence.clear();
        let err = a.validate().unwrap_err();
        assert!(
            err.contains("ruled_out[0]"),
            "error should name the ruled_out item: {err}"
        );
    }

    #[test]
    fn whitespace_summary_is_rejected_before_evidence_rule() {
        let mut a = sample_assessment();
        // Violate both rules at once: summary check must win.
        a.overall.finding.summary = "   ".to_string();
        a.overall.finding.evidence.clear();
        let err = a.validate().unwrap_err();
        assert!(
            err.contains("summary"),
            "summary rule should fire first: {err}"
        );
    }

    #[test]
    fn confidence_is_ordered() {
        assert!(Confidence::Low < Confidence::Medium);
        assert!(Confidence::Medium < Confidence::High);
    }

    use crate::analysis::record::{
        AnalysisStatus, AnomalyFeature, Context, CorrelationFeature, Coverage, DetailTier,
        MetricFeatures, NoiseSummary, OverviewRecord, Rankings, Selection, Stats,
        UncertaintySummary, RECORD_SCHEMA_VERSION,
    };

    fn record_with(
        metrics: Vec<MetricFeatures>,
        correlations: Vec<CorrelationFeature>,
    ) -> OverviewRecord {
        OverviewRecord {
            schema_version: RECORD_SCHEMA_VERSION,
            context: Context {
                source: "rezolus".to_string(),
                agent_version: None,
                duration_s: 60.0,
                sampling_interval_s: 1.0,
                systeminfo: None,
                coverage: Coverage {
                    subsystems_present: vec![],
                    subsystems_absent: vec![],
                },
            },
            metrics,
            correlations,
            rankings: Rankings {
                cpu: vec![],
                memory: vec![],
                io: vec![],
                network: vec![],
            },
            selection: Selection {
                full_detail_count: 0,
                summary_count: 0,
                promotions: vec![],
                correlation_candidate_set: String::new(),
                total_pairs_tested: 0,
            },
        }
    }

    fn metric(name: &str, anomaly_count: usize, within_band: Option<bool>) -> MetricFeatures {
        MetricFeatures {
            name: name.to_string(),
            metric_type: "counter".to_string(),
            labels: std::collections::BTreeMap::new(),
            tier: DetailTier::Full,
            status: AnalysisStatus::Analyzed,
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
            anomalies: (0..anomaly_count)
                .map(|i| AnomalyFeature {
                    timestamp: i as f64,
                    index: i,
                    anomaly_type: "PointOutlier".to_string(),
                    severity: "High".to_string(),
                    confidence: 0.9,
                    magnitude: 3.0,
                })
                .collect(),
            regime_shifts: vec![],
            uncertainty: within_band.map(|w| UncertaintySummary {
                band_to_signal_ratio: if w { 2.0 } else { 0.1 },
                within_band: w,
            }),
        }
    }

    #[test]
    fn valid_assessment_resolves_against_record() {
        let record = record_with(vec![metric("cpu_usage", 1, Some(false))], vec![]);
        let mut a = sample_assessment();
        // point every evidence ref at the one real metric/anomaly
        a.overall.finding.evidence = vec![EvidenceRef {
            metric: Some("cpu_usage".to_string()),
            anomaly_index: Some(0),
            regime_shift_index: None,
            correlation_index: None,
            rationale: "r".to_string(),
        }];
        a.findings[0].finding.evidence = a.overall.finding.evidence.clone();
        a.ruled_out[0].evidence = a.overall.finding.evidence.clone();
        assert!(a.validate_against(&record).is_ok());
    }

    #[test]
    fn unknown_metric_is_rejected() {
        let record = record_with(vec![metric("cpu_usage", 1, Some(false))], vec![]);
        let mut a = sample_assessment();
        a.overall.finding.evidence[0].metric = Some("nonexistent".to_string());
        let err = a.validate_against(&record).unwrap_err();
        assert!(err.contains("nonexistent"), "{err}");
    }

    #[test]
    fn out_of_range_anomaly_index_is_rejected() {
        let record = record_with(vec![metric("cpu_usage", 1, Some(false))], vec![]);
        let mut a = sample_assessment();
        a.overall.finding.evidence = vec![EvidenceRef {
            metric: Some("cpu_usage".to_string()),
            anomaly_index: Some(5),
            regime_shift_index: None,
            correlation_index: None,
            rationale: "r".to_string(),
        }];
        assert!(a.validate_against(&record).is_err());
    }

    #[test]
    fn index_without_metric_is_rejected() {
        let record = record_with(vec![metric("cpu_usage", 1, Some(false))], vec![]);
        let mut a = sample_assessment();
        a.overall.finding.evidence[0] = EvidenceRef {
            metric: None,
            anomaly_index: Some(0),
            regime_shift_index: None,
            correlation_index: None,
            rationale: "r".to_string(),
        };
        assert!(a.validate_against(&record).is_err());
    }

    #[test]
    fn correlation_index_resolves_against_record() {
        let record = record_with(vec![metric("cpu_usage", 1, Some(false))], vec![]);
        let mut a = sample_assessment();
        let anchored = EvidenceRef {
            metric: Some("cpu_usage".to_string()),
            anomaly_index: Some(0),
            regime_shift_index: None,
            correlation_index: None,
            rationale: "r".to_string(),
        };
        a.overall.finding.evidence = vec![anchored.clone()];
        a.ruled_out[0].evidence = vec![anchored];
        // findings[0] still cites correlation_index 0; the record has none
        let err = a.validate_against(&record).unwrap_err();
        assert!(err.contains("correlation_index"), "{err}");
    }

    #[test]
    fn medium_confidence_may_cite_within_band_metric() {
        let record = record_with(vec![metric("cpu_usage", 1, Some(true))], vec![]);
        let mut a = sample_assessment();
        let anchored = EvidenceRef {
            metric: Some("cpu_usage".to_string()),
            anomaly_index: Some(0),
            regime_shift_index: None,
            correlation_index: None,
            rationale: "r".to_string(),
        };
        a.overall.finding.confidence = Confidence::Medium;
        a.overall.finding.evidence = vec![anchored.clone()];
        a.findings[0].finding.confidence = Confidence::Medium;
        a.findings[0].finding.evidence = vec![anchored.clone()];
        a.ruled_out[0].confidence = Confidence::Medium;
        a.ruled_out[0].evidence = vec![anchored];
        assert!(a.validate_against(&record).is_ok());
    }

    #[test]
    fn within_band_metric_caps_confidence() {
        let record = record_with(vec![metric("cpu_usage", 1, Some(true))], vec![]);
        let mut a = sample_assessment();
        a.overall.finding.evidence = vec![EvidenceRef {
            metric: Some("cpu_usage".to_string()),
            anomaly_index: Some(0),
            regime_shift_index: None,
            correlation_index: None,
            rationale: "r".to_string(),
        }];
        a.findings.clear();
        a.ruled_out.clear();
        // overall is High and cites a within-band metric -> rejected
        let err = a.validate_against(&record).unwrap_err();
        assert!(err.contains("within"), "{err}");
    }

    #[test]
    fn high_confidence_requires_a_resolvable_pointer() {
        let mut a = sample_assessment();
        // rationale-only evidence (all pointers None) cannot ground High
        a.overall.finding.evidence = vec![EvidenceRef {
            metric: None,
            anomaly_index: None,
            regime_shift_index: None,
            correlation_index: None,
            rationale: "vibes".to_string(),
        }];
        let err = a.validate().unwrap_err();
        assert!(err.contains("pointer"), "{err}");
    }

    #[test]
    fn empty_needs_metric_is_rejected() {
        let mut a = sample_assessment();
        if let FindingKind::NeedsMetric { missing, .. } = &mut a.findings[1].kind {
            *missing = String::new();
        }
        let err = a.validate().unwrap_err();
        assert!(err.contains("missing"), "{err}");
    }
}
