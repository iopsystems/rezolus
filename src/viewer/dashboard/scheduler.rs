use super::*;

pub fn generate(data: &Arc<Tsdb>, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Scheduler
     */

    let mut scheduler = Group::new("Scheduler", "scheduler");

    // Runqueue Latency percentiles - p50, p90, p99, p99.9, p99.99
    scheduler.plot_promql(
        PlotOpts::scatter("Runqueue Latency", "scheduler-runqueue-latency", Unit::Time)
            .with_axis_label("Latency")
            .with_unit_system("time")
            .with_log_scale(true),
        "histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], scheduler_runqueue_latency)"
            .to_string(),
    );

    // Off CPU Time percentiles
    scheduler.plot_promql(
        PlotOpts::scatter("Off CPU Time", "off-cpu-time", Unit::Time)
            .with_axis_label("Time")
            .with_unit_system("time")
            .with_log_scale(true),
        "histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], scheduler_offcpu)".to_string(),
    );

    // Running Time percentiles
    // Note: Original code seems to have a bug using scheduler_offcpu for running time,
    // keeping it for compatibility
    scheduler.plot_promql(
        PlotOpts::scatter("Running Time", "running-time", Unit::Time)
            .with_axis_label("Time")
            .with_unit_system("time")
            .with_log_scale(true),
        "histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], scheduler_running)".to_string(),
    );

    // Context Switch rate
    scheduler.plot_promql(
        PlotOpts::line("Context Switch", "cswitch", Unit::Rate),
        "sum(irate(scheduler_context_switch[5m]))".to_string(),
    );

    view.group(scheduler);

    view
}
