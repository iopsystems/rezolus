#[distributed_slice(SYSCALL_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = SyscallLatency::new(config) {
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
/// * `syscall/yield/latency`
pub struct SyscallLatency {
    bpf: Bpf<ModSkel<'static>>,
    interval: Interval,
}

impl SyscallLatency {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !(config.enabled(NAME) && config.bpf(NAME)) {
            return Err(());
        }

        let open_object: &'static mut MaybeUninit<OpenObject> =
            Box::leak(Box::new(MaybeUninit::uninit()));

        let builder = ModSkelBuilder::default();
        let mut skel = builder
            .open(open_object)
            .map_err(|e| error!("failed to open bpf builder: {e}"))?
            .load()
            .map_err(|e| error!("failed to load bpf program: {e}"))?;

        debug!(
            "{NAME} sys_enter() BPF instruction count: {}",
            skel.progs.sys_enter.insn_cnt()
        );
        debug!(
            "{NAME} sys_exit() BPF instruction count: {}",
            skel.progs.sys_exit.insn_cnt()
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
            .distribution("yield_latency", &SYSCALL_YIELD_LATENCY)
            .map("syscall_lut", &syscall_lut)
            .build();

        let now = Instant::now();

        Ok(Self {
            bpf,
            interval: Interval::new(now, config.interval(NAME)),
        })
    }
}

impl Sampler for SyscallLatency {
    fn sample(&mut self) {
        let now = Instant::now();

        if let Ok(elapsed) = self.interval.try_wait(now) {
            METADATA_SYSCALL_LATENCY_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

            self.bpf.refresh(elapsed);

            let elapsed = now.elapsed().as_nanos() as u64;
            METADATA_SYSCALL_LATENCY_RUNTIME.add(elapsed);
            let _ = METADATA_SYSCALL_LATENCY_RUNTIME_HISTOGRAM.increment(elapsed);
        }
    }
}
