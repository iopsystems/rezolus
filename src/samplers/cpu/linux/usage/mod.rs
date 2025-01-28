/// Collects CPU usage stats using BPF and traces:
/// * `cpuacct_account_field`
///
/// And produces these stats:
/// * `cpu_usage`
/// * `cgroup_cpu_usage`

const NAME: &str = "cpu_usage";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_usage.bpf.rs"));
}

use bpf::*;

use crate::common::*;
use crate::*;

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
            format!("{gpname}_{pname}_{name}")
        } else if !pname.is_empty() {
            format!("{pname}_{name}")
        } else {
            name.to_string()
        };

        let id = cgroup_info.id;

        if !name.is_empty() {
            CGROUP_CPU_USAGE_BUSY.insert_metadata(id as usize, "name".to_string(), name);
        }
    }

    0
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let counters = vec![
        &CPU_USAGE_BUSY,
        &CPU_USAGE_USER,
        &CPU_USAGE_NICE,
        &CPU_USAGE_SYSTEM,
        &CPU_USAGE_SOFTIRQ,
        &CPU_USAGE_IRQ,
        &CPU_USAGE_STEAL,
        &CPU_USAGE_GUEST,
        &CPU_USAGE_GUEST_NICE,
    ];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .cpu_counters("counters", counters)
        .packed_counters("cgroup_busy", &CGROUP_CPU_USAGE_BUSY)
        .ringbuf_handler("cgroup_info", handle_event)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "cgroup_info" => &self.maps.cgroup_info,
            "cgroup_busy" => &self.maps.cgroup_busy,
            "counters" => &self.maps.counters,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} cpuacct_account_field() BPF instruction count: {}",
            self.progs.cpuacct_account_field_kprobe.insn_cnt()
        );
    }
}
