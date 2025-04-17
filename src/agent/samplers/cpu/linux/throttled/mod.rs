//! Collects CPU throttle stats using BPF and traces:
//! * `throttle_cfs_rq`
//! * `unthrottle_cfs_rq`
//!
//! And produces these stats:
//! * `cgroup_cpu_throttled_time`
//! * `cgroup_cpu_throttled`
//!
//! These stats can be used to understand when and for how long cgroups are being
//! throttled by the CPU controller.

const NAME: &str = "cpu_throttled";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_throttled.bpf.rs"));
}

use bpf::*;

use crate::agent::*;

use std::sync::Arc;

mod stats;

use stats::*;

unsafe impl plain::Plain for bpf::types::cgroup_info {}

fn handle_event(data: &[u8]) -> i32 {
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

        set_name(id as usize, name)
    }

    0
}

fn set_name(id: usize, name: String) {
    if !name.is_empty() {
        CGROUP_CPU_THROTTLED_TIME.insert_metadata(id, "name".to_string(), name.clone());
        CGROUP_CPU_THROTTLED.insert_metadata(id, "name".to_string(), name);
    }
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    set_name(1, "/".to_string());

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .packed_counters("throttled_time", &CGROUP_CPU_THROTTLED_TIME)
        .packed_counters("throttled_count", &CGROUP_CPU_THROTTLED)
        .ringbuf_handler("cgroup_info", handle_event)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "cgroup_info" => &self.maps.cgroup_info,
            "throttled_time" => &self.maps.throttled_time,
            "throttled_count" => &self.maps.throttled_count,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
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
