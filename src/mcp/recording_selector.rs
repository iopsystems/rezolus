//! Selecting one recording out of a multi-recording `.rez` archive.
//!
//! Pure logic over label sets: no file access, no reader. Kept separate from
//! `mcp/mod.rs` because this is the part most worth testing directly, and
//! because `mod.rs` is already large.
//!
//! Nothing here is called from production code yet — only the tests below —
//! so `dead_code` is silenced module-wide until Task 2 wires `resolve` into
//! the CLI and stdio server.
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
    pub(crate) fn parse(pairs: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut labels = BTreeMap::new();
        for p in pairs {
            let (k, v) = p
                .split_once('=')
                .ok_or_else(|| format!("--recording expects key=value, got {p:?} with no '='"))?;
            labels.insert(k.to_string(), v.to_string());
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
    fn an_empty_selector_selects_nothing_in_particular() {
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
    fn parses_a_json_object() {
        let v = serde_json::json!({"source": "redis"});
        let s = RecordingSelector::from_json(&v).unwrap();
        assert_eq!(
            s,
            RecordingSelector::parse(["source=redis".to_string()]).unwrap()
        );
    }

    #[test]
    fn a_non_object_json_selector_is_an_error() {
        let err = RecordingSelector::from_json(&serde_json::json!("redis")).unwrap_err();
        assert!(err.contains("object"), "{err}");
    }
}
