use crate::Tsdb;
use crate::plot::*;
use crate::service_extension::ServiceExtension;

pub fn generate(data: &Tsdb, sections: Vec<Section>, service_ext: &ServiceExtension) -> View {
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

    // Group KPIs by role so that charts sharing a role render side-by-side
    // via the 2-column CSS grid on `.group .charts`.
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

        group.plot_promql(opts, kpi.query.clone());
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

/// Convert a title into a kebab-case slug for use as a DOM id.
fn slug(s: &str) -> String {
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
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}
