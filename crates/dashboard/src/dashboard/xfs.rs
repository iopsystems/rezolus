//! XFS's own per-mount counters, one line per filesystem (its `mount`
//! label), from the `xfs_stats` sampler; renders only when the recording
//! carries them.

use crate::MetricsSource;
use crate::plot::*;

fn rate(metric: &str) -> String {
    format!("sum by (mount) (irate({metric}[5m]))")
}

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    let has_stats = has_metric(data, "xfs_log_forces");
    let has_log = has_metric(data, "xfs_log_waits");
    if !has_stats && !has_log {
        return view;
    }

    if has_log {
        blocked_time(&mut view);
    }

    if !has_stats {
        return view;
    }

    let mut log = Group::new("Log", "log");

    let forces = log.subgroup("Forces");
    forces.describe(
        "Log forces are the synchronous flush an fsync or a synchronous transaction demands; \
         a force sleep is a force that waited for a log write to complete. A synchronous \
         force sleeps once for its own write, so sleeps per force above one is fsyncs \
         waiting on each other's log writes as well.",
    );
    forces.plot_promql(
        PlotOpts::counter("Forces", "log-forces", Unit::Count),
        rate("xfs_log_forces"),
    );
    forces.plot_promql(
        PlotOpts::counter("Force Sleeps", "log-force-sleeps", Unit::Count),
        rate("xfs_log_force_sleeps"),
    );
    forces.plot_promql(
        PlotOpts::counter("Log Writes", "log-writes", Unit::Count),
        rate("xfs_log_writes"),
    );
    forces.plot_promql(
        PlotOpts::counter("Log Bytes", "log-bytes", Unit::Datarate),
        format!("{} * 512", rate("xfs_log_blocks_written")),
    );

    let space = log.subgroup("Log Space");
    space.describe(
        "Every transaction reserves log space; one that sleeps here is waiting for the AIL \
         pusher to write logged metadata to its final place so the log tail can move. Any \
         sleeps at all mean the log is too small or its device too slow for the write rate. \
         Buffer stalls are log writes that found every in-core log buffer in flight.",
    );
    space.plot_promql(
        PlotOpts::counter("Space Requests", "log-space-requests", Unit::Count),
        rate("xfs_log_space_requests"),
    );
    space.plot_promql(
        PlotOpts::counter("Space Sleeps", "log-space-sleeps", Unit::Count),
        rate("xfs_log_space_sleeps"),
    );
    space.plot_promql(
        PlotOpts::counter("Buffer Stalls", "log-iclog-stalls", Unit::Count),
        rate("xfs_log_iclog_stalls"),
    );

    let ail = log.subgroup("AIL Pusher");
    ail.describe(
        "The xfsaild thread pushes logged metadata out so the log can reclaim space. Items \
         it finds pinned need a log force first; locked ones are retried; a flush is the \
         pusher forcing the log itself because everything was pinned.",
    );
    ail.plot_promql(
        PlotOpts::counter("Pushes", "ail-pushes", Unit::Count),
        rate("xfs_ail_pushes"),
    );
    ail.plot_promql(
        PlotOpts::counter("Items by Outcome", "ail-items", Unit::Count),
        "sum by (outcome) (irate(xfs_ail_push_items[5m]))".to_string(),
    );
    ail.plot_promql(
        PlotOpts::counter("Restarts", "ail-restarts", Unit::Count),
        rate("xfs_ail_push_restarts"),
    );
    ail.plot_promql(
        PlotOpts::counter("Flushes", "ail-flushes", Unit::Count),
        rate("xfs_ail_flushes"),
    );

    let trans = log.subgroup("Transactions");
    trans.describe("Transactions committed, by whether the caller waited for the log.");
    trans.plot_promql(
        PlotOpts::counter("By Kind", "transactions", Unit::Count),
        "sum by (kind) (irate(xfs_transactions[5m]))".to_string(),
    );

    view.group(log);

    let mut inodes = Group::new("Inode Cache", "inode-cache");

    let lookups = inodes.subgroup("Lookups");
    lookups.describe(
        "Inode-cache lookups by outcome. A miss reads the inode from disk on the calling \
         thread; on a file-per-object store with more inodes than cache, the miss rate is the \
         cold metadata cost of every stat and atime update.",
    );
    lookups.plot_promql(
        PlotOpts::counter("By Outcome", "inode-lookups", Unit::Count),
        "sum by (outcome) (irate(xfs_inode_cache_lookups[5m]))".to_string(),
    );
    lookups.plot_promql(
        PlotOpts::counter("Misses", "inode-misses", Unit::Count),
        rate("xfs_inode_cache_lookups{outcome=\"missed\"}"),
    );
    lookups.plot_promql(
        PlotOpts::counter("Reclaims", "inode-reclaims", Unit::Count),
        rate("xfs_inode_reclaims"),
    );

    view.group(inodes);

    let mut alloc = Group::new("Allocator", "allocator");

    let extents = alloc.subgroup("Extents");
    extents.describe(
        "Extents and blocks allocated and freed. Blocks per extent falling is free space \
         fragmenting: files are being split across more, smaller extents.",
    );
    extents.plot_promql(
        PlotOpts::counter("Extents", "extents", Unit::Count),
        "sum by (op) (irate(xfs_extents[5m]))".to_string(),
    );
    extents.plot_promql(
        PlotOpts::counter("Blocks", "extent-blocks", Unit::Count),
        "sum by (op) (irate(xfs_extent_blocks[5m]))".to_string(),
    );
    extents.plot_promql(
        PlotOpts::counter(
            "Blocks per Extent Allocated",
            "blocks-per-extent",
            Unit::Count,
        ),
        "sum(irate(xfs_extent_blocks{op=\"allocated\"}[5m])) / \
         sum(irate(xfs_extents{op=\"allocated\"}[5m]))"
            .to_string(),
    );

    let dirs = alloc.subgroup("Directories");
    dirs.describe("Directory operations at the XFS layer.");
    dirs.plot_promql(
        PlotOpts::counter("By Operation", "directory-ops", Unit::Count),
        "sum by (op) (irate(xfs_directory_ops[5m]))".to_string(),
    );

    view.group(alloc);

    let mut io = Group::new("I/O", "io");

    let files = io.subgroup("File I/O");
    files.describe(
        "Read and write calls into XFS and the bytes they moved. Bytes written against log \
         bytes and the device's write bytes is the write-amplification chain.",
    );
    files.plot_promql(
        PlotOpts::counter("Calls", "file-calls", Unit::Count),
        "sum by (op) (irate(xfs_file_calls[5m]))".to_string(),
    );
    files.plot_promql(
        PlotOpts::counter("Bytes", "file-bytes", Unit::Datarate),
        "sum by (op) (irate(xfs_file_bytes[5m]))".to_string(),
    );

    let buffers = io.subgroup("Metadata Buffers");
    buffers.describe(
        "The metadata buffer cache: lookups, misses and the reads a miss caused, and \
         lookups that waited for or found a busy lock.",
    );
    buffers.plot_promql(
        PlotOpts::counter("Lookups", "buffer-lookups", Unit::Count),
        rate("xfs_buffer_lookups"),
    );
    buffers.plot_promql(
        PlotOpts::counter("Misses", "buffer-misses", Unit::Count),
        rate("xfs_buffer_misses"),
    );
    buffers.plot_promql(
        PlotOpts::counter("Reads", "buffer-reads", Unit::Count),
        rate("xfs_buffer_reads"),
    );
    buffers.plot_promql(
        PlotOpts::counter("Lock Waits", "buffer-lock-waits", Unit::Count),
        rate("xfs_buffer_lock_waits"),
    );

    view.group(io);

    view
}

