//! Collects CPU perf counters using BPF and traces:
//! * `sched_switch`
//!
//! Initializes perf events to collect cycles and instructions.
//!
//! And produces these stats:
//! * `cpu_cycles`
//! * `cpu_instructions`
//! * `cgroup_cpu_cycles`
//! * `cgroup_cpu_instructions`
//!
//! These stats can be used to calculate the IPC and IPNS in post-processing or
//! in an observability stack.

const NAME: &str = "cpu_perf";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_perf.bpf.rs"));
}

use bpf::*;

use crate::agent::*;

use std::sync::Arc;

mod stats;

use stats::*;

unsafe impl plain::Plain for bpf::types::cgroup_info {}
impl_cgroup_info!(bpf::types::cgroup_info);

// Static slice of metrics that track cgroup-specific data
static CGROUP_METRICS: &[&dyn MetricGroup] = &[&CGROUP_CPU_CYCLES, &CGROUP_CPU_INSTRUCTIONS];

fn handle_event(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, CGROUP_METRICS)
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Set metadata for root cgroup
    for metric in CGROUP_METRICS {
        metric.insert_metadata(1, "name".to_string(), "/".to_string());
    }

    let bpf = BpfBuilder::new(
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .perf_event("cycles", PerfEvent::cpu_cycles(), &CPU_CYCLES)
    .perf_event("instructions", PerfEvent::instructions(), &CPU_INSTRUCTIONS)
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
            "cycles" => &self.maps.cycles,
            "instructions" => &self.maps.instructions,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} handle__sched_switch() BPF instruction count: {}",
            self.progs.handle__sched_switch.insn_cnt()
        );
    }
}
