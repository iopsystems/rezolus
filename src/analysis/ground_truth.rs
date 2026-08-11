//! Ground truth for label-harness recordings: the machine-checkable claim of
//! what bottleneck an experiment induced, and where. A recording enters the
//! labeled corpus only if [`GroundTruth::verify`] passes against its
//! extracted [`OverviewRecord`]. Unlike the assessment validators, `verify`
//! accumulates ALL failures — the runbook triages them in one pass.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::analysis::record::{DetailTier, MetricFeatures, OverviewRecord};

/// Schema version for `GroundTruth`. Bump on any shape change.
pub const GROUND_TRUTH_SCHEMA_VERSION: u32 = 1;

/// The closed set of bottleneck classes the harness knows how to induce.
/// `verify` rejects unknown classes — the class string is the supervised
/// label, and a typo here poisons exactly what the corpus trains on.
pub const KNOWN_CLASSES: &[&str] = &["cpu_saturation", "scheduler_contention"];

/// A checkable claim about one metric in the extracted record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedSignal {
    /// Emitted record metric name (histogram quantiles use the `:pNN` suffix).
    pub metric: String,
    pub check: SignalCheck,
}

/// The closed set of mechanical checks `verify` knows how to evaluate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalCheck {
    /// At least 1 regime shift with direction "Increase" whose index falls inside
    /// the induced window. Offsets convert to indices via `sampling_interval_s`.
    RegimeShiftIncreaseInWindow,
    /// The metric shows elevated activity: Full-tier with >=1 in-window
    /// anomaly or regime shift, OR stats.p99 > 2.0 * stats.p50.
    ///
    /// v1 caveat: the stats ratio is whole-recording (the record carries no
    /// windowed stats) and many metrics are bursty at idle, so this check is
    /// corroboration-grade — `verify` requires every ground truth to also
    /// carry at least one `RegimeShiftIncreaseInWindow` signal, so this can
    /// never be the sole gate. Revisit when extraction grows windowed stats.
    ElevatedInWindow,
    /// The metric exists in the record and its sampler label appears in
    /// coverage.subsystems_present.
    SubsystemPresent,
}

/// Experiment provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Experiment {
    pub spec_file: String,
    pub params: BTreeMap<String, String>,
    pub host_tag: String,
}

/// The ground truth an experiment attaches to its recording: what bottleneck
/// was induced, where in the recording, and which record signals must show
/// it. Mirrors `FindingKind::Bottleneck`'s fields so distillation can check
/// a candidate assessment's claim by field equality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroundTruth {
    pub schema_version: u32,
    /// "cpu_saturation" | "scheduler_contention" (documented closed set).
    pub class: String,
    pub subsystem: String,
    pub mechanism: String,
    pub direction: String,
    /// Induced-phase offsets in seconds relative to recording start.
    pub induced_window_s: (f64, f64),
    pub expected_signals: Vec<ExpectedSignal>,
    pub experiment: Experiment,
}

