//! Times the two places a thread blocks on the XFS log, using BPF and traces:
//! * `xfs_log_grant_sleep` / `xfs_log_grant_wake` — a transaction waiting for
//!   log space
//! * `fentry`/`fexit` on `xfs_log_force` and `xfs_log_force_seq` — a log
//!   force, the synchronous log write an fsync waits for
//! * `xfs_log_cil_wait` — a committing transaction that found the CIL over
//!   its hard limit (count only; nothing traces the wake)
//!
//! And produces these stats:
//! * `xfs_log_wait_latency{wait}` — host-wide histograms
//! * `xfs_log_waits{wait}`, `xfs_log_wait_time{wait}` — per filesystem
//!   (`mount`, `fstype`, `devnum`, `block_device`, plus `mount="other"`), as
//!   the ext4 samplers' counters are
//! * `cgroup_xfs_log_waits{wait}`, `cgroup_xfs_log_wait_time{wait}` — per
//!   cgroup of the waiting thread: the time request threads are held by the
//!   log. Only with `cgroup_attribution = true`; the per-cgroup path is half
//!   the end hook's cost, so it is off by default
//!
//! The counts duplicate two fields the `xfs_stats` sampler reads from sysfs
//! (`xfs_log_space_sleeps`, `xfs_log_forces`) and should equal them per
//! mount; the time and the cgroup are what the stats file cannot carry.
//! `xfs_log_wait_time / xfs_log_waits` is the mean per filesystem.
//!
//! **Opt-in**: the force pairs run on every fsync, so this costs on the
//! request path (measured in the journal entry). It is in the config's
//! `OPT_IN_SAMPLERS`, so `[defaults]` never enables it; only
//! `[samplers.xfs_log] enabled = true` does.
//!
//! Kernel support: the start timestamp is in task local storage
//! (`BPF_MAP_TYPE_TASK_STORAGE`, usable from tracing programs since 5.12),
//! and the tracepoints and functions are XFS's, so where XFS is a module the
//! kernel needs module BTF (5.11). The floor is 5.12. The fsync path's force
//! is `xfs_log_force_seq` from 5.13 and `xfs_log_force_lsn` before; the
//! sampler loads whichever the kernel's BTF has. On a kernel whose BTF lacks
//! `xfs_log_cil_wait` that counter reads 0, not absent. See
//! docs/journal/2026-09-29-xfs-samplers.md (step 2).

const NAME: &str = "xfs_log";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/xfs_log.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::agent::*;

use std::sync::Arc;

unsafe impl plain::Plain for bpf::types::cgroup_info {}
impl_cgroup_info!(bpf::types::cgroup_info);

/// The tracepoints the log-space pair attaches to. Both or nothing: a `tp_btf`
/// target missing from BTF fails the whole skeleton at load.
const TRACEPOINTS: &[&str] = &["xfs_log_grant_sleep", "xfs_log_grant_wake"];

/// What a filesystem slot means, published when the mount table changes.
static FS_IDENTITY: crate::agent::identity::SlotIdentity =
    crate::agent::identity::SlotIdentity::new(FS_IDENTITY_GROUPS);

#[linkme::distributed_slice(crate::agent::identity::SLOT_IDENTITIES)]
static FS_IDENTITY_REG: &'static crate::agent::identity::SlotIdentity = &FS_IDENTITY;

static FS_IDENTITY_GROUPS: &[crate::agent::identity::GroupMetrics] = &[(
    &COUNTERS_ACQ,
    &[
        &XFS_LOG_WAITS_SPACE,
        &XFS_LOG_WAITS_FORCE,
        &XFS_LOG_WAITS_CIL,
        &XFS_LOG_WAIT_TIME_SPACE,
        &XFS_LOG_WAIT_TIME_FORCE,
    ],
)];

/// What a cgroup slot means, published as the BPF side discovers cgroups.
static CGROUP_IDENTITY: crate::agent::identity::SlotIdentity =
    crate::agent::identity::SlotIdentity::new(CGROUP_IDENTITY_GROUPS);

#[linkme::distributed_slice(crate::agent::identity::SLOT_IDENTITIES)]
static CGROUP_IDENTITY_REG: &'static crate::agent::identity::SlotIdentity = &CGROUP_IDENTITY;

