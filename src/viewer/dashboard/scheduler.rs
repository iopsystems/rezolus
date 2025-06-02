use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Scheduler
     */

    let mut scheduler = Group::new("Scheduler", "scheduler");

    scheduler.scatter(
        PlotOpts::scatter("Runqueue Latency", "scheduler-runqueue-latency", Unit::Time)
            .with_axis_label("Latency")
            .with_unit_system("time")
            .with_log_scale(true),
        data.percentiles("scheduler_runqueue_latency", (), PERCENTILES),
    );

    scheduler.scatter(
        PlotOpts::scatter("Off CPU Time", "off-cpu-time", Unit::Time)
            .with_axis_label("Time")
            .with_unit_system("time")
            .with_log_scale(true),
        data.percentiles("scheduler_offcpu", (), PERCENTILES),
    );

    scheduler.scatter(
        PlotOpts::scatter("Running Time", "running-time", Unit::Time)
            .with_axis_label("Time")
            .with_unit_system("time")
            .with_log_scale(true),
        data.percentiles("scheduler_offcpu", (), PERCENTILES),
    );

    view.group(scheduler);

    view
}
