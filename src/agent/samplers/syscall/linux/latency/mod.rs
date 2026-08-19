//! Collects Syscall stats using BPF and traces:
//! * `raw_syscalls/sys_enter`
//! * `raw_syscalls/sys_exit`
//!
//! And produces these stats:
//! * `syscall_latency`

const NAME: &str = "syscall_latency";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/syscall_latency.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use super::syscall_lut;
use crate::agent::*;

use std::sync::Arc;

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
    // All 16 syscall-class latency histograms share ONE group: they are
    // LIKE ENTITIES (one "syscall latency" family, distinguished by the
    // `op` label) read as a single sweep — see stats.rs's `LATENCIES_ACQ`
    // doc comment. `BpfBuilder` batches every `.histogram()` call below
    // (same group reference) into one `HistogramBatch`, so it is stamped
    // once per refresh, not 16 times.
    .histogram("other_latency", &SYSCALL_OTHER_LATENCY, &LATENCIES_ACQ)
    .histogram("read_latency", &SYSCALL_READ_LATENCY, &LATENCIES_ACQ)
    .histogram("write_latency", &SYSCALL_WRITE_LATENCY, &LATENCIES_ACQ)
    .histogram("poll_latency", &SYSCALL_POLL_LATENCY, &LATENCIES_ACQ)
    .histogram("lock_latency", &SYSCALL_LOCK_LATENCY, &LATENCIES_ACQ)
    .histogram("time_latency", &SYSCALL_TIME_LATENCY, &LATENCIES_ACQ)
    .histogram("sleep_latency", &SYSCALL_SLEEP_LATENCY, &LATENCIES_ACQ)
    .histogram("socket_latency", &SYSCALL_SOCKET_LATENCY, &LATENCIES_ACQ)
    .histogram("yield_latency", &SYSCALL_YIELD_LATENCY, &LATENCIES_ACQ)
    .histogram(
        "filesystem_latency",
        &SYSCALL_FILESYSTEM_LATENCY,
        &LATENCIES_ACQ,
    )
    .histogram("memory_latency", &SYSCALL_MEMORY_LATENCY, &LATENCIES_ACQ)
    .histogram("process_latency", &SYSCALL_PROCESS_LATENCY, &LATENCIES_ACQ)
    .histogram("query_latency", &SYSCALL_QUERY_LATENCY, &LATENCIES_ACQ)
    .histogram("ipc_latency", &SYSCALL_IPC_LATENCY, &LATENCIES_ACQ)
    .histogram("timer_latency", &SYSCALL_TIMER_LATENCY, &LATENCIES_ACQ)
    .histogram("event_latency", &SYSCALL_EVENT_LATENCY, &LATENCIES_ACQ)
    .map("syscall_lut", syscall_lut())
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
            "other_latency" => &self.maps.other_latency,
            "read_latency" => &self.maps.read_latency,
            "write_latency" => &self.maps.write_latency,
            "poll_latency" => &self.maps.poll_latency,
            "lock_latency" => &self.maps.lock_latency,
            "time_latency" => &self.maps.time_latency,
            "sleep_latency" => &self.maps.sleep_latency,
            "socket_latency" => &self.maps.socket_latency,
            "yield_latency" => &self.maps.yield_latency,
            "filesystem_latency" => &self.maps.filesystem_latency,
            "memory_latency" => &self.maps.memory_latency,
            "process_latency" => &self.maps.process_latency,
            "query_latency" => &self.maps.query_latency,
            "ipc_latency" => &self.maps.ipc_latency,
            "timer_latency" => &self.maps.timer_latency,
            "event_latency" => &self.maps.event_latency,
            "syscall_lut" => &self.maps.syscall_lut,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} sys_enter() BPF instruction count: {}",
            self.progs.sys_enter.insn_cnt()
        );
        debug!(
            "{NAME} sys_exit() BPF instruction count: {}",
            self.progs.sys_exit.insn_cnt()
        );
    }
}
