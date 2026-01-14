mod builder;
mod counters;
mod histogram;
mod sync_primitive;

pub use builder::Builder as BpfBuilder;
pub use builder::{BpfProgStats, PerfEvent};

use crate::agent::samplers::Sampler;
use crate::agent::MetricGroup;
use crate::*;

pub trait OpenSkelExt {
    /// When called, the SkelBuilder should log instruction counts for each of
    /// the programs within the skeleton. Log level should be debug.
    fn log_prog_instructions(&self);
}

pub trait SkelExt {
    fn map(&self, name: &str) -> &libbpf_rs::Map<'_>;
}

pub trait CgroupInfo {
    fn id(&self) -> i32;
    fn level(&self) -> i32;
    fn name(&self) -> &[u8];
    fn pname(&self) -> &[u8];
    fn gpname(&self) -> &[u8];
}

#[macro_export]
macro_rules! impl_cgroup_info {
    ($type:ty) => {
        impl $crate::agent::bpf::CgroupInfo for $type {
            fn id(&self) -> i32 {
                self.id
            }

            fn level(&self) -> i32 {
                self.level
            }

            fn name(&self) -> &[u8] {
                &self.name
            }

            fn pname(&self) -> &[u8] {
                &self.pname
            }

            fn gpname(&self) -> &[u8] {
                &self.gpname
            }
        }
    };
}

const CACHELINE_SIZE: usize = 64;
const PAGE_SIZE: usize = 4096;

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
pub use sync_primitive::SyncPrimitive;

pub fn process_cgroup_info<T>(data: &[u8], metrics: &[&dyn MetricGroup]) -> i32
where
    T: CgroupInfo + plain::Plain + Default,
{
    let mut cgroup_info = T::default();

    if plain::copy_from_bytes(&mut cgroup_info, data).is_ok() {
        // Process name fields from bytes to strings
        let name = std::str::from_utf8(cgroup_info.name())
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let pname = std::str::from_utf8(cgroup_info.pname())
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let gpname = std::str::from_utf8(cgroup_info.gpname())
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        // Construct hierarchical path based on level and available parent names
        let path = if name == "/" {
            // Root cgroup - just use "/"
            "/".to_string()
        } else if !gpname.is_empty() {
            if cgroup_info.level() > 3 {
                format!(".../{gpname}/{pname}/{name}")
            } else {
                format!("/{gpname}/{pname}/{name}")
            }
        } else if !pname.is_empty() {
            format!("/{pname}/{name}")
        } else if !name.is_empty() {
            format!("/{name}")
        } else {
            "".to_string()
        };

        // Update metadata for all provided metrics
        if !path.is_empty() {
            let id = cgroup_info.id() as usize;
            for metric in metrics {
                metric.insert_metadata(id, "name".to_string(), path.clone());
            }
        }
    }

    0
}

pub struct AsyncBpf {
    name: &'static str,
    thread: std::thread::JoinHandle<Result<(), libbpf_rs::Error>>,
    sync: SyncPrimitive,
    perf_threads: Vec<std::thread::JoinHandle<()>>,
    perf_sync: Vec<SyncPrimitive>,
}

#[async_trait]
impl Sampler for AsyncBpf {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn refresh(&self) {
        // check that the thread has not exited
        if self.thread.is_finished() {
            panic!("{} bpf thread exited early", self.name);
        }

        // notify the thread to start
        self.sync.trigger();

        // wait for notification that thread has finished
        self.sync.wait_notify().await;

        // check that no perf threads have exited
        for thread in self.perf_threads.iter() {
            if thread.is_finished() {
                panic!("{} perf thread exited early", self.name);
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
