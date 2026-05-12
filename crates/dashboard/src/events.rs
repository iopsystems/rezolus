use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A one-off event annotation attached to a parquet recording. Events mark
/// key moments (restarts, config changes, deploys, anomalies, ...) on top of
/// existing time-series metrics. They are stored as a JSON blob in the
/// parquet footer under [`KEY_EVENTS`](crate::events) and are self-describing
/// — every event carries its own optional `source` / `node` / `instance`
/// scope rather than inheriting from file-level metadata.
///
/// Field semantics:
/// - `timestamp` is nanoseconds since the Unix epoch. Required.
/// - `description` is a short title rendered inline next to the marker.
/// - `kind` is a free-form tag (`restart`, `config_change`, `deploy`,
///   `incident`, `anomaly`, `marker`, `note`, ...). Conventions only, not
///   validated, so users may invent their own without a release.
/// - `details` is longer optional text (e.g. a paragraph of context).
/// - `source` / `node` / `instance` scope the event to a specific
///   recording stream within a (possibly combined) file. When all three are
///   absent the event is global.
/// - `labels` is an open map for arbitrary user tags.
/// - `duration_ns` lets an event span an interval rather than a point —
///   when absent the event renders as a vertical line, when present as a
///   band.
/// - `id` is an optional stable identifier used to dedupe across merges.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub timestamp: u64,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// Wrapper for the JSON payload stored in the parquet footer. Lives as an
/// object with a single `events` array so future fields (schema version,
/// global labels) can be added without breaking parsers.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Events {
    #[serde(default)]
    pub events: Vec<Event>,
}

impl Events {
    pub fn new(events: Vec<Event>) -> Self {
        Self { events }
    }

    /// Sort events by timestamp and drop later duplicates that share the
    /// same non-empty `id`. Stable: earlier occurrences of a given id win.
    pub fn normalize(&mut self) {
        self.events
            .sort_by_key(|e| (e.timestamp, e.id.clone().unwrap_or_default()));
        let mut seen = std::collections::HashSet::new();
        self.events.retain(|e| match &e.id {
            Some(id) if !id.is_empty() => seen.insert(id.clone()),
            _ => true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_minimal_event() {
        let json = r#"{"timestamp":1700000000000000000,"description":"restart"}"#;
        let e: Event = serde_json::from_str(json).unwrap();
        assert_eq!(e.timestamp, 1_700_000_000_000_000_000);
        assert_eq!(e.description, "restart");
        assert!(e.kind.is_none());
        // Optional fields are skipped on serialize
        assert_eq!(serde_json::to_string(&e).unwrap(), json);
    }

    #[test]
    fn round_trips_full_event() {
        let e = Event {
            timestamp: 1,
            description: "d".into(),
            kind: Some("restart".into()),
            details: Some("long".into()),
            source: Some("vllm".into()),
            node: Some("gpu01".into()),
            instance: Some("0".into()),
            labels: BTreeMap::from([("reason".into(), "OOM".into())]),
            duration_ns: Some(1000),
            id: Some("evt-1".into()),
        };
        let json = serde_json::to_string(&e).unwrap();
        let parsed: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, e);
    }

    #[test]
    fn normalize_sorts_and_dedupes_by_id() {
        let mut events = Events::new(vec![
            Event {
                timestamp: 30,
                description: "later".into(),
                id: Some("a".into()),
                kind: None,
                details: None,
                source: None,
                node: None,
                instance: None,
                labels: BTreeMap::new(),
                duration_ns: None,
            },
            Event {
                timestamp: 10,
                description: "first".into(),
                id: Some("a".into()),
                kind: None,
                details: None,
                source: None,
                node: None,
                instance: None,
                labels: BTreeMap::new(),
                duration_ns: None,
            },
            Event {
                timestamp: 20,
                description: "no id, kept".into(),
                id: None,
                kind: None,
                details: None,
                source: None,
                node: None,
                instance: None,
                labels: BTreeMap::new(),
                duration_ns: None,
            },
        ]);
        events.normalize();
        let descs: Vec<&str> = events
            .events
            .iter()
            .map(|e| e.description.as_str())
            .collect();
        assert_eq!(descs, vec!["first", "no id, kept"]);
    }

    #[test]
    fn normalize_keeps_all_when_ids_missing_or_empty() {
        let make = |ts: u64, id: Option<&str>| Event {
            timestamp: ts,
            description: ts.to_string(),
            id: id.map(str::to_string),
            kind: None,
            details: None,
            source: None,
            node: None,
            instance: None,
            labels: BTreeMap::new(),
            duration_ns: None,
        };
        let mut events = Events::new(vec![make(2, None), make(1, Some("")), make(3, Some(""))]);
        events.normalize();
        // Empty id is treated as no-id; nothing gets deduped.
        assert_eq!(events.events.len(), 3);
        assert_eq!(
            events
                .events
                .iter()
                .map(|e| e.timestamp)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }
}
