use metriken_query::MetricsSource;
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Serialize)]
pub struct MetricInfo {
    pub name: String,
    pub metric_type: String,
    pub series_count: usize,
    pub label_keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Serialize)]
pub struct MetricsResponse {
    pub source: String,
    pub metrics: Vec<MetricInfo>,
}

fn info_for(
    name: &str,
    metric_type: &str,
    labels: Vec<std::collections::BTreeMap<String, String>>,
    descriptions: &serde_json::Map<String, serde_json::Value>,
) -> MetricInfo {
    let keys: BTreeSet<String> = labels.iter().flat_map(|m| m.keys().cloned()).collect();
    MetricInfo {
        name: name.to_string(),
        metric_type: metric_type.to_string(),
        series_count: labels.len(),
        label_keys: keys.into_iter().collect(),
        description: descriptions
            .get(name)
            .and_then(|v| v.as_str())
            .map(str::to_string),
    }
}

/// Assemble the metric catalog from a source's schema.
///
/// `_source_filter` is reserved for combined-file filtering (Task 5);
/// single-source files expose every metric regardless.
pub fn assemble_catalog(
    data: &dyn MetricsSource,
    descriptions: &serde_json::Map<String, serde_json::Value>,
    _source_filter: Option<&str>,
) -> Vec<MetricInfo> {
    let mut out = Vec::new();
    for name in data.counter_names() {
        let labels = data.counter_labels(&name);
        out.push(info_for(&name, "counter", labels, descriptions));
    }
    for name in data.gauge_names() {
        let labels = data.gauge_labels(&name);
        out.push(info_for(&name, "gauge", labels, descriptions));
    }
    for name in data.histogram_names() {
        let labels = data.histogram_labels(&name);
        out.push(info_for(&name, "histogram", labels, descriptions));
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn labels_from(sets: &[&[(&str, &str)]]) -> Vec<BTreeMap<String, String>> {
        sets.iter()
            .map(|pairs| {
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            })
            .collect()
    }

    fn desc(entries: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect()
    }

    // ── info_for: metric_type passthrough ────────────────────────────────────

    #[test]
    fn metric_type_passes_through() {
        let no_desc = desc(&[]);
        let info = info_for("m", "counter", vec![], &no_desc);
        assert_eq!(info.metric_type, "counter");

        let info = info_for("m", "gauge", vec![], &no_desc);
        assert_eq!(info.metric_type, "gauge");

        let info = info_for("m", "histogram", vec![], &no_desc);
        assert_eq!(info.metric_type, "histogram");
    }

    // ── info_for: series_count == labels.len() ────────────────────────────────

    #[test]
    fn series_count_equals_label_set_count() {
        let no_desc = desc(&[]);
        let two_series = labels_from(&[&[("cpu", "0")], &[("cpu", "1")]]);
        let info = info_for("cpu_cycles", "counter", two_series, &no_desc);
        assert_eq!(info.series_count, 2);

        let info = info_for("load", "gauge", vec![], &no_desc);
        assert_eq!(info.series_count, 0);
    }

    // ── info_for: label_keys is sorted union of all keys ─────────────────────

    #[test]
    fn label_keys_are_sorted_union() {
        let no_desc = desc(&[]);
        // Series 0 has keys "cpu"; series 1 has keys "cpu" and "mode".
        // Union should be ["cpu", "mode"].
        let sets = labels_from(&[&[("cpu", "0")], &[("cpu", "1"), ("mode", "user")]]);
        let info = info_for("metric", "counter", sets, &no_desc);
        assert_eq!(info.label_keys, vec!["cpu", "mode"]);
    }

    #[test]
    fn label_keys_deduplicated_and_sorted() {
        let no_desc = desc(&[]);
        // All three series have the same key; result must not repeat it.
        let sets = labels_from(&[
            &[("z_key", "a")],
            &[("a_key", "b"), ("z_key", "c")],
            &[("m_key", "d")],
        ]);
        let info = info_for("m", "histogram", sets, &no_desc);
        assert_eq!(info.label_keys, vec!["a_key", "m_key", "z_key"]);
    }

    // ── info_for: description populated when key present, None when absent ───

    #[test]
    fn description_from_map_when_present() {
        let descriptions = desc(&[("cpu_cycles", "CPU cycle counter")]);
        let info = info_for("cpu_cycles", "counter", vec![], &descriptions);
        assert_eq!(info.description.as_deref(), Some("CPU cycle counter"));
    }

    #[test]
    fn description_is_none_when_absent() {
        let descriptions = desc(&[("other_metric", "something")]);
        let info = info_for("cpu_cycles", "counter", vec![], &descriptions);
        assert!(info.description.is_none());
    }

    #[test]
    fn description_is_none_for_empty_map() {
        let info = info_for("m", "gauge", vec![], &desc(&[]));
        assert!(info.description.is_none());
    }
}
