//! Collects CPU CFS bandwidth control and throttling stats using BPF and traces:
//! * `tg_set_cfs_bandwidth` (fentry when the kernel's BTF has it, else kprobe)
//! * `throttle_cfs_rq` (fentry when the kernel's BTF has it, else kprobe)
//! * `unthrottle_cfs_rq` (fentry when the kernel's BTF has it, else kprobe)
//!
//! And produces these stats:
//! * `cgroup_cpu_bandwidth_quota`
//! * `cgroup_cpu_bandwidth_period`
//! * `cgroup_cpu_throttled_time`
//! * `cgroup_cpu_throttled`

const NAME: &str = "cpu_bandwidth";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_bandwidth.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::agent::*;

use std::sync::Arc;

unsafe impl plain::Plain for bpf::types::cgroup_info {}
unsafe impl plain::Plain for bpf::types::bandwidth_info {}

impl_cgroup_info!(bpf::types::cgroup_info);

static CGROUP_METRICS: &[&dyn GroupMetadata] = &[
    &CGROUP_CPU_BANDWIDTH_QUOTA,
    &CGROUP_CPU_BANDWIDTH_PERIOD_DURATION,
    &CGROUP_CPU_THROTTLED_TIME,
    &CGROUP_CPU_THROTTLED,
    &CGROUP_CPU_BANDWIDTH_PERIODS,
    &CGROUP_CPU_BANDWIDTH_THROTTLED_PERIODS,
    &CGROUP_CPU_BANDWIDTH_THROTTLED_TIME,
];

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, CGROUP_METRICS)
}

/// The kernel's "no quota" spelling: `RUNTIME_INF`, defined as `(u64)~0ULL`.
const RUNTIME_INF: u64 = u64::MAX;

/// What `cgroup_cpu_bandwidth_quota` reports for an unquota'd cgroup.
///
/// Negative, so it cannot be read as a nanosecond count, and distinct from `0`
/// — which is what an unwritten gauge slot reads as, and so cannot mean
/// "unlimited" without colliding with "never observed".
const QUOTA_UNLIMITED: i64 = -1;

/// Map a raw `tg_set_cfs_bandwidth` quota onto the gauge.
///
/// Every cgroup written as `cpu.max = "max <period>"` arrives here as
/// `RUNTIME_INF`, which is the common case rather than an edge one: that is what
/// a Kubernetes pod with no CPU limit gets. Left to the plain `as i64` cast it
/// would land on `-1` anyway, but by accident of two's complement rather than by
/// decision — so name the sentinel and document it on the metric.
fn quota_gauge(quota: u64) -> i64 {
    if quota == RUNTIME_INF {
        QUOTA_UNLIMITED
    } else {
        quota as i64
    }
}

fn handle_bandwidth_info(data: &[u8]) -> i32 {
    let mut bandwidth_info = bpf::types::bandwidth_info::default();

    if plain::copy_from_bytes(&mut bandwidth_info, data).is_ok() {
        let id = bandwidth_info.id;
        let quota = bandwidth_info.quota;
        let period = bandwidth_info.period;

        if id < MAX_CGROUPS as u32 {
            let _ = CGROUP_CPU_BANDWIDTH_QUOTA.set(id as usize, quota_gauge(quota));
            let _ = CGROUP_CPU_BANDWIDTH_PERIOD_DURATION.set(id as usize, period as i64);
        }
    }

    0
}

