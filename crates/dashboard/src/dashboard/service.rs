use crate::data::DashboardData;
use crate::plot::*;
use crate::service_extension::ServiceExtension;

/// Substitute `{{view}}` in a KPI SQL string with the source-specific
/// view name (`_src_<sanitized-source>`). Mirrors the wasm viewer's
/// `viewNameForSource` rule: non-`[a-zA-Z0-9_]` chars in the source
/// name become `_`, so `vllm-prefill` resolves to `_src_vllm_prefill`
/// on both backends. Authors write `{{view}}` once and the same KPI
/// template renders correctly across every parquet that ships a
/// matching source.
///
/// Shared between the dashboard emitter (here) and `parquet annotate`'s
/// KPI validator (`src/parquet_tools/annotate.rs`), which both need to
/// resolve the placeholder to a runnable SQL string against the same
/// engine-side per-source view.
pub fn substitute_view(sql: &str, source: &str) -> String {
    let mut view = String::with_capacity(source.len() + 5);
    view.push_str("_src_");
    for ch in source.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            view.push(ch);
        } else {
            view.push('_');
        }
    }
    sql.replace("{{view}}", &view)
}

pub fn generate(
    data: &dyn DashboardData,
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
    // first use by `Group::plot_sql*`).
    let mut groups: Vec<(String, Group)> = Vec::new();
    let mut unavailable: Vec<serde_json::Value> = Vec::new();

    for kpi in &service_ext.kpis {
        if !kpi.available {
            unavailable.push(serde_json::json!({
                "title": kpi.title,
                "role": kpi.role,
            }));
            continue;
        }

        // KPIs without `sql` are skipped entirely — the frontend
        // renders them as `_unavailable` placeholder cards via the
        // silent-render path. Transcribe SQL in the template to make
        // a KPI plot render.
        let Some(sql) = kpi.sql.as_deref() else {
            continue;
        };

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
            "gauge" => PlotOpts::gauge(&kpi.title, &plot_id, Unit::Count),
            "histogram" => PlotOpts::histogram(
                &kpi.title,
                &plot_id,
                Unit::Count,
                kpi.subtype.as_deref().unwrap_or("percentiles"),
            ),
            _ => PlotOpts::counter(&kpi.title, &plot_id, Unit::Count),
        };
        let opts = opts.maybe_unit_system(kpi.unit_system.as_deref());
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

        // Resolve `{{view}}` against the service name and emit the
        // plot (full-width or half).
        let sql = substitute_view(sql, &service_ext.service_name);
        if kpi.full_width {
            sg.plot_sql_full(opts, sql);
        } else {
            sg.plot_sql(opts, sql);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EmptyDashboardData;
    use crate::service_extension::{Kpi, ServiceExtension};
    use std::collections::HashMap;

    fn kpi(title: &str, query: &str, sql: Option<&str>) -> Kpi {
        Kpi {
            role: "throughput".to_string(),
            title: title.to_string(),
            description: None,
            sql: sql.map(str::to_string),
            metric_type: "delta_counter".to_string(),
            subtype: None,
            unit_system: Some("rate".to_string()),
            percentiles: None,
            available: true,
            denominator: false,
            subgroup: None,
            subgroup_description: None,
            full_width: false,
        }
    }

    /// `sql: None` KPIs are skipped entirely after the P3 PromQL-emitter
    /// purge — the frontend no longer renders PromQL, so a plot that
    /// only carries `promql_query` produces nothing useful. KPIs pending
    /// SQL transcription will reappear once their template gains a `sql`
    /// field. SQL-bearing KPIs emit `sql_query` and never `promql_query`
    /// (the field is reserved for a P4 struct deletion).
    #[test]
    fn kpi_sql_none_is_skipped() {
        let ext = ServiceExtension {
            service_name: "vllm".to_string(),
            aliases: vec![],
            service_metadata: HashMap::new(),
            slo: None,
            kpis: vec![
                kpi("Rate (SQL)", "rate_promql", Some("SELECT 1")),
                kpi("Rate (legacy)", "legacy_promql", None),
            ],
        };

        let view = generate(&EmptyDashboardData, vec![], &ext);
        let json = serde_json::to_string(&view).unwrap();

        // The SQL kpi's body is serialized; the legacy kpi is absent
        // from the JSON entirely (no plot, no promql_query reference).
        assert!(
            json.contains("\"sql_query\":\"SELECT 1\""),
            "SQL kpi's sql_query body missing: {json}"
        );
        assert_eq!(
            json.matches("\"sql_query\"").count(),
            1,
            "expected exactly one sql_query field (the SQL kpi); got: {json}"
        );
        // The PromQL-only KPI is gone; neither its title nor its query
        // string should appear anywhere in the output.
        assert!(
            !json.contains("legacy_promql"),
            "PromQL-only KPI leaked into emitted plot: {json}"
        );
        assert!(
            !json.contains("Rate (legacy)"),
            "PromQL-only KPI title leaked into emitted plot: {json}"
        );
        // The SQL KPI carries no promql_query — the field is `None` and
        // therefore elided by `skip_serializing_if`.
        assert!(
            !json.contains("\"promql_query\""),
            "no plot should serialize a promql_query field: {json}"
        );
    }

    /// End-to-end: a KPI whose `sql` carries `{{view}}` lands in the
    /// generated plot with the placeholder resolved to
    /// `_src_<service_name>`. Pins the substitution wiring through the
    /// service emitter.
    #[test]
    fn kpi_sql_view_placeholder_is_resolved() {
        let ext = ServiceExtension {
            service_name: "vllm-prefill".to_string(),
            aliases: vec![],
            service_metadata: HashMap::new(),
            slo: None,
            kpis: vec![kpi(
                "Rate",
                "metric{source=\"vllm-prefill\"}",
                Some("SELECT t FROM {{view}}"),
            )],
        };
        let view = generate(&EmptyDashboardData, vec![], &ext);
        let json = serde_json::to_string(&view).unwrap();
        // Placeholder resolved (non-alphanumeric `-` → `_`).
        assert!(
            json.contains("FROM _src_vllm_prefill"),
            "expected resolved view: {json}",
        );
        // Placeholder doesn't leak through.
        assert!(!json.contains("{{view}}"), "placeholder leaked: {json}");
    }

    /// `{{view}}` is substituted to `_src_<source>` (wasm-compatible
    /// sanitisation: non-`[a-zA-Z0-9_]` chars become `_`). Pinned so a
    /// future change can't accidentally diverge the server's view-name
    /// rule from `viewNameForSource` in `duckdb-registry.js`.
    #[test]
    fn substitute_view_mirrors_wasm_sanitisation() {
        assert_eq!(
            substitute_view("SELECT * FROM {{view}}", "cachecannon"),
            "SELECT * FROM _src_cachecannon"
        );
        assert_eq!(
            substitute_view("SELECT * FROM {{view}}", "vllm-prefill"),
            "SELECT * FROM _src_vllm_prefill"
        );
        // Multiple occurrences all substitute.
        assert_eq!(
            substitute_view("a {{view}} b {{view}} c", "x"),
            "a _src_x b _src_x c"
        );
        // No placeholder → pass-through.
        assert_eq!(substitute_view("SELECT 1", "x"), "SELECT 1");
    }
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
