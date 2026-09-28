use crate::MetricsSource;
use crate::plot::*;

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

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

    // The subgroups below exist only for recordings from an agent that
    // exports the wider /proc/meminfo set; older recordings render as before.
    if has_metric(data, "memory_dirty") {
        let writeback = usage.subgroup("Writeback");
        writeback.describe(
            "Dirty is page cache written by processes and not yet on storage; it grows between \
             flusher runs and is what the writeback throttle acts on when it nears the dirty \
             limits. Writeback is the part in flight to the device right now.",
        );
        writeback.plot_promql(
            PlotOpts::gauge("Dirty", "dirty", Unit::Bytes),
            "memory_dirty".to_string(),
        );
        writeback.plot_promql(
            PlotOpts::gauge("Writeback", "writeback", Unit::Bytes),
            "memory_writeback".to_string(),
        );
    }

    if has_metric(data, "writeback_runs") {
        let throttle = usage.subgroup("Writeback Throttle");
        throttle.describe(
            "When dirty pages near their limit the kernel makes the writer sleep in \
             balance_dirty_pages. The sleep lands on the writer's own thread, so this is \
             where a write path's tail comes from when writeback cannot keep up; zero events \
             means dirty pages never reached the limit.",
        );
        throttle.plot_promql(
            PlotOpts::counter("Throttle Events", "throttle-events", Unit::Count),
            "sum(irate(writeback_throttle_events[5m]))".to_string(),
        );
        throttle.plot_promql(
            PlotOpts::histogram_latency("Throttle Sleep", "throttle-latency"),
            "writeback_throttle_latency".to_string(),
        );
        throttle.plot_promql(
            PlotOpts::counter("Dirty-limit Checks", "throttle-checks", Unit::Count),
            "sum(irate(writeback_throttle_checks[5m]))".to_string(),
        );

        let flusher = usage.subgroup("Flusher");
        flusher.describe(
            "Writeback work items by why they ran: periodic is the dirty_writeback_centisecs \
             cadence, background is dirty pages over the background threshold, sync is an \
             explicit sync, vmscan is memory reclaim. Pages written is what reached storage.",
        );
        flusher.plot_promql(
            PlotOpts::counter("Runs by Reason", "writeback-runs", Unit::Count),
            "sum by (reason) (irate(writeback_runs[5m]))".to_string(),
        );
        flusher.plot_promql(
            PlotOpts::counter("Pages Written", "writeback-pages", Unit::Count),
            "sum(irate(writeback_pages_written[5m]))".to_string(),
        );
    }

    if has_metric(data, "memory_active") {
        let cache = usage.subgroup("Page Cache");
        cache.describe(
            "File-backed memory by LRU list: inactive is reclaimed first, active last. A working \
             set that fits shows a stable active list; Shmem and Mapped are the parts of the \
             cache that are tmpfs or mmap'd into processes.",
        );
        cache.plot_promql(
            PlotOpts::gauge("Active (file)", "active-file", Unit::Bytes),
            "memory_active{kind=\"file\"}".to_string(),
        );
        cache.plot_promql(
            PlotOpts::gauge("Inactive (file)", "inactive-file", Unit::Bytes),
            "memory_inactive{kind=\"file\"}".to_string(),
        );
        cache.plot_promql(
            PlotOpts::gauge("Shmem", "shmem", Unit::Bytes),
            "memory_shmem".to_string(),
        );
        cache.plot_promql(
            PlotOpts::gauge("Mapped", "mapped", Unit::Bytes),
            "memory_mapped".to_string(),
        );

        let anon = usage.subgroup("Anonymous");
        anon.describe(
            "Process heaps, stacks and private data by LRU list, plus memory the kernel cannot \
             reclaim at all.",
        );
        anon.plot_promql(
            PlotOpts::gauge("Anonymous", "anon", Unit::Bytes),
            "memory_anon".to_string(),
        );
        anon.plot_promql(
            PlotOpts::gauge("Active (anon)", "active-anon", Unit::Bytes),
            "memory_active{kind=\"anon\"}".to_string(),
        );
        anon.plot_promql(
            PlotOpts::gauge("Inactive (anon)", "inactive-anon", Unit::Bytes),
            "memory_inactive{kind=\"anon\"}".to_string(),
        );
        anon.plot_promql(
            PlotOpts::gauge("Unevictable", "unevictable", Unit::Bytes),
            "memory_unevictable".to_string(),
        );
    }

    if has_metric(data, "memory_slab") {
        let kernel = usage.subgroup("Kernel");
        kernel.describe(
            "Kernel allocations. Reclaimable slab is mostly the dentry and inode caches, which \
             the kernel drops under pressure at the cost of re-reading metadata from disk; \
             unreclaimable slab, kernel stacks and page tables are not recoverable.",
        );
        kernel.plot_promql(
            PlotOpts::gauge("Slab (reclaimable)", "slab-reclaimable", Unit::Bytes),
            "memory_slab{kind=\"reclaimable\"}".to_string(),
        );
        kernel.plot_promql(
            PlotOpts::gauge("Slab (unreclaimable)", "slab-unreclaimable", Unit::Bytes),
            "memory_slab{kind=\"unreclaimable\"}".to_string(),
        );
        kernel.plot_promql(
            PlotOpts::gauge("Page Tables", "page-tables", Unit::Bytes),
            "memory_page_tables".to_string(),
        );
        kernel.plot_promql(
            PlotOpts::gauge("Kernel Stacks", "kernel-stack", Unit::Bytes),
            "memory_kernel_stack".to_string(),
        );
    }

    if has_metric(data, "memory_swap_total") {
        let swap = usage.subgroup("Swap");
        swap.describe(
            "Swap in use and the overcommit position. Committed is every allocation processes \
             could touch; it exceeding the commit limit only matters under strict overcommit.",
        );
        swap.plot_promql(
            PlotOpts::gauge("Swap Used", "swap-used", Unit::Bytes),
            "memory_swap_total - memory_swap_free".to_string(),
        );
        swap.plot_promql(
            PlotOpts::gauge("Swap Cached", "swap-cached", Unit::Bytes),
            "memory_swap_cached".to_string(),
        );
        swap.plot_promql(
            PlotOpts::gauge("Committed", "committed", Unit::Bytes),
            "memory_committed".to_string(),
        );
        swap.plot_promql(
            PlotOpts::gauge("Commit Limit", "commit-limit", Unit::Bytes),
            "memory_commit_limit".to_string(),
        );
    }

    if has_metric(data, "memory_hugepages_anon") || has_metric(data, "memory_hugetlb") {
        let huge = usage.subgroup("Huge Pages");
        huge.describe(
            "Transparent huge pages by what they back, and the hugetlbfs pool: reserved memory \
             that is outside MemAvailable whether or not anything uses it.",
        );
        huge.plot_promql(
            PlotOpts::gauge("THP (anon)", "thp-anon", Unit::Bytes),
            "memory_hugepages_anon".to_string(),
        );
        huge.plot_promql(
            PlotOpts::gauge("THP (shmem)", "thp-shmem", Unit::Bytes),
            "memory_hugepages_shmem".to_string(),
        );
        huge.plot_promql(
            PlotOpts::gauge("THP (file)", "thp-file", Unit::Bytes),
            "memory_hugepages_file".to_string(),
        );
        huge.plot_promql(
            PlotOpts::gauge("hugetlb Reserved", "hugetlb", Unit::Bytes),
            "memory_hugetlb".to_string(),
        );
        huge.plot_promql(
            PlotOpts::gauge("hugetlb Pages", "hugetlb-pages", Unit::Count),
            "sum by (state) (memory_hugetlb_pages)".to_string(),
        );
    }

    view.group(usage);

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

