/// Collects Syscall stats using BPF and traces:
/// * `raw_syscalls/sys_enter`
///
/// And produces these stats:
/// * `syscall/total`
/// * `syscall/read`
/// * `syscall/write`
/// * `syscall/poll`
/// * `syscall/lock`
/// * `syscall/time`
/// * `syscall/sleep`
/// * `syscall/socket`
/// * `syscall/yield`

const NAME: &str = "syscall_counts";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/syscall_latency.bpf.rs"));
}

use bpf::*;

use crate::common::*;
use crate::samplers::syscall::linux::stats::*;
use crate::samplers::syscall::linux::syscall_lut;
use crate::*;

use std::sync::Arc;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .histogram("total_latency", &SYSCALL_TOTAL_LATENCY)
        .histogram("read_latency", &SYSCALL_READ_LATENCY)
        .histogram("write_latency", &SYSCALL_WRITE_LATENCY)
        .histogram("poll_latency", &SYSCALL_POLL_LATENCY)
        .histogram("lock_latency", &SYSCALL_LOCK_LATENCY)
        .histogram("time_latency", &SYSCALL_TIME_LATENCY)
        .histogram("sleep_latency", &SYSCALL_SLEEP_LATENCY)
        .histogram("socket_latency", &SYSCALL_SOCKET_LATENCY)
        .histogram("yield_latency", &SYSCALL_YIELD_LATENCY)
        .map("syscall_lut", syscall_lut())
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "total_latency" => &self.maps.total_latency,
            "read_latency" => &self.maps.read_latency,
            "write_latency" => &self.maps.write_latency,
            "poll_latency" => &self.maps.poll_latency,
            "lock_latency" => &self.maps.lock_latency,
            "time_latency" => &self.maps.time_latency,
            "sleep_latency" => &self.maps.sleep_latency,
            "socket_latency" => &self.maps.socket_latency,
            "yield_latency" => &self.maps.yield_latency,
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
