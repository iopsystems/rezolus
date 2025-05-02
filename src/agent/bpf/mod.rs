mod builder;
mod counters;
mod histogram;
mod sync_primitive;

pub use builder::Builder as BpfBuilder;
pub use builder::PerfEvent;

use crate::agent::samplers::Sampler;
use crate::*;

pub trait OpenSkelExt {
    /// When called, the SkelBuilder should log instruction counts for each of
    /// the programs within the skeleton. Log level should be debug.
    fn log_prog_instructions(&self);
}

pub trait SkelExt {
    fn map(&self, name: &str) -> &libbpf_rs::Map;
}

const CACHELINE_SIZE: usize = 64;
const PAGE_SIZE: usize = 4096;

// This is the maximum number of CPUs we track with BPF counters.
pub const MAX_CPUS: usize = 1024;

// This is the maximum number of cgroups we track with BPF counters.
pub const MAX_CGROUPS: usize = 4096;

// This is the maximum PID we track
pub const MAX_PID: usize = 4194304;

const COUNTER_SIZE: usize = std::mem::size_of::<u64>();
const COUNTERS_PER_CACHELINE: usize = CACHELINE_SIZE / COUNTER_SIZE;

fn whole_cachelines<T>(count: usize) -> usize {
    (count * std::mem::size_of::<T>()).div_ceil(CACHELINE_SIZE)
}

fn whole_pages<T>(count: usize) -> usize {
    (count * std::mem::size_of::<T>()).div_ceil(PAGE_SIZE)
}

use counters::{Counters, CpuCounters, PackedCounters};
use histogram::Histogram;
use sync_primitive::SyncPrimitive;

pub struct AsyncBpf {
    thread: std::thread::JoinHandle<Result<(), libbpf_rs::Error>>,
    sync: SyncPrimitive,
    perf_threads: Vec<std::thread::JoinHandle<()>>,
    perf_sync: Vec<SyncPrimitive>,
}

#[async_trait]
impl Sampler for AsyncBpf {
    async fn refresh(&self) {
        // check that the thread has not exited
        if self.thread.is_finished() {
            panic!("thread exited early");
        }

        // notify the thread to start
        self.sync.trigger();

        // wait for notification that thread has finished
        self.sync.wait_notify().await;

        // check that no perf threads have exited
        for thread in self.perf_threads.iter() {
            if thread.is_finished() {
                panic!("perf thread exited early");
            }
        }

        // trigger and wait on all perf threads
        let perf_futures: Vec<_> = self
            .perf_sync
            .iter()
            .map(|s| {
                s.trigger();
                s.wait_notify()
            })
            .collect();

        futures::future::join_all(perf_futures.into_iter()).await;
    }
}
