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

const MAX_SYSCALL_ID: usize = 1024;

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
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
/// * `syscall/total`
/// * `syscall/total/latency`
pub struct Syscall {
    bpf: Bpf<ModSkel<'static>>,
    counter_interval: Interval,
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

        let counters = vec![
            Counter::new(&SYSCALL_TOTAL, Some(&SYSCALL_TOTAL_HISTOGRAM)),
            Counter::new(&SYSCALL_READ, Some(&SYSCALL_READ_HISTOGRAM)),
            Counter::new(&SYSCALL_WRITE, Some(&SYSCALL_WRITE_HISTOGRAM)),
            Counter::new(&SYSCALL_POLL, Some(&SYSCALL_POLL_HISTOGRAM)),
            Counter::new(&SYSCALL_LOCK, Some(&SYSCALL_LOCK_HISTOGRAM)),
            Counter::new(&SYSCALL_TIME, Some(&SYSCALL_TIME_HISTOGRAM)),
            Counter::new(&SYSCALL_SLEEP, Some(&SYSCALL_SLEEP_HISTOGRAM)),
            Counter::new(&SYSCALL_SOCKET, Some(&SYSCALL_SOCKET_HISTOGRAM)),
        ];

        let syscall_lut: Vec<u64> = (0..MAX_SYSCALL_ID)
            .map(|id| {
                if let Some(syscall_name) = syscall_numbers::native::sys_call_name(id as i64) {
                    match syscall_name {
                        // read related
                        "pread64" | "preadv" | "preadv2" | "read" | "readv" | "recvfrom"
                        | "recvmmsg" | "recvmsg" => 1,
                        // write related
                        "pwrite64" | "pwritev" | "pwritev2" | "sendmmsg" | "sendmsg" | "sendto"
                        | "write" | "writev" => 2,
                        // poll/select/epoll
                        "epoll_create" | "epoll_create1" | "epoll_ctl" | "epoll_ctl_old"
                        | "epoll_pwait" | "epoll_pwait2" | "epoll_wait" | "epoll_wait_old"
                        | "poll" | "ppoll" | "ppoll_time64" | "pselect6" | "pselect6_time64"
                        | "select" => 3,
                        // locking
                        "futex" => 4,
                        // time
                        "adjtimex" | "clock_adjtime" | "clock_getres" | "clock_gettime"
                        | "clock_settime" | "gettimeofday" | "settimeofday" | "time" => 5,
                        // sleep
                        "clock_nanosleep" | "nanosleep" => 6,
                        // socket creation and management
                        "accept" | "bind" | "connect" | "getpeername" | "getsockname"
                        | "getsockopt" | "listen" | "setsockopt" | "shutdown" | "socket"
                        | "socketpair" => 7,
                        _ => {
                            // no group defined for these syscalls
                            0
                        }
                    }
                } else {
                    0
                }
            })
            .collect();

        let bpf = BpfBuilder::new(skel)
            .counters("counters", counters)
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
            counter_interval: Interval::new(now, config.interval(NAME)),
            distribution_interval: Interval::new(now, config.distribution_interval(NAME)),
        })
    }

    pub fn refresh_counters(&mut self, now: Instant) -> Result<(), ()> {
        let elapsed = self.counter_interval.try_wait(now)?;

        self.bpf.refresh_counters(elapsed);

        Ok(())
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
        let _ = self.refresh_counters(now);
        let _ = self.refresh_distributions(now);
    }
}
