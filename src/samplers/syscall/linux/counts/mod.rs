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
    if !(config.enabled(NAME) && config.bpf(NAME)) {
        return;
    }

    let counters = vec![
        Counter::new(&SYSCALL_TOTAL,  None),
        Counter::new(&SYSCALL_READ,  None),
        Counter::new(&SYSCALL_WRITE,  None),
        Counter::new(&SYSCALL_POLL,  None),
        Counter::new(&SYSCALL_LOCK,  None),
        Counter::new(&SYSCALL_TIME,  None),
        Counter::new(&SYSCALL_SLEEP,  None),
        Counter::new(&SYSCALL_SOCKET,  None),
        Counter::new(&SYSCALL_YIELD,  None),
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
