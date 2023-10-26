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

use super::stats::*;
use super::*;
use crate::common::bpf::*;
use crate::common::*;

use std::os::fd::{AsFd, AsRawFd, FromRawFd};

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
    counter_interval: Duration,
    counter_next: Instant,
    counter_prev: Instant,
    distribution_interval: Duration,
    distribution_next: Instant,
    distribution_prev: Instant,
}

impl Syscall {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let builder = ModSkelBuilder::default();
        let mut skel = builder
            .open()
            .map_err(|e| error!("failed to open bpf builder: {e}"))?
            .load()
            .map_err(|e| error!("failed to load bpf program: {e}"))?;

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let mut bpf = Bpf::from_skel(skel);

        let fd = bpf.map("syscall_lut").as_fd().as_raw_fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mut syscall_lut = unsafe {
            memmap2::MmapOptions::new()
                .len(1024 * 8)
                .map_mut(&file)
                .expect("failed to mmap() bpf syscall lut")
        };

        for (syscall_id, bytes) in syscall_lut.chunks_exact_mut(8).enumerate() {
            let counter_offset = bytes.as_mut_ptr() as *mut u64;
            if let Some(syscall_name) = syscall_numbers::native::sys_call_name(syscall_id as i64) {
                let group = match syscall_name {
                    // read related
                    "pread64" | "preadv" | "preadv2" | "read" | "readv" | "recvfrom"
                    | "recvmmsg" | "recvmsg" => 1,
                    // write related
                    "pwrite64" | "pwritev" | "pwritev2" | "sendmmsg" | "sendmsg" | "sendto"
                    | "write" | "writev" => 2,
                    // poll/select/epoll
                    "epoll_create" | "epoll_create1" | "epoll_ctl" | "epoll_ctl_old"
                    | "epoll_pwait" | "epoll_pwait2" | "epoll_wait" | "epoll_wait_old" | "poll"
                    | "ppoll" | "ppoll_time64" | "pselect6" | "pselect6_time64" | "select" => 3,
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
                };
                unsafe {
                    *counter_offset = group;
                }
            } else {
                unsafe {
                    *counter_offset = 0;
                }
            }
        }

        let _ = syscall_lut.flush();

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

        bpf.add_counters("counters", counters);

        let mut distributions = vec![
            ("total_latency", &SYSCALL_TOTAL_LATENCY),
            ("read_latency", &SYSCALL_READ_LATENCY),
            ("write_latency", &SYSCALL_WRITE_LATENCY),
            ("poll_latency", &SYSCALL_POLL_LATENCY),
            ("lock_latency", &SYSCALL_LOCK_LATENCY),
            ("time_latency", &SYSCALL_TIME_LATENCY),
            ("sleep_latency", &SYSCALL_SLEEP_LATENCY),
            ("socket_latency", &SYSCALL_SOCKET_LATENCY),
        ];

        for (name, histogram) in distributions.drain(..) {
            bpf.add_distribution(name, histogram);
        }

        Ok(Self {
            bpf,
            counter_interval: config.interval(NAME),
            counter_next: Instant::now(),
            counter_prev: Instant::now(),
            distribution_interval: config.distribution_interval(NAME),
            distribution_next: Instant::now(),
            distribution_prev: Instant::now(),
        })
    }

    pub fn refresh_counters(&mut self, now: Instant) {
        if now < self.counter_next {
            return;
        }

        let elapsed = (now - self.counter_prev).as_secs_f64();

        self.bpf.refresh_counters(elapsed);

        // determine when to sample next
        let next = self.counter_next + self.counter_interval;

        // check that next sample time is in the future
        if next > now {
            self.counter_next = next;
        } else {
            self.counter_next = now + self.counter_interval;
        }

        // mark when we last sampled
        self.counter_prev = now;
    }

    pub fn refresh_distributions(&mut self, now: Instant) {
        if now < self.distribution_next {
            return;
        }

        self.bpf.refresh_distributions();

        // determine when to sample next
        let next = self.distribution_next + self.distribution_interval;

        // check that next sample time is in the future
        if next > now {
            self.distribution_next = next;
        } else {
            self.distribution_next = now + self.distribution_interval;
        }

        // mark when we last sampled
        self.distribution_prev = now;
    }
}

impl Sampler for Syscall {
    fn sample(&mut self) {
        let now = Instant::now();
        self.refresh_counters(now);
        self.refresh_distributions(now);
    }
}
