//! Naming the captures an A/B view is showing.

use std::collections::{BTreeMap, HashSet};

/// Pick the label that actually tells these recordings apart.
///
/// A fixed `arm`-then-`host` precedence breaks on the case `record` now makes
/// easy: two agents on ONE host differ only in `source`, so both arms would be
/// aliased to the same hostname and the A/B view could not be read. It breaks
/// the other way too — two hosts recorded without `--label` share
/// `source=rezolus`, so keying on `source` alone would collapse those.
///
/// So prefer `arm` when it discriminates, then `host`, then `source`, then any
/// other label that does; `None` means nothing distinguishes them and the
/// caller falls back to positional names. `--label` cannot help here: it
/// applies to every recording in a run, which is why the recorder warns when
/// two recordings end up with identical label sets.
///
/// **Shared by both viewers on purpose.** `rezolus view` and the static-site
/// WASM viewer both name their captures with this, so one archive is labelled
/// the same wherever it is opened. Two spellings of "which label tells these
/// apart" would be free to drift, and the symptom — one viewer showing
/// `web-01` where the other shows `baseline` — is the kind nobody reports.
pub fn discriminating_alias_key(recordings: &[BTreeMap<String, String>]) -> Option<String> {
    // Nothing to discriminate against. Without this, one recording makes every
    // predicate vacuously true and the first merely-PRESENT preferred key wins
    // — so a hindsight snapshot, whose labels are just `{source: rezolus}`,
    // would be aliased "rezolus" instead of the positional "baseline". An
    // empty slice is worse: `[].iter().all(..)` is true, so it would return
    // `Some("arm")` for no recordings at all.
    if recordings.len() < 2 {
        return None;
    }
    let discriminates = |k: &str| {
        let mut seen = HashSet::new();
        recordings
            .iter()
            .all(|labels| labels.get(k).is_some_and(|v| seen.insert(v.clone())))
    };
    const PREFERRED: [&str; 3] = ["arm", "host", "source"];
    PREFERRED
        .into_iter()
        .find(|k| discriminates(k))
        .map(str::to_string)
        .or_else(|| {
            let mut others: Vec<&String> = recordings
                .first()
                .map(|l| l.keys().collect())
                .unwrap_or_default();
            others.sort();
            others
                .into_iter()
                .find(|k| !PREFERRED.contains(&k.as_str()) && discriminates(k))
                .cloned()
        })
}

#[cfg(test)]
mod tests {
    use super::discriminating_alias_key;
    use std::collections::BTreeMap;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn same_host_ab_aliases_off_the_label_that_differs() {
        // The case `record --endpoint :4241,source=redis --endpoint
        // :4242,source=valkey -o ab.rez` produces. Both recordings carry the
        // SAME host, so an arm-then-host precedence named both arms after the
        // host and the comparison could not be read. `--label` cannot fix it:
        // it applies to every recording in the run.
        let recs = vec![
            labels(&[("host", "alpha"), ("source", "redis")]),
            labels(&[("host", "alpha"), ("source", "valkey")]),
        ];
        assert_eq!(discriminating_alias_key(&recs).as_deref(), Some("source"));
    }

    #[test]
    fn multi_host_still_aliases_off_host() {
        // Two agents on different hosts, no --label: both are source=rezolus,
        // so keying on source would collapse them the other way.
        let recs = vec![
            labels(&[("host", "web-01"), ("source", "rezolus")]),
            labels(&[("host", "web-02"), ("source", "rezolus")]),
        ];
        assert_eq!(discriminating_alias_key(&recs).as_deref(), Some("host"));
    }

    #[test]
    fn arm_wins_when_it_discriminates() {
        let recs = vec![
            labels(&[("arm", "base"), ("host", "a"), ("source", "rezolus")]),
            labels(&[("arm", "exp"), ("host", "b"), ("source", "rezolus")]),
        ];
        assert_eq!(discriminating_alias_key(&recs).as_deref(), Some("arm"));
    }

    #[test]
    fn indistinguishable_recordings_fall_back_to_positional_names() {
        // The degenerate case the recorder warns about at startup.
        let recs = vec![
            labels(&[("host", "alpha"), ("source", "rezolus")]),
            labels(&[("host", "alpha"), ("source", "rezolus")]),
        ];
        assert_eq!(discriminating_alias_key(&recs), None);
    }

    #[test]
    fn a_single_recording_gets_no_alias_key() {
        // With one recording every predicate is vacuously true, so the first
        // merely-PRESENT preferred key would win. A hindsight snapshot's
        // labels are just `{source: rezolus}`, so it would be aliased
        // "rezolus" — strictly worse than the positional "baseline" it used to
        // get, and not a name at all.
        let recs = vec![labels(&[("source", "rezolus")])];
        assert_eq!(discriminating_alias_key(&recs), None);
        // Empty is worse still: `[].iter().all(..)` is true, so this returned
        // Some("arm") for no recordings at all.
        assert_eq!(discriminating_alias_key(&[]), None);
    }

    #[test]
    fn a_non_preferred_label_is_used_when_nothing_else_separates() {
        let recs = vec![
            labels(&[("host", "alpha"), ("source", "rezolus"), ("run", "1")]),
            labels(&[("host", "alpha"), ("source", "rezolus"), ("run", "2")]),
        ];
        assert_eq!(discriminating_alias_key(&recs).as_deref(), Some("run"));
    }
}
