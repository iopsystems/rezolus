//! The ext4 journal: commit phases, checkpoints, fsync and errors. Host-wide,
//! from the `ext4_journal` sampler.

use crate::MetricsSource;
use crate::plot::*;

fn rate(metric: &str) -> String {
    format!("sum(irate({metric}[5m]))")
}

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    if !has_metric(data, "ext4_journal_commits") {
        return view;
    }

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

    view
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
        assert!(j.contains("sum(irate(ext4_journal_commits[5m]))"));
        assert!(j.contains("sum(irate(ext4_journal_checkpoint_forced_to_close[5m]))"));
    }

    #[test]
    fn fsync_calls_are_split_by_op() {
        let view = generate(&store_with(&["ext4_journal_commits"]), vec![]);
        let j = json(&view);
        assert!(j.contains("sum by (op) (irate(ext4_sync_file[5m]))"));
        assert!(j.contains("sum(irate(ext4_sync_file_errors[5m]))"));
        assert!(j.contains("sum(irate(ext4_errors[5m]))"));
    }

    #[test]
    fn a_recording_without_the_sampler_renders_an_empty_section() {
        let view = generate(&MemoryStore::builder().build(), vec![]);
        assert!(!json(&view).contains("ext4_"));
    }
}
