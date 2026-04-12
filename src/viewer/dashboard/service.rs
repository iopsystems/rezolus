use super::*;
use crate::viewer::ServiceExtension;

pub fn generate(data: &Tsdb, sections: Vec<Section>, service_ext: &ServiceExtension) -> View {
    let mut view = View::new(data, sections);

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

        group.plot_promql(opts, kpi.query.clone());
        view.group(group);
    }

    view
}
