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

unsafe impl plain::Plain for bpf::types::cgroup_info {}

fn handle_event(data: &[u8]) -> i32 {
    let mut cgroup_info = bpf::types::cgroup_info::default();

    if plain::copy_from_bytes(&mut cgroup_info, data).is_ok() {
        let name = std::str::from_utf8(&cgroup_info.name)
            .unwrap()
            .trim_end_matches(char::from(0));

        let id = cgroup_info.id;

        if !name.is_empty() {
            CGROUP_CPU_APERF.insert_metadata(id as usize, "name".to_string(), name.to_string());
            CGROUP_CPU_MPERF.insert_metadata(id as usize, "name".to_string(), name.to_string());
            CGROUP_CPU_TSC.insert_metadata(id as usize, "name".to_string(), name.to_string());
        }
    }

    0
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let totals = vec![&CPU_APERF, &CPU_MPERF, &CPU_TSC];
    let individual = vec![&CPU_APERF_PERCORE, &CPU_MPERF_PERCORE, &CPU_TSC_PERCORE];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .perf_event("aperf", PerfEvent::msr(MsrId::APERF)?)
        .perf_event("mperf", PerfEvent::msr(MsrId::MPERF)?)
        .perf_event("tsc", PerfEvent::msr(MsrId::TSC)?)
        .cpu_counters("counters", totals, individual)
        .packed_counters("cgroup_aperf", &CGROUP_CPU_APERF)
        .packed_counters("cgroup_mperf", &CGROUP_CPU_MPERF)
        .packed_counters("cgroup_tsc", &CGROUP_CPU_TSC)
        .ringbuf_handler("cgroup_info", handle_event)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "cgroup_aperf" => &self.maps.cgroup_aperf,
            "cgroup_info" => &self.maps.cgroup_info,
            "cgroup_mperf" => &self.maps.cgroup_mperf,
            "cgroup_tsc" => &self.maps.cgroup_tsc,
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
            "{NAME} cpuacct_account_field() BPF instruction count: {}",
            self.progs.cpuacct_account_field_kprobe.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_switch() BPF instruction count: {}",
            self.progs.handle__sched_switch.insn_cnt()
        );
    }
}
