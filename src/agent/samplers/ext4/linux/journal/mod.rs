//! Collects ext4 journal (jbd2) stats using BPF and traces:
//! * `jbd2_run_stats` — once per commit, every phase of the commit
//! * `jbd2_checkpoint_stats` — once per checkpoint
//! * `jbd2_lock_buffer_stall`
//! * `ext4_sync_file_enter` / `ext4_sync_file_exit` — fsync counts and errors
//! * `ext4_error` / `ext4_shutdown`
//!
//! And produces these stats:
//! * `ext4_journal_commit_latency` — labeled by `phase`
//! * `ext4_journal_commits`, `ext4_journal_commit_handles`,
//!   `ext4_journal_commit_blocks` (labeled by `kind`)
//! * `ext4_journal_checkpoint_latency`, `ext4_journal_checkpoints`,
//!   `ext4_journal_checkpoint_buffers` (labeled by `outcome`),
//!   `ext4_journal_checkpoint_forced_to_close`
//! * `ext4_journal_lock_buffer_stall_latency`, `ext4_journal_lock_buffer_stalls`
//! * `ext4_sync_file` (labeled by `op`), `ext4_sync_file_errors`
//! * `ext4_errors`, `ext4_shutdowns`
//!
//! Counters are per filesystem: each is a `CounterGroup` with one slot per
//! mounted ext4 (or ocfs2, since jbd2 is its journal too) filesystem, labeled
//! `mount`, `fstype`, `devnum` and `block_device` as the `filesystem` sampler
//! labels the same mount, plus slot 0 `mount="other"` for a device the mount
//! table does not know yet. The slot comes from a `dev_t` lookup the BPF
//! program does on each event (`bpf/filesystem.h`); userspace keeps that map
//! in step with the mount table (`bpf/filesystems.rs`). Histograms stay
//! host-wide until histogram groups have slots.
//!
//! jbd2 reports commit and checkpoint phases in jiffies. The BPF program
//! converts them to nanoseconds with a tick length this module measures with
//! `clock_getres(CLOCK_MONOTONIC_COARSE)` — which the kernel answers with
//! `TICK_NSEC` — and hands over through the `jiffy_ns` map. `sysconf(_SC_CLK_TCK)`
//! would be wrong here: that is `USER_HZ`, a constant 100.
//!
//! Kernel support: reading the jbd2 stats structs needs their BTF, which is in
//! vmlinux when ext4 is built in and in module BTF (`/sys/kernel/btf/jbd2`,
//! kernels 5.11+) when it is a module. A 5.8–5.10 kernel with `CONFIG_EXT4_FS=m`
//! has neither, so the struct-reading programs fail to load there and the
//! sampler reports failed rather than publishing zeros. See
//! docs/journal/2026-09-28-ext4-sampler.md and its entry in docs/backlog.md.

const NAME: &str = "ext4_journal";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/ext4_journal.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::agent::*;

use std::sync::Arc;

/// Every hook: the tracepoint name, its `tp_btf` and `raw_tp` program names,
/// and the capability label `rezolus status` shows if the active twin fails
/// to attach.
const HOOKS: &[(&str, &str, &str, &str)] = &[
    (
        "jbd2_run_stats",
        "jbd2_run_stats_btf",
        "jbd2_run_stats_raw",
        "journal commit phases",
    ),
    (
        "jbd2_checkpoint_stats",
        "jbd2_checkpoint_stats_btf",
        "jbd2_checkpoint_stats_raw",
        "journal checkpoints",
    ),
    (
        "jbd2_lock_buffer_stall",
        "jbd2_lock_buffer_stall_btf",
        "jbd2_lock_buffer_stall_raw",
        "journal lock-buffer stalls",
    ),
    (
        "ext4_sync_file_enter",
        "ext4_sync_file_enter_btf",
        "ext4_sync_file_enter_raw",
        "fsync counts",
    ),
    (
        "ext4_sync_file_exit",
        "ext4_sync_file_exit_btf",
        "ext4_sync_file_exit_raw",
        "fsync errors",
    ),
    (
        "ext4_error",
        "ext4_error_btf",
        "ext4_error_raw",
        "filesystem errors",
    ),
    (
        "ext4_shutdown",
        "ext4_shutdown_btf",
        "ext4_shutdown_raw",
        "filesystem shutdowns",
    ),
];

/// What a filesystem slot means, published when the mount table changes.
static FS_IDENTITY: crate::agent::identity::SlotIdentity =
    crate::agent::identity::SlotIdentity::new(FS_IDENTITY_GROUPS);

