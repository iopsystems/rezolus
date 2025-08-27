use super::*;
use crate::agent::*;
use crate::{error, warn};

use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use libbpf_rs::{MapCore, MapFlags, OpenObject, PrintLevel, RingBuffer, RingBufferBuilder};
use metriken::{LazyCounter, RwLockHistogram};
use perf_event::ReadFormat;

use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub struct BpfProgStats {
    pub run_time: &'static LazyCounter,
    pub run_count: &'static LazyCounter,
}

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

    fn spawn_multi(
        self,
        perf_threads_tx: SyncSender<JoinHandle<()>>,
        perf_sync_tx: SyncSender<SyncPrimitive>,
    ) {
        if !self.inner.is_empty() {
            debug!("using multi-threaded perf counter collection");

            let pt_pending = Arc::new(AtomicUsize::new(self.inner.len()));

            debug!(
                "launching {} threads to read perf counters",
                pt_pending.load(Ordering::SeqCst)
            );

            for (cpu, mut counters) in self.inner.into_iter() {
                trace!("launching perf thread for cpu {}", cpu);

                let psync = SyncPrimitive::new();
                let psync2 = psync.clone();

                let perf_threads = perf_threads_tx.clone();
                let perf_sync = perf_sync_tx.clone();

                let pt_pending = pt_pending.clone();

                perf_threads
                    .send(std::thread::spawn(move || {
                        if !core_affinity::set_for_current(core_affinity::CoreId { id: cpu }) {
                            warn!("failed to pin perf thread for core: {}", cpu);
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

            debug!("all perf threads launched");
        }
    }

    fn spawn_single(
        self,
        perf_threads_tx: SyncSender<JoinHandle<()>>,
        perf_sync_tx: SyncSender<SyncPrimitive>,
    ) {
        if !self.inner.is_empty() {
            debug!("using single-threaded perf counter collection");

            let mut counters: Vec<_> = self.inner.into_values().collect();

            let psync = SyncPrimitive::new();
            let psync2 = psync.clone();

            let perf_threads = perf_threads_tx.clone();
            let perf_sync = perf_sync_tx.clone();

            perf_threads
                .send(std::thread::spawn(move || loop {
                    psync.wait_trigger();

                    for c in counters.iter_mut() {
                        c.refresh();
                    }

                    psync.notify();
                }))
                .expect("failed to send perf thread handle");

            perf_sync
                .send(psync2)
                .expect("failed to send perf thread sync primitive");
        }
    }

    pub fn spawn(
        self,
        perf_threads_tx: SyncSender<JoinHandle<()>>,
        perf_sync_tx: SyncSender<SyncPrimitive>,
    ) {
        if !self.inner.is_empty() {
            // on virtualized environments, it is typically better to use
            // multiple threads to read the perf counters to get more
            // consistent snapshot latency
            if is_virt() {
                self.spawn_multi(perf_threads_tx, perf_sync_tx);
            } else {
                self.spawn_single(perf_threads_tx, perf_sync_tx);
            }
        }
    }
}

enum Event {
    Hardware(perf_event::events::Hardware),
}

impl Event {
    fn builder(&self) -> perf_event::Builder {
        match self {
            Self::Hardware(e) => perf_event::Builder::new(*e),
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
}

pub struct Builder<T: 'static + SkelBuilder<'static>> {
    name: &'static str,
    skel: fn() -> T,
    prog_stats: BpfProgStats,
    counters: Vec<(&'static str, Vec<&'static LazyCounter>)>,
    histograms: Vec<(&'static str, &'static RwLockHistogram)>,
    maps: Vec<(&'static str, Vec<u64>)>,
    cpu_counters: Vec<(&'static str, Vec<&'static CounterGroup>)>,
    perf_events: Vec<(&'static str, PerfEvent, &'static CounterGroup)>,
    packed_counters: Vec<(&'static str, &'static CounterGroup)>,
    #[allow(clippy::type_complexity)]
    ringbuf_handler: Vec<(&'static str, fn(&[u8]) -> i32)>,
    btf_path: Option<String>,
}

impl<T: 'static> Builder<T>
where
    T: SkelBuilder<'static>,
    <<T as SkelBuilder<'static>>::Output as OpenSkel<'static>>::Output: OpenSkelExt,
    <<T as SkelBuilder<'static>>::Output as OpenSkel<'static>>::Output: SkelExt,
{
    pub fn new(
        config: &crate::agent::Config,
        name: &'static str,
        prog_stats: BpfProgStats,
        skel: fn() -> T,
    ) -> Self {
        Self {
            name,
            skel,
            prog_stats,
            counters: Vec::new(),
            histograms: Vec::new(),
            maps: Vec::new(),
            cpu_counters: Vec::new(),
            perf_events: Vec::new(),
            packed_counters: Vec::new(),
            ringbuf_handler: Vec::new(),
            btf_path: config.general().btf_path().map(|s| s.to_string()),
        }
    }

    pub fn build(self) -> Result<AsyncBpf, libbpf_rs::Error> {
        let sync = SyncPrimitive::new();
        let sync2 = sync.clone();

        let initialized = Arc::new(AtomicBool::new(false));
        let initialized2 = initialized.clone();

        let cpus = match crate::common::cpus() {
            Ok(cpus) => cpus.last().copied().unwrap_or(1023),
            Err(_) => 1023,
        };

        let cpus = cpus + 1;

        let (perf_threads_tx, perf_threads_rx) = sync_channel(cpus);
        let (perf_sync_tx, perf_sync_rx) = sync_channel(cpus);

        let thread = std::thread::spawn(move || {
            // log all messages from libbpf at debug level
            fn libbpf_print_fn(_level: PrintLevel, msg: String) {
                debug!("libbpf: {}", msg.trim_end());
            }
            libbpf_rs::set_print(Some((PrintLevel::Debug, libbpf_print_fn)));

            // storage for the BPF object file
            let open_object: &'static mut MaybeUninit<OpenObject> =
                Box::leak(Box::new(MaybeUninit::uninit()));

            // Open the BPF program with optional custom BTF path
            let open_skel = if let Some(ref btf_path) = self.btf_path {
                debug!("Loading BPF program with external BTF from: {}", btf_path);

                // Create C string for the BTF path
                let btf_path_cstr = std::ffi::CString::new(btf_path.as_str())
                    .map_err(|_| libbpf_rs::Error::from_raw_os_error(libc::EINVAL))?;

                // Create open options with custom BTF path
                let open_opts = unsafe {
                    let mut opts: libbpf_sys::bpf_object_open_opts = std::mem::zeroed();
                    opts.sz = std::mem::size_of::<libbpf_sys::bpf_object_open_opts>()
                        as libbpf_sys::size_t;
                    opts.btf_custom_path = btf_path_cstr.as_ptr();
                    opts
                };

                // Open with custom BTF path using open_opts
                match (self.skel)().open_opts(open_opts, open_object) {
                    Ok(skel) => {
                        debug!("Successfully loaded external BTF from: {}", btf_path);
                        skel
                    }
                    Err(e) => {
                        error!("Failed to load external BTF from {}: {}", btf_path, e);
                        return Err(e);
                    }
                }
            } else {
                // Open normally without custom BTF
                (self.skel)().open(open_object)?
            };

            // load the BPF program
            let mut skel = open_skel.load()?;

            // log the number of instructions for each probe in the program
            skel.log_prog_instructions();

            // attach the BPF program
            match skel.attach() {
                Ok(_) => {}
                Err(e) if e.kind() == libbpf_rs::ErrorKind::NotFound => {
                    debug!("Some BPF probes skipped due to missing kernel symbols");
                }
                Err(e) => return Err(e),
            }

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
                "{} initializing perf counters for: {} events",
                self.name,
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

            perf_counters.spawn(perf_threads_tx.clone(), perf_sync_tx.clone());

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

                let mut run_time: u64 = 0;
                let mut run_count: u64 = 0;

                for prog in skel.object().progs() {
                    let mut info = libbpf_sys::bpf_prog_info::default();
                    let mut len = std::mem::size_of::<libbpf_sys::bpf_prog_info>() as u32;

                    let fd = prog.as_fd().as_raw_fd();

                    let result =
                        unsafe { libbpf_sys::bpf_prog_get_info_by_fd(fd, &mut info, &mut len) };

                    if result == 0 {
                        run_time = run_time.wrapping_add(info.run_time_ns);
                        run_count = run_count.wrapping_add(info.run_cnt);
                    }
                }

                if run_time > 0 {
                    self.prog_stats.run_time.set(run_time);
                }

                if run_count > 0 {
                    self.prog_stats.run_count.set(run_count);
                }

                // notify that we have finished running
                sync.notify();
            }
        });

        debug!(
            "{} waiting for sampler thread to finish initialization",
            self.name
        );

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

        debug!(
            "{} gathering perf thread sync primitives and join handles",
            self.name
        );

        // gather perf thread sync primitives and join handles
        let perf_sync = perf_sync_rx.try_iter().collect();
        let perf_threads = perf_threads_rx.try_iter().collect();

        debug!("{} completed BPF sampler initialization", self.name);

        Ok(AsyncBpf {
            thread,
            name: self.name,
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
