/// Collects Syscall Latency stats using BPF and traces:
/// * `raw_syscalls/sys_enter`
/// * `raw_syscalls/sys_exit`
///
/// And produces these stats:
/// * `syscall/total/latency`
/// * `syscall/read/latency`
/// * `syscall/write/latency`
/// * `syscall/poll/latency`
/// * `syscall/lock/latency`
/// * `syscall/time/latency`
/// * `syscall/sleep/latency`
/// * `syscall/socket/latency`
/// * `syscall/yield/latency`

const NAME: &str = "syscall_latency";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/syscall_latency.bpf.rs"));
}

use bpf::*;

use crate::common::bpf::*;
use crate::samplers::syscall::linux::syscall_lut;
use crate::samplers::syscall::stats::*;
use crate::*;

#[distributed_slice(ASYNC_SAMPLERS)]
fn spawn(config: Arc<Config>, runtime: &Runtime) {
    // check if sampler should be enabled
    if !config.enabled(NAME) {
        return;
    }

    let bpf = AsyncBpfBuilder::new(ModSkelBuilder::default)
        .distribution("total_latency", &SYSCALL_TOTAL_LATENCY)
        .distribution("read_latency", &SYSCALL_READ_LATENCY)
        .distribution("write_latency", &SYSCALL_WRITE_LATENCY)
        .distribution("poll_latency", &SYSCALL_POLL_LATENCY)
        .distribution("lock_latency", &SYSCALL_LOCK_LATENCY)
        .distribution("time_latency", &SYSCALL_TIME_LATENCY)
        .distribution("sleep_latency", &SYSCALL_SLEEP_LATENCY)
        .distribution("socket_latency", &SYSCALL_SOCKET_LATENCY)
        .distribution("yield_latency", &SYSCALL_YIELD_LATENCY)
        .map("syscall_lut", syscall_lut())
        .collected_at(&METADATA_SYSCALL_LATENCY_COLLECTED_AT)
        .runtime(
            &METADATA_SYSCALL_LATENCY_RUNTIME,
            &METADATA_SYSCALL_LATENCY_RUNTIME_HISTOGRAM,
        )
        .build();

    if bpf.is_err() {
        return;
    }

    runtime.spawn(async move {
        let mut sampler = AsyncBpfSampler::new(bpf.unwrap(), config.async_interval(NAME));

        loop {
            if sampler.is_finished() {
                return;
            }

            sampler.sample().await;
        }
    });
}

impl GetMap for ModSkel<'_> {
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