static CGROUP_IDENTITY_GROUPS: &[crate::agent::identity::GroupMetrics] = &[(
    &CGROUP_ACQ,
    &[
        &CGROUP_XFS_LOG_WAITS_SPACE,
        &CGROUP_XFS_LOG_WAITS_FORCE,
        &CGROUP_XFS_LOG_WAIT_TIME_SPACE,
        &CGROUP_XFS_LOG_WAIT_TIME_FORCE,
    ],
)];

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, &CGROUP_IDENTITY)
}

/// Which programs to load, from the kernel's BTF: the names to disable and
/// the (program, capability) pairs required. The log-space pair and the
/// whole-log force pair are always required; the fsync path's force is one
/// of two functions by kernel version, and the CIL wait is optional.
fn select_programs(
    has_force_seq: bool,
    has_force_lsn: bool,
    has_cil_wait: bool,
) -> (Vec<&'static str>, Vec<(&'static str, &'static str)>) {
    let mut disabled = Vec::new();
    let mut required = vec![
        ("xfs_log_grant_sleep", "log space wait latency"),
        ("xfs_log_grant_wake", "log space wait latency"),
        ("xfs_log_force_fentry", "log force latency"),
        ("xfs_log_force_fexit", "log force latency"),
    ];

    // The two are the same function under two names across kernel versions;
    // a kernel has one of them. Prefer _seq if BTF somehow had both.
    if has_force_seq {
        required.push(("xfs_log_force_seq_fentry", "fsync log force latency"));
        required.push(("xfs_log_force_seq_fexit", "fsync log force latency"));
        disabled.push("xfs_log_force_lsn_fentry");
        disabled.push("xfs_log_force_lsn_fexit");
    } else if has_force_lsn {
        required.push(("xfs_log_force_lsn_fentry", "fsync log force latency"));
        required.push(("xfs_log_force_lsn_fexit", "fsync log force latency"));
        disabled.push("xfs_log_force_seq_fentry");
        disabled.push("xfs_log_force_seq_fexit");
    } else {
        disabled.push("xfs_log_force_seq_fentry");
        disabled.push("xfs_log_force_seq_fexit");
        disabled.push("xfs_log_force_lsn_fentry");
        disabled.push("xfs_log_force_lsn_fexit");
    }

    if has_cil_wait {
        required.push(("xfs_log_cil_wait", "CIL wait count"));
    } else {
        disabled.push("xfs_log_cil_wait");
    }

    (disabled, required)
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // This machine cannot, rather than this sampler failed: both checks report
    // unsupported, so `rezolus status` does not exit non-zero for a kernel
    // that predates what the sampler needs, or a host without XFS at all.
    if !kernel_tracing_has_task_storage() {
        return Err(crate::agent::sampler_status::Unsupported(
            "tracing programs cannot use task local storage on this kernel (needs 5.12+)"
                .to_string(),
        )
        .into());
    }

    if !kernel_btf_has_tracepoints(TRACEPOINTS) {
        return Err(crate::agent::sampler_status::Unsupported(
            "the kernel's BTF does not describe the XFS log grant tracepoints; needs XFS \
             loaded, with vmlinux BTF if built in or module BTF (kernels 5.11+) if a module"
                .to_string(),
        )
        .into());
    }

    if !kernel_btf_has_funcs(&["xfs_log_force"]) {
        return Err(crate::agent::sampler_status::Unsupported(
            "the kernel's BTF has no xfs_log_force to attach to".to_string(),
        )
        .into());
    }

    let has_force_seq = kernel_btf_has_funcs(&["xfs_log_force_seq"]);
    let has_force_lsn = !has_force_seq && kernel_btf_has_funcs(&["xfs_log_force_lsn"]);
    if !has_force_seq && !has_force_lsn {
        info!(
            "{NAME}: the kernel's BTF has neither xfs_log_force_seq nor xfs_log_force_lsn; \
             forces from the fsync path are not timed"
        );
    }

    let has_cil_wait = kernel_btf_has_tracepoints(&["xfs_log_cil_wait"]);
    if !has_cil_wait {
        info!("{NAME}: the kernel's BTF has no xfs_log_cil_wait; the CIL wait count reads 0");
    }

    let (disabled, required) = select_programs(has_force_seq, has_force_lsn, has_cil_wait);

    // Order MUST match the C_* indices in mod.bpf.c: waits (space, force,
    // cil), then time (space, force).
    let counters = vec![
        &XFS_LOG_WAITS_SPACE,
        &XFS_LOG_WAITS_FORCE,
        &XFS_LOG_WAITS_CIL,
        &XFS_LOG_WAIT_TIME_SPACE,
        &XFS_LOG_WAIT_TIME_FORCE,
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
    // The two latencies share ONE group: like entities of one family,
    // distinguished by `wait` — see stats.rs.
    .histogram("space_latency", &XFS_LOG_WAIT_LATENCY_SPACE, &LATENCIES_ACQ)
    .histogram("force_latency", &XFS_LOG_WAIT_LATENCY_FORCE, &LATENCIES_ACQ)
    .disabled_programs(&disabled)
    .required_programs(&required)
    // The switch is read-only data the verifier folds at load (see
    // `cgroup_attribution` in mod.bpf.c); the cgroup maps and their series
    // exist only when it is on.
    .pre_load(move |open| {
        if let Some(rodata) = open.maps.rodata_data.as_mut() {
            rodata.cgroup_attribution = cgroup_attribution as u8;
        }
    });

    if cgroup_attribution {
        builder = builder
            .packed_counters(
                "cgroup_waits_space",
                &CGROUP_XFS_LOG_WAITS_SPACE,
                &CGROUP_ACQ,
            )
            .packed_counters(
                "cgroup_waits_force",
                &CGROUP_XFS_LOG_WAITS_FORCE,
                &CGROUP_ACQ,
            )
            .packed_counters(
                "cgroup_time_space",
                &CGROUP_XFS_LOG_WAIT_TIME_SPACE,
                &CGROUP_ACQ,
            )
            .packed_counters(
                "cgroup_time_force",
                &CGROUP_XFS_LOG_WAIT_TIME_FORCE,
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
            "space_latency" => &self.maps.space_latency,
            "force_latency" => &self.maps.force_latency,
            "cgroup_info" => &self.maps.cgroup_info,
            "cgroup_waits_space" => &self.maps.cgroup_waits_space,
            "cgroup_waits_force" => &self.maps.cgroup_waits_force,
            "cgroup_time_space" => &self.maps.cgroup_time_space,
            "cgroup_time_force" => &self.maps.cgroup_time_force,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} xfs_log_grant_wake() BPF instruction count: {}",
            self.progs.xfs_log_grant_wake.insn_cnt()
        );
        debug!(
            "{NAME} xfs_log_force_seq_fexit() BPF instruction count: {}",
            self.progs.xfs_log_force_seq_fexit.insn_cnt()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly one fsync-path force pair is loaded, the one whose function
    /// the kernel has, and none when it has neither.
    #[test]
    fn the_fsync_force_pair_follows_the_kernels_function_name() {
        let (disabled, required) = select_programs(true, false, true);
        assert!(required
            .iter()
            .any(|(p, _)| *p == "xfs_log_force_seq_fexit"));
        assert!(disabled.contains(&"xfs_log_force_lsn_fentry"));
        assert!(!disabled.contains(&"xfs_log_cil_wait"));

        let (disabled, required) = select_programs(false, true, false);
        assert!(required
            .iter()
            .any(|(p, _)| *p == "xfs_log_force_lsn_fexit"));
        assert!(disabled.contains(&"xfs_log_force_seq_fentry"));
        assert!(disabled.contains(&"xfs_log_cil_wait"));

        let (disabled, required) = select_programs(false, false, false);
        assert!(disabled.contains(&"xfs_log_force_seq_fentry"));
        assert!(disabled.contains(&"xfs_log_force_lsn_fentry"));
        assert!(!required.iter().any(|(p, _)| p.contains("force_seq")));
        assert!(!required.iter().any(|(p, _)| p.contains("force_lsn")));
    }

    /// No program is both disabled and required, and the grant pair and the
    /// whole-log force pair are always required.
    #[test]
    fn disabled_and_required_are_disjoint() {
        for (seq, lsn, cil) in [
            (true, false, true),
            (false, true, false),
            (false, false, true),
        ] {
            let (disabled, required) = select_programs(seq, lsn, cil);
            for (prog, _) in &required {
                assert!(
                    !disabled.contains(prog),
                    "{prog} both disabled and required"
                );
            }
            assert!(required.iter().any(|(p, _)| *p == "xfs_log_grant_wake"));
            assert!(required.iter().any(|(p, _)| *p == "xfs_log_force_fexit"));
        }
    }
}
