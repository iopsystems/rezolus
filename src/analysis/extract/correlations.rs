//! Cross-metric correlation features. Candidates are *salient* metrics
//! (anomalous or regime-shifted), capped by salience — a principled,
//! documented subset (correlation is O(N^2); exhaustive is impossible).
//! Absorbs the orphaned `src/mcp/discover_correlations.rs` prototype, whose
//! hardcoded query cross-product predated the salience policy.

use metriken_query::MetricsSource;

use crate::analysis::record::CorrelationFeature;
use crate::mcp::correlation::calculate_correlation;

/// Human description of the candidate space, recorded in `Selection`.
pub(crate) const CANDIDATE_SET_DESCRIPTION: &str = "all pairs among salient metrics (anomalous or \
    regime-shifted) and top-consumer base metrics, excluding same-base-metric pairs, capped at 12 \
    by salience";

pub(crate) const CANDIDATE_CAP: usize = 12;
const MIN_ABS_R: f64 = 0.7;
const TOP_N: usize = 10;

/// A correlation candidate: an emitted metric entry plus its salience score
/// (anomaly count + regime-shift count).
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub name: String,
    pub query: String,
    pub subsystem: String,
    pub salience: usize,
}

/// Intermediate result of one engine call, decoupled from the engine type
/// so ranking/mapping is unit-testable.
#[derive(Debug, Clone)]
pub(crate) struct RawCorrelation {
    pub metric1: String,
    pub metric2: String,
    pub max_r: f64,
    pub r_band: Option<(f64, f64)>,
    pub optimal_lag_s: i64,
    pub subsystem1: String,
    pub subsystem2: String,
}

/// Salience desc, then name asc; truncate to `cap`. Clones into a local
/// `Vec` and sorts that — the caller's ordering is left untouched.
pub(crate) fn select_candidates(candidates: &[Candidate], cap: usize) -> Vec<Candidate> {
    let mut sorted: Vec<Candidate> = candidates.to_vec();
    sorted.sort_by(|a, b| {
        b.salience
            .cmp(&a.salience)
            .then_with(|| a.name.cmp(&b.name))
    });
    sorted.truncate(cap);
    sorted
}

/// Strip a trailing `:p50`/`:p90`/`:p99` histogram-quantile suffix, if
/// present, leaving the base metric name. Used to exclude same-base-metric
/// pairs: quantile siblings of one histogram (e.g. `lat:p50`, `lat:p99`) are
/// tautologically correlated and would otherwise crowd out real findings.
fn base_name(name: &str) -> &str {
    for suffix in [":p50", ":p90", ":p99"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped;
        }
    }
    name
}

/// All unordered index pairs (i < j) over the candidate list, in list order —
/// deterministic because the list is sorted — excluding pairs whose names
/// share a base metric (histogram quantile siblings).
pub(crate) fn candidate_pairs(picked: &[Candidate]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for i in 0..picked.len() {
        for j in (i + 1)..picked.len() {
            if base_name(&picked[i].name) == base_name(&picked[j].name) {
                continue;
            }
            pairs.push((i, j));
        }
    }
    pairs
}

/// Threshold, sort (|r| desc, then pair names), truncate, map to record
/// features. Non-finite r (or band) is dropped/stripped per the finite-float
/// invariant.
pub(crate) fn build_features(raw: Vec<RawCorrelation>, top_n: usize) -> Vec<CorrelationFeature> {
    let mut kept: Vec<RawCorrelation> = raw
        .into_iter()
        .filter(|r| r.max_r.is_finite() && r.max_r.abs() >= MIN_ABS_R)
        .collect();
    kept.sort_by(|a, b| {
        b.max_r.abs().total_cmp(&a.max_r.abs()).then_with(|| {
            (a.metric1.as_str(), a.metric2.as_str()).cmp(&(b.metric1.as_str(), b.metric2.as_str()))
        })
    });
    kept.truncate(top_n);
    kept.into_iter()
        .map(|r| CorrelationFeature {
            relationship: if r.subsystem1 == r.subsystem2 {
                "SameSubsystem".to_string()
            } else {
                "CrossSubsystem".to_string()
            },
            metric1: r.metric1,
            metric2: r.metric2,
            max_r: r.max_r,
            r_band: r.r_band.filter(|(lo, hi)| lo.is_finite() && hi.is_finite()),
            optimal_lag_s: r.optimal_lag_s,
        })
        .collect()
}

