//! Selecting one recording out of a multi-recording `.rez` archive.
//!
//! Pure logic over label sets: no file access, no reader. Kept separate from
//! `mcp/mod.rs` because this is the part most worth testing directly, and
//! because `mod.rs` is already large.
//!
//! Nothing here is called from production code yet, only the tests below, so
//! `dead_code` is silenced module-wide. Both allows (here and on the
//! `mod.rs` re-export) come off once the MCP CLI/server actually pass a
//! selector through to the reader.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fmt;

/// Labels a recording must carry to be selected.
///
/// Subset semantics: every `k=v` here must appear in the recording's labels,
/// but the recording may carry more. Recordings auto-carry `source` and
/// `host` plus any `record --label`, so requiring the full set would make
/// selectors long and brittle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RecordingSelector {
    // `BTreeMap`, not `HashMap`: `Display` renders selector text into error
    // messages, so iteration order has to be deterministic (pinned by
    // `display_orders_pairs_deterministically` below); a `BTreeMap` also
    // matches `rez_sqlite::RecordingMeta.labels`, so a manifest's labels can
    // be consumed with no conversion.
    labels: BTreeMap<String, String>,
}

/// Why a selector did not name exactly one recording.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SelectError {
    /// No recording carries every label in the selector.
    NoMatch,
    /// Several do; the indices of those that matched.
    Ambiguous(Vec<usize>),
}

impl RecordingSelector {
    /// Parse repeated `k=v` arguments (the CLI shape).
    ///
    /// A key must be non-empty and appear at most once: this is the one
    /// module whose entire job is refusing to resolve ambiguity silently, so
    /// letting `--recording host=a --recording host=b` collapse to `host=b`
    /// via last-write-wins would be that same failure creeping in one layer
    /// earlier. A value may itself contain `=` (`split_once` splits on the
    /// first one only), which is deliberate: label values are free text.
    pub(crate) fn parse(pairs: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut labels = BTreeMap::new();
        for p in pairs {
            let (k, v) = p
                .split_once('=')
                .ok_or_else(|| format!("--recording expects key=value, got {p:?} with no '='"))?;
            if k.is_empty() {
                return Err(format!(
                    "--recording expects key=value, got {p:?} with an empty key"
                ));
            }
            if let Some(prev) = labels.insert(k.to_string(), v.to_string()) {
                return Err(format!(
                    "--recording {k}= specified more than once ({prev:?} and {v:?})"
                ));
            }
        }
        Ok(Self { labels })
    }

    /// Parse a JSON object of label key to value (the stdio-server shape).
    pub(crate) fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        let obj = v
            .as_object()
            .ok_or_else(|| "recording must be an object of label key to value".to_string())?;
        let mut labels = BTreeMap::new();
        for (k, val) in obj {
            let s = val
                .as_str()
                .ok_or_else(|| format!("recording label {k:?} must be a string"))?;
            labels.insert(k.clone(), s.to_string());
        }
        Ok(Self { labels })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    /// Whether `labels` carries every pair in this selector.
    pub(crate) fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        self.labels
            .iter()
            .all(|(k, v)| labels.get(k).is_some_and(|got| got == v))
    }

    /// The index of the single recording this selector names.
    ///
    /// An EMPTY selector matches every candidate, so with several recordings
    /// it resolves to `Ambiguous` — "no selector given" and "an ambiguous
    /// selector" reach the caller through one path, and there is a single
    /// place that decides what to say.
    pub(crate) fn resolve(
        &self,
        candidates: &[BTreeMap<String, String>],
    ) -> Result<usize, SelectError> {
        let hits: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, l)| self.matches(l))
            .map(|(i, _)| i)
            .collect();
        match hits.as_slice() {
            [] => Err(SelectError::NoMatch),
            [one] => Ok(*one),
            _ => Err(SelectError::Ambiguous(hits)),
        }
    }
}

impl fmt::Display for RecordingSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered: Vec<String> = self
            .labels
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        write!(f, "{}", rendered.join(","))
    }
}

