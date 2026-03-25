use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    // Signal to the frontend that this section uses the topology renderer
    view.metadata.insert(
        "hardware_topology".to_string(),
        serde_json::json!({ "enabled": true }),
    );

    // Metric options for coloring CPU cells in the topology diagram
    view.metadata.insert(
        "topology_metrics".to_string(),
        serde_json::json!([
            {
                "label": "CPU Busy %",
                "query": "sum by (id) (irate(cpu_usage[5m])) / 1000000000",
                "unit": "percentage",
                "max": 1.0
            },
            {
                "label": "User %",
                "query": "sum by (id) (irate(cpu_usage{state=\"user\"}[5m])) / 1000000000",
                "unit": "percentage",
                "max": 1.0
            },
            {
                "label": "System %",
                "query": "sum by (id) (irate(cpu_usage{state=\"system\"}[5m])) / 1000000000",
                "unit": "percentage",
                "max": 1.0
            },
            {
                "label": "IPC",
                "query": "sum by (id) (irate(cpu_instructions[5m])) / sum by (id) (irate(cpu_cycles[5m]))",
                "unit": "count"
            },
            {
                "label": "CPU Migrations (to)",
                "query": "sum by (id) (irate(cpu_migrations{direction=\"to\"}[5m]))",
                "unit": "rate"
            }
        ]),
    );

    // Standard line charts below the topology diagram
    let mut summary = Group::new("System Metrics", "system-metrics");

    summary.plot_promql(
        PlotOpts::line("Total CPU Busy %", "hw-cpu-busy", Unit::Percentage),
        "sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000".to_string(),
    );

    summary.plot_promql(
        PlotOpts::line("IPC", "hw-ipc", Unit::Count),
        "sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m]))".to_string(),
    );

    view.group(summary);

    view
}