/// Run the engine over every candidate pair. Returns the kept features and
/// the number of pairs actually tested. Engine failures on a pair are
/// skipped (the pair still counts as tested).
pub(crate) fn discover(
    data: &dyn MetricsSource,
    candidates: &[Candidate],
) -> (Vec<CorrelationFeature>, usize) {
    let picked = select_candidates(candidates, CANDIDATE_CAP);
    let pairs = candidate_pairs(&picked);
    let total = pairs.len();
    let mut raw = Vec::new();
    for (i, j) in pairs {
        let (a, b) = (&picked[i], &picked[j]);
        let Ok(result) = calculate_correlation(data, &a.query, &b.query) else {
            continue;
        };
        raw.push(RawCorrelation {
            metric1: a.name.clone(),
            metric2: b.name.clone(),
            max_r: result.max_correlation,
            r_band: result.r_band,
            optimal_lag_s: result.optimal_lag,
            subsystem1: a.subsystem.clone(),
            subsystem2: b.subsystem.clone(),
        });
    }
    (build_features(raw, TOP_N), total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(name: &str, salience: usize, subsystem: &str) -> Candidate {
        Candidate {
            name: name.to_string(),
            query: format!("sum(rate({name}[1m]))"),
            subsystem: subsystem.to_string(),
            salience,
        }
    }

    #[test]
    fn candidates_capped_by_salience_then_name() {
        let input = vec![
            cand("c", 1, "cpu_usage"),
            cand("a", 3, "cpu_usage"),
            cand("b", 3, "scheduler_runqueue"),
            cand("d", 2, "tcp_traffic"),
        ];
        let picked = select_candidates(&input, 3);
        let names: Vec<&str> = picked.iter().map(|c| c.name.as_str()).collect();
        // salience desc, name asc tiebreak, cap 3
        assert_eq!(names, vec!["a", "b", "d"]);
    }

    #[test]
    fn pairs_generated_in_order() {
        let picked = vec![cand("a", 1, "x"), cand("b", 1, "x"), cand("c", 1, "y")];
        let pairs = candidate_pairs(&picked);
        let keys: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(i, j)| (picked[*i].name.as_str(), picked[*j].name.as_str()))
            .collect();
        assert_eq!(keys, vec![("a", "b"), ("a", "c"), ("b", "c")]);
    }

    #[test]
    fn features_kept_sorted_and_truncated() {
        let raw = vec![
            ("a", "b", 0.72, "x", "x"),
            ("a", "c", -0.95, "x", "y"),
            ("b", "c", 0.3, "x", "y"),  // below threshold, dropped
            ("a", "d", 0.72, "x", "z"), // ties with (a,b); tiebreak on names
        ];
        let feats = build_features(
            raw.into_iter()
                .map(|(m1, m2, r, s1, s2)| RawCorrelation {
                    metric1: m1.to_string(),
                    metric2: m2.to_string(),
                    max_r: r,
                    r_band: None,
                    optimal_lag_s: 0,
                    subsystem1: s1.to_string(),
                    subsystem2: s2.to_string(),
                })
                .collect(),
            2,
        );
        assert_eq!(feats.len(), 2);
        assert_eq!(feats[0].metric1, "a");
        assert_eq!(feats[0].metric2, "c");
        assert_eq!(feats[0].relationship, "CrossSubsystem");
        // tie at |0.72| resolved by (metric1, metric2) ordering: (a,b) < (a,d)
        assert_eq!(feats[1].metric2, "b");
        assert_eq!(feats[1].relationship, "SameSubsystem");
    }

    #[test]
    fn non_finite_r_dropped() {
        let feats = build_features(
            vec![RawCorrelation {
                metric1: "a".into(),
                metric2: "b".into(),
                max_r: f64::NAN,
                r_band: Some((f64::NAN, 1.0)),
                optimal_lag_s: 0,
                subsystem1: "x".into(),
                subsystem2: "x".into(),
            }],
            10,
        );
        assert!(feats.is_empty());
    }

    #[test]
    fn candidate_set_description_mentions_cap() {
        assert!(CANDIDATE_SET_DESCRIPTION.contains(&CANDIDATE_CAP.to_string()));
    }

    #[test]
    fn finite_r_with_non_finite_band_is_kept_with_band_stripped() {
        let feats = build_features(
            vec![RawCorrelation {
                metric1: "a".into(),
                metric2: "b".into(),
                max_r: 0.9,
                r_band: Some((f64::NAN, 0.95)),
                optimal_lag_s: 0,
                subsystem1: "x".into(),
                subsystem2: "x".into(),
            }],
            10,
        );
        assert_eq!(feats.len(), 1);
        assert_eq!(feats[0].r_band, None);
    }

    #[test]
    fn exactly_min_abs_r_boundary_is_kept() {
        let feats = build_features(
            vec![RawCorrelation {
                metric1: "a".into(),
                metric2: "b".into(),
                max_r: 0.7,
                r_band: None,
                optimal_lag_s: 0,
                subsystem1: "x".into(),
                subsystem2: "x".into(),
            }],
            10,
        );
        assert_eq!(feats.len(), 1);
    }

    #[test]
    fn thirteen_candidates_cap_to_twelve_yields_66_pairs() {
        let input: Vec<Candidate> = (0..13)
            .map(|i| cand(&format!("m{i:02}"), 13 - i, "cpu_usage"))
            .collect();
        let picked = select_candidates(&input, CANDIDATE_CAP);
        assert_eq!(picked.len(), 12);
        let pairs = candidate_pairs(&picked);
        assert_eq!(pairs.len(), 66); // C(12,2)
    }

    #[test]
    fn quantile_siblings_generate_no_pair() {
        let picked = vec![cand("lat:p50", 1, "x"), cand("lat:p99", 1, "x")];
        let pairs = candidate_pairs(&picked);
        assert!(pairs.is_empty());
    }
}