#[cfg(test)]
mod tests {
    use super::*;
    use metriken_query::MemoryStore;
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    /// A store holding one sample of each named gauge, unlabeled.
    fn store_with(metrics: &[&str]) -> MemoryStore {
        let store = MemoryStore::builder().sampling_interval_ms(1000).build();
        let gauges = metrics
            .iter()
            .map(|name| metriken_exposition::Gauge::new(name.to_string(), 1, HashMap::new()))
            .collect();
        store.ingest_snapshot(metriken_exposition::Snapshot::V2(
            metriken_exposition::SnapshotV2 {
                systemtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                duration: Duration::from_secs(0),
                metadata: HashMap::new(),
                counters: vec![],
                gauges,
                histograms: vec![],
            },
        ));
        store
    }

    fn json(view: &View) -> String {
        serde_json::to_string(view).unwrap().replace("\\\"", "\"")
    }

    #[test]
    fn an_old_recording_renders_only_the_original_plots() {
        let view = generate(&store_with(&["memory_total", "memory_free"]), vec![]);
        let j = json(&view);
        assert!(j.contains("memory_total - memory_available"));
        assert!(!j.contains("memory_dirty"));
        assert!(!j.contains("memory_slab"));
        assert!(!j.contains("memory_swap"));
        assert!(!j.contains("memory_hugetlb"));
    }

    #[test]
    fn the_wider_meminfo_set_adds_gated_subgroups() {
        let view = generate(
            &store_with(&[
                "memory_total",
                "memory_dirty",
                "memory_active",
                "memory_slab",
                "memory_swap_total",
                "memory_hugetlb",
            ]),
            vec![],
        );
        let j = json(&view);
        assert!(j.contains("\"memory_dirty\""));
        assert!(j.contains("memory_active{kind=\"file\"}"));
        assert!(j.contains("memory_slab{kind=\"reclaimable\"}"));
        assert!(j.contains("memory_swap_total - memory_swap_free"));
        assert!(j.contains("sum by (state) (memory_hugetlb_pages)"));
        // No writeback sampler in this recording: its subgroups stay out.
        assert!(!j.contains("writeback_throttle"));
    }

    #[test]
    fn the_writeback_sampler_adds_throttle_and_flusher_subgroups() {
        let view = generate(
            &store_with(&["memory_total", "memory_dirty", "writeback_runs"]),
            vec![],
        );
        let j = json(&view);
        assert!(j.contains("sum(irate(writeback_throttle_events[5m]))"));
        assert!(j.contains("\"writeback_throttle_latency\""));
        assert!(j.contains("sum by (reason) (irate(writeback_runs[5m]))"));
        assert!(j.contains("sum(irate(writeback_pages_written[5m]))"));
    }
}
