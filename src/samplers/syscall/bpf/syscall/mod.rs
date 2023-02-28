#[distributed_slice(SYSCALL_BPF_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    Box::new(Syscall::new(config))
}

mod bpf;

use bpf::*;

use common::bpf::{Counter, Distribution};
use super::super::stats::*;
use super::super::*;
use syscall_numbers::native::*;
use std::collections::HashMap;

/// Collects Scheduler Runqueue Latency stats using BPF
/// tracepoints:
/// * "tracepoint/raw_syscalls/sys_exit"
///
/// stats:
/// * syscall/*
pub struct Syscall {
    skel: ModSkel<'static>,
    total: Counter,
    counters: HashMap<String, Counter>,
    // distributions: Vec<Distribution>,

    next: Instant,
    dist_next: Instant,
    prev: Instant,
    interval: Duration,
    dist_interval: Duration,
}

// This should match the size of the array in the BPF. Choosen to be adaquate
// for x86, x86_64, arm, aarch64
pub const COUNTERS: usize = 512;

impl Syscall {
    pub fn new(_config: &Config) -> Self {
        let now = Instant::now();

        let builder = ModSkelBuilder::default();
        let mut skel = builder.open().expect("failed to open bpf builder").load().expect("failed to load bpf program");
        skel.attach().expect("failed to attach bpf");

        // one counter for total syscalls
        let total = Counter::new("total", &SYSCALL_TOTAL, Some(&SYSCALL_TOTAL_HIST));

        // counters are stored in a hashmap by their syscall name
        let mut counters = HashMap::new();
        for (name, counter) in [
            ("read", &SYSCALL_READ),
            ("write", &SYSCALL_WRITE),
            ("open", &SYSCALL_OPEN),
            ("close", &SYSCALL_CLOSE),
            ("recvfrom", &SYSCALL_RECVFROM),
            ("recvmsg", &SYSCALL_RECVMSG),
            ("recvmmsg", &SYSCALL_RECVMMSG),
            ("sendto", &SYSCALL_SENDTO),
            ("sendmsg", &SYSCALL_SENDMSG),
            ("shutdown", &SYSCALL_SHUTDOWN),
            ("bind", &SYSCALL_BIND),
            ("listen", &SYSCALL_LISTEN),
            ("epoll_wait", &SYSCALL_EPOLL_WAIT),
            ("epoll_ctl", &SYSCALL_EPOLL_CTL),
            ("bpf", &SYSCALL_BPF),
            ("clock_nanosleep", &SYSCALL_CLOCK_NANOSLEEP),
            ("madvise", &SYSCALL_MADVISE),
            ("openat", &SYSCALL_OPENAT),
            ("futex", &SYSCALL_FUTEX),
            ("ioctl", &SYSCALL_IOCTL),
            ("setsockopt", &SYSCALL_SETSOCKOPT),
            ("accept4", &SYSCALL_ACCEPT4),
        ] {
            let counter = Counter::new(name, counter, None);
            counters.insert(name.to_owned(), counter);
        }

        // let distributions = vec![
        //     Distribution::new("latency", &SCHEDULER_RUNQUEUE_LATENCY),
        // ];

        Self {
            skel,
            total,
            counters,
            // distributions,
            next: now,
            prev: now,
            dist_next: now,
            interval: Duration::from_millis(50),
            dist_interval: Duration::from_millis(100),
        }
    }   
}

impl Sampler for Syscall {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let elapsed = (now - self.prev).as_secs_f64();

        let maps = self.skel.maps();

        let counts = crate::common::bpf::read_counters(maps.counters(), COUNTERS);

        let mut total: u64 = 0;
        for (id, count) in counts.iter().enumerate() {
            total = total.wrapping_add(*count);
            if let Some(name) = sys_call_name(id as core::ffi::c_long) {
                if let Some(counter) = self.counters.get_mut(name) {
                    counter.set(now, elapsed, *count);
                }
            }
        }
        self.total.set(now, elapsed, total);


        // for (id, counter) in self.counters.iter_mut().enumerate() {
        //     if let Some(current) = counts.get(&id) {
        //         counter.update(now, elapsed, *current);
        //     }
        // }

        // // determine if we should sample the distributions
        // if now >= self.dist_next {
        //     for distribution in self.distributions.iter_mut() {
        //         distribution.update(&self.skel.obj);
        //     }

        //     // determine when to sample next
        //     let next = self.dist_next + self.dist_interval;

        //     // check that next sample time is in the future
        //     if next > now {
        //         self.dist_next = next;
        //     } else {
        //         self.dist_next = now + self.dist_interval;
        //     }
        // }

        // determine when to sample next
        let next = self.next + self.interval;
        
        // check that next sample time is in the future
        if next > now {
            self.next = next;
        } else {
            self.next = now + self.interval;
        }

        // mark when we last sampled
        self.prev = now;
    }
}

impl std::fmt::Display for Syscall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "syscall::bpf::syscall")
    }
}