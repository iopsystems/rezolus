use crate::common::bpf::*;
use crate::common::*;

use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use libbpf_rs::{MapCore, MapFlags, OpenObject, RingBuffer, RingBufferBuilder};
use metriken::{LazyCounter, RwLockHistogram};
use perf_event::ReadFormat;

use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::time::Duration;

pub struct PerfEvent {
    inner: Event,
}

pub struct PerfCounter {
    counter: perf_event::Counter,
    group: &'static CounterGroup,
}

pub struct CpuPerfCounters {
    cpu: usize,
    counters: Vec<PerfCounter>,
}

impl CpuPerfCounters {
    pub fn new(cpu: usize) -> Self {
        Self {
            cpu,
            counters: Vec::new(),
        }
    }

    pub fn push(
        &mut self,
        counter: perf_event::Counter,
        group: &'static CounterGroup,
    ) -> &mut Self {
        self.counters.push(PerfCounter { counter, group });

        self
    }

    pub fn refresh(&mut self) {
        for c in self.counters.iter_mut() {
            if let Ok(value) = c.counter.read() {
                let _ = c.group.set(self.cpu, value);
            }
        }
    }
}

pub struct PerfCounters {
    inner: HashMap<usize, CpuPerfCounters>,
}

impl PerfCounters {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    pub fn push(&mut self, cpu: usize, counter: perf_event::Counter, group: &'static CounterGroup) {
        let counters = self.inner.entry(cpu).or_insert(CpuPerfCounters::new(cpu));
        counters.push(counter, group);
    }
}

enum Event {
    Hardware(perf_event::events::Hardware),
    Msr(perf_event::events::x86::Msr),
}

impl Event {
    fn builder(&self) -> perf_event::Builder {
        match self {
            Self::Hardware(e) => perf_event::Builder::new(*e),
            Self::Msr(m) => perf_event::Builder::new(*m),
        }
    }
}

impl PerfEvent {
    pub fn cpu_cycles() -> Self {
        Self {
            inner: Event::Hardware(perf_event::events::Hardware::CPU_CYCLES),
        }
    }

    pub fn instructions() -> Self {
        Self {
            inner: Event::Hardware(perf_event::events::Hardware::INSTRUCTIONS),
        }
    }

    pub fn msr(msr_id: perf_event::events::x86::MsrId) -> Result<Self, std::io::Error> {
        let msr = perf_event::events::x86::Msr::new(msr_id)?;

        Ok(Self {
            inner: Event::Msr(msr),
        })
    }
}

pub struct Builder<T: 'static + SkelBuilder<'static>> {
    skel: fn() -> T,
    counters: Vec<(&'static str, Vec<&'static LazyCounter>)>,
    histograms: Vec<(&'static str, &'static RwLockHistogram)>,
    maps: Vec<(&'static str, Vec<u64>)>,
    cpu_counters: Vec<(&'static str, Vec<&'static CounterGroup>)>,
    perf_events: Vec<(&'static str, PerfEvent, &'static CounterGroup)>,
    packed_counters: Vec<(&'static str, &'static CounterGroup)>,
    ringbuf_handler: Vec<(&'static str, fn(&[u8]) -> i32)>,
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
            perf_events: Vec::new(),
            packed_counters: Vec::new(),
            ringbuf_handler: Vec::new(),
        }
    }

