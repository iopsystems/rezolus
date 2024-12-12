//! Collects CPU perf counters using BPF and traces:
//! * `sched_switch`
//!
//! Initializes perf events to collect MSRs for APERF, MPERF, and TSC.
//!
//! And produces these stats:
//! * `cpu/aperf`
//! * `cpu/mperf`
//! * `cpu/tsc`
//!
//! These stats can be used to calculate the base frequency and running
//! frequency in post-processing or in an observability stack.

const NAME: &str = "cpu_frequency";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_frequency.bpf.rs"));
}

use bpf::*;
use perf_event::events::x86::MsrId;

use crate::common::*;
use crate::samplers::cpu::linux::stats::*;
use crate::*;

use std::sync::Arc;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let cpus = crate::common::linux::cpus()?;

    let totals = vec![&CPU_APERF, &CPU_MPERF, &CPU_TSC];

    let metrics = ["cpu/aperf", "cpu/mperf", "cpu/tsc"];

    let mut individual = ScopedCounters::new();

    for cpu in cpus {
        for metric in metrics {
            individual.push(
                cpu,
                DynamicCounterBuilder::new(metric)
                    .metadata("id", format!("{}", cpu))
                    .formatter(cpu_metric_percore_formatter)
                    .build(),
            );
        }
    }

    println!("initializing bpf for: {NAME}");

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .perf_event("aperf", PerfEvent::msr(MsrId::APERF)?)
        .perf_event("mperf", PerfEvent::msr(MsrId::MPERF)?)
        .perf_event("tsc", PerfEvent::msr(MsrId::TSC)?)
        .cpu_counters("counters", totals, individual)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "counters" => &self.maps.counters,
            "aperf" => &self.maps.aperf,
            "mperf" => &self.maps.mperf,
            "tsc" => &self.maps.tsc,
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
