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

fn handle_cgroup_info(data: &[u8]) -> i32 {
    let mut cgroup_info = bpf::types::cgroup_info::default();

    if plain::copy_from_bytes(&mut cgroup_info, data).is_ok() {
        let name = std::str::from_utf8(&cgroup_info.name)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let pname = std::str::from_utf8(&cgroup_info.pname)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let gpname = std::str::from_utf8(&cgroup_info.gpname)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let name = if !gpname.is_empty() {
            if cgroup_info.level > 3 {
                format!(".../{gpname}/{pname}/{name}")
            } else {
                format!("/{gpname}/{pname}/{name}")
            }
        } else if !pname.is_empty() {
            format!("/{pname}/{name}")
        } else if !name.is_empty() {
            format!("/{name}")
        } else {
            "".to_string()
        };

        let id = cgroup_info.id;

        set_cgroup_name(id as usize, name)
    }

    0
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

fn set_cgroup_name(id: usize, name: String) {
    if !name.is_empty() {
        for m in &[
            &CGROUP_CPU_BANDWIDTH_QUOTA,
            &CGROUP_CPU_BANDWIDTH_PERIOD_DURATION,
        ] {
            m.insert_metadata(id, "name".to_string(), name.clone());
        }

        for m in &[
            &CGROUP_CPU_THROTTLED_TIME,
            &CGROUP_CPU_THROTTLED,
            &CGROUP_CPU_BANDWIDTH_PERIODS,
            &CGROUP_CPU_BANDWIDTH_THROTTLED_PERIODS,
            &CGROUP_CPU_BANDWIDTH_THROTTLED_TIME,
        ] {
            m.insert_metadata(id, "name".to_string(), name.clone());
        }
    }
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    set_cgroup_name(1, "/".to_string());

    let bpf = BpfBuilder::new(NAME, ModSkelBuilder::default)
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
