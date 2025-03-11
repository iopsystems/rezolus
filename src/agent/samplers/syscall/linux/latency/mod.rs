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
use crate::*;

use std::sync::Arc;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .histogram("other_latency", &SYSCALL_OTHER_LATENCY)
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
            "other_latency" => &self.maps.other_latency,
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
