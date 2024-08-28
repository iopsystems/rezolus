#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> Box<dyn Sampler> {
    if let Ok(s) = SyscallCounts::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/syscall_counts.bpf.rs"));
}

const NAME: &str = "syscall_counts";

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::syscall::linux::*;
use crate::samplers::syscall::stats::*;
use crate::samplers::syscall::*;

impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "counters" => &self.maps.counters,
            "syscall_lut" => &self.maps.syscall_lut,
            _ => unimplemented!(),
        }
    }
}

/// Collects Scheduler Runqueue Latency stats using BPF and traces:
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
pub struct SyscallCounts {
    bpf: Bpf<ModSkel<'static>>,
    interval: Interval,
}

impl SyscallCounts {
    pub fn new(config: Arc<Config>) -> Result<Self, ()> {
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

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

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

        let bpf = BpfBuilder::new(skel)
            .counters("counters", counters)
            .map("syscall_lut", &syscall_lut())
            .build();

        let now = Instant::now();

        Ok(Self {
            bpf,
            interval: Interval::new(now, config.interval(NAME)),
        })
    }
}

impl Sampler for SyscallCounts {
    fn sample(&mut self) {
        let now = Instant::now();

        if let Ok(elapsed) = self.interval.try_wait(now) {
            METADATA_SYSCALL_COUNTS_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

            self.bpf.refresh(elapsed);

            let elapsed = now.elapsed().as_nanos() as u64;
            METADATA_SYSCALL_COUNTS_RUNTIME.add(elapsed);
            let _ = METADATA_SYSCALL_COUNTS_RUNTIME_HISTOGRAM.increment(elapsed);
        }
    }
}
