mod builder;
mod counters;
mod histogram;
mod sync_primitive;

pub use builder::Builder as BpfBuilder;

use crate::samplers::Sampler;
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
const MAX_CPUS: usize = 1024;

const COUNTER_SIZE: usize = std::mem::size_of::<u64>();

const COUNTERS_PER_CACHELINE: usize = CACHELINE_SIZE / COUNTER_SIZE;

fn whole_cachelines<T>(count: usize) -> usize {
    ((count * std::mem::size_of::<T>()) + CACHELINE_SIZE - 1) / CACHELINE_SIZE
}

fn whole_pages<T>(count: usize) -> usize {
    ((count * std::mem::size_of::<T>()) + PAGE_SIZE - 1) / PAGE_SIZE
}

use counters::{Counters, CpuCounters};
use histogram::Histogram;
use sync_primitive::SyncPrimitive;

pub struct AsyncBpf {
    thread: std::thread::JoinHandle<Result<(), libbpf_rs::Error>>,
    sync: SyncPrimitive,
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
    }
}
