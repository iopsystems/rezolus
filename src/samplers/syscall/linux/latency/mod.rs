#[distributed_slice(SYSCALL_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = Syscall::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/syscall_latency.bpf.rs"));
}

const NAME: &str = "syscall_latency";

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::syscall::linux::*;
use crate::samplers::syscall::stats::*;
use crate::samplers::syscall::*;

impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        self.obj.map(name).unwrap()
    }
}

/// Collects Scheduler Runqueue Latency stats using BPF and traces:
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
pub struct Syscall {
    bpf: Bpf<ModSkel<'static>>,
    distribution_interval: Interval,
}

impl Syscall {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !(config.enabled(NAME) && config.bpf(NAME)) {
            return Err(());
        }

        let builder = ModSkelBuilder::default();
        let mut skel = builder
            .open()
            .map_err(|e| error!("failed to open bpf builder: {e}"))?
            .load()
            .map_err(|e| error!("failed to load bpf program: {e}"))?;

        debug!(
            "{NAME} sys_enter() BPF instruction count: {}",
            skel.progs().sys_enter().insn_cnt()
        );
        debug!(
            "{NAME} sys_exit() BPF instruction count: {}",
            skel.progs().sys_exit().insn_cnt()
        );

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let syscall_lut = syscall_lut();

        let bpf = BpfBuilder::new(skel)
            .distribution("total_latency", &SYSCALL_TOTAL_LATENCY)
            .distribution("read_latency", &SYSCALL_READ_LATENCY)
            .distribution("write_latency", &SYSCALL_WRITE_LATENCY)
            .distribution("poll_latency", &SYSCALL_POLL_LATENCY)
            .distribution("lock_latency", &SYSCALL_LOCK_LATENCY)
            .distribution("time_latency", &SYSCALL_TIME_LATENCY)
            .distribution("sleep_latency", &SYSCALL_SLEEP_LATENCY)
            .distribution("socket_latency", &SYSCALL_SOCKET_LATENCY)
            .map("syscall_lut", &syscall_lut)
            .build();

        let now = Instant::now();

        Ok(Self {
            bpf,
            distribution_interval: Interval::new(now, config.distribution_interval(NAME)),
        })
    }

    pub fn refresh_distributions(&mut self, now: Instant) -> Result<(), ()> {
        self.distribution_interval.try_wait(now)?;

        self.bpf.refresh_distributions();

        Ok(())
    }
}

impl Sampler for Syscall {
    fn sample(&mut self) {
        let now = Instant::now();
        let _ = self.refresh_distributions(now);
    }
}