impl GroundTruth {
    /// Evaluate every expected signal against an extracted record,
    /// accumulating all failures. Empty error vec never returned — Ok means
    /// every check passed.
    pub fn verify(&self, record: &OverviewRecord) -> Result<(), Vec<String>> {
        let mut errs = Vec::new();
        // Structural preconditions: a malformed truth file must say so
        // plainly, not degrade into misleading "no shift in window" errors.
        if !KNOWN_CLASSES.contains(&self.class.as_str()) {
            errs.push(format!(
                "class {:?} is not a known bottleneck class ({KNOWN_CLASSES:?})",
                self.class
            ));
        }
        let (start, end) = self.induced_window_s;
        if !(start.is_finite() && end.is_finite() && start >= 0.0 && start < end) {
            errs.push(format!(
                "malformed induced_window_s ({start}, {end}): need 0 <= start < end"
            ));
        }
        if end > record.context.duration_s {
            errs.push(format!(
                "induced_window_s end {end} exceeds recording duration {}",
                record.context.duration_s
            ));
        }
        // ElevatedInWindow is corroboration-grade (see its doc); every truth
        // must anchor on at least one regime-shift signal.
        if !self
            .expected_signals
            .iter()
            .any(|s| s.check == SignalCheck::RegimeShiftIncreaseInWindow)
        {
            errs.push(
                "expected_signals must include at least one RegimeShiftIncreaseInWindow \
                 anchor (ElevatedInWindow alone can pass vacuously on bursty-idle metrics)"
                    .to_string(),
            );
        }
        if !errs.is_empty() {
            return Err(errs);
        }
        let step = if record.context.sampling_interval_s > 0.0 {
            record.context.sampling_interval_s
        } else {
            1.0
        };
        // NOTE: shift/anomaly indices come from the engine's analysis series,
        // which can start ~1 step after recording start; with truncation the
        // index↔offset mapping may skew by 1-2 samples. Pad harness windows
        // rather than relying on exact boundaries. Bounds are inclusive.
        let window = ((start / step) as usize, (end / step) as usize);
        for signal in &self.expected_signals {
            let Some(m) = record.metrics.iter().find(|m| m.name == signal.metric) else {
                errs.push(format!("{}: metric not present in record", signal.metric));
                continue;
            };
            if let Err(e) = check_signal(m, signal.check, window, record) {
                errs.push(format!("{}: {e}", signal.metric));
            }
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }

    /// Distillation claim-equality gate: the candidate assessment must carry
    /// exactly one Bottleneck finding, at High priority, whose fields exactly
    /// equal this ground truth's. Accumulates all failures. Ruled-out and
    /// additional non-Bottleneck findings are deliberately unconstrained
    /// (agent-reviewed, not machine-gated).
    pub fn matches_assessment(
        &self,
        assessment: &crate::analysis::assessment::Assessment,
    ) -> Result<(), Vec<String>> {
        use crate::analysis::assessment::{FindingKind, Priority};
        let bottlenecks: Vec<_> = assessment
            .findings
            .iter()
            .filter(|f| matches!(f.kind, FindingKind::Bottleneck { .. }))
            .collect();
        if bottlenecks.len() != 1 {
            return Err(vec![format!(
                "candidate must carry exactly one Bottleneck finding (found {})",
                bottlenecks.len()
            )]);
        }
        let finding = bottlenecks[0];
        let mut errs = Vec::new();
        if finding.priority != Priority::High {
            errs.push(format!(
                "Bottleneck finding must be High priority (found {:?})",
                finding.priority
            ));
        }
        if let FindingKind::Bottleneck {
            subsystem,
            mechanism,
            direction,
        } = &finding.kind
        {
            for (field, got, want) in [
                ("subsystem", subsystem, &self.subsystem),
                ("mechanism", mechanism, &self.mechanism),
                ("direction", direction, &self.direction),
            ] {
                if got != want {
                    errs.push(format!(
                        "{field} mismatch: candidate {got:?} != truth {want:?}"
                    ));
                }
            }
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }
}

fn in_window(index: usize, window: (usize, usize)) -> bool {
    index >= window.0 && index <= window.1
}

fn check_signal(
    m: &MetricFeatures,
    check: SignalCheck,
    window: (usize, usize),
    record: &OverviewRecord,
) -> Result<(), String> {
    match check {
        SignalCheck::RegimeShiftIncreaseInWindow => {
            if m.regime_shifts
                .iter()
                .any(|s| s.direction == "Increase" && in_window(s.index, window))
            {
                Ok(())
            } else {
                Err(format!(
                    "no Increase regime shift inside window {}..{} ({} shifts total)",
                    window.0,
                    window.1,
                    m.regime_shifts.len()
                ))
            }
        }
        SignalCheck::ElevatedInWindow => {
            let findings_in_window = m.tier == DetailTier::Full
                && (m.anomalies.iter().any(|a| in_window(a.index, window))
                    || m.regime_shifts.iter().any(|s| in_window(s.index, window)));
            let stats_elevated = m.stats.p99 > 2.0 * m.stats.p50;
            if findings_in_window || stats_elevated {
                Ok(())
            } else {
                Err(format!(
                    "not elevated: no in-window findings and p99 {:.3} <= 2x p50 {:.3}",
                    m.stats.p99, m.stats.p50
                ))
            }
        }
        SignalCheck::SubsystemPresent => {
            let Some(sampler) = m.labels.get("sampler") else {
                return Err("metric has no sampler label".to_string());
            };
            if record.context.coverage.subsystems_present.contains(sampler) {
                Ok(())
            } else {
                Err(format!("sampler {sampler} not in subsystems_present"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::record::*;
    use std::collections::BTreeMap;

    fn record(metrics: Vec<MetricFeatures>) -> OverviewRecord {
        OverviewRecord {
            schema_version: RECORD_SCHEMA_VERSION,
            context: Context {
                source: "rezolus".to_string(),
                agent_version: None,
                duration_s: 250.0,
                sampling_interval_s: 1.0,
                systeminfo: None,
                coverage: Coverage {
                    subsystems_present: vec![
                        "cpu_usage".to_string(),
                        "scheduler_runqueue".to_string(),
                    ],
                    subsystems_absent: vec![],
                },
            },
            metrics,
            correlations: vec![],
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

    fn metric(
        name: &str,
        sampler: &str,
        shifts: Vec<(usize, &str)>,
        p50: f64,
        p99: f64,
    ) -> MetricFeatures {
        MetricFeatures {
            name: name.to_string(),
            metric_type: "counter".to_string(),
            labels: BTreeMap::from([("sampler".to_string(), sampler.to_string())]),
            tier: if shifts.is_empty() {
                DetailTier::Summary
            } else {
                DetailTier::Full
            },
            status: AnalysisStatus::Analyzed,
            stats: Stats {
                min: 0.0,
                max: p99,
                mean: p50,
                last: p50,
                p50,
                p99,
            },
            noise: NoiseSummary {
                noise_type: "WhitePhase".to_string(),
                optimal_tau_s: None,
            },
            anomalies: vec![],
            regime_shifts: shifts
                .into_iter()
                .map(|(index, direction)| RegimeShiftFeature {
                    index,
                    direction: direction.to_string(),
                    before_mean: 1.0,
                    after_mean: 2.0,
                    mean_change_pct: 100.0,
                    confidence: 0.9,
                    allan_significance: 3.0,
                })
                .collect(),
            uncertainty: None,
        }
    }

    fn truth(signals: Vec<ExpectedSignal>) -> GroundTruth {
        GroundTruth {
            schema_version: GROUND_TRUTH_SCHEMA_VERSION,
            class: "cpu_saturation".to_string(),
            subsystem: "cpu".to_string(),
            mechanism: "cpu_saturation".to_string(),
            direction: "cores saturated".to_string(),
            induced_window_s: (60.0, 180.0),
            expected_signals: signals,
            experiment: Experiment {
                spec_file: "labels/specs/cpu_saturation.toml".to_string(),
                params: BTreeMap::new(),
                host_tag: "z1.baremetal".to_string(),
            },
        }
    }

    #[test]
    fn round_trips_and_is_deterministic() {
        let t = truth(vec![ExpectedSignal {
            metric: "cpu_usage".to_string(),
            check: SignalCheck::RegimeShiftIncreaseInWindow,
        }]);
        let a = serde_json::to_string(&t).unwrap();
        let back: GroundTruth = serde_json::from_str(&a).unwrap();
        assert_eq!(t, back);
        assert_eq!(a, serde_json::to_string(&back).unwrap());
    }

    #[test]
    fn regime_shift_in_window_passes_and_out_of_window_fails() {
        let signal = vec![ExpectedSignal {
            metric: "cpu_usage".to_string(),
            check: SignalCheck::RegimeShiftIncreaseInWindow,
        }];
        // shift at index 70 (inside 60..180 at 1s cadence)
        let ok = record(vec![metric(
            "cpu_usage",
            "cpu_usage",
            vec![(70, "Increase")],
            0.5,
            0.9,
        )]);
        assert!(truth(signal.clone()).verify(&ok).is_ok());
        // shift outside the window
        let outside = record(vec![metric(
            "cpu_usage",
            "cpu_usage",
            vec![(20, "Increase")],
            0.5,
            0.9,
        )]);
        let errs = truth(signal.clone()).verify(&outside).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("cpu_usage"), "{errs:?}");
        // decrease inside the window doesn't count
        let wrong_dir = record(vec![metric(
            "cpu_usage",
            "cpu_usage",
            vec![(70, "Decrease")],
            0.5,
            0.9,
        )]);
        assert!(truth(signal).verify(&wrong_dir).is_err());
    }

    /// A shifted cpu metric + the anchor signal for it — every truth must
    /// carry a RegimeShiftIncreaseInWindow anchor, so tests of OTHER checks
    /// pair their signal with this satisfiable anchor.
    fn anchor_metric() -> MetricFeatures {
        metric("cpu_usage", "cpu_usage", vec![(70, "Increase")], 0.5, 0.9)
    }

    fn anchor_signal() -> ExpectedSignal {
        ExpectedSignal {
            metric: "cpu_usage".to_string(),
            check: SignalCheck::RegimeShiftIncreaseInWindow,
        }
    }

    #[test]
    fn elevated_in_window_checks_findings_or_stats_ratio() {
        let signals = vec![
            ExpectedSignal {
                metric: "lat:p99".to_string(),
                check: SignalCheck::ElevatedInWindow,
            },
            anchor_signal(),
        ];
        // full-tier with an in-window shift -> pass
        let with_shift = record(vec![
            metric(
                "lat:p99",
                "scheduler_runqueue",
                vec![(100, "Increase")],
                1.0,
                1.5,
            ),
            anchor_metric(),
        ]);
        assert!(truth(signals.clone()).verify(&with_shift).is_ok());
        // no findings but p99 > 2x p50 -> pass
        let ratio = record(vec![
            metric("lat:p99", "scheduler_runqueue", vec![], 1.0, 3.0),
            anchor_metric(),
        ]);
        assert!(truth(signals.clone()).verify(&ratio).is_ok());
        // quiet and flat -> fail (only the elevated check fails)
        let flat = record(vec![
            metric("lat:p99", "scheduler_runqueue", vec![], 1.0, 1.5),
            anchor_metric(),
        ]);
        let errs = truth(signals).verify(&flat).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("not elevated"), "{errs:?}");
    }

    #[test]
    fn subsystem_present_resolves_through_labels_and_coverage() {
        let signals = vec![
            ExpectedSignal {
                metric: "cpu_usage".to_string(),
                check: SignalCheck::SubsystemPresent,
            },
            anchor_signal(),
        ];
        let ok = record(vec![anchor_metric()]);
        assert!(truth(signals.clone()).verify(&ok).is_ok());
        // metric absent from the record entirely
        let missing = record(vec![]);
        assert!(truth(signals).verify(&missing).is_err());
    }

    #[test]
    fn subsystem_present_rejects_uncovered_sampler_and_missing_label() {
        let signal_for = |name: &str| {
            vec![
                ExpectedSignal {
                    metric: name.to_string(),
                    check: SignalCheck::SubsystemPresent,
                },
                anchor_signal(),
            ]
        };
        // metric exists but its sampler is not in coverage.subsystems_present
        let uncovered = record(vec![
            metric("lat:p99", "not_in_coverage", vec![], 0.5, 0.9),
            anchor_metric(),
        ]);
        let errs = truth(signal_for("lat:p99")).verify(&uncovered).unwrap_err();
        assert!(errs[0].contains("not_in_coverage"), "{errs:?}");
        // metric exists but carries no sampler label at all
        let mut unlabeled = metric("lat:p99", "scheduler_runqueue", vec![], 0.5, 0.9);
        unlabeled.labels.clear();
        let errs = truth(signal_for("lat:p99"))
            .verify(&record(vec![unlabeled, anchor_metric()]))
            .unwrap_err();
        assert!(errs[0].contains("no sampler label"), "{errs:?}");
    }

    #[test]
    fn failures_accumulate() {
        let signals = vec![
            ExpectedSignal {
                metric: "a".to_string(),
                check: SignalCheck::SubsystemPresent,
            },
            ExpectedSignal {
                metric: "b".to_string(),
                check: SignalCheck::SubsystemPresent,
            },
            anchor_signal(),
        ];
        let errs = truth(signals).verify(&record(vec![])).unwrap_err();
        assert_eq!(
            errs.len(),
            3,
            "all failures reported, not fail-fast: {errs:?}"
        );
    }

    #[test]
    fn structural_preconditions_reject_malformed_truths() {
        let anchored = vec![anchor_signal()];
        let rec = record(vec![anchor_metric()]);
        // unknown class
        let mut t = truth(anchored.clone());
        t.class = "cpu_saturaton".to_string(); // typo'd label
        let errs = t.verify(&rec).unwrap_err();
        assert!(errs[0].contains("not a known bottleneck class"), "{errs:?}");
        // inverted window
        let mut t = truth(anchored.clone());
        t.induced_window_s = (180.0, 60.0);
        let errs = t.verify(&rec).unwrap_err();
        assert!(errs[0].contains("malformed induced_window_s"), "{errs:?}");
        // window past the recording's end
        let mut t = truth(anchored.clone());
        t.induced_window_s = (60.0, 500.0);
        let errs = t.verify(&rec).unwrap_err();
        assert!(errs[0].contains("exceeds recording duration"), "{errs:?}");
        // no regime-shift anchor among the signals
        let t = truth(vec![ExpectedSignal {
            metric: "cpu_usage".to_string(),
            check: SignalCheck::ElevatedInWindow,
        }]);
        let errs = t.verify(&rec).unwrap_err();
        assert!(errs[0].contains("RegimeShiftIncreaseInWindow"), "{errs:?}");
    }

    #[test]
    fn committed_ground_truths_parse_and_declare_v1() {
        for path in [
            "labels/ground_truth/cpu_saturation.json",
            "labels/ground_truth/scheduler_contention.json",
        ] {
            let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
            let t: GroundTruth =
                serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!(t.schema_version, GROUND_TRUTH_SCHEMA_VERSION, "{path}");
            assert!(!t.expected_signals.is_empty(), "{path}");
            assert!(t.induced_window_s.0 < t.induced_window_s.1, "{path}");
            // Structural preconditions verify() itself requires: a known
            // class and a RegimeShiftIncreaseInWindow anchor among the
            // signals. Assert them here so a malformed committed asset
            // fails loudly at this test rather than only inside verify().
            assert!(
                KNOWN_CLASSES.contains(&t.class.as_str()),
                "{path}: class {:?} not in {KNOWN_CLASSES:?}",
                t.class
            );
            assert!(
                t.expected_signals
                    .iter()
                    .any(|s| s.check == SignalCheck::RegimeShiftIncreaseInWindow),
                "{path}: missing a RegimeShiftIncreaseInWindow anchor signal"
            );
        }
    }

    #[test]
    fn committed_exemplars_parse_and_validate() {
        use crate::analysis::assessment::Assessment;
        for path in [
            "labels/exemplars/cpu_saturation.assessment.json",
            "labels/exemplars/scheduler_contention.assessment.json",
        ] {
            let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
            let a: Assessment =
                serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{path}: {e}"));
            a.validate().unwrap_or_else(|e| panic!("{path}: {e}"));
        }
    }

    /// End-to-end exemplar check against a real verified corpus record: every
    /// evidence pointer must resolve and the acquisition-window confidence cap
    /// must hold. Not run by default — point it at a fetched record:
    ///
    /// ```text
    /// EXEMPLAR=labels/exemplars/cpu_saturation.assessment.json \
    /// CORPUS_RECORD=labels/corpus/cpu_saturation/<id>/record.json \
    /// cargo test exemplar_validates_against_corpus_record -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn exemplar_validates_against_corpus_record() {
        use crate::analysis::assessment::Assessment;
        let usage = "set EXEMPLAR and CORPUS_RECORD, e.g.\n\
             EXEMPLAR=labels/exemplars/cpu_saturation.assessment.json \\\n\
             CORPUS_RECORD=labels/corpus/cpu_saturation/<experiment-id>/record.json \\\n\
             cargo test exemplar_validates_against_corpus_record -- --ignored --nocapture";
        let exemplar_path =
            std::env::var("EXEMPLAR").unwrap_or_else(|_| panic!("EXEMPLAR unset; {usage}"));
        let record_path = std::env::var("CORPUS_RECORD")
            .unwrap_or_else(|_| panic!("CORPUS_RECORD unset; {usage}"));
        let a: Assessment = serde_json::from_str(
            &std::fs::read_to_string(&exemplar_path)
                .unwrap_or_else(|e| panic!("{exemplar_path}: {e}")),
        )
        .unwrap_or_else(|e| panic!("{exemplar_path}: {e}"));
        let record: crate::analysis::record::OverviewRecord = serde_json::from_str(
            &std::fs::read_to_string(&record_path).unwrap_or_else(|e| panic!("{record_path}: {e}")),
        )
        .unwrap_or_else(|e| panic!("{record_path}: {e}"));
        a.validate_against(&record)
            .unwrap_or_else(|e| panic!("exemplar does not validate against record: {e}"));
        println!("exemplar {exemplar_path} validates against {record_path}");
    }

    /// Verify a fetched corpus pair (a real extracted record + its
    /// ground-truth document) end to end. Not run by default — point it at
    /// a `labels/corpus/<class>/<experiment-id>/` fetch via env vars:
    ///
    /// ```text
    /// CORPUS_RECORD=labels/corpus/cpu_saturation/<id>/record.json \
    /// CORPUS_TRUTH=labels/corpus/cpu_saturation/<id>/ground_truth.json \
    /// cargo test verify_corpus_pair -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn verify_corpus_pair() {
        let record_path = std::env::var("CORPUS_RECORD").unwrap_or_else(|_| {
            panic!(
                "verify_corpus_pair: set CORPUS_RECORD to the extracted record.json path, e.g.\n\
                 CORPUS_RECORD=labels/corpus/<class>/<experiment-id>/record.json \\\n\
                 CORPUS_TRUTH=labels/corpus/<class>/<experiment-id>/ground_truth.json \\\n\
                 cargo test verify_corpus_pair -- --ignored --nocapture"
            )
        });
        let truth_path = std::env::var("CORPUS_TRUTH").unwrap_or_else(|_| {
            panic!(
                "verify_corpus_pair: set CORPUS_TRUTH to the fetched ground_truth.json path, e.g.\n\
                 CORPUS_RECORD=labels/corpus/<class>/<experiment-id>/record.json \\\n\
                 CORPUS_TRUTH=labels/corpus/<class>/<experiment-id>/ground_truth.json \\\n\
                 cargo test verify_corpus_pair -- --ignored --nocapture"
            )
        });
        let record: crate::analysis::record::OverviewRecord = serde_json::from_str(
            &std::fs::read_to_string(&record_path).unwrap_or_else(|e| panic!("{record_path}: {e}")),
        )
        .unwrap_or_else(|e| panic!("{record_path}: {e}"));
        let truth: GroundTruth = serde_json::from_str(
            &std::fs::read_to_string(&truth_path).unwrap_or_else(|e| panic!("{truth_path}: {e}")),
        )
        .unwrap_or_else(|e| panic!("{truth_path}: {e}"));
        truth
            .verify(&record)
            .unwrap_or_else(|errs| panic!("verify() failed:\n{}", errs.join("\n")));
    }

    use crate::analysis::assessment::{
        Assessment, Confidence, DataQuality, EvidenceRef, Finding, FindingKind, Overall,
        OverallStatus, Priority, TieredFinding,
    };

    fn bottleneck_assessment(subsystem: &str, mechanism: &str, direction: &str) -> Assessment {
        let evidence = vec![EvidenceRef {
            metric: Some("cpu_usage".to_string()),
            anomaly_index: None,
            regime_shift_index: Some(0),
            correlation_index: None,
            rationale: "sustained in-window step increase".to_string(),
        }];
        Assessment {
            schema_version: crate::analysis::assessment::ASSESSMENT_SCHEMA_VERSION,
            overall: Overall {
                finding: Finding {
                    summary: "Bottlenecked".to_string(),
                    confidence: Confidence::High,
                    evidence: evidence.clone(),
                    recommended_action: Some("act".to_string()),
                },
                status: OverallStatus::Actionable,
                data_quality: DataQuality {
                    coverage_gaps: vec![],
                    uncertainty_limited: false,
                    note: None,
                },
            },
            findings: vec![TieredFinding {
                finding: Finding {
                    summary: "The bottleneck".to_string(),
                    confidence: Confidence::High,
                    evidence,
                    recommended_action: Some("act".to_string()),
                },
                priority: Priority::High,
                kind: FindingKind::Bottleneck {
                    subsystem: subsystem.to_string(),
                    mechanism: mechanism.to_string(),
                    direction: direction.to_string(),
                },
            }],
            ruled_out: vec![],
        }
    }

    #[test]
    fn matching_bottleneck_claim_passes() {
        let t = truth(vec![anchor_signal()]);
        let a = bottleneck_assessment("cpu", "cpu_saturation", "cores saturated");
        assert!(t.matches_assessment(&a).is_ok());
    }

    #[test]
    fn field_mismatches_are_each_reported() {
        let t = truth(vec![anchor_signal()]);
        let a = bottleneck_assessment("scheduler", "runqueue_latency", "queuing");
        let errs = t.matches_assessment(&a).unwrap_err();
        assert_eq!(
            errs.len(),
            3,
            "all three field mismatches accumulate: {errs:?}"
        );
    }

    #[test]
    fn zero_or_multiple_bottlenecks_rejected() {
        let t = truth(vec![anchor_signal()]);
        let mut none = bottleneck_assessment("cpu", "cpu_saturation", "cores saturated");
        none.findings.clear();
        let errs = t.matches_assessment(&none).unwrap_err();
        assert!(errs[0].contains("exactly one"), "{errs:?}");
        let mut two = bottleneck_assessment("cpu", "cpu_saturation", "cores saturated");
        let dup = two.findings[0].clone();
        two.findings.push(dup);
        assert!(t.matches_assessment(&two).is_err());
    }

    #[test]
    fn non_high_priority_bottleneck_rejected() {
        let t = truth(vec![anchor_signal()]);
        let mut a = bottleneck_assessment("cpu", "cpu_saturation", "cores saturated");
        a.findings[0].priority = Priority::Medium;
        let errs = t.matches_assessment(&a).unwrap_err();
        assert!(errs[0].contains("High"), "{errs:?}");
    }

    /// Full distillation acceptance gate: structural validate ->
    /// cross-record validate_against -> ground-truth claim equality.
    ///
    /// ```text
    /// CANDIDATE=... CORPUS_RECORD=... CORPUS_TRUTH=... \
    /// cargo test gate_distillation_candidate -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn gate_distillation_candidate() {
        use crate::analysis::assessment::Assessment;
        let usage = "set CANDIDATE, CORPUS_RECORD, CORPUS_TRUTH (see labels/distill.md)";
        let candidate_path =
            std::env::var("CANDIDATE").unwrap_or_else(|_| panic!("CANDIDATE unset; {usage}"));
        let record_path = std::env::var("CORPUS_RECORD")
            .unwrap_or_else(|_| panic!("CORPUS_RECORD unset; {usage}"));
        let truth_path =
            std::env::var("CORPUS_TRUTH").unwrap_or_else(|_| panic!("CORPUS_TRUTH unset; {usage}"));
        let candidate: Assessment = serde_json::from_str(
            &std::fs::read_to_string(&candidate_path)
                .unwrap_or_else(|e| panic!("{candidate_path}: {e}")),
        )
        .unwrap_or_else(|e| panic!("{candidate_path}: {e}"));
        let record: crate::analysis::record::OverviewRecord = serde_json::from_str(
            &std::fs::read_to_string(&record_path).unwrap_or_else(|e| panic!("{record_path}: {e}")),
        )
        .unwrap_or_else(|e| panic!("{record_path}: {e}"));
        let truth: GroundTruth = serde_json::from_str(
            &std::fs::read_to_string(&truth_path).unwrap_or_else(|e| panic!("{truth_path}: {e}")),
        )
        .unwrap_or_else(|e| panic!("{truth_path}: {e}"));
        // Stage 1+2 (structural then cross-record; each short-circuits since
        // later stages can't run on malformed input):
        candidate
            .validate_against(&record)
            .unwrap_or_else(|e| panic!("GATE FAIL (validators): {e}"));
        // Stage 3 (claim equality), accumulating:
        truth
            .matches_assessment(&candidate)
            .unwrap_or_else(|errs| panic!("GATE FAIL (claim equality):\n  {}", errs.join("\n  ")));
        println!("GATE PASS: {candidate_path}");
    }
}
