use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Usage
     */

    let mut usage = Group::new("Usage", "usage");

    // Total memory
    usage.plot_promql(
        PlotOpts::gauge("Total", "total", Unit::Bytes),
        "memory_total".to_string(),
    );

    // Available memory - memory available for allocation
    usage.plot_promql(
        PlotOpts::gauge("Available", "available", Unit::Bytes),
        "memory_available".to_string(),
    );

    // Free memory - completely unused memory
    usage.plot_promql(
        PlotOpts::gauge("Free", "free", Unit::Bytes),
        "memory_free".to_string(),
    );

    // Buffers - memory used for file buffers
    usage.plot_promql(
        PlotOpts::gauge("Buffers", "buffers", Unit::Bytes),
        "memory_buffers".to_string(),
    );

    // Cached - memory used by page cache
    usage.plot_promql(
        PlotOpts::gauge("Cached", "cached", Unit::Bytes),
        "memory_cached".to_string(),
    );

    // Memory used (calculated)
    usage.plot_promql(
        PlotOpts::gauge("Used", "used", Unit::Bytes),
        "memory_total - memory_available".to_string(),
    );

    // Memory utilization percentage
    usage.plot_promql(
        PlotOpts::gauge("Utilization %", "utilization-pct", Unit::Percentage).range(0.0, 1.0),
        "(memory_total - memory_available) / memory_total".to_string(),
    );

    view.group(usage);

    /*
     * NUMA
     */

    let mut numa = Group::new("NUMA", "numa");

    // Local allocations - fast (same NUMA node)
    numa.plot_promql(
        PlotOpts::counter("Local Rate", "numa-local-rate", Unit::Rate),
        "rate(memory_numa_local[5m])".to_string(),
    );

    // Remote allocations - slow (cross-node access)
    numa.plot_promql(
        PlotOpts::counter("Remote Rate", "numa-remote-rate", Unit::Rate),
        "rate(memory_numa_foreign[5m])".to_string(),
    );

    view.group(numa);

    view
}
