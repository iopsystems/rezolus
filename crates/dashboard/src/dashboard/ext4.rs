//! The ext4 journal, allocator, inodes, writeback and metadata reads, one
//! line per filesystem (its `mount` label; `other` is a device the agent's
//! mount table did not know), from the `ext4_journal` and `ext4_alloc`
//! samplers; each sampler's groups render only when the recording carries
//! its metrics. Plots that already split by another label sum over mounts.

use crate::MetricsSource;
use crate::plot::*;

fn rate(metric: &str) -> String {
    format!("sum by (mount) (irate({metric}[5m]))")
}

/// A ratio of two host totals; per-mount ratios of sparse counters are noise.
fn total_rate(metric: &str) -> String {
    format!("sum(irate({metric}[5m]))")
}

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    if has_metric(data, "ext4_journal_commits") {
        journal(&mut view);
    }

    if has_metric(data, "ext4_allocations") {
        allocator(&mut view);
    }

    view
}

fn journal(view: &mut View) {
    let mut journal = Group::new("Journal", "journal");

    let commits = journal.subgroup("Commits");
    commits.describe(
        "jbd2 transaction commits across every ext4 filesystem on the host. A commit happens \
         every commit interval (5 s by default) or sooner when something calls fsync, so the \
         commit rate tracks fsync rate on a durability-bound workload.",
    );
    commits.plot_promql(
        PlotOpts::counter("Commits", "commits", Unit::Count),
        rate("ext4_journal_commits"),
    );
    commits.plot_promql(
        PlotOpts::counter("Handles", "commit-handles", Unit::Count),
        rate("ext4_journal_commit_handles"),
    );
    commits.plot_promql(
        PlotOpts::counter("Blocks Logged", "commit-blocks-logged", Unit::Count),
        rate("ext4_journal_commit_blocks{kind=\"logged\"}"),
    );
    commits.plot_promql(
        PlotOpts::counter("Blocks Dirtied", "commit-blocks-dirtied", Unit::Count),
        rate("ext4_journal_commit_blocks{kind=\"dirtied\"}"),
    );

    let phases = journal.subgroup("Commit Phases");
    phases.describe(
        "Where each commit spent its time. Flushing is data blocks reaching the device in \
         ordered mode and is where fsync waits; logging is the metadata and commit record \
         reaching the journal; running is how long the transaction stayed open before the \
         commit began. jbd2 reports these in jiffies, so the resolution is one tick (1–10 ms).",
    );
    for (phase, title) in [
        ("flushing", "Flushing"),
        ("logging", "Logging"),
        ("running", "Running"),
        ("locked", "Locked"),
        ("wait", "Handle Wait"),
        ("request_delay", "Request Delay"),
    ] {
        phases.plot_promql(
            PlotOpts::histogram_latency(title, format!("commit-{phase}")),
            format!("ext4_journal_commit_latency{{phase=\"{phase}\"}}"),
        );
    }

    let checkpoint = journal.subgroup("Checkpoint");
    checkpoint.describe(
        "A checkpoint writes a committed transaction's buffers to their final location and \
         frees its journal space. Forced-to-close rising means the journal is too small for \
         the write rate: transactions are being closed early to make room.",
    );
    checkpoint.plot_promql(
        PlotOpts::counter("Checkpoints", "checkpoints", Unit::Count),
        rate("ext4_journal_checkpoints"),
    );
    checkpoint.plot_promql(
        PlotOpts::histogram_latency("Checkpoint Latency", "checkpoint-latency"),
        "ext4_journal_checkpoint_latency".to_string(),
    );
    checkpoint.plot_promql(
        PlotOpts::counter("Forced to Close", "checkpoint-forced-to-close", Unit::Count),
        rate("ext4_journal_checkpoint_forced_to_close"),
    );

    let stalls = journal.subgroup("Lock Buffer Stalls");
    stalls.describe(
        "Time the journal spent waiting on a locked buffer. Rare on a healthy host; jbd2 \
         reports these in whole milliseconds.",
    );
    stalls.plot_promql(
        PlotOpts::counter("Stalls", "lock-buffer-stalls", Unit::Count),
        rate("ext4_journal_lock_buffer_stalls"),
    );
    stalls.plot_promql(
        PlotOpts::histogram_latency("Stall Latency", "lock-buffer-stall-latency"),
        "ext4_journal_lock_buffer_stall_latency".to_string(),
    );

    view.group(journal);

    let mut sync = Group::new("Sync", "sync");

    let calls = sync.subgroup("fsync");
    calls.describe(
        "fsync and fdatasync calls that reached ext4, and the ones that failed. Latency is \
         under Syscall, in the sync class.",
    );
    calls.plot_promql(
        PlotOpts::counter("Calls", "sync-calls", Unit::Count),
        "sum by (op) (irate(ext4_sync_file[5m]))".to_string(),
    );
    calls.plot_promql(
        PlotOpts::counter("Errors", "sync-errors", Unit::Count),
        rate("ext4_sync_file_errors"),
    );

    view.group(sync);

    let mut errors = Group::new("Errors", "errors");

    let reported = errors.subgroup("Reported");
    reported.describe(
        "Errors ext4 reported, counted as they happen and before any errors=remount-ro takes \
         effect, and forced shutdowns. Zero is the expected reading; the Filesystem section's \
         read-only gauge shows the state a remount leaves behind.",
    );
    reported.plot_promql(
        PlotOpts::counter("Errors", "errors", Unit::Count),
        rate("ext4_errors"),
    );
    reported.plot_promql(
        PlotOpts::counter("Shutdowns", "shutdowns", Unit::Count),
        rate("ext4_shutdowns"),
    );

    view.group(errors);
}

