use crate::Tsdb;
use crate::plot::*;

/// True iff the recording has more than one CPU. Per-core charts are
/// suppressed when this is false because they degenerate to the aggregate.
fn has_multiple_cpus(data: &Tsdb) -> bool {
    ["scheduler_runqueue_wait", "scheduler_context_switch"]
        .iter()
        .any(|m| metric_unique_label_count(data, m, "id") > 1)
}

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);
    let multi_cpu = has_multiple_cpus(data);

    let mut scheduler = Group::new("Scheduler", "scheduler");

    let queueing = scheduler.subgroup("Runqueue Latency");
    queueing.describe("How long tasks waited on the runqueue before getting CPU time.");
    queueing.plot_promql_full(
        PlotOpts::histogram_latency("Runqueue Latency", "scheduler-runqueue-latency")
            .with_axis_label("Latency")
            .with_unit_system("time"),
        "scheduler_runqueue_latency".to_string(),
    );

    let wait = scheduler.subgroup("Runqueue Wait");
    wait.describe(
        "Accumulated runqueue wait time, averaged across CPUs and broken out per-CPU. \
         A value of 1s/s means one task was waiting for the entire interval; values above \
         1s/s mean multiple tasks were queued concurrently — an indicator of scheduler pressure.",
    );
    if multi_cpu {
        wait.plot_promql(
            PlotOpts::counter("Wait", "scheduler-runqueue-wait", Unit::Time)
                .with_unit_system("time"),
            "sum(irate(scheduler_runqueue_wait[5m])) / cpu_cores".to_string(),
        );
        wait.plot_promql(
            PlotOpts::counter(
                "Wait (Per-CPU)",
                "scheduler-runqueue-wait-per-cpu",
                Unit::Time,
            )
            .with_unit_system("time"),
            "sum by (id) (irate(scheduler_runqueue_wait[5m]))".to_string(),
        );
    } else {
        wait.plot_promql_full(
            PlotOpts::counter("Wait", "scheduler-runqueue-wait", Unit::Time)
                .with_unit_system("time"),
            "sum(irate(scheduler_runqueue_wait[5m])) / cpu_cores".to_string(),
        );
    }

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
    switches.describe("Involuntary context-switch rate, aggregate and per-core.");
    if multi_cpu {
        switches.plot_promql(
            PlotOpts::counter("Context Switch", "cswitch", Unit::Rate),
            "sum(irate(scheduler_context_switch[5m]))".to_string(),
        );
        switches.plot_promql(
            PlotOpts::counter("Context Switch (Per-CPU)", "cswitch-per-cpu", Unit::Rate),
            "sum by (id) (irate(scheduler_context_switch[5m]))".to_string(),
        );
    } else {
        switches.plot_promql_full(
            PlotOpts::counter("Context Switch", "cswitch", Unit::Rate),
            "sum(irate(scheduler_context_switch[5m]))".to_string(),
        );
    }

    view.group(scheduler);

    view
}
