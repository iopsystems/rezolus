use super::*;
use crate::viewer::ServiceExtension;

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

    for kpi in &service_ext.kpis {
        let id = format!("kpi-{}", kpi.role);
        let mut group = Group::new(&kpi.title, &id);

        let opts = match kpi.metric_type.as_str() {
            "gauge" => PlotOpts::gauge(&kpi.title, &id, Unit::Count),
            "histogram" => PlotOpts::histogram(
                &kpi.title,
                &id,
                Unit::Count,
                kpi.subtype.as_deref().unwrap_or("percentiles"),
            ),
            _ => PlotOpts::counter(&kpi.title, &id, Unit::Count),
        };
        let opts = opts.maybe_unit_system(kpi.unit_system.as_deref());
        let opts = match &kpi.percentiles {
            Some(p) => opts.with_percentiles(p.clone()),
            None => opts,
        };

        group.plot_promql(opts, kpi.query.clone());
        view.group(group);
    }

    view
}
