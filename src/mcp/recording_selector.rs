//! Selecting one recording out of a multi-recording `.rez` archive.
//!
//! Pure logic over label sets: no file access, no reader. Kept separate from
//! `mcp/mod.rs` because this is the part most worth testing directly, and
//! because `mod.rs` is already large.
//!
//! Nothing here is called from production code yet, only the tests below, so
//! `dead_code` is silenced module-wide. Both allows (here and on the
//! `mod.rs` re-export) come off once `resolve` lands and the MCP CLI/server
//! actually pass a selector through to the reader.
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
}
