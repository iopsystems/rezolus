//! Collects CPU CFS bandwidth control and throttling stats using BPF and traces:
//! * `tg_set_cfs_bandwidth`
//! * `throttle_cfs_rq`
//! * `unthrottle_cfs_rq`
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

static CGROUP_METRICS: &[&dyn MetricGroup] = &[
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

fn handle_bandwidth_info(data: &[u8]) -> i32 {
    let mut bandwidth_info = bpf::types::bandwidth_info::default();

    if plain::copy_from_bytes(&mut bandwidth_info, data).is_ok() {
        let id = bandwidth_info.id;
        let quota = bandwidth_info.quota;
        let period = bandwidth_info.period;

        if id < MAX_CGROUPS as u32 {
            let _ = CGROUP_CPU_BANDWIDTH_QUOTA.set(id as usize, quota as i64);
            let _ = CGROUP_CPU_BANDWIDTH_PERIOD_DURATION.set(id as usize, period as i64);
        }
    }

    0
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
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
    .packed_counters("throttled_time", &CGROUP_CPU_THROTTLED_TIME)
    .packed_counters("throttled_count", &CGROUP_CPU_THROTTLED)
    .packed_counters("bandwidth_periods", &CGROUP_CPU_BANDWIDTH_PERIODS)
    .packed_counters(
        "bandwidth_throttled_periods",
        &CGROUP_CPU_BANDWIDTH_THROTTLED_PERIODS,
    )
    .packed_counters(
        "bandwidth_throttled_time",
        &CGROUP_CPU_BANDWIDTH_THROTTLED_TIME,
    )
    .ringbuf_handler("cgroup_info", handle_cgroup_info)
    .ringbuf_handler("bandwidth_info", handle_bandwidth_info)
    .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
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
        debug!(
            "{NAME} tg_set_cfs_bandwidth() BPF instruction count: {}",
            self.progs.tg_set_cfs_bandwidth.insn_cnt()
        );
        debug!(
            "{NAME} throttle_cfs_rq() BPF instruction count: {}",
            self.progs.throttle_cfs_rq.insn_cnt()
        );
        debug!(
            "{NAME} unthrottle_cfs_rq() BPF instruction count: {}",
            self.progs.unthrottle_cfs_rq.insn_cnt()
        );
    }
}
