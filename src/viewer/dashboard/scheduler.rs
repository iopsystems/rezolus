use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Scheduler
     */

    let mut scheduler = Group::new("Scheduler", "scheduler");

    // Runqueue Latency percentiles - p50, p90, p99, p99.9, p99.99
    scheduler.plot_promql(
        PlotOpts::histogram_latency("Runqueue Latency", "scheduler-runqueue-latency")
            .with_axis_label("Latency")
            .with_unit_system("time"),
        "scheduler_runqueue_latency".to_string(),
    );

    // Off CPU Time percentiles
    scheduler.plot_promql(
        PlotOpts::histogram_latency("Off CPU Time", "off-cpu-time")
            .with_axis_label("Time")
            .with_unit_system("time"),
        "scheduler_offcpu".to_string(),
    );

    // Running Time percentiles
    scheduler.plot_promql(
        PlotOpts::histogram_latency("Running Time", "running-time")
            .with_axis_label("Time")
            .with_unit_system("time"),
        "scheduler_running".to_string(),
    );

    // Context Switch rate
    scheduler.plot_promql(
        PlotOpts::counter("Context Switch", "cswitch", Unit::Rate),
        "sum(irate(scheduler_context_switch[5m]))".to_string(),
    );

    view.group(scheduler);

    view
}
