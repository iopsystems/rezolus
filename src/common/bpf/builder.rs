use crate::common::bpf::*;
use crate::common::*;

use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use libbpf_rs::OpenObject;
use metriken::{LazyCounter, RwLockHistogram};

use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Builder<T: 'static + SkelBuilder<'static>> {
    skel: fn() -> T,
    counters: Vec<(&'static str, Vec<&'static LazyCounter>)>,
    histograms: Vec<(&'static str, &'static RwLockHistogram)>,
    maps: Vec<(&'static str, Vec<u64>)>,
    cpu_counters: Vec<(&'static str, Vec<&'static LazyCounter>, ScopedCounters)>,
}

impl<T: 'static> Builder<T>
where
    T: SkelBuilder<'static>,
    <<T as SkelBuilder<'static>>::Output as OpenSkel<'static>>::Output: OpenSkelExt,
    <<T as SkelBuilder<'static>>::Output as OpenSkel<'static>>::Output: SkelExt,
{
    pub fn new(skel: fn() -> T) -> Self {
        Self {
            skel,
            counters: Vec::new(),
            histograms: Vec::new(),
            maps: Vec::new(),
            cpu_counters: Vec::new(),
        }
    }

    pub fn build(self) -> Result<AsyncBpf, libbpf_rs::Error> {
        let sync = SyncPrimitive::new();
        let sync2 = sync.clone();

        let initialized = Arc::new(AtomicBool::new(false));
        let initialized2 = initialized.clone();

        let thread = std::thread::spawn(move || {
            // storage for the BPF object file
            let open_object: &'static mut MaybeUninit<OpenObject> =
                Box::leak(Box::new(MaybeUninit::uninit()));

            // open and load the BPF program
            let mut skel = (self.skel)().open(open_object)?.load()?;

            // log the number of instructions for each probe in the program
            skel.log_prog_instructions();

            // attach the BPF program
            skel.attach()?;

            // convert our metrics into wrapped types that we can refresh

            let mut counters: Vec<Counters> = self
                .counters
                .into_iter()
                .map(|(name, counters)| Counters::new(skel.map(name), counters))
                .collect();

            let mut histograms: Vec<Histogram> = self
                .histograms
                .into_iter()
                .map(|(name, histogram)| Histogram::new(skel.map(name), histogram))
                .collect();

            let mut cpu_counters: Vec<CpuCounters> = self
                .cpu_counters
                .into_iter()
                .map(|(name, totals, individual)| {
                    CpuCounters::new(skel.map(name), totals, individual)
                })
                .collect();

            // load any data from userspace into BPF maps
            for (name, values) in self.maps.into_iter() {
                let fd = skel.map(name).as_fd().as_raw_fd();
                let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
                let mut mmap = unsafe {
                    memmap2::MmapOptions::new()
                        .len(std::mem::size_of::<u64>() * values.len())
                        .map_mut(&file)
                        .expect("failed to mmap() bpf map")
                };

                for (index, bytes) in mmap
                    .chunks_exact_mut(std::mem::size_of::<u64>())
                    .enumerate()
                {
                    let value = bytes.as_mut_ptr() as *mut u64;
                    unsafe {
                        *value = values[index];
                    }
                }

                let _ = mmap.flush();
            }

            // indicate that we have finished initialization
            initialized.store(true, Ordering::Relaxed);

            // the sampling loop
            loop {
                // blocking wait until we are notified to start, no cpu consumed
                sync.wait_trigger();

                // refresh all the metrics

                for v in &mut counters {
                    v.refresh();
                }

                for v in &mut histograms {
                    v.refresh();
                }

                for v in &mut cpu_counters {
                    v.refresh();
                }

                // notify that we have finished running
                sync.notify();
            }
        });

        // wait for the sampler thread to either error out or finish initializing
        loop {
            if thread.is_finished() {
                if let Err(e) = thread.join().unwrap() {
                    return Err(e);
                } else {
                    // the thread can't terminate without an error
                    unreachable!();
                }
            }

            if initialized2.load(Ordering::Relaxed) {
                break;
            }
        }

        Ok(AsyncBpf {
            thread,
            sync: sync2,
        })
    }

    /// Register a set of counters for this BPF sampler. The `name` is the BPF
    /// map name and the `counters` are a set of userspace lazy counters which
    /// must match the ordering used in the BPF map. See `Counters` for more
    /// details on the assumptions and requirements.
    pub fn counters(mut self, name: &'static str, counters: Vec<&'static LazyCounter>) -> Self {
        self.counters.push((name, counters));
        self
    }

    /// Register a histogram for this BPF sampler. The `name` is the BPF map
    /// name and the `histogram` is the userspace histogram. The histogram
    /// parameters used in both the BPF and userpsace histograms must match
    /// exactly. See `Histogram` for more details on the assumptions and
    /// requirements.
    pub fn histogram(mut self, name: &'static str, histogram: &'static RwLockHistogram) -> Self {
        self.histograms.push((name, histogram));
        self
    }

    /// Register a map which is loaded from userspace values into the BPF
    /// program. This is useful for dynamic configuration or providing lookup
    /// tables.
    pub fn map(mut self, name: &'static str, values: Vec<u64>) -> Self {
        self.maps.push((name, values));
        self
    }

    /// Register a set of counters for this BPF sampler where both totals and
    /// individual CPU counters are tracked. See `Counters` for more details on
    /// the details and assumptions for the BPF map.
    pub fn cpu_counters(
        mut self,
        name: &'static str,
        totals: Vec<&'static LazyCounter>,
        individual: ScopedCounters,
    ) -> Self {
        self.cpu_counters.push((name, totals, individual));
        self
    }
}