/// The `xfs_log` sampler's view: how long threads were blocked on the log,
/// per mount, and the host-wide distributions.
fn blocked_time(view: &mut View) {
    let mut blocked = Group::new("Blocked Time", "blocked-time");

    let space = blocked.subgroup("Log Space");
    space.describe(
        "A transaction that found the log full sleeps until the AIL pusher frees space. Any \
         waits at all mean the log is too small or its device too slow for the metadata write \
         rate; the wait count equals xfs_log_space_sleeps, and the time is what that count \
         cannot say.",
    );
    space.plot_promql(
        PlotOpts::histogram_latency("Log Space Wait Latency", "blocked-space-latency"),
        "xfs_log_wait_latency{wait=\"space\"}".to_string(),
    );
    space.plot_promql(
        PlotOpts::counter("Log Space Waits", "blocked-space-waits", Unit::Count),
        rate("xfs_log_waits{wait=\"space\"}"),
    );
    space.plot_promql(
        PlotOpts::counter("Mean Log Space Wait", "blocked-space-mean", Unit::Time),
        "sum by (mount) (irate(xfs_log_wait_time{wait=\"space\"}[5m])) / \
         sum by (mount) (irate(xfs_log_waits{wait=\"space\"}[5m]))"
            .to_string(),
    );

    let force = blocked.subgroup("Log Force");
    force.describe(
        "A log force writes the committed log out and waits for it: the device round trip \
         every fsync pays. Mean force latency per mount against the block device's write \
         latency says whether the fsync is waiting on the device or on other commits.",
    );
    force.plot_promql(
        PlotOpts::histogram_latency("Log Force Latency", "blocked-force-latency"),
        "xfs_log_wait_latency{wait=\"force\"}".to_string(),
    );
    force.plot_promql(
        PlotOpts::counter("Log Forces", "blocked-forces", Unit::Count),
        rate("xfs_log_waits{wait=\"force\"}"),
    );
    force.plot_promql(
        PlotOpts::counter("Mean Log Force Latency", "blocked-force-mean", Unit::Time),
        "sum by (mount) (irate(xfs_log_wait_time{wait=\"force\"}[5m])) / \
         sum by (mount) (irate(xfs_log_waits{wait=\"force\"}[5m]))"
            .to_string(),
    );

    let cil = blocked.subgroup("CIL");
    cil.describe(
        "Committing transactions that found the committed item list over its hard limit and \
         waited for a push to the log. A count only: the log's checkpoint writer is behind the \
         commit rate.",
    );
    cil.plot_promql(
        PlotOpts::counter("CIL Waits", "blocked-cil-waits", Unit::Count),
        rate("xfs_log_waits{wait=\"cil\"}"),
    );

    view.group(blocked);
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriken_query::MemoryStore;
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    fn store_with(metrics: &[&str]) -> MemoryStore {
        let store = MemoryStore::builder().sampling_interval_ms(1000).build();
        let counters = metrics
            .iter()
            .map(|name| {
                let mut metadata = HashMap::new();
                metadata.insert("mount".to_string(), "/".to_string());
                metriken_exposition::Counter::new(name.to_string(), 1, metadata)
            })
            .collect();
        store.ingest_snapshot(metriken_exposition::Snapshot::V2(
            metriken_exposition::SnapshotV2 {
                systemtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                duration: Duration::from_secs(0),
                metadata: HashMap::new(),
                counters,
                gauges: vec![],
                histograms: vec![],
            },
        ));
        store
    }

    fn json(view: &View) -> String {
        serde_json::to_string(view).unwrap().replace("\\\"", "\"")
    }

    #[test]
    fn the_section_renders_per_mount_when_the_recording_has_xfs_stats() {
        let view = generate(&store_with(&["xfs_log_forces"]), vec![]);
        let j = json(&view);
        assert!(j.contains("sum by (mount) (irate(xfs_log_forces[5m]))"));
        assert!(j.contains("sum by (mount) (irate(xfs_log_space_sleeps[5m]))"));
        assert!(j.contains("sum by (outcome) (irate(xfs_ail_push_items[5m]))"));
        assert!(
            j.contains("sum by (mount) (irate(xfs_inode_cache_lookups{outcome=\"missed\"}[5m]))")
        );
        assert!(j.contains("sum by (mount) (irate(xfs_log_blocks_written[5m])) * 512"));
        assert!(j.contains(
            "sum(irate(xfs_extent_blocks{op=\"allocated\"}[5m])) / sum(irate(xfs_extents{op=\"allocated\"}[5m]))"
        ));
    }

    #[test]
    fn the_blocked_time_group_renders_from_xfs_log_alone() {
        let view = generate(&store_with(&["xfs_log_waits"]), vec![]);
        let j = json(&view);
        assert!(j.contains("xfs_log_wait_latency{wait=\"space\"}"));
        assert!(j.contains("sum by (mount) (irate(xfs_log_waits{wait=\"force\"}[5m]))"));
        assert!(j.contains(
            "sum by (mount) (irate(xfs_log_wait_time{wait=\"force\"}[5m])) / sum by (mount) (irate(xfs_log_waits{wait=\"force\"}[5m]))"
        ));
        assert!(
            !j.contains("xfs_log_forces"),
            "the stats groups need xfs_stats"
        );
    }

    #[test]
    fn a_recording_without_xfs_renders_an_empty_section() {
        let view = generate(&MemoryStore::builder().build(), vec![]);
        assert!(!json(&view).contains("xfs_"));
    }
}
