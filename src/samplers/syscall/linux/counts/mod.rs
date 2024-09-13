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
    include!(concat!(env!("OUT_DIR"), "/syscall_counts.bpf.rs"));
}

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::syscall::linux::syscall_lut;
use crate::samplers::syscall::stats::*;
use crate::*;

#[distributed_slice(ASYNC_SAMPLERS)]
fn spawn(config: Arc<Config>, runtime: &Runtime) {
    // check if sampler should be enabled
    if !config.enabled(NAME) {
        return;
    }

    let counters = vec![
        Counter::new(&SYSCALL_TOTAL, Some(&SYSCALL_TOTAL_HISTOGRAM)),
        Counter::new(&SYSCALL_READ, Some(&SYSCALL_READ_HISTOGRAM)),
        Counter::new(&SYSCALL_WRITE, Some(&SYSCALL_WRITE_HISTOGRAM)),
        Counter::new(&SYSCALL_POLL, Some(&SYSCALL_POLL_HISTOGRAM)),
        Counter::new(&SYSCALL_LOCK, Some(&SYSCALL_LOCK_HISTOGRAM)),
        Counter::new(&SYSCALL_TIME, Some(&SYSCALL_TIME_HISTOGRAM)),
        Counter::new(&SYSCALL_SLEEP, Some(&SYSCALL_SLEEP_HISTOGRAM)),
        Counter::new(&SYSCALL_SOCKET, Some(&SYSCALL_SOCKET_HISTOGRAM)),
        Counter::new(&SYSCALL_YIELD, Some(&SYSCALL_YIELD_HISTOGRAM)),
    ];

    let bpf = AsyncBpfBuilder::new(ModSkelBuilder::default)
        .counters("counters", counters)
        .map("syscall_lut", syscall_lut())
        .collected_at(&METADATA_SYSCALL_COUNTS_COLLECTED_AT)
        .runtime(
            &METADATA_SYSCALL_COUNTS_RUNTIME,
            &METADATA_SYSCALL_COUNTS_RUNTIME_HISTOGRAM,
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
            "counters" => &self.maps.counters,
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
    }
}
