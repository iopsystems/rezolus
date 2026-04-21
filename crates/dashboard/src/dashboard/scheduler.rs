use crate::Tsdb;
use crate::plot::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Scheduler
     */

    let mut scheduler = Group::new("Scheduler", "scheduler");

    let queueing = scheduler.subgroup("Runqueue Latency");
    queueing.describe("How long tasks waited on the runqueue before getting CPU time.");
    queueing.plot_promql_full(
        PlotOpts::histogram_latency("Runqueue Latency", "scheduler-runqueue-latency")
            .with_axis_label("Latency")
            .with_unit_system("time"),
        "scheduler_runqueue_latency".to_string(),
    );

    let timing = scheduler.subgroup("Task Timing");
    timing.describe("Time tasks spent off-CPU (blocked, waiting) and on-CPU (running).");
    timing.plot_promql(
        PlotOpts::histogram_latency("Off CPU Time", "off-cpu-time")
            .with_axis_label("Time")
            .with_unit_system("time"),
        "scheduler_offcpu".to_string(),
    );
    timing.plot_promql(
        PlotOpts::histogram_latency("Running Time", "running-time")
            .with_axis_label("Time")
            .with_unit_system("time"),
        "scheduler_running".to_string(),
    );

    let switches = scheduler.subgroup("Context Switches");
    switches.describe("Overall context-switch rate across all cores.");
    switches.plot_promql_full(
        PlotOpts::counter("Context Switch", "cswitch", Unit::Rate),
        "sum(irate(scheduler_context_switch[5m]))".to_string(),
    );

    view.group(scheduler);

    view
}