static FS_IDENTITY_GROUPS: &[crate::agent::identity::GroupMetrics] = &[(
    &COUNTERS_ACQ,
    &[
        &EXT4_JOURNAL_COMMITS,
        &EXT4_JOURNAL_COMMIT_HANDLES,
        &EXT4_JOURNAL_COMMIT_BLOCKS_DIRTIED,
        &EXT4_JOURNAL_COMMIT_BLOCKS_LOGGED,
        &EXT4_JOURNAL_CHECKPOINTS,
        &EXT4_JOURNAL_CHECKPOINT_BUFFERS_WRITTEN,
        &EXT4_JOURNAL_CHECKPOINT_BUFFERS_DROPPED,
        &EXT4_JOURNAL_CHECKPOINT_FORCED_TO_CLOSE,
        &EXT4_SYNC_FILE_FSYNC,
        &EXT4_SYNC_FILE_FDATASYNC,
        &EXT4_SYNC_FILE_ERRORS,
        &EXT4_ERRORS,
        &EXT4_SHUTDOWNS,
        &EXT4_JOURNAL_LOCK_BUFFER_STALLS,
    ],
)];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Order MUST match the C_* indices in mod.bpf.c.
    let counters = vec![
        &EXT4_JOURNAL_COMMITS,
        &EXT4_JOURNAL_COMMIT_HANDLES,
        &EXT4_JOURNAL_COMMIT_BLOCKS_DIRTIED,
        &EXT4_JOURNAL_COMMIT_BLOCKS_LOGGED,
        &EXT4_JOURNAL_CHECKPOINTS,
        &EXT4_JOURNAL_CHECKPOINT_BUFFERS_WRITTEN,
        &EXT4_JOURNAL_CHECKPOINT_BUFFERS_DROPPED,
        &EXT4_JOURNAL_CHECKPOINT_FORCED_TO_CLOSE,
        &EXT4_SYNC_FILE_FSYNC,
        &EXT4_SYNC_FILE_FDATASYNC,
        &EXT4_SYNC_FILE_ERRORS,
        &EXT4_ERRORS,
        &EXT4_SHUTDOWNS,
        &EXT4_JOURNAL_LOCK_BUFFER_STALLS,
    ];

    let tick = match jiffy_ns() {
        Some(ns) => ns,
        None => {
            warn!(
                "{NAME}: clock_getres(CLOCK_MONOTONIC_COARSE) gave no tick length; \
                 journal commit and checkpoint latency histograms will stay empty"
            );
            0
        }
    };
    debug!("{NAME} jiffy = {tick} ns");

    // Per hook, keep the tp_btf twin only when the kernel's BTF (vmlinux or a
    // module's) carries that tracepoint; otherwise the raw_tp twin, whose miss
    // on a kernel without the tracepoint is a tolerated ENOENT at attach. See
    // `kernel_btf_has_tracepoints` for why `kernel_has_btf()` is not enough.
    let mut disabled: Vec<&'static str> = Vec::with_capacity(HOOKS.len());
    let mut required: Vec<(&'static str, &'static str)> = Vec::with_capacity(HOOKS.len());

    for (tracepoint, btf, raw, label) in HOOKS {
        if kernel_btf_has_tracepoints(&[tracepoint]) {
            disabled.push(raw);
            required.push((btf, label));
        } else {
            disabled.push(btf);
            required.push((raw, label));
        }
    }

    let bpf = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .filesystem_counters(
        "counters",
        "fs_slots",
        counters,
        &COUNTERS_ACQ,
        &FS_IDENTITY,
    )
    // The six commit phases share ONE group: like entities of one family,
    // distinguished by `phase` — see stats.rs.
    .histogram(
        "commit_wait",
        &EXT4_JOURNAL_COMMIT_WAIT,
        &COMMIT_LATENCIES_ACQ,
    )
    .histogram(
        "commit_request_delay",
        &EXT4_JOURNAL_COMMIT_REQUEST_DELAY,
        &COMMIT_LATENCIES_ACQ,
    )
    .histogram(
        "commit_running",
        &EXT4_JOURNAL_COMMIT_RUNNING,
        &COMMIT_LATENCIES_ACQ,
    )
    .histogram(
        "commit_locked",
        &EXT4_JOURNAL_COMMIT_LOCKED,
        &COMMIT_LATENCIES_ACQ,
    )
    .histogram(
        "commit_flushing",
        &EXT4_JOURNAL_COMMIT_FLUSHING,
        &COMMIT_LATENCIES_ACQ,
    )
    .histogram(
        "commit_logging",
        &EXT4_JOURNAL_COMMIT_LOGGING,
        &COMMIT_LATENCIES_ACQ,
    )
    .histogram(
        "checkpoint_latency",
        &EXT4_JOURNAL_CHECKPOINT_LATENCY,
        &CHECKPOINT_LATENCIES_ACQ,
    )
    .histogram(
        "lock_buffer_stall_latency",
        &EXT4_JOURNAL_LOCK_BUFFER_STALL_LATENCY,
        &STALL_LATENCIES_ACQ,
    )
    .map("jiffy_ns", vec![tick])
    .disabled_programs(&disabled)
    .required_programs(&required)
    .build()?;

    Ok(Some(Box::new(bpf)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map<'_> {
        match name {
            "counters" => &self.maps.counters,
            "fs_slots" => &self.maps.fs_slots,
            "jiffy_ns" => &self.maps.jiffy_ns,
            "commit_wait" => &self.maps.commit_wait,
            "commit_request_delay" => &self.maps.commit_request_delay,
            "commit_running" => &self.maps.commit_running,
            "commit_locked" => &self.maps.commit_locked,
            "commit_flushing" => &self.maps.commit_flushing,
            "commit_logging" => &self.maps.commit_logging,
            "checkpoint_latency" => &self.maps.checkpoint_latency,
            "lock_buffer_stall_latency" => &self.maps.lock_buffer_stall_latency,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} jbd2_run_stats_btf() BPF instruction count: {}",
            self.progs.jbd2_run_stats_btf.insn_cnt()
        );
        debug!(
            "{NAME} jbd2_checkpoint_stats_btf() BPF instruction count: {}",
            self.progs.jbd2_checkpoint_stats_btf.insn_cnt()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every hook names two distinct programs, and no program is named twice:
    /// `disabled_programs` refuses a name that is not in the skeleton, so a
    /// typo here would fail every load, and a duplicate would leave a twin
    /// autoloaded and double-count.
    #[test]
    fn hook_program_names_are_distinct() {
        let mut names: Vec<&str> = HOOKS
            .iter()
            .flat_map(|(_, btf, raw, _)| [*btf, *raw])
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
        assert_eq!(total, 2 * HOOKS.len());
    }
}
