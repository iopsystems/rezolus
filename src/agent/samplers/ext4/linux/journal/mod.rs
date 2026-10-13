//! Collects ext4 journal (jbd2) stats using BPF and traces:
//! * `jbd2_commit_locking` / `jbd2_commit_flushing` / `jbd2_commit_logging` —
//!   the boundaries of each commit's phases
//! * `jbd2_run_stats` — once per commit, the end of logging and the counts
//! * `jbd2_checkpoint` / `jbd2_checkpoint_stats` — a checkpoint's start and end
//! * `jbd2__journal_start`, `jbd2_journal_start`, `jbd2_journal_start_reserved`
//!   (fentry, or kprobe) and `jbd2_handle_start` — handle start latency
//! * `jbd2_lock_buffer_stall`
//! * `ext4_sync_file_enter` / `ext4_sync_file_exit` — fsync counts and errors
//! * `ext4_error` / `ext4_shutdown`
//!
//! And produces these stats:
//! * `ext4_journal_commit_latency` — labeled by `phase`
//! * `ext4_journal_handle_start_latency`
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
//! Every latency except lock-buffer stalls is timed in BPF with
//! `bpf_ktime_get_ns()`. jbd2's own `jbd2_run_stats` and
//! `jbd2_checkpoint_stats` times are differences of two jiffies reads, which
//! put a phase shorter than one tick (1–10 ms) at 0 or 1 tick, so they are not
//! used. jbd2's `rs_wait` is not published; `ext4_journal_handle_start_latency`
//! measures every handle's wait to start. `rs_request_delay` is not published:
//! jbd2 stamps the request only in `__jbd2_log_start_commit` (static from 6.0,
//! called from four places in one file, so it may be inlined), and no
//! tracepoint marks the request.
//!
//! Kernel support: reading jbd2's structs (stats, transaction, journal) needs
//! their BTF, which is in vmlinux when ext4 is built in and in module BTF
//! (`/sys/kernel/btf/jbd2`, kernels 5.11+) when it is a module. A 5.8–5.10
//! kernel with `CONFIG_EXT4_FS=m` has neither, so the struct-reading programs
//! fail to load there and the sampler reports failed rather than publishing
//! zeros. Each handle-start entry hook uses a kprobe where fentry cannot
//! attach to its function (`kernel_can_fentry`): `jbd2__journal_start` on
//! x86_64 before 6.6, which takes trampolines of up to 6 arguments, and every
//! hook on aarch64 without ftrace direct calls (before 6.4). See
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

