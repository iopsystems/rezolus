//! Save-as-Report write-side helpers — column-trim a source parquet
//! (or a combined-A/B tarball's per-side parquets) down to just the
//! columns referenced by the saved selection's queries.

use serde::Deserialize;

/// Subset of the JSON body POSTed to `/api/v1/save_with_selection`
/// that the trim path actually consumes. The full body carries more
/// (tagline, anchors, chartToggles, time_range, …) — we ignore those
/// here because they don't influence which columns the report needs.
#[derive(Debug, Clone, Deserialize)]
pub struct ReportPayload {
    #[serde(default)]
    pub entries: Vec<ReportEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReportEntry {
    pub promql_query: String,
    #[serde(default)]
    pub promql_query_experiment: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_payload() {
        let json = r#"{
            "version": 1,
            "entries": [
                {"chartId": "c1", "promql_query": "cpu_cores"},
                {
                    "chartId": "c2",
                    "promql_query": "cpu_usage",
                    "promql_query_experiment": "cpu_usage{state=\"user\"}"
                }
            ]
        }"#;
        let payload: ReportPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.entries.len(), 2);
        assert_eq!(payload.entries[0].promql_query, "cpu_cores");
        assert_eq!(payload.entries[1].promql_query, "cpu_usage");
        assert_eq!(
            payload.entries[1].promql_query_experiment.as_deref(),
            Some("cpu_usage{state=\"user\"}")
        );
    }

    #[test]
    fn experiment_query_optional() {
        let json = r#"{ "entries": [{"chartId": "c", "promql_query": "m"}] }"#;
        let payload: ReportPayload = serde_json::from_str(json).unwrap();
        assert!(payload.entries[0].promql_query_experiment.is_none());
    }
}