/// The functions the fentry twins name as attach targets. All three live behind
/// `CONFIG_CFS_BANDWIDTH`, so on a kernel built without it none of them exists —
/// and an fentry program naming a target the kernel's BTF does not have fails to
/// LOAD, which takes the whole sampler down with it. The kprobe twins miss at
/// attach instead, which is tolerated and reported as unsupported. Hence the
/// selection below asks whether these specific functions are in BTF, not merely
/// whether BTF exists. See `kernel_btf_has_funcs`.
const FENTRY_TARGETS: &[&str] = &[
    "tg_set_cfs_bandwidth",
    "throttle_cfs_rq",
    "unthrottle_cfs_rq",
];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Prefer the fentry twins (cheaper dispatch); fall back to kprobe when this
    // kernel's BTF cannot name the targets.
    let fentry = kernel_btf_has_funcs(FENTRY_TARGETS);

    if !fentry {
        debug!("{NAME}: no kernel BTF for the CFS bandwidth hooks, using kprobes");
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
    .packed_counters(
        "throttled_time",
        &CGROUP_CPU_THROTTLED_TIME,
        &CGROUP_THROTTLED_TIME_ACQ,
    )
    .packed_counters(
        "throttled_count",
        &CGROUP_CPU_THROTTLED,
        &CGROUP_THROTTLED_COUNT_ACQ,
    )
    .packed_counters(
        "bandwidth_periods",
        &CGROUP_CPU_BANDWIDTH_PERIODS,
        &CGROUP_BANDWIDTH_PERIODS_ACQ,
    )
    .packed_counters(
        "bandwidth_throttled_periods",
        &CGROUP_CPU_BANDWIDTH_THROTTLED_PERIODS,
        &CGROUP_BANDWIDTH_THROTTLED_PERIODS_ACQ,
    )
    .packed_counters(
        "bandwidth_throttled_time",
        &CGROUP_CPU_BANDWIDTH_THROTTLED_TIME,
        &CGROUP_BANDWIDTH_THROTTLED_TIME_ACQ,
    )
    .ringbuf_handler("cgroup_info", handle_cgroup_info)
    .ringbuf_handler("bandwidth_info", handle_bandwidth_info)
    .disabled_programs(if fentry {
        &[
            "tg_set_cfs_bandwidth_kprobe",
            "throttle_cfs_rq_kprobe",
            "unthrottle_cfs_rq_kprobe",
        ]
    } else {
        &[
            "tg_set_cfs_bandwidth_fentry",
            "throttle_cfs_rq_fentry",
            "unthrottle_cfs_rq_fentry",
        ]
    })
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
            "cgroup_info" => &self.maps.cgroup_info,
            "bandwidth_info" => &self.maps.bandwidth_info,
            "throttled_time" => &self.maps.throttled_time,
            "throttled_count" => &self.maps.throttled_count,
            "bandwidth_periods" => &self.maps.bandwidth_periods,
            "bandwidth_throttled_periods" => &self.maps.bandwidth_throttled_periods,
            "bandwidth_throttled_time" => &self.maps.bandwidth_throttled_time,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        // Both twins of each hook: only one set is autoloaded, and which one is
        // a runtime decision, so logging just the fentry names would report
        // programs that are not running on a kernel that took the kprobe path.
        debug!(
            "{NAME} tg_set_cfs_bandwidth_fentry() BPF instruction count: {}",
            self.progs.tg_set_cfs_bandwidth_fentry.insn_cnt()
        );
        debug!(
            "{NAME} tg_set_cfs_bandwidth_kprobe() BPF instruction count: {}",
            self.progs.tg_set_cfs_bandwidth_kprobe.insn_cnt()
        );
        debug!(
            "{NAME} throttle_cfs_rq_fentry() BPF instruction count: {}",
            self.progs.throttle_cfs_rq_fentry.insn_cnt()
        );
        debug!(
            "{NAME} throttle_cfs_rq_kprobe() BPF instruction count: {}",
            self.progs.throttle_cfs_rq_kprobe.insn_cnt()
        );
        debug!(
            "{NAME} unthrottle_cfs_rq_fentry() BPF instruction count: {}",
            self.progs.unthrottle_cfs_rq_fentry.insn_cnt()
        );
        debug!(
            "{NAME} unthrottle_cfs_rq_kprobe() BPF instruction count: {}",
            self.progs.unthrottle_cfs_rq_kprobe.insn_cnt()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{quota_gauge, QUOTA_UNLIMITED, RUNTIME_INF};

    #[test]
    fn an_unlimited_quota_maps_to_the_sentinel() {
        // `cpu.max = "max <period>"` reaches tg_set_cfs_bandwidth as
        // RUNTIME_INF. Pinning this is the point of naming the sentinel: the
        // plain cast lands on -1 by two's-complement accident, and a later
        // refactor could just as easily land on i64::MAX.
        assert_eq!(quota_gauge(RUNTIME_INF), QUOTA_UNLIMITED);
    }

    #[test]
    fn a_real_quota_passes_through_as_nanoseconds() {
        // `cpu.max = "50000 100000"` — 50ms of quota, in nanoseconds.
        assert_eq!(quota_gauge(50_000_000), 50_000_000);
    }
}
