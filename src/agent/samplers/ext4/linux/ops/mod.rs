//! Times ext4's request-path operations from the calling thread's side using
//! BPF and traces:
//! * `ext4_sync_file_enter` / `ext4_sync_file_exit` — fsync and fdatasync
//! * `ext4_unlink_enter` / `ext4_unlink_exit` — unlink
//! * `fentry`/`fexit` on `ext4_file_write_iter` — write, with bytes written
//! * `fentry`/`fexit` on `ext4_rename2` — rename
//!
//! And produces these stats:
//! * `ext4_op_latency{op}` — host-wide histograms
//! * `ext4_ops{op}`, `ext4_op_time{op}`, `ext4_op_errors{op}`,
//!   `ext4_write_bytes` — per filesystem (`mount`, `fstype`, `devnum`,
//!   `block_device`, plus `mount="other"`), as `ext4_journal`'s counters are
//! * `cgroup_ext4_ops{op}`, `cgroup_ext4_op_time{op}` — per cgroup of the
//!   calling thread: the time request threads are held inside the filesystem.
//!   Only with `cgroup_attribution = true`: the per-cgroup path is half the
//!   end hook's cost (265 of 535 ns), so it is off by default
//!
//! `ext4_op_time / ext4_ops` is the mean latency per filesystem, and
//! `ext4_write_bytes` is the first term of the write-amplification chain the
//! ext4 dashboard's Write Path group draws (bytes written, pages written back,
//! journal blocks logged, device bytes).
//!
//! **Opt-in**: both probes of a pair run on the request path, once per call,
//! so this is the most expensive of the ext4 samplers (measured in the journal
//! entry). It is in the config's `OPT_IN_SAMPLERS`, so `[defaults]` never
//! enables it; only `[samplers.ext4_ops] enabled = true` does.
//!
//! Kernel support: the start timestamp is in task local storage
//! (`BPF_MAP_TYPE_TASK_STORAGE`, usable from tracing programs since 5.12),
//! write and rename are `fentry`/`fexit` on ext4 functions (module BTF where
//! ext4 is a module, 5.11), and the fsync and unlink tracepoints are `tp_btf`
//! only, with no `raw_tp` twin, because a kernel old enough to lack the BTF
//! for them cannot load the rest either. So the floor is 5.12. `ext4_rename2`
//! has taken six arguments since 5.12 (a namespace argument first); the
//! sampler confirms that from BTF and disables the rename pair on any other
//! count rather than read the wrong slot. On a kernel whose BTF lacks
//! `ext4_file_write_iter` or `ext4_rename2` the corresponding programs are
//! disabled: that operation's histogram stays empty and its counters read 0,
//! not absent. See docs/journal/2026-09-28-filesystem-telemetry-gaps.md
//! (C5, C6).

const NAME: &str = "ext4_ops";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/ext4_ops.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::agent::*;

use std::sync::Arc;

unsafe impl plain::Plain for bpf::types::cgroup_info {}
impl_cgroup_info!(bpf::types::cgroup_info);

/// The tracepoints the fsync and unlink pairs attach to. All four or nothing:
/// a `tp_btf` target missing from BTF fails the whole skeleton at load.
const TRACEPOINTS: &[&str] = &[
    "ext4_sync_file_enter",
    "ext4_sync_file_exit",
    "ext4_unlink_enter",
    "ext4_unlink_exit",
];

/// What a filesystem slot means, published when the mount table changes.
static FS_IDENTITY: crate::agent::identity::SlotIdentity =
    crate::agent::identity::SlotIdentity::new(FS_IDENTITY_GROUPS);

static FS_IDENTITY_GROUPS: &[crate::agent::identity::GroupMetrics] = &[(
    &COUNTERS_ACQ,
    &[
        &EXT4_OPS_FSYNC,
        &EXT4_OPS_UNLINK,
        &EXT4_OPS_WRITE,
        &EXT4_OPS_RENAME,
        &EXT4_OP_TIME_FSYNC,
        &EXT4_OP_TIME_UNLINK,
        &EXT4_OP_TIME_WRITE,
        &EXT4_OP_TIME_RENAME,
        &EXT4_OP_ERRORS_FSYNC,
        &EXT4_OP_ERRORS_UNLINK,
        &EXT4_OP_ERRORS_WRITE,
        &EXT4_OP_ERRORS_RENAME,
        &EXT4_WRITE_BYTES,
    ],
)];

/// What a cgroup slot means, published as the BPF side discovers cgroups.
static CGROUP_IDENTITY: crate::agent::identity::SlotIdentity =
    crate::agent::identity::SlotIdentity::new(CGROUP_IDENTITY_GROUPS);

