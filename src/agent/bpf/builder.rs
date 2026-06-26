use super::*;
use crate::agent::*;
use crate::{error, warn};
use tracing::trace;

use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use libbpf_rs::{MapCore, MapFlags, OpenObject, PrintLevel, RingBuffer, RingBufferBuilder};
use metriken::{CounterGroup, LazyCounter, RwLockHistogram};
use perf_event::ReadFormat;

use std::collections::HashMap;
use std::collections::HashSet;
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
    fn builder(&self) -> perf_event::Builder<'_> {
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
    /// Optional list of program names to enable. If None, all programs are
    /// enabled (default behavior). If Some, only the listed programs will have
    /// autoload enabled; all others will be disabled before loading.
    enabled_programs: Option<HashSet<&'static str>>,
    /// Optional list of program names to disable. Any program named here has
    /// autoload disabled before load, regardless of `enabled_programs`. Used to
    /// drop the unused variant when a sampler ships both a `tp_btf` and a
    /// `raw_tp` version of a hook.
    disabled_programs: Option<HashSet<&'static str>>,
    /// Optional per-program intent overrides. Programs absent from this map
    /// default to `ProbeIntent::Required`. Used to mark per-driver probes.
    program_intents: HashMap<&'static str, crate::agent::sampler_status::ProbeIntent>,
    /// Optional human capability labels per program, for readable health
    /// reasons. Intent stays whatever `program_intents` says (default Required).
    program_labels: HashMap<&'static str, &'static str>,
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
            enabled_programs: None,
            disabled_programs: None,
            program_intents: HashMap::new(),
            program_labels: HashMap::new(),
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
            fn libbpf_print_fn(_level: PrintLevel, msg: String) {
                debug!("libbpf: {}", msg.trim_end());
            }
            libbpf_rs::set_print(Some((PrintLevel::Debug, libbpf_print_fn)));

            let open_object: &'static mut MaybeUninit<OpenObject> =
                Box::leak(Box::new(MaybeUninit::uninit()));

            // Open the BPF program with optional custom BTF path
            let mut open_skel = if let Some(ref btf_path) = self.btf_path {
                debug!("Loading BPF program with external BTF from: {}", btf_path);

                let btf_path_cstr = std::ffi::CString::new(btf_path.as_str())
                    .map_err(|_| libbpf_rs::Error::from_raw_os_error(libc::EINVAL))?;

                let open_opts = unsafe {
                    let mut opts: libbpf_sys::bpf_object_open_opts = std::mem::zeroed();
                    opts.sz = std::mem::size_of::<libbpf_sys::bpf_object_open_opts>()
                        as libbpf_sys::size_t;
                    opts.btf_custom_path = btf_path_cstr.as_ptr();
                    opts
                };

                match (self.skel)().open_opts(open_opts, open_object) {
                    Ok(skel) => {
                        debug!("Successfully loaded external BTF from: {}", btf_path);
                        skel
                    }
                    Err(e) => {
                        error!("Failed to load external BTF from {}: {}", btf_path, e);
                        crate::agent::sampler_status::set_failed(self.name, e.to_string());
                        return Err(e);
                    }
                }
            } else {
                match (self.skel)().open(open_object) {
                    Ok(skel) => skel,
                    Err(e) => {
                        crate::agent::sampler_status::set_failed(self.name, e.to_string());
                        return Err(e);
                    }
                }
            };

            // If enabled_programs is set, disable autoload for programs not in the list
            if let Some(ref enabled) = self.enabled_programs {
                for mut prog in open_skel.open_object_mut().progs_mut() {
                    let prog_name = prog.name().to_string_lossy();
                    if !enabled.contains(prog_name.as_ref()) {
                        debug!(
                            "{} disabling autoload for program: {}",
                            self.name, prog_name
                        );
                        prog.set_autoload(false);
                    } else {
                        debug!("{} enabling program: {}", self.name, prog_name);
                    }
                }
            }

            // If disabled_programs is set, disable autoload for those programs
            // (leaving all others at their default). Used to drop the unused
            // tp_btf/raw_tp variant based on in-kernel BTF availability.
            if let Some(ref disabled) = self.disabled_programs {
                for mut prog in open_skel.open_object_mut().progs_mut() {
                    let prog_name = prog.name().to_string_lossy();
                    if disabled.contains(prog_name.as_ref()) {
                        debug!(
                            "{} disabling autoload for program: {}",
                            self.name, prog_name
                        );
                        prog.set_autoload(false);
                    }
                }
            }

            let skel = match open_skel.load() {
                Ok(skel) => skel,
                Err(e) => {
                    crate::agent::sampler_status::set_failed(self.name, e.to_string());
                    return Err(e);
                }
            };

            skel.log_prog_instructions();

            // Attach each program individually so one failing probe (missing
            // kernel symbol, no kprobe support, etc.) does not prevent the
            // others in this skeleton from attaching. Records per-program
            // status. Load/verify failures above remain fatal; only attach
            // failures are tolerated here.
            let bound_drivers = crate::agent::bpf::drivers::bound_drivers();
            let mut links: Vec<libbpf_rs::Link> = Vec::new();
            // (name, attached, is_enoent, error_string) collected first, then
            // classified against declared intent + bound drivers below.
            let mut raw: Vec<(String, bool, bool, Option<String>)> = Vec::new();
            for prog in skel.object().progs_mut() {
                if !prog.autoload() {
                    continue; // intentionally-disabled tp_btf/raw_tp twin
                }
                let prog_name = prog.name().to_string_lossy().to_string();
                match prog.attach() {
                    Ok(link) => {
                        links.push(link);
                        raw.push((prog_name, true, false, None));
                    }
                    Err(e) if e.kind() == libbpf_rs::ErrorKind::NotFound => {
                        debug!(
                            "{} program '{}' not attached (no kernel support): {}",
                            self.name, prog_name, e
                        );
                        raw.push((
                            prog_name,
                            false,
                            true,
                            Some("no kernel support (ENOENT)".to_string()),
                        ));
                    }
                    Err(e) => {
                        debug!(
                            "{} program '{}' failed to attach, skipping: {}",
                            self.name, prog_name, e
                        );
                        raw.push((prog_name, false, false, Some(e.to_string())));
                    }
                }
            }

            // Classify each attempted program against its declared intent and
            // the set of drivers bound to present devices.
            let mut prog_status: Vec<crate::agent::sampler_status::ProgramStatus> = Vec::new();
            for (name, attached, is_enoent, error) in raw {
                let intent = self
                    .program_intents
                    .get(name.as_str())
                    .cloned()
                    .unwrap_or_default();
                let driver_present = match &intent {
                    crate::agent::sampler_status::ProbeIntent::Driver { driver } => {
                        bound_drivers.contains(driver)
                    }
                    _ => false,
                };
                let verdict = crate::agent::sampler_status::classify_program(
                    &intent,
                    attached,
                    is_enoent,
                    driver_present,
                );
                let label = self
                    .program_labels
                    .get(name.as_str())
                    .map(|s| s.to_string());
                // A required probe is always expected to attach; a driver probe
                // only when its driver is bound to a present device.
                let expected = match &intent {
                    crate::agent::sampler_status::ProbeIntent::Required => true,
                    crate::agent::sampler_status::ProbeIntent::Driver { .. } => driver_present,
                };
                prog_status.push(crate::agent::sampler_status::ProgramStatus {
                    name,
                    attached,
                    error,
                    intent: Some(intent),
                    label,
                    expected,
                    verdict,
                });
            }
            // Guard against typos in declared probe names: every program named
            // in an intent/label override must correspond to a real attached or
            // attempted program. A mismatch means the override silently does
            // nothing. Debug-only — names are stringly-typed.
            #[cfg(debug_assertions)]
            {
                let actual: std::collections::HashSet<&str> =
                    prog_status.iter().map(|p| p.name.as_str()).collect();
                for declared in self
                    .program_intents
                    .keys()
                    .chain(self.program_labels.keys())
                {
                    debug_assert!(
                        actual.contains(*declared),
                        "{}: declared program '{}' not found among attached/attempted programs {:?}",
                        self.name,
                        declared,
                        actual
                    );
                }
            }
            let verdicts: Vec<crate::agent::sampler_status::ProbeVerdict> =
                prog_status.iter().map(|p| p.verdict).collect();
            let health = crate::agent::sampler_status::rollup_health(true, &verdicts);
            crate::agent::sampler_status::set_active_with_programs(self.name, health, prog_status);
            // `_links` must outlive the loop for the sampler thread's lifetime;
            // dropping a Link detaches its program.
            let _links = links;

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

            initialized.store(true, Ordering::Relaxed);

            loop {
                // blocking wait until we are notified to start, no cpu consumed
                sync.wait_trigger();

                if let Some(ref rb) = ringbuffer {
                    let _ = rb.consume();
                }

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

                sync.notify();
            }
        });

        debug!(
            "{} waiting for sampler thread to finish initialization",
            self.name
        );

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

    /// Register a set of sparse packed counters. Alias for `packed_counters`
    /// since metriken's `CounterGroup` uses sparse metadata by default.
    pub fn sparse_packed_counters(
        self,
        name: &'static str,
        counters: &'static CounterGroup,
    ) -> Self {
        self.packed_counters(name, counters)
    }

    pub fn ringbuf_handler(mut self, name: &'static str, handler: fn(&[u8]) -> i32) -> Self {
        self.ringbuf_handler.push((name, handler));
        self
    }

    /// Specify which BPF programs to enable. By default, all programs in the
    /// skeleton are enabled. When this method is called, only the listed
    /// programs will be loaded and attached; all others will have autoload
    /// disabled.
    ///
    /// This is useful for architecture-specific program selection, where
    /// different probe types are needed on different platforms (e.g., using
    /// a tracepoint on x86_64 but a kprobe on ARM64).
    ///
    /// # Example
    ///
    /// ```ignore
    /// // On ARM64, only attach the kprobe version
    /// BpfBuilder::new(...)
    ///     .enabled_programs(&["tlb_finish_mmu"])
    ///     .build()?;
    ///
    /// // On x86_64, only attach the tracepoint version
    /// BpfBuilder::new(...)
    ///     .enabled_programs(&["tlb_flush"])
    ///     .build()?;
    /// ```
    pub fn enabled_programs(mut self, names: &[&'static str]) -> Self {
        self.enabled_programs = Some(names.iter().copied().collect());
        self
    }

    /// Specify BPF programs to disable (autoload off). Unlike
    /// [`Self::enabled_programs`], which is an allowlist, this is a denylist:
    /// only the named programs are disabled; everything else loads as usual.
    ///
    /// Use this to drop the unused variant when a sampler defines both a
    /// `tp_btf` and a `raw_tp` version of a hook, selecting at runtime on
    /// [`crate::agent::bpf::kernel_has_btf`].
    ///
    /// # Example
    ///
    /// ```ignore
    /// BpfBuilder::new(...)
    ///     .disabled_programs(if kernel_has_btf() {
    ///         &["handle__sched_switch_raw"]
    ///     } else {
    ///         &["handle__sched_switch_btf"]
    ///     })
    ///     .build()?;
    /// ```
    pub fn disabled_programs(mut self, names: &[&'static str]) -> Self {
        self.disabled_programs = Some(names.iter().copied().collect());
        self
    }

    /// Attach human capability labels to programs so health reasons read well
    /// (e.g. `("cpuacct_account_field_kprobe", "CPU time by category")`).
    /// Intent is unaffected (stays `Required` unless also set via
    /// [`Self::driver_programs`]).
    pub fn required_programs(mut self, items: &[(&'static str, &'static str)]) -> Self {
        for (prog, label) in items {
            self.program_labels.insert(prog, label);
        }
        self
    }

    /// Declare per-driver probes. `driver` is the sysfs driver name (e.g.
    /// `virtio_net`, `mlx5_core`), which may differ from the probe symbol
    /// prefix. Such a probe is expected to attach iff its driver is bound to a
    /// present device; otherwise its non-attach is silent (not a problem).
    pub fn driver_programs(mut self, items: &[(&'static str, &'static str)]) -> Self {
        for (prog, driver) in items {
            self.program_intents.insert(
                prog,
                crate::agent::sampler_status::ProbeIntent::Driver {
                    driver: (*driver).to_string(),
                },
            );
        }
        self
    }
}
