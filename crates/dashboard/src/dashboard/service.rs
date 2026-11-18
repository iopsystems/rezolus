use crate::MetricsSource;
use crate::plot::*;
use crate::service_extension::ServiceExtension;

pub fn generate(
    data: &dyn MetricsSource,
    sections: Vec<Section>,
    service_ext: &ServiceExtension,
) -> View {
    let mut view = View::new(data, sections);

    // Embed service metadata in the view so the frontend can display it
    // without a separate API call.
    view.metadata.insert(
        "service_name".to_string(),
        serde_json::Value::String(service_ext.service_name.clone()),
    );
    if !service_ext.service_metadata.is_empty() {
        view.metadata.insert(
            "service_metadata".to_string(),
            serde_json::to_value(&service_ext.service_metadata).unwrap_or_default(),
        );
    }

    // Group KPIs by role. Within each role group, KPIs with the same
    // `subgroup` value land in a named subgroup; KPIs without a subgroup
    // land in the role's default unnamed subgroup (lazily created on
    // first use by `Group::plot_promql*`).
    let mut groups: Vec<(String, Group)> = Vec::new();
    let mut unavailable: Vec<serde_json::Value> = Vec::new();

    for kpi in &service_ext.kpis {
        if !kpi.available {
            unavailable.push(serde_json::json!({
                "title": kpi.title,
                "role": kpi.role,
                "query": kpi.query,
            }));
            continue;
        }

        let plot_id = format!("kpi-{}-{}", kpi.role, slug(&kpi.title));

        let group = match groups.iter_mut().find(|(r, _)| *r == kpi.role) {
            Some((_, g)) => g,
            None => {
                groups.push((
                    kpi.role.clone(),
                    Group::new(capitalize(&kpi.role), format!("kpi-{}", kpi.role)),
                ));
                &mut groups.last_mut().unwrap().1
            }
        };

        let opts = match kpi.metric_type.as_str() {
            "gauge" => Plot::gauge(&kpi.title, &plot_id, Unit::Count),
            "histogram" => Plot::histogram(
                &kpi.title,
                &plot_id,
                Unit::Count,
                crate::service_extension::kpi_histogram_view(kpi.subtype.as_deref()),
            ),
            _ => Plot::counter(&kpi.title, &plot_id, Unit::Count),
        };
        let opts = opts.maybe_unit_system(kpi.unit_system.as_deref());
        let opts = match stored_unit(data, &kpi.metric_type, &kpi.query) {
            Some(unit) => opts.with_stored_unit(unit),
            None => opts,
        };
        let opts = match &kpi.percentiles {
            Some(p) => opts.with_percentiles(p.clone()),
            None => opts,
        };

        // Resolve the destination subgroup. Named subgroup is opened on
        // first use; subsequent KPIs with the same name extend it.
        let sg = match kpi.subgroup.as_deref() {
            Some(name) => {
                if group.find_subgroup(name).is_none() {
                    let new_sg = group.subgroup(name);
                    if let Some(desc) = kpi.subgroup_description.as_deref() {
                        new_sg.describe(desc);
                    }
                    new_sg
                } else {
                    group.find_subgroup(name).unwrap()
                }
            }
            None => group.default_subgroup(),
        };

        if kpi.full_width {
            sg.plot_promql_full(opts, kpi.query.clone());
        } else {
            sg.plot_promql(opts, kpi.query.clone());
        }
    }

    if !unavailable.is_empty() {
        view.metadata.insert(
            "unavailable_kpis".to_string(),
            serde_json::Value::Array(unavailable),
        );
    }

    for (_, group) in groups {
        view.group(group);
    }

    view
}