    pub fn build(self) -> Result<AsyncBpf, libbpf_rs::Error> {
        let sync = SyncPrimitive::new();
        let sync2 = sync.clone();

        let initialized = Arc::new(AtomicBool::new(false));
        let initialized2 = initialized.clone();

        let cpus = match common::linux::cpus() {
            Ok(cpus) => cpus.last().copied().unwrap_or(1023),
            Err(_) => 1023,
        };

        let cpus = cpus + 1;

        let (perf_threads_tx, perf_threads_rx) = sync_channel(cpus);
        let (perf_sync_tx, perf_sync_rx) = sync_channel(cpus);

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
                .map(|(name, counters)| CpuCounters::new(skel.map(name), counters))
                .collect();

            debug!(
                "initializing perf counters for: {} events",
                self.perf_events.len()
            );

            let mut perf_counters = PerfCounters::new();

            for (name, event, group) in self.perf_events.into_iter() {
                let map = skel.map(name);

                for cpu in 0..cpus {
                    if let Ok(mut counter) = event
                        .inner
                        .builder()
                        .one_cpu(cpu)
                        .any_pid()
                        .exclude_hv(false)
                        .exclude_kernel(false)
                        .pinned(true)
                        .read_format(
                            ReadFormat::TOTAL_TIME_ENABLED
                                | ReadFormat::TOTAL_TIME_RUNNING
                                | ReadFormat::GROUP,
                        )
                        .build()
                    {
                        let _ = counter.enable();

                        let fd = counter.as_raw_fd();

                        let _ = map.update(
                            &((cpu as u32).to_ne_bytes()),
                            &(fd.to_ne_bytes()),
                            MapFlags::ANY,
                        );

                        perf_counters.push(cpu, counter, group);
                    }
                }
            }

            if !perf_counters.inner.is_empty() {
                let (unpinned_tx, unpinned_rx) = sync_channel(cpus);

                let pt_pending = Arc::new(AtomicUsize::new(perf_counters.inner.len()));

                debug!(
                    "launching {} threads to read perf counters",
                    pt_pending.load(Ordering::SeqCst)
                );

                for (cpu, mut counters) in perf_counters.inner.into_iter() {
                    trace!("launching perf thread for cpu {}", cpu);

                    let psync = SyncPrimitive::new();
                    let psync2 = psync.clone();

                    let unpinned = unpinned_tx.clone();
                    let perf_threads = perf_threads_tx.clone();
                    let perf_sync = perf_sync_tx.clone();

                    let pt_pending = pt_pending.clone();

                    perf_threads
                        .send(std::thread::spawn(move || {
                            if !core_affinity::set_for_current(core_affinity::CoreId { id: cpu }) {
                                unpinned
                                    .send(counters)
                                    .expect("failed to send unpinned perf counters");
                                pt_pending.fetch_sub(1, Ordering::Relaxed);
                                return;
                            }

                            pt_pending.fetch_sub(1, Ordering::Relaxed);

                            loop {
                                psync.wait_trigger();

                                counters.refresh();

                                psync.notify();
                            }
                        }))
                        .expect("failed to send perf thread handle");

                    perf_sync
                        .send(psync2)
                        .expect("failed to send perf thread sync primitive");
                }

                debug!("waiting for perf threads to launch");

                while pt_pending.load(Ordering::Relaxed) > 0 {
                    std::thread::sleep(Duration::from_millis(50));
                }

                debug!("checking for unpinned perf threads");

                let mut unpinned: Vec<_> = unpinned_rx.try_iter().collect();

                debug!(
                    "there are {} perf threads which could not be pinned",
                    unpinned.len()
                );

                if !unpinned.is_empty() {
                    let psync = SyncPrimitive::new();
                    let psync2 = psync.clone();

                    let perf_threads = perf_threads_tx.clone();
                    let perf_sync = perf_sync_tx.clone();

                    perf_threads
                        .send(std::thread::spawn(move || loop {
                            psync.wait_trigger();

                            for counters in unpinned.iter_mut() {
                                counters.refresh();
                            }

                            psync.notify();
                        }))
                        .expect("failed to send perf thread handle");

                    perf_sync
                        .send(psync2)
                        .expect("failed to send perf thread sync primitive");
                }

                debug!("all perf threads launched");
            }

            let ringbuffer: Option<RingBuffer> = if self.ringbuf_handler.is_empty() {
                None
            } else {
                let mut builder = RingBufferBuilder::new();

                for (name, handler) in self.ringbuf_handler.into_iter() {
                    let _ = builder.add(skel.map(name), handler);
                }

                Some(builder.build().expect("failed to initialize ringbuffer"))
            };

            let mut packed_counters: Vec<PackedCounters> = self
                .packed_counters
                .into_iter()
                .map(|(name, counters)| PackedCounters::new(skel.map(name), counters))
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

                // consume all data from ringbuffers
                if let Some(ref rb) = ringbuffer {
                    let _ = rb.consume();
                }

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

                for v in &mut packed_counters {
                    v.refresh();
                }

                // notify that we have finished running
                sync.notify();
            }
        });

        debug!("waiting for sampler thread to finish initialization");

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

        debug!("gathering perf thread sync primitives and join handles");

        // gather perf thread sync primitives and join handles
        let perf_sync = perf_sync_rx.try_iter().collect();
        let perf_threads = perf_threads_rx.try_iter().collect();

        debug!("completed BPF sampler initialization");

        Ok(AsyncBpf {
            thread,
            sync: sync2,
            perf_threads,
            perf_sync,
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

    /// Register a set of counters for this BPF sampler where just the
    /// individual CPU counters are tracked. See `Counters` for more details on
    /// the details and assumptions for the BPF map.
    pub fn cpu_counters(
        mut self,
        name: &'static str,
        counters: Vec<&'static CounterGroup>,
    ) -> Self {
        self.cpu_counters.push((name, counters));
        self
    }

    /// Specify a perf event array name and an associated perf event.
    pub fn perf_event(
        mut self,
        name: &'static str,
        event: PerfEvent,
        group: &'static CounterGroup,
    ) -> Self {
        self.perf_events.push((name, event, group));
        self
    }

    /// Register a set of packed counters. The `name` is the BPF map name and
    /// the `counters` are a set of userspace dynamic counters. The BPF map is
    /// expected to be densely packed, meaning there is no padding. The order of
    /// the `counters` must exactly match the order in the BPF map.
    pub fn packed_counters(mut self, name: &'static str, counters: &'static CounterGroup) -> Self {
        self.packed_counters.push((name, counters));
        self
    }

    pub fn ringbuf_handler(mut self, name: &'static str, handler: fn(&[u8]) -> i32) -> Self {
        self.ringbuf_handler.push((name, handler));
        self
    }
}