/// Render specific recordings from an archive, each with the flag that picks
/// it.
///
/// `all` is every recording's labels; `highlight` is which indices of `all`
/// to render (an empty slice means "render all of them"). Uniqueness is
/// always computed against `all`, never against `highlight` — that split is
/// deliberate and prevents a real bug: describing only a matched subset
/// (e.g. the indices `resolve` returned inside `Ambiguous`) using that
/// subset as the uniqueness universe would call a label unique because it
/// only looks that way among the couple of recordings being described, then
/// hand back a `--recording` that still collides with a THIRD recording
/// sitting elsewhere in the archive. Taking `all` separately from
/// `highlight` makes that mistake impossible to write.
///
/// Printing the selector rather than only the labels is the point: the
/// caller's next command is a copy of a line it was just given.
///
/// Deliberately unnumbered — an index would invite `--recording 1`, which is
/// not a supported selector.
pub(crate) fn describe_candidates(all: &[BTreeMap<String, String>], highlight: &[usize]) -> String {
    let indices: Vec<usize> = if highlight.is_empty() {
        (0..all.len()).collect()
    } else {
        highlight.to_vec()
    };

    let mut out = String::new();
    for i in indices {
        let labels = &all[i];
        let rendered: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();

        // Two recordings sharing every label are indistinguishable to any
        // selector, and a recording with no labels at all sitting among
        // labeled peers is unselectable for the same reason (no combination
        // of its labels — there are none — can single it out). Echoes the
        // recorder's own `warn_if_indistinguishable`, so both messages point
        // at the same fix.
        let unselectable =
            labels.is_empty() || all.iter().enumerate().any(|(j, c)| j != i && c == labels);
        if unselectable {
            out.push_str(&format!(
                "  - {}\n    cannot be selected by labels — no combination of its labels picks \
                 it out from every other recording in the archive; re-capture giving each \
                 --endpoint its own source=NAME\n",
                rendered.join(", ")
            ));
            continue;
        }

        // Any single label unique to this recording (checked against `all`,
        // not just `highlight`) selects it on its own; take the first in key
        // order so the listing is byte-stable across runs. If none exists,
        // fall back to the whole set, one pair per `--recording` flag (the
        // flag is repeatable and ANDs its pairs together).
        let unique = labels
            .iter()
            .filter(|(k, v)| {
                all.iter()
                    .filter(|c| c.get(*k).is_some_and(|got| got == *v))
                    .count()
                    == 1
            })
            .map(|(k, v)| format!("{k}={v}"))
            .next();
        let picker = unique.unwrap_or_else(|| rendered.join(" --recording "));

        out.push_str(&format!(
            "  - {}\n    select with: --recording {picker}\n",
            rendered.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_key_value_pairs() {
        let s = RecordingSelector::parse(["source=redis".to_string()]).unwrap();
        assert!(!s.is_empty());
        assert_eq!(s.to_string(), "source=redis");
    }

    #[test]
    fn a_value_without_an_equals_is_an_error() {
        // Not silently ignored: `--recording redis` is a plausible typo for
        // `--recording source=redis`, and dropping it would run against the
        // wrong recording rather than say so.
        let err = RecordingSelector::parse(["redis".to_string()]).unwrap_err();
        assert!(
            err.contains("redis"),
            "the error must quote the input: {err}"
        );
        assert!(err.contains('='), "and say what was expected: {err}");
    }

    #[test]
    fn an_empty_key_is_an_error() {
        // `--recording =redis` is a typo (probably for `source=redis`), not
        // a label named "". Left unchecked it would silently match no
        // recording ever, since `matches` looks the empty key up on real
        // label maps and never finds it, so the failure would surface as
        // "no recording matches" instead of "that isn't a key=value".
        let err = RecordingSelector::parse(["=redis".to_string()]).unwrap_err();
        assert!(
            err.contains("=redis"),
            "the error must quote the input: {err}"
        );
        assert!(err.contains("empty key"), "{err}");
    }

    #[test]
    fn an_empty_value_is_fine() {
        // Unlike an empty key, an empty value is a legitimate label value,
        // not a typo, so it must not be rejected the same way.
        let s = RecordingSelector::parse(["source=".to_string()]).unwrap();
        assert!(s.matches(&labels(&[("source", "")])));
    }

    #[test]
    fn a_repeated_key_is_an_error() {
        // Silently taking the last value would be the exact ambiguity this
        // module exists to refuse, just resolved one step before `resolve`
        // ever runs.
        let err =
            RecordingSelector::parse(["host=a".to_string(), "host=b".to_string()]).unwrap_err();
        assert!(err.contains("host"), "{err}");
        assert!(err.contains('a') && err.contains('b'), "{err}");
    }

    #[test]
    fn a_value_may_itself_contain_an_equals_sign() {
        // `split_once` (not `split('=')`) is deliberate: a label value is
        // free text and may legitimately contain '='. Pinned here so a
        // future "simplify this" pass doesn't swap it and silently truncate
        // values like this one.
        let s = RecordingSelector::parse(["label=a=b".to_string()]).unwrap();
        assert!(s.matches(&labels(&[("label", "a=b")])));
    }

    #[test]
    fn an_empty_selector_matches_everything() {
        let s = RecordingSelector::default();
        assert!(s.is_empty());
        // An empty selector matches everything, which is what makes
        // "no selector given" fall through to the caller's own policy
        // rather than being a special case inside the matcher.
        assert!(s.matches(&labels(&[("source", "redis")])));
    }

    #[test]
    fn matching_is_a_subset_not_an_equality() {
        let s = RecordingSelector::parse(["source=redis".to_string()]).unwrap();
        assert!(s.matches(&labels(&[("source", "redis"), ("host", "web-01")])));
    }

    #[test]
    fn a_differing_value_does_not_match() {
        let s = RecordingSelector::parse(["source=redis".to_string()]).unwrap();
        assert!(!s.matches(&labels(&[("source", "valkey")])));
    }

    #[test]
    fn a_missing_key_does_not_match() {
        let s = RecordingSelector::parse(["arm=a".to_string()]).unwrap();
        assert!(!s.matches(&labels(&[("source", "redis")])));
    }

    #[test]
    fn several_pairs_are_anded() {
        let s = RecordingSelector::parse(["source=redis".to_string(), "host=web-01".to_string()])
            .unwrap();
        assert!(s.matches(&labels(&[("source", "redis"), ("host", "web-01")])));
        assert!(!s.matches(&labels(&[("source", "redis"), ("host", "web-02")])));
    }

    #[test]
    fn display_orders_pairs_deterministically() {
        // Not just "some order" — swapping the backing map for a `HashMap`
        // must not pass this test, since `Display` output lands in error
        // messages that need to be reproducible.
        let s = RecordingSelector::parse(["host=b".to_string(), "source=a".to_string()]).unwrap();
        assert_eq!(s.to_string(), "host=b,source=a");
    }

    #[test]
    fn parses_a_json_object() {
        let v = serde_json::json!({"source": "redis"});
        let s = RecordingSelector::from_json(&v).unwrap();
        assert_eq!(s.to_string(), "source=redis");
    }

    #[test]
    fn a_non_object_json_selector_is_an_error() {
        let err = RecordingSelector::from_json(&serde_json::json!("redis")).unwrap_err();
        assert!(err.contains("object"), "{err}");
    }

    #[test]
    fn a_non_string_json_value_is_an_error() {
        // An LLM-driven MCP client emitting a bare number for a
        // numeric-looking label (`{"arm": 1}`) is realistic; the error it
        // sees needs to name the offending key.
        let err = RecordingSelector::from_json(&serde_json::json!({"arm": 1})).unwrap_err();
        assert!(err.contains("arm"), "{err}");
    }

    fn two_arms() -> Vec<BTreeMap<String, String>> {
        vec![
            labels(&[("source", "redis"), ("host", "web-01")]),
            labels(&[("source", "valkey"), ("host", "web-01")]),
        ]
    }

    #[test]
    fn resolves_to_the_one_matching_recording() {
        let s = RecordingSelector::parse(["source=valkey".to_string()]).unwrap();
        assert_eq!(s.resolve(&two_arms()), Ok(1));
    }

    #[test]
    fn no_match_is_an_error_not_a_default() {
        let s = RecordingSelector::parse(["source=nope".to_string()]).unwrap();
        assert_eq!(s.resolve(&two_arms()), Err(SelectError::NoMatch));
    }

    #[test]
    fn resolve_against_no_candidates_is_no_match() {
        // Not a panic, not "vacuously ambiguous" — an archive that somehow
        // holds no recordings is just as much "no match" as one that holds
        // several none of which fit.
        let s = RecordingSelector::default();
        assert_eq!(s.resolve(&[]), Err(SelectError::NoMatch));
    }

    #[test]
    fn several_matches_is_an_error_not_the_first() {
        // Both arms share host=web-01. Picking the first would silently
        // analyze one arm of an A/B and present it as the answer.
        let s = RecordingSelector::parse(["host=web-01".to_string()]).unwrap();
        assert_eq!(
            s.resolve(&two_arms()),
            Err(SelectError::Ambiguous(vec![0, 1]))
        );
    }

    #[test]
    fn ambiguous_reports_only_the_indices_that_matched() {
        // Three recordings, exactly two ("arm=a") match, and they are not
        // contiguous from zero. `two_arms` alone can't catch a `resolve`
        // that returns `(0..candidates.len())` instead of the real hits,
        // since there every candidate matches every ambiguous test selector.
        // The caller uses these indices to list which recordings the
        // selector matched, so a wrong payload here would show the user
        // recordings their selector never named.
        let candidates = vec![
            labels(&[("arm", "a"), ("host", "1")]),
            labels(&[("arm", "b"), ("host", "2")]),
            labels(&[("arm", "a"), ("host", "3")]),
        ];
        let s = RecordingSelector::parse(["arm=a".to_string()]).unwrap();
        assert_eq!(
            s.resolve(&candidates),
            Err(SelectError::Ambiguous(vec![0, 2]))
        );
    }

    #[test]
    fn an_empty_selector_over_several_recordings_is_ambiguous() {
        // "No selector given" reaches the caller as ambiguity over all of
        // them, so there is one code path for "you must choose".
        let s = RecordingSelector::default();
        assert_eq!(
            s.resolve(&two_arms()),
            Err(SelectError::Ambiguous(vec![0, 1]))
        );
    }

    #[test]
    fn an_empty_selector_over_one_recording_resolves() {
        let s = RecordingSelector::default();
        assert_eq!(s.resolve(&two_arms()[..1]), Ok(0));
    }

    /// Parse the `--recording ...` lines a listing prints and confirm each
    /// one, fed straight back through `parse` and `resolve`, actually names
    /// the recording it was printed under.
    ///
    /// A round trip is the only check strong enough for this job: a `picker`
    /// that is always the first label in key order (for `two_arms`, that's
    /// `host=web-01` for BOTH recordings, since "host" sorts before
    /// "source") would still pass plain substring assertions — `out` would
    /// contain "source=redis", "source=valkey", and "--recording" no matter
    /// what `picker` actually was. Feeding it back through `resolve` is what
    /// catches a selector that looks plausible but doesn't work.
    fn assert_listing_round_trips(all: &[BTreeMap<String, String>], out: &str, expect: &[usize]) {
        let pickers: Vec<&str> = out
            .lines()
            .filter_map(|l| l.trim_start().strip_prefix("select with: --recording "))
            .collect();
        assert_eq!(
            pickers.len(),
            expect.len(),
            "one selector line per rendered recording: {out}"
        );
        for (picker, &i) in pickers.iter().zip(expect) {
            let pairs: Vec<String> = picker.split(" --recording ").map(str::to_string).collect();
            let s = RecordingSelector::parse(pairs).unwrap_or_else(|e| {
                panic!("emitted an unparseable selector for recording {i}: {e} ({picker:?})")
            });
            assert_eq!(
                s.resolve(all),
                Ok(i),
                "the selector printed for recording {i} must resolve back to it: {picker:?}\n{out}"
            );
        }
    }

    #[test]
    fn the_candidate_listing_names_every_recording_and_its_selector() {
        let all = two_arms();
        let out = describe_candidates(&all, &[]);
        assert!(out.contains("source=redis"), "{out}");
        assert!(out.contains("source=valkey"), "{out}");
        assert_listing_round_trips(&all, &out, &[0, 1]);
        // No indices: they invite `--recording 1`, which is not a selector.
        // Checked against a few plausible numbering styles, not just one.
        for pat in ["[1]", "[2]", "(1)", "(2)", "1.", "2.", "#1", "#2"] {
            assert!(!out.contains(pat), "found index-like text {pat:?}: {out}");
        }
    }

    #[test]
    fn describe_candidates_computes_uniqueness_against_the_full_archive_not_the_highlighted_subset()
    {
        // `arm=a` matches recordings 0 and 1 (Ambiguous([0, 1])). Recording 2
        // is not in the highlighted set, but it still carries `host=1`, the
        // same as recording 0 — so `host=1` must NOT be offered as recording
        // 0's selector, even though within {0, 1} alone it would look
        // unique.
        let all = vec![
            labels(&[("arm", "a"), ("host", "1")]),
            labels(&[("arm", "a"), ("host", "2")]),
            labels(&[("arm", "b"), ("host", "1")]),
        ];
        assert_eq!(
            RecordingSelector::parse(["arm=a".to_string()])
                .unwrap()
                .resolve(&all),
            Err(SelectError::Ambiguous(vec![0, 1]))
        );

        let out = describe_candidates(&all, &[0, 1]);
        assert_listing_round_trips(&all, &out, &[0, 1]);
    }

    #[test]
    fn identical_labels_are_reported_as_unselectable_not_a_dead_end_selector() {
        // Two recordings that carry exactly the same labels: the recorder
        // permits this (warning only) and the reader treats it as legal, so
        // this is reachable, not hypothetical. Offering a `--recording` for
        // either one would be a dead end — pasting it resolves to
        // `Ambiguous` again, forever.
        let dup = vec![
            labels(&[("host", "web-01"), ("source", "redis")]),
            labels(&[("host", "web-01"), ("source", "redis")]),
        ];
        let out = describe_candidates(&dup, &[]);
        assert!(
            !out.contains("select with: --recording"),
            "no selector can pick between identical label sets, so none should be offered: {out}"
        );
        assert!(
            out.contains("re-capture") && out.contains("source=NAME"),
            "must explain why, echoing the recorder's own warning: {out}"
        );
    }
}
