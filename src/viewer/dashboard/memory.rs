use super::*;

pub fn generate(data: &Arc<Tsdb>, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Usage
     */

    let mut usage = Group::new("Usage", "usage");

    // Total memory
    usage.plot_promql(
        PlotOpts::line("Total", "total", Unit::Bytes),
        "memory_total".to_string(),
    );

    // Available memory - memory available for allocation
    usage.plot_promql(
        PlotOpts::line("Available", "available", Unit::Bytes),
        "memory_available".to_string(),
    );

    // Free memory - completely unused memory
    usage.plot_promql(
        PlotOpts::line("Free", "free", Unit::Bytes),
        "memory_free".to_string(),
    );

    // Buffers - memory used for file buffers
    usage.plot_promql(
        PlotOpts::line("Buffers", "buffers", Unit::Bytes),
        "memory_buffers".to_string(),
    );

    // Cached - memory used by page cache
    usage.plot_promql(
        PlotOpts::line("Cached", "cached", Unit::Bytes),
        "memory_cached".to_string(),
    );

    // Memory used (calculated)
    usage.plot_promql(
        PlotOpts::line("Used", "used", Unit::Bytes),
        "memory_total - memory_available".to_string(),
    );

    // Memory utilization percentage
    usage.plot_promql(
        PlotOpts::line("Utilization %", "utilization-pct", Unit::Percentage),
        "(memory_total - memory_available) / memory_total".to_string(),
    );

    view.group(usage);

    /*
     * NUMA
     */

    let mut numa = Group::new("NUMA", "numa");

    // Local allocations - fast (same NUMA node)
    numa.plot_promql(
        PlotOpts::line("Local Rate", "numa-local-rate", Unit::Rate),
        "rate(memory_numa_local[5m])".to_string(),
    );

    // Remote allocations - slow (cross-node access)
    numa.plot_promql(
        PlotOpts::line("Remote Rate", "numa-remote-rate", Unit::Rate),
        "rate(memory_numa_foreign[5m])".to_string(),
    );

    view.group(numa);

    view
}