/// The unit `query`'s series are stored in ([`MetricsSource::metric_unit`]),
/// when the chart draws them in that unit: a gauge or histogram KPI whose
/// query is a bare selector (`name` or `name{...}`), which the viewer
/// charts as stored or wraps in a quantile. A Prometheus `_seconds`
/// histogram recorded as served reads `seconds`, and the chart rescales it
/// for `unit_system: time`; a recording that stored it in nanoseconds has
/// no unit for it. Any other query (a rate, an arithmetic expression) has
/// changed the unit, so it is `None`.
pub(crate) fn stored_unit(
    data: &dyn MetricsSource,
    metric_type: &str,
    query: &str,
) -> Option<String> {
    if !matches!(metric_type, "gauge" | "histogram") {
        return None;
    }
    bare_selector_name(query).and_then(|name| data.metric_unit(name))
}

/// The metric name of a query that is a bare selector, `name` or
/// `name{matchers}`, else `None`.
pub(crate) fn bare_selector_name(query: &str) -> Option<&str> {
    let query = query.trim();
    let (name, rest) = match query.find('{') {
        Some(i) => (&query[..i], &query[i..]),
        None => (query, ""),
    };
    let mut chars = name.chars();
    let first = chars.next()?;
    let valid = (first.is_ascii_alphabetic() || first == '_' || first == ':')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':');
    let selector = rest.is_empty() || (rest.ends_with('}') && rest.matches('}').count() == 1);
    (valid && selector).then_some(name)
}

/// Convert a title into a kebab-case slug for use as a DOM id.
pub(crate) fn slug(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Capitalize the first letter of a string (for group titles).
pub(crate) fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriken_query::MemoryStore;
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    /// A store holding the gauges `wait_seconds` (unit `seconds`) and
    /// `depth` (no unit).
    fn store() -> MemoryStore {
        let store = MemoryStore::builder().sampling_interval_ms(1000).build();
        let gauge = |name: &str, unit: Option<&str>| {
            let mut metadata = HashMap::new();
            if let Some(unit) = unit {
                metadata.insert("unit".to_string(), unit.to_string());
            }
            metriken_exposition::Gauge::new(name.to_string(), 1, metadata)
        };
        store.ingest_snapshot(metriken_exposition::Snapshot::V2(
            metriken_exposition::SnapshotV2 {
                systemtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                duration: Duration::from_secs(0),
                metadata: HashMap::new(),
                counters: vec![],
                gauges: vec![gauge("wait_seconds", Some("seconds")), gauge("depth", None)],
                histograms: vec![],
                gauge_histograms: Vec::new(),
            },
        ));
        store
    }

    #[test]
    fn a_bare_selector_names_its_metric() {
        assert_eq!(bare_selector_name("rpc_seconds"), Some("rpc_seconds"));
        assert_eq!(
            bare_selector_name(" rpc_seconds{stage=\"decode\"} "),
            Some("rpc_seconds")
        );
        assert_eq!(bare_selector_name("ns:rpc_seconds"), Some("ns:rpc_seconds"));
        for query in [
            "",
            "{stage=\"decode\"}",
            "sum(rpc_seconds)",
            "rate(rpc_seconds[1m])",
            "rpc_seconds / 2",
            "rpc_seconds{a=\"b\"} / x{c=\"d\"}",
            "9lives",
        ] {
            assert_eq!(bare_selector_name(query), None, "{query}");
        }
    }

    #[test]
    fn a_gauge_or_histogram_kpi_over_a_bare_selector_takes_the_stored_unit() {
        let store = store();
        assert_eq!(
            stored_unit(&store, "gauge", "wait_seconds").as_deref(),
            Some("seconds")
        );
        assert_eq!(
            stored_unit(&store, "histogram", "wait_seconds{a=\"b\"}").as_deref(),
            Some("seconds")
        );
        // A counter KPI is a rate, and an expression has changed the unit.
        assert_eq!(stored_unit(&store, "counter", "wait_seconds"), None);
        assert_eq!(stored_unit(&store, "gauge", "wait_seconds * 1e9"), None);
        // A series with no unit key, and a metric the store lacks.
        assert_eq!(stored_unit(&store, "gauge", "depth"), None);
        assert_eq!(stored_unit(&store, "gauge", "absent"), None);
    }
}