static CGROUP_IDENTITY_GROUPS: &[crate::agent::identity::GroupMetrics] = &[(
    &CGROUP_ACQ,
    &[
        &CGROUP_EXT4_OPS_FSYNC,
        &CGROUP_EXT4_OPS_UNLINK,
        &CGROUP_EXT4_OPS_WRITE,
        &CGROUP_EXT4_OPS_RENAME,
        &CGROUP_EXT4_OP_TIME_FSYNC,
        &CGROUP_EXT4_OP_TIME_UNLINK,
        &CGROUP_EXT4_OP_TIME_WRITE,
        &CGROUP_EXT4_OP_TIME_RENAME,
    ],
)];

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, &CGROUP_IDENTITY)
}

/// Which programs to load for the write and rename pairs, from the kernel's
/// BTF: the names to disable and the (program, capability) pairs required.
fn select_programs(
    has_write_iter: bool,
    rename_arity: Option<u32>,
) -> (Vec<&'static str>, Vec<(&'static str, &'static str)>) {
    let mut disabled = Vec::new();
    let mut required = vec![
        ("ext4_sync_file_enter", "fsync latency"),
        ("ext4_sync_file_exit", "fsync latency"),
        ("ext4_unlink_enter", "unlink latency"),
        ("ext4_unlink_exit", "unlink latency"),
    ];

    if has_write_iter {
        required.push(("ext4_file_write_iter_fentry", "write latency"));
        required.push(("ext4_file_write_iter_fexit", "write latency"));
    } else {
        disabled.push("ext4_file_write_iter_fentry");
        disabled.push("ext4_file_write_iter_fexit");
    }

    // The program reads six arguments and the return value by position; on
    // any other arity it would read the wrong slot, so it is disabled.
    if rename_arity == Some(6) {
        required.push(("ext4_rename2_fentry", "rename latency"));
        required.push(("ext4_rename2_fexit", "rename latency"));
    } else {
        disabled.push("ext4_rename2_fentry");
        disabled.push("ext4_rename2_fexit");
    }

    (disabled, required)
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // This machine cannot, rather than this sampler failed: both checks report
    // unsupported, so `rezolus status` does not exit non-zero for a kernel
    // that predates what the sampler needs. The helper probe is the one that
    // catches 5.8–5.11: those kernels can have every tracepoint in BTF and
    // still refuse `bpf_task_storage_get` from a tracing program at load.
    if !kernel_tracing_has_task_storage() {
        return Err(crate::agent::sampler_status::Unsupported(
            "tracing programs cannot use task local storage on this kernel (needs 5.12+)"
                .to_string(),
        )
        .into());
    }

    if !kernel_btf_has_tracepoints(TRACEPOINTS) {
        return Err(crate::agent::sampler_status::Unsupported(
            "the kernel's BTF does not describe the ext4 fsync and unlink tracepoints; needs \
             vmlinux BTF with ext4 built in, or module BTF (kernels 5.11+) with ext4 as a module"
                .to_string(),
        )
        .into());
    }

    let has_write_iter = kernel_btf_has_funcs(&["ext4_file_write_iter"]);
    if !has_write_iter {
        info!("{NAME}: the kernel's BTF has no ext4_file_write_iter; the write series read 0");
    }

    let rename_arity = kernel_btf_func_arg_count("ext4_rename2");
    match rename_arity {
        Some(6) => {}
        Some(n) => info!(
            "{NAME}: ext4_rename2 takes {n} arguments, not the 6 the program reads; the rename \
             series read 0"
        ),
        None => info!("{NAME}: the kernel's BTF has no ext4_rename2; the rename series read 0"),
    }

    let (disabled, required) = select_programs(has_write_iter, rename_arity);

    // Order MUST match the C_* indices in mod.bpf.c: ops, time, errors (each
    // fsync, unlink, write, rename), then write bytes.
    let counters = vec![
        &EXT4_OPS_FSYNC,
        &EXT4_OPS_UNLINK,
        &EXT4_OPS_WRITE,
        &EXT4_OPS_RENAME,
        &EXT4_OP_TIME_FSYNC,
        &EXT4_OP_TIME_UNLINK,
        &EXT4_OP_TIME_WRITE,
        &EXT4_OP_TIME_RENAME,
        &EXT4_OP_ERRORS_FSYNC,
        &EXT4_OP_ERRORS_UNLINK,
        &EXT4_OP_ERRORS_WRITE,
        &EXT4_OP_ERRORS_RENAME,
        &EXT4_WRITE_BYTES,
    ];

    let cgroup_attribution = config.cgroup_attribution(NAME);

    let mut builder = BpfBuilder::new(
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
    // The four latencies share ONE group: like entities of one family,
    // distinguished by `op` — see stats.rs.
    .histogram("fsync_latency", &EXT4_OP_LATENCY_FSYNC, &LATENCIES_ACQ)
    .histogram("unlink_latency", &EXT4_OP_LATENCY_UNLINK, &LATENCIES_ACQ)
    .histogram("write_latency", &EXT4_OP_LATENCY_WRITE, &LATENCIES_ACQ)
    .histogram("rename_latency", &EXT4_OP_LATENCY_RENAME, &LATENCIES_ACQ)
    .disabled_programs(&disabled)
    .required_programs(&required)
    // The switch is read-only data the verifier folds at load (see
    // `cgroup_attribution` in mod.bpf.c); the cgroup maps and their series
    // exist only when it is on.
    .pre_load(move |open| {
        open.maps
            .rodata_data
            .as_mut()
            .expect("the program declares read-only data")
            .cgroup_attribution = cgroup_attribution as u8;
    });

    if cgroup_attribution {
        builder = builder
            .packed_counters("cgroup_ops_fsync", &CGROUP_EXT4_OPS_FSYNC, &CGROUP_ACQ)
            .packed_counters("cgroup_ops_unlink", &CGROUP_EXT4_OPS_UNLINK, &CGROUP_ACQ)
            .packed_counters("cgroup_ops_write", &CGROUP_EXT4_OPS_WRITE, &CGROUP_ACQ)
            .packed_counters("cgroup_ops_rename", &CGROUP_EXT4_OPS_RENAME, &CGROUP_ACQ)
            .packed_counters("cgroup_time_fsync", &CGROUP_EXT4_OP_TIME_FSYNC, &CGROUP_ACQ)
            .packed_counters(
                "cgroup_time_unlink",
                &CGROUP_EXT4_OP_TIME_UNLINK,
                &CGROUP_ACQ,
            )
            .packed_counters("cgroup_time_write", &CGROUP_EXT4_OP_TIME_WRITE, &CGROUP_ACQ)
            .packed_counters(
                "cgroup_time_rename",
                &CGROUP_EXT4_OP_TIME_RENAME,
                &CGROUP_ACQ,
            )
            .ringbuf_handler("cgroup_info", handle_cgroup_info);
    }

    let bpf = builder.build()?;

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
            "fsync_latency" => &self.maps.fsync_latency,
            "unlink_latency" => &self.maps.unlink_latency,
            "write_latency" => &self.maps.write_latency,
            "rename_latency" => &self.maps.rename_latency,
            "cgroup_info" => &self.maps.cgroup_info,
            "cgroup_ops_fsync" => &self.maps.cgroup_ops_fsync,
            "cgroup_ops_unlink" => &self.maps.cgroup_ops_unlink,
            "cgroup_ops_write" => &self.maps.cgroup_ops_write,
            "cgroup_ops_rename" => &self.maps.cgroup_ops_rename,
            "cgroup_time_fsync" => &self.maps.cgroup_time_fsync,
            "cgroup_time_unlink" => &self.maps.cgroup_time_unlink,
            "cgroup_time_write" => &self.maps.cgroup_time_write,
            "cgroup_time_rename" => &self.maps.cgroup_time_rename,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} ext4_sync_file_exit() BPF instruction count: {}",
            self.progs.ext4_sync_file_exit.insn_cnt()
        );
        debug!(
            "{NAME} ext4_file_write_iter_fexit() BPF instruction count: {}",
            self.progs.ext4_file_write_iter_fexit.insn_cnt()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rename pair is loaded on the one arity the program reads and on no
    /// other, so a signature the program was not written for is never read
    /// by position.
    #[test]
    fn rename_programs_are_selected_by_arity() {
        let (disabled, required) = select_programs(true, Some(6));
        assert!(required.iter().any(|(p, _)| *p == "ext4_rename2_fexit"));
        assert!(!disabled.contains(&"ext4_rename2_fentry"));
        assert!(!disabled.contains(&"ext4_file_write_iter_fexit"));

        for arity in [None, Some(4), Some(5), Some(7)] {
            let (disabled, required) = select_programs(false, arity);
            assert!(disabled.contains(&"ext4_rename2_fentry"));
            assert!(disabled.contains(&"ext4_rename2_fexit"));
            assert!(disabled.contains(&"ext4_file_write_iter_fentry"));
            assert!(!required.iter().any(|(p, _)| p.contains("rename")));
            assert!(!required.iter().any(|(p, _)| p.contains("write_iter")));
        }
    }

    /// No program is both disabled and required, and the tracepoint pairs
    /// are always required.
    #[test]
    fn disabled_and_required_are_disjoint() {
        for (write, arity) in [(true, Some(6)), (false, Some(5)), (true, None)] {
            let (disabled, required) = select_programs(write, arity);
            for (prog, _) in &required {
                assert!(
                    !disabled.contains(prog),
                    "{prog} both disabled and required"
                );
            }
            assert!(required.iter().any(|(p, _)| *p == "ext4_sync_file_exit"));
            assert!(required.iter().any(|(p, _)| *p == "ext4_unlink_exit"));
        }
    }
}
