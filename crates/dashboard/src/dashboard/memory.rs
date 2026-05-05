use crate::data::DashboardData;
use crate::plot::*;

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Usage
     */

    let mut usage = Group::new("Usage", "usage");

    let capacity = usage.subgroup("Capacity");
    capacity.describe("How much memory exists and how much of it is unclaimed.");
    capacity.plot_promql(
        PlotOpts::gauge("Total", "total", Unit::Bytes),
        "memory_total".to_string(),
    );
    capacity.plot_promql(
        PlotOpts::gauge("Available", "available", Unit::Bytes),
        "memory_available".to_string(),
    );
    capacity.plot_promql(
        PlotOpts::gauge("Free", "free", Unit::Bytes),
        "memory_free".to_string(),
    );

    let breakdown = usage.subgroup("Breakdown");
    breakdown.describe("Where allocated memory is going — kernel buffers, page cache, anonymous use — with overall utilization.");
    breakdown.plot_promql(
        PlotOpts::gauge("Buffers", "buffers", Unit::Bytes),
        "memory_buffers".to_string(),
    );
    breakdown.plot_promql(
        PlotOpts::gauge("Cached", "cached", Unit::Bytes),
        "memory_cached".to_string(),
    );
    breakdown.plot_promql(
        PlotOpts::gauge("Used", "used", Unit::Bytes),
        "memory_total - memory_available".to_string(),
    );
    breakdown.plot_promql(
        PlotOpts::gauge("Utilization %", "utilization-pct", Unit::Percentage).percentage_range(),
        "(memory_total - memory_available) / memory_total".to_string(),
    );

    view.group(usage);

    /*
     * NUMA
     */

    let mut numa = Group::new("NUMA", "numa");

    let locality = numa.subgroup("Local vs Remote");
    locality.describe("Local allocations hit node-local RAM (fast); remote allocations cross the interconnect (slow).");
    locality.plot_promql(
        PlotOpts::counter("Local Rate", "numa-local-rate", Unit::Rate),
        "rate(memory_numa_local[5m])".to_string(),
    );
    locality.plot_promql(
        PlotOpts::counter("Remote Rate", "numa-remote-rate", Unit::Rate),
        "rate(memory_numa_foreign[5m])".to_string(),
    );

    view.group(numa);

    view
}
