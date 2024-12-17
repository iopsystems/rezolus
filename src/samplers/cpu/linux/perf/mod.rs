//! Collects CPU perf counters using BPF and traces:
//! * `sched_switch`
//!
//! Initializes perf events to collect cycles and instructions.
//!
//! And produces these stats:
//! * `cpu/cycles`
//! * `cpu/instructions`
//!
//! These stats can be used to calculate the IPC and IPNS in post-processing or
//! in an observability stack.

const NAME: &str = "cpu_perf";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_perf.bpf.rs"));
}

use bpf::*;

use crate::common::*;
use crate::samplers::cpu::linux::stats::*;
use crate::*;

use std::sync::Arc;

unsafe impl plain::Plain for bpf::types::cgroup_info {}

fn handle_event(data: &[u8]) -> i32 {
    let mut cgroup_info = bpf::types::cgroup_info::default();

    if plain::copy_from_bytes(&mut cgroup_info, data).is_ok() {
        let name = std::str::from_utf8(&cgroup_info.name)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d","-");

        let pname = std::str::from_utf8(&cgroup_info.pname)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d","-");

        let gpname = std::str::from_utf8(&cgroup_info.gpname)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d","-");

        let name = if !gpname.is_empty() {
            format!("{gpname}_{pname}_{name}")
        } else if !pname.is_empty() {
            format!("{pname}_{name}")
        } else {
            name.to_string()
        };

        let id = cgroup_info.id;

        if !name.is_empty() {
            CGROUP_CPU_CYCLES.insert_metadata(id as usize, "name".to_string(), name.clone());
            CGROUP_CPU_INSTRUCTIONS.insert_metadata(id as usize, "name".to_string(), name);
        }
    }

    0
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let totals = vec![&CPU_CYCLES, &CPU_INSTRUCTIONS];
    let individual = vec![&CPU_CYCLES_PERCORE, &CPU_INSTRUCTIONS_PERCORE];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .perf_event("cycles", PerfEvent::cpu_cycles())
        .perf_event("instructions", PerfEvent::instructions())
        .cpu_counters("counters", totals, individual)
        .packed_counters("cgroup_cycles", &CGROUP_CPU_CYCLES)
        .packed_counters("cgroup_instructions", &CGROUP_CPU_INSTRUCTIONS)
        .ringbuf_handler("cgroup_info", handle_event)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "cgroup_cycles" => &self.maps.cgroup_cycles,
            "cgroup_info" => &self.maps.cgroup_info,
            "cgroup_instructions" => &self.maps.cgroup_instructions,
            "counters" => &self.maps.counters,
            "cycles" => &self.maps.cycles,
            "instructions" => &self.maps.instructions,
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
        debug!(
            "{NAME} handle__sched_switch() BPF instruction count: {}",
            self.progs.handle__sched_switch.insn_cnt()
        );
    }
}
