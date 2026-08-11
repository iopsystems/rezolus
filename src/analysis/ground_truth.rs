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
}