fn allocator(view: &mut View) {
    let mut alloc = Group::new("Allocator", "allocator");

    let effort = alloc.subgroup("Effort");
    effort.describe(
        "What it costs the block allocator to find space. Groups scanned per allocation rises \
         and allocations finish at the slower criteria as free space fragments; on a healthy \
         filesystem nearly everything finishes at criterion 0 or 1 after scanning one group. \
         This is the always-on form of what e2freefrag reports offline.",
    );
    effort.plot_promql(
        PlotOpts::counter("Allocations", "allocations", Unit::Count),
        rate("ext4_allocations"),
    );
    effort.plot_promql(
        PlotOpts::counter(
            "Groups Scanned per Allocation",
            "groups-per-allocation",
            Unit::Count,
        ),
        format!(
            "{} / {}",
            total_rate("ext4_allocation_groups_scanned"),
            total_rate("ext4_allocations")
        ),
    );
    effort.plot_promql(
        PlotOpts::counter("By Criterion", "allocations-by-criterion", Unit::Count),
        "sum by (criterion) (irate(ext4_allocations_by_criterion[5m]))".to_string(),
    );

    let extents = alloc.subgroup("Extents");
    extents.describe(
        "Blocks asked for against blocks returned, and the size of each returned extent. \
         Returned falling short of requested means a request had to be split across extents \
         because no free run was long enough.",
    );
    extents.plot_promql(
        PlotOpts::counter(
            "Blocks Requested vs Allocated",
            "allocation-blocks",
            Unit::Count,
        ),
        "sum by (kind) (irate(ext4_allocation_blocks[5m]))".to_string(),
    );
    extents.plot_promql(
        PlotOpts::histogram("Extent Size", "allocation-size", Unit::Count, "percentiles")
            .with_log_scale(true),
        "ext4_allocation_size".to_string(),
    );
    extents.plot_promql(
        PlotOpts::counter("Blocks Freed", "freed-blocks", Unit::Count),
        rate("ext4_freed_blocks"),
    );

    let discard = alloc.subgroup("Discard and Preallocation");
    discard.describe(
        "Blocks handed back to the device by fstrim or online discard, and preallocated \
         blocks released back to the pool when files are closed, truncated or unlinked.",
    );
    discard.plot_promql(
        PlotOpts::counter("Trimmed Blocks", "trimmed-blocks", Unit::Count),
        rate("ext4_trimmed_blocks"),
    );
    discard.plot_promql(
        PlotOpts::counter("Preallocation Releases", "prealloc-discards", Unit::Count),
        rate("ext4_preallocation_discards"),
    );
    discard.plot_promql(
        PlotOpts::counter(
            "Preallocated Blocks Released",
            "prealloc-blocks",
            Unit::Count,
        ),
        rate("ext4_preallocation_discarded_blocks"),
    );

    view.group(alloc);

    let mut inodes = Group::new("Inodes", "inodes");

    let churn = inodes.subgroup("Churn");
    churn.describe(
        "Files and directories created and removed. An eviction pass on a file-per-object \
         store shows here as a burst of frees.",
    );
    churn.plot_promql(
        PlotOpts::counter("Allocated vs Freed", "inodes", Unit::Count),
        "sum by (op) (irate(ext4_inodes[5m]))".to_string(),
    );

    view.group(inodes);

    let mut writeback = Group::new("Writeback", "writeback");

    let passes = writeback.subgroup("Passes");
    passes.describe(
        "Writeback passes ext4 ran over dirty inodes, the pages they wrote, and the pages they \
         skipped and left dirty for a later pass. Skipped rising means writeback is not keeping \
         up with the dirtying rate.",
    );
    passes.plot_promql(
        PlotOpts::counter("Passes", "writepages", Unit::Count),
        rate("ext4_writepages"),
    );
    passes.plot_promql(
        PlotOpts::counter("Pages Written vs Skipped", "writepages-pages", Unit::Count),
        "sum by (outcome) (irate(ext4_writepages_pages[5m]))".to_string(),
    );
    passes.plot_promql(
        PlotOpts::counter("Errors", "writepages-errors", Unit::Count),
        rate("ext4_writepages_errors"),
    );

    view.group(writeback);

    let mut metadata = Group::new("Metadata Reads", "metadata");

    let reads = metadata.subgroup("From the Device");
    reads.describe(
        "Metadata blocks read from the device because they were not cached, each a \
         synchronous read on the calling thread. Inode-table reads are the cost of stat and \
         atime on a cold inode cache; bitmap reads are the allocator's own cold-metadata cost.",
    );
    reads.plot_promql(
        PlotOpts::counter("Inode-table Reads", "inode-loads", Unit::Count),
        rate("ext4_inode_loads"),
    );
    reads.plot_promql(
        PlotOpts::counter("Bitmap Reads", "bitmap-loads", Unit::Count),
        "sum by (kind) (irate(ext4_bitmap_loads[5m]))".to_string(),
    );

    view.group(metadata);
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriken_query::MemoryStore;
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    /// A store holding one sample of each named counter.
    fn store_with(metrics: &[&str]) -> MemoryStore {
        let store = MemoryStore::builder().sampling_interval_ms(1000).build();
        let counters = metrics
            .iter()
            .map(|name| metriken_exposition::Counter::new(name.to_string(), 1, HashMap::new()))
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
    fn every_commit_phase_gets_a_percentile_plot() {
        let view = generate(&store_with(&["ext4_journal_commits"]), vec![]);
        let j = json(&view);
        for phase in [
            "wait",
            "request_delay",
            "running",
            "locked",
            "flushing",
            "logging",
        ] {
            assert!(
                j.contains(&format!("ext4_journal_commit_latency{{phase=\"{phase}\"}}")),
                "missing phase {phase}"
            );
        }
        assert!(j.contains("sum by (mount) (irate(ext4_journal_commits[5m]))"));
        assert!(j.contains("sum by (mount) (irate(ext4_journal_checkpoint_forced_to_close[5m]))"));
        // No allocator sampler in this recording: its groups stay out.
        assert!(!j.contains("ext4_allocations"));
    }

    #[test]
    fn fsync_calls_are_split_by_op() {
        let view = generate(&store_with(&["ext4_journal_commits"]), vec![]);
        let j = json(&view);
        assert!(j.contains("sum by (op) (irate(ext4_sync_file[5m]))"));
        assert!(j.contains("sum by (mount) (irate(ext4_sync_file_errors[5m]))"));
        assert!(j.contains("sum by (mount) (irate(ext4_errors[5m]))"));
    }

    #[test]
    fn the_allocator_sampler_renders_without_the_journal_sampler() {
        let view = generate(&store_with(&["ext4_allocations"]), vec![]);
        let j = json(&view);
        assert!(j.contains("sum by (criterion) (irate(ext4_allocations_by_criterion[5m]))"));
        assert!(j.contains(
            "sum(irate(ext4_allocation_groups_scanned[5m])) / sum(irate(ext4_allocations[5m]))"
        ));
        assert!(j.contains("sum by (kind) (irate(ext4_allocation_blocks[5m]))"));
        assert!(j.contains("\"ext4_allocation_size\""));
        assert!(j.contains("sum by (op) (irate(ext4_inodes[5m]))"));
        assert!(j.contains("sum by (outcome) (irate(ext4_writepages_pages[5m]))"));
        assert!(j.contains("sum by (mount) (irate(ext4_inode_loads[5m]))"));
        assert!(j.contains("sum by (kind) (irate(ext4_bitmap_loads[5m]))"));
        assert!(!j.contains("ext4_journal_commits"));
    }

    #[test]
    fn a_recording_without_either_sampler_renders_an_empty_section() {
        let view = generate(&MemoryStore::builder().build(), vec![]);
        assert!(!json(&view).contains("ext4_"));
    }
}