/// Every tracepoint hook: the tracepoint name, its `tp_btf` and `raw_tp`
/// program names, and the capability label `rezolus status` shows if the
/// active twin fails to attach.
const HOOKS: &[(&str, &str, &str, &str)] = &[
    (
        "jbd2_commit_locking",
        "jbd2_commit_locking_btf",
        "jbd2_commit_locking_raw",
        "journal commit phases",
    ),
    (
        "jbd2_commit_flushing",
        "jbd2_commit_flushing_btf",
        "jbd2_commit_flushing_raw",
        "journal commit phases",
    ),
    (
        "jbd2_commit_logging",
        "jbd2_commit_logging_btf",
        "jbd2_commit_logging_raw",
        "journal commit phases",
    ),
    (
        "jbd2_run_stats",
        "jbd2_run_stats_btf",
        "jbd2_run_stats_raw",
        "journal commit phases",
    ),
    (
        "jbd2_checkpoint",
        "jbd2_checkpoint_btf",
        "jbd2_checkpoint_raw",
        "journal checkpoints",
    ),
    (
        "jbd2_checkpoint_stats",
        "jbd2_checkpoint_stats_btf",
        "jbd2_checkpoint_stats_raw",
        "journal checkpoints",
    ),
    (
        "jbd2_handle_start",
        "jbd2_handle_start_btf",
        "jbd2_handle_start_raw",
        "journal handle start latency",
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

/// Every function hook: the kernel function, its `fentry` and `kprobe` program
/// names, and the capability label. All three stamp a handle's start; the
/// `jbd2_handle_start` tracepoint ends it.
const FUNCTION_HOOKS: &[(&str, &str, &str, &str)] = &[
    (
        "jbd2__journal_start",
        "jbd2__journal_start_fentry",
        "jbd2__journal_start_kprobe",
        "journal handle start latency",
    ),
    (
        "jbd2_journal_start",
        "jbd2_journal_start_fentry",
        "jbd2_journal_start_kprobe",
        "journal handle start latency",
    ),
    (
        "jbd2_journal_start_reserved",
        "jbd2_journal_start_reserved_fentry",
        "jbd2_journal_start_reserved_kprobe",
        "journal handle start latency",
    ),
];

/// What a filesystem slot means, published when the mount table changes.
static FS_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(FS_IDENTITY_GROUPS);

static FS_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[&[
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
]];

/// Kernel modules this sampler needs loaded; see `samplers::deferred`.
const MODULES: &[&str] = &["ext4", "jbd2"];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    crate::agent::samplers::deferred::until_loaded(NAME, MODULES, config, init_now)
}

fn init_now(config: Arc<Config>) -> SamplerResult {
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

    // Per hook, keep the tp_btf twin only when the kernel's BTF (vmlinux or a
    // module's) carries that tracepoint; otherwise the raw_tp twin, whose miss
    // on a kernel without the tracepoint is a tolerated ENOENT at attach. See
    // `kernel_btf_has_tracepoints` for why `kernel_has_btf()` is not enough.
    let hooks = HOOKS.len() + FUNCTION_HOOKS.len();
    let mut disabled: Vec<&'static str> = Vec::with_capacity(hooks);
    let mut required: Vec<(&'static str, &'static str)> = Vec::with_capacity(hooks);

    for (tracepoint, btf, raw, label) in HOOKS {
        if kernel_btf_has_tracepoints(&[tracepoint]) {
            disabled.push(raw);
            required.push((btf, label));
        } else {
            disabled.push(btf);
            required.push((raw, label));
        }
    }

    // Per function, the fentry twin when it can attach there (BTF, vmlinux or
    // jbd2's module, describes the function and the trampoline supports its
    // architecture and argument count), else the kprobe twin.
    // jbd2__journal_start takes 7 arguments, more than x86_64 trampolines
    // take before 6.6.
    for (function, fentry, kprobe, label) in FUNCTION_HOOKS {
        if kernel_can_fentry(function) {
            disabled.push(kprobe);
            required.push((fentry, label));
        } else {
            disabled.push(fentry);
            required.push((kprobe, label));
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
    // The four commit phases share ONE group: like entities of one family,
    // distinguished by `phase` — see stats.rs.
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
    .histogram(
        "handle_start_latency",
        &EXT4_JOURNAL_HANDLE_START_LATENCY,
        &HANDLE_LATENCIES_ACQ,
    )
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
            "commit_running" => &self.maps.commit_running,
            "commit_locked" => &self.maps.commit_locked,
            "commit_flushing" => &self.maps.commit_flushing,
            "commit_logging" => &self.maps.commit_logging,
            "checkpoint_latency" => &self.maps.checkpoint_latency,
            "lock_buffer_stall_latency" => &self.maps.lock_buffer_stall_latency,
            "handle_start_latency" => &self.maps.handle_start_latency,
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
            .chain(FUNCTION_HOOKS)
            .flat_map(|(_, first, second, _)| [*first, *second])
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
        assert_eq!(total, 2 * (HOOKS.len() + FUNCTION_HOOKS.len()));
    }

    /// Every hook's programs exist in the BPF source under the section that
    /// attaches them to the named tracepoint or function. A wrong `raw_tp` or
    /// `kprobe` target is an ENOENT at attach, which the builder reports as
    /// unsupported, so on a kernel that selects that twin the histogram stays
    /// empty with no error.
    #[test]
    fn hook_programs_attach_to_their_targets() {
        let source = include_str!("mod.bpf.c");

        for (tracepoint, btf, raw, _) in HOOKS {
            for (section, program) in [("tp_btf", btf), ("raw_tp", raw)] {
                let attach = format!("SEC(\"{section}/{tracepoint}\")\nint BPF_PROG({program}");
                assert!(source.contains(&attach), "missing {attach}");
            }
        }

        for (function, fentry, kprobe, _) in FUNCTION_HOOKS {
            let attach = format!("SEC(\"fentry/{function}\")\nint BPF_PROG({fentry})");
            assert!(source.contains(&attach), "missing {attach}");
            let attach = format!("SEC(\"kprobe/{function}\")\nint BPF_KPROBE({kprobe})");
            assert!(source.contains(&attach), "missing {attach}");
        }
    }

    /// Each histogram's map is read into its own metric: the builder pairs
    /// the map name with the metric, and the skeleton lookup returns the map
    /// of that name. Swapping two phases in either place fails here.
    #[test]
    fn histogram_maps_feed_their_metrics() {
        let source = include_str!("mod.rs");

        for (map, metric) in [
            ("commit_running", "EXT4_JOURNAL_COMMIT_RUNNING"),
            ("commit_locked", "EXT4_JOURNAL_COMMIT_LOCKED"),
            ("commit_flushing", "EXT4_JOURNAL_COMMIT_FLUSHING"),
            ("commit_logging", "EXT4_JOURNAL_COMMIT_LOGGING"),
            ("checkpoint_latency", "EXT4_JOURNAL_CHECKPOINT_LATENCY"),
            (
                "lock_buffer_stall_latency",
                "EXT4_JOURNAL_LOCK_BUFFER_STALL_LATENCY",
            ),
            ("handle_start_latency", "EXT4_JOURNAL_HANDLE_START_LATENCY"),
        ] {
            let pair = format!("\"{map}\",\n        &{metric},");
            assert!(source.contains(&pair), "missing {pair}");
            let arm = format!("\"{map}\" => &self.maps.{map},");
            assert!(source.contains(&arm), "missing {arm}");
        }
    }
}
