//! Collects CPU perf counters using BPF and traces:
//! * `sched_switch`
//!
//! Initializes perf events to collect cycles and instructions.
//!
//! And produces these stats:
//! * `cpu/cycles`
//! * `cpu/instructions`

const NAME: &str = "cpu_perf";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_perf.bpf.rs"));
}

use bpf::*;

use crate::common::*;
use crate::samplers::cpu::linux::stats::*;
use crate::samplers::cpu::stats::*;
use crate::*;

use std::sync::Arc;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let cpus = crate::common::linux::cpus()?;

    let totals = vec![&CPU_CYCLES, &CPU_INSTRUCTIONS];

    let metrics = ["cpu/cycles", "cpu/instructions"];

    let mut individual = ScopedCounters::new();

    for cpu in cpus {
        for metric in metrics {
            individual.push(
                cpu,
                DynamicCounterBuilder::new(metric)
                    .metadata("id", format!("{}", cpu))
                    .formatter(cpu_metric_formatter)
                    .build(),
            );
        }
    }

    println!("initializing bpf for: {NAME}");

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .perf_event("cycles", perf_event::events::Hardware::CPU_CYCLES)
        .perf_event("instructions", perf_event::events::Hardware::INSTRUCTIONS)
        .cpu_counters("counters", totals, individual)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
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
            "{NAME} handle__sched_switch() BPF instruction count: {}",
            self.progs.handle__sched_switch.insn_cnt()
        );
    }
}
