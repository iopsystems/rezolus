use super::*;
use crate::agent::*;
use crate::{error, warn};
use tracing::trace;

use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use libbpf_rs::{MapCore, MapFlags, OpenObject, PrintLevel, RingBuffer, RingBufferBuilder};
use metriken::{CounterGroup, LazyCounter, RwLockHistogram};

use crate::agent::timing::AcquisitionGroup;
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

/// Group a flat, ordered list of `(item, group)` pairs into batches of
/// same-group items, by the group's POINTER identity (not any name or
/// value equality — see [`crate::agent::timing::AcquisitionGroup`], which
/// has no `PartialEq`; two different `AcquisitionGroup` statics are always
/// distinct even if they happened to share a `name`). Two pairs land in
/// the same batch iff they name literally the same `&'static
/// AcquisitionGroup`, regardless of how far apart they are in `items` —
/// this is what lets `BpfBuilder::histogram` registrations for a
/// multi-member family (e.g. syscall_latency's 16 op-class histograms)
/// merge into one `HistogramBatch` (see `histogram.rs`) even though the
/// call sites list them as 16 separate `.histogram()` lines, not one
/// `.histograms(group, vec![...])` call.
///
/// Batch order is first-appearance order (the first pair naming a given
/// group determines where that group's batch sits in the output); within
/// a batch, member order is registration order. Pure grouping logic, no
/// I/O — generic over the item type so it's unit-testable without a real
/// BPF skeleton (see the `tests` module below); `Histogram`/`HistogramBatch`
/// construction (which DOES need a live skeleton) happens at the call site
/// in [`Builder::build`], not in here.
fn batch_by_group<T>(
    items: Vec<(T, &'static AcquisitionGroup)>,
) -> Vec<(&'static AcquisitionGroup, Vec<T>)> {
    let mut batches: Vec<(&'static AcquisitionGroup, Vec<T>)> = Vec::new();
    for (item, group) in items {
        match batches
            .iter_mut()
            .find(|(batch_group, _)| std::ptr::eq(*batch_group, group))
        {
            Some((_, members)) => members.push(item),
            None => batches.push((group, vec![item])),
        }
    }
    batches
}

#[cfg(test)]
mod batch_by_group_tests {
    use super::*;

    static GROUP_A: AcquisitionGroup = AcquisitionGroup::new("batch_test", "a");
    static GROUP_B: AcquisitionGroup = AcquisitionGroup::new("batch_test", "b");

    /// Non-consecutive registrations naming the SAME group (by pointer)
    /// merge into one batch, preserving member order — the exact shape
    /// `syscall_latency`'s 16 `.histogram()` calls rely on: every call
    /// names `&LATENCIES_ACQ`, and they must all land in one batch stamped
    /// once, not 16.
    #[test]
    fn non_consecutive_same_group_registrations_merge_preserving_order() {
        let items = vec![
            ("one", &GROUP_A),
            ("two", &GROUP_B),
            ("three", &GROUP_A),
            ("four", &GROUP_A),
        ];

        let batches = batch_by_group(items);

        assert_eq!(batches.len(), 2, "two distinct groups, two batches");

        let (group, members) = &batches[0];
        assert!(std::ptr::eq(*group, &GROUP_A), "first batch is GROUP_A");
        assert_eq!(
            members,
            &["one", "three", "four"],
            "GROUP_A's members merge in registration order, even though \
             GROUP_B's registration split them"
        );

        let (group, members) = &batches[1];
        assert!(std::ptr::eq(*group, &GROUP_B), "second batch is GROUP_B");
        assert_eq!(members, &["two"]);
    }

    /// Distinct groups never merge, even a single-member "family of one"
    /// (e.g. tcp_packet_latency's lone histogram) — each keeps its own
    /// batch.
    #[test]
    fn distinct_groups_do_not_merge() {
        let items = vec![("a", &GROUP_A), ("b", &GROUP_B)];

        let batches = batch_by_group(items);

        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].1, &["a"]);
        assert_eq!(batches[1].1, &["b"]);
    }

    /// A single group's registrations, even just one, all land in exactly
    /// one batch — the degenerate "family of one" case every
    /// single-histogram sampler (tcp_packet_latency, tcp_connect_latency)
    /// hits.
    #[test]
    fn single_group_single_item_is_one_batch_of_one() {
        let items = vec![("only", &GROUP_A)];

        let batches = batch_by_group(items);

        assert_eq!(batches.len(), 1);
        assert!(std::ptr::eq(batches[0].0, &GROUP_A));
        assert_eq!(batches[0].1, &["only"]);
    }
}

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
    /// `(time_enabled, time_running)` from the previous read, used to notice a
    /// counter that stops advancing while still enabled.
    prev: Option<(u64, u64)>,
    /// Whether we have already reported this counter as not measuring, so the
    /// condition is announced once rather than on every refresh.
    reported: bool,
}

/// What a single perf counter read means.
///
/// The counters are opened requesting `TOTAL_TIME_ENABLED` and
/// `TOTAL_TIME_RUNNING`; this is the rule for turning those into a decision.
/// Kept as a pure function because the interesting inputs -- a PMU that is
/// advertised but does not count, an event that dies mid-flight -- cannot be
/// produced on demand on real hardware, but are trivial to express as values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfRead {
    /// The event was never placed on the PMU since it was enabled. A `pinned`
    /// event that cannot be scheduled goes to `PERF_EVENT_STATE_ERROR` and
    /// reads a frozen value forever, so publishing it would present "no free
    /// PMU counter" as a measurement of zero.
    NeverRan,
    /// The event was placed at some point but has stopped advancing while still
    /// enabled -- it died after having worked. The value is still a true count
    /// of what was observed, so it is published, but the condition is worth
    /// surfacing because the series has silently gone flat.
    Stalled(u64),
    /// A live measurement.
    Live(u64),
}

/// Classify one perf counter read. Pure; see [`PerfRead`].
///
/// Note these are *lifetime* totals, not per-interval deltas, and the value is
/// published as an absolute cumulative counter. That is precisely why no
/// multiplex scaling is applied here: the correction factor would be the
/// lifetime ratio `time_enabled / time_running`, so an event that froze would
/// have a numerator that keeps growing and a denominator that does not, and the
/// published value would climb forever off a dead counter -- fabricating a
/// steady rate out of nothing. Scaling is right for a bounded `perf stat`
/// window; it is wrong for a monotonic published counter.
pub fn classify_perf_read(
    count: u64,
    time_enabled: u64,
    time_running: u64,
    prev: Option<(u64, u64)>,
) -> PerfRead {
    if time_running == 0 {
        return PerfRead::NeverRan;
    }

    if let Some((prev_enabled, prev_running)) = prev {
        if time_running == prev_running && time_enabled > prev_enabled {
            return PerfRead::Stalled(count);
        }
    }

    PerfRead::Live(count)
}

pub struct CpuPerfCounters {
    cpu: usize,
    name: &'static str,
    counters: Vec<PerfCounter>,
}

impl CpuPerfCounters {
    pub fn new(cpu: usize, name: &'static str) -> Self {
        Self {
            cpu,
            name,
            counters: Vec::new(),
        }
    }

    pub fn push(
        &mut self,
        counter: perf_event::Counter,
        group: &'static CounterGroup,
    ) -> &mut Self {
        self.counters.push(PerfCounter {
            counter,
            group,
            prev: None,
            reported: false,
        });

        self
    }

    pub fn refresh(&mut self) {
        for c in self.counters.iter_mut() {
            // Read the scheduling info the counter was built to report, rather
            // than discarding it. Without it, a counter that never got a PMU
            // slot is indistinguishable from one that legitimately measured
            // zero.
            let Ok(cat) = c.counter.read_count_and_time() else {
                continue;
            };

            let verdict = classify_perf_read(cat.count, cat.time_enabled, cat.time_running, c.prev);
            c.prev = Some((cat.time_enabled, cat.time_running));

            match verdict {
                PerfRead::NeverRan => {
                    if !c.reported {
                        c.reported = true;
                        crate::agent::sampler_status::note_perf_unavailable(
                            self.name,
                            "hardware counters were never scheduled onto the PMU \
                             (time_running = 0); no free counter, or no usable PMU",
                        );
                    }
                }
                PerfRead::Stalled(value) => {
                    if !c.reported {
                        c.reported = true;
                        crate::agent::sampler_status::note_perf_unavailable(
                            self.name,
                            "hardware counters stopped advancing while still enabled; \
                             the counter was descheduled and its series has gone flat",
                        );
                    }

                    let _ = c.group.set(self.cpu, value);
                }
                PerfRead::Live(value) => {
                    let _ = c.group.set(self.cpu, value);
                }
            }
        }
    }
}

pub struct PerfCounters {
    name: &'static str,
    inner: HashMap<usize, CpuPerfCounters>,
}

impl PerfCounters {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            inner: HashMap::new(),
        }
    }

    pub fn push(&mut self, cpu: usize, counter: perf_event::Counter, group: &'static CounterGroup) {
        let name = self.name;
        let counters = self
            .inner
            .entry(cpu)
            .or_insert(CpuPerfCounters::new(cpu, name));
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
    counters: Vec<(
        &'static str,
        Vec<&'static LazyCounter>,
        &'static AcquisitionGroup,
    )>,
    histograms: Vec<(
        &'static str,
        &'static RwLockHistogram,
        &'static AcquisitionGroup,
    )>,
    maps: Vec<(&'static str, Vec<u64>)>,
    cpu_counters: Vec<(
        &'static str,
        Vec<&'static CounterGroup>,
        &'static AcquisitionGroup,
    )>,
    perf_events: Vec<(&'static str, PerfEvent, &'static CounterGroup)>,
    /// The single [`AcquisitionGroup`] shared by every `.perf_event()` call
    /// in this builder, if any were made. See `perf_event`'s doc comment.
    perf_group: Option<&'static AcquisitionGroup>,
    packed_counters: Vec<(
        &'static str,
        &'static CounterGroup,
        &'static AcquisitionGroup,
    )>,
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
            perf_group: None,
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
                .map(|(name, counters, group)| Counters::new(skel.map(name), counters, group))
                .collect();

            // Batch histogram registrations by their declared group's
            // pointer identity (NOT registration order or map name): every
            // `.histogram()` call that named the SAME `&'static
            // AcquisitionGroup` lands in one `HistogramBatch`, so a
            // multi-member family (e.g. syscall_latency's 16 op-class
            // histograms, all registered against one shared group) is read
            // — and its window stamped — as a single sweep. Calls naming
            // different groups, including a single-member family's own
            // group, each get their own one-member batch. See
            // `HistogramBatch`'s doc comment for the granularity rule this
            // implements, and `batch_by_group`'s doc comment / tests for
            // the pure grouping logic itself.
            let mut histogram_batches: Vec<HistogramBatch> = batch_by_group(
                self.histograms
                    .into_iter()
                    .map(|(name, histogram, group)| {
                        (Histogram::new(skel.map(name), histogram), group)
                    })
                    .collect(),
            )
            .into_iter()
            .map(|(group, histograms)| HistogramBatch::new(group, histograms))
            .collect();

            let mut cpu_counters: Vec<CpuCounters> = self
                .cpu_counters
                .into_iter()
                .map(|(name, counters, group)| CpuCounters::new(skel.map(name), counters, group))
                .collect();

            debug!(
                "{} initializing perf counters for: {} events",
                self.name,
                self.perf_events.len()
            );

            let mut perf_counters = PerfCounters::new(self.name);

            // One perf event GROUP per CPU, not one independent event per
            // (event, CPU).
            //
            // The kernel schedules a group all-or-nothing: either every member
            // gets a counter or none do. Opened independently, a sampler whose
            // events outnumber the free counters gets some of them placed and
            // the rest pinned-but-never-scheduled, reading a frozen value
            // forever. For `cpu_perf` that means `cycles` counts and
            // `instructions` does not, and the IPC computed downstream is
            // fabricated rather than missing — a wrong answer where a group
            // gives no answer. Half a sampler is worse than none.
            //
            // This mirrors what `cpu_branch`, `cpu_l3`, `cpu_dtlb` and
            // `cpu_frequency` already do; those open a leader and attach the
            // rest with `build_with_group`. The BPF-builder path was the one
            // that did not. `ReadFormat::GROUP` below reads as if it did — it
            // only sets the read buffer's layout, and with no leader every
            // event was its own singleton group.
            //
            // Only the leader carries `pinned`/`read_format`: they are group
            // properties, applied to the whole group through it.
            let maps: Vec<&libbpf_rs::Map<'_>> = self
                .perf_events
                .iter()
                .map(|(name, _, _)| skel.map(name))
                .collect();

            for cpu in 0..cpus {
                let mut leader: Option<perf_event::Counter> = None;
                let mut members: Vec<perf_event::Counter> = Vec::new();
                let mut complete = true;

                for (_, event, _) in self.perf_events.iter() {
                    // Bound as an owned local: the setters take `&mut self` and
                    // return `&mut Self`, so chaining straight off `builder()`
                    // would borrow a temporary that dies at the end of the
                    // statement.
                    let mut builder = event.inner.builder();
                    builder
                        .one_cpu(cpu)
                        .any_pid()
                        .exclude_hv(false)
                        .exclude_kernel(false);
                    if leader.is_none() {
                        builder.pinned(true).read_format(
                            ReadFormat::TOTAL_TIME_ENABLED
                                | ReadFormat::TOTAL_TIME_RUNNING
                                | ReadFormat::GROUP,
                        );
                    }

                    let built = match leader.as_mut() {
                        Some(leader) => builder.build_with_group(leader),
                        None => builder.build(),
                    };

                    match built {
                        Ok(counter) => {
                            if leader.is_none() {
                                leader = Some(counter);
                            } else {
                                members.push(counter);
                            }
                        }
                        Err(e) => {
                            debug!(
                                "{}: could not open the full perf event group on CPU{cpu}, \
                                 taking none of it: {e}",
                                self.name
                            );
                            complete = false;
                            break;
                        }
                    }
                }

                // Anything short of the whole group is dropped rather than
                // published half-measured; the counters close with the Vec.
                let Some(mut leader) = leader.filter(|_| complete) else {
                    continue;
                };

                let _ = leader.enable_group();

                for (i, counter) in std::iter::once(leader).chain(members).enumerate() {
                    let fd = counter.as_raw_fd();

                    let _ = maps[i].update(
                        &((cpu as u32).to_ne_bytes()),
                        &(fd.to_ne_bytes()),
                        MapFlags::ANY,
                    );

                    perf_counters.push(cpu, counter, self.perf_events[i].2);
                }
            }

            // Boot-fixed population bound, same rationale as
            // `CpuCounters::new` (principle 18): the real number of per-CPU
            // slots this sweep will ever populate, not each member
            // `CounterGroup`'s `MAX_CPUS`-sized backing array.
            if let Some(group) = self.perf_group {
                group.set_member_bound(possible_cpus());
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
                .map(|(name, counters, group)| PackedCounters::new(skel.map(name), counters, group))
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

                for v in &mut histogram_batches {
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
            perf_group: self.perf_group,
        })
    }

    /// Register a set of counters for this BPF sampler. The `name` is the BPF
    /// map name and the `counters` are a set of userspace lazy counters which
    /// must match the ordering used in the BPF map. `group` is the declared
    /// [`AcquisitionGroup`] whose acquisition brackets this map's refresh
    /// (single writer: the group must not be shared with any other read
    /// section). See `Counters` for more details on the assumptions and
    /// requirements.
    pub fn counters(
        mut self,
        name: &'static str,
        counters: Vec<&'static LazyCounter>,
        group: &'static AcquisitionGroup,
    ) -> Self {
        self.counters.push((name, counters, group));
        self
    }

    /// Register a histogram for this BPF sampler. The `name` is the BPF map
    /// name and the `histogram` is the userspace histogram. The histogram
    /// parameters used in both the BPF and userpsace histograms must match
    /// exactly. `group` is the declared [`AcquisitionGroup`] whose
    /// acquisition brackets this map's refresh.
    ///
    /// Unlike `counters`/`cpu_counters`, `group` here is NOT required to be
    /// unique per call: registering several histograms against the SAME
    /// group is how a multi-member metric family (e.g. syscall_latency's 16
    /// op-class latency histograms) shares one read section instead of
    /// getting one each — `build()` batches every `.histogram()` call that
    /// named the same group (by pointer identity) into one
    /// [`HistogramBatch`], stamped once per refresh. See its doc comment
    /// for the granularity rule (LIKE
    /// entities within one family share a group; DIFFERENT families get
    /// their own, even read back-to-back) and its `# Single-writer
    /// contract` section for what "not shared with any other read section"
    /// actually means once histograms can share a group: the group must
    /// still never be named by a `.counters()`/`.cpu_counters()` call, or
    /// by a histogram belonging to a conceptually different family.
    pub fn histogram(
        mut self,
        name: &'static str,
        histogram: &'static RwLockHistogram,
        group: &'static AcquisitionGroup,
    ) -> Self {
        self.histograms.push((name, histogram, group));
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
    /// individual CPU counters are tracked. `group` is the declared
    /// [`AcquisitionGroup`] whose acquisition brackets this map's refresh
    /// (single writer: the group must not be shared with any other read
    /// section). See `Counters` for more details on the details and
    /// assumptions for the BPF map.
    pub fn cpu_counters(
        mut self,
        name: &'static str,
        counters: Vec<&'static CounterGroup>,
        group: &'static AcquisitionGroup,
    ) -> Self {
        self.cpu_counters.push((name, counters, group));
        self
    }

    /// Specify a perf event array name and an associated perf event.
    /// `counters` is the per-CPU target metric; `group` is the declared
    /// [`AcquisitionGroup`] whose acquisition brackets the perf-thread
    /// sweep that reads it (see `PerfCounters`/`AsyncBpf::refresh`'s
    /// bracket). Every `.perf_event()` call in one builder is merged, by
    /// the underlying `PerfCounters` machinery, into ONE per-CPU sweep
    /// triggered and joined together each refresh — so `group` must name
    /// the SAME `&'static AcquisitionGroup` across every call in a
    /// builder, exactly like several `.histogram()`/`.packed_counters()`
    /// calls sharing one like-entities group (debug-asserted).
    pub fn perf_event(
        mut self,
        name: &'static str,
        event: PerfEvent,
        counters: &'static CounterGroup,
        group: &'static AcquisitionGroup,
    ) -> Self {
        self.perf_events.push((name, event, counters));

        if let Some(existing) = self.perf_group {
            // A real assert, not debug_assert: this runs once at sampler init
            // (not a hot path), and a silent mismatch in release would leave
            // the second group's window slot permanently unstamped — a
            // never-advancing window with no diagnostic.
            assert!(
                std::ptr::eq(existing, group),
                "all .perf_event() calls in one BpfBuilder must share the same \
                 AcquisitionGroup: the underlying per-CPU sweep merges them into \
                 one read section regardless of how many .perf_event() calls \
                 registered it"
            );
        } else {
            self.perf_group = Some(group);
        }

        self
    }

    /// Register a set of packed counters. The `name` is the BPF map name and
    /// the `counters` are a set of userspace dynamic counters. The BPF map is
    /// expected to be densely packed, meaning there is no padding. The order of
    /// the `counters` must exactly match the order in the BPF map.
    ///
    /// `group` is the declared [`AcquisitionGroup`] this map's values belong
    /// to. Unlike `counters`/`cpu_counters`, `PackedCounters` never calls
    /// `acquire()`/`finish()` itself — there is no `refresh()`-time read to
    /// bracket, since values are read directly from the mmap by the
    /// exposition code. `group` is marked
    /// [reader-stamped](crate::agent::timing::AcquisitionGroup::set_reader_stamped)
    /// instead, and `create`/`create_v3` bracket its window at exposition
    /// time. As with `histogram`, `group` is NOT required to be unique per
    /// call — several `.packed_counters()` calls naming the same group (the
    /// like-entities case, e.g. `cgroup_syscall`'s 16 op-class maps) share
    /// one window, not one each.
    pub fn packed_counters(
        mut self,
        name: &'static str,
        counters: &'static CounterGroup,
        group: &'static AcquisitionGroup,
    ) -> Self {
        self.packed_counters.push((name, counters, group));
        self
    }

    /// Register a set of sparse packed counters. Alias for `packed_counters`
    /// since metriken's `CounterGroup` uses sparse metadata by default —
    /// both dense (cgroup) and sparse (task) packed groups resolve their
    /// V3 declared-group membership from registration (metadata presence),
    /// not from a walked bound; see `create_v3`'s reader-stamped handling.
    pub fn sparse_packed_counters(
        self,
        name: &'static str,
        counters: &'static CounterGroup,
        group: &'static AcquisitionGroup,
    ) -> Self {
        self.packed_counters(name, counters, group)
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

#[cfg(test)]
mod perf_read_tests {
    use super::{classify_perf_read, PerfRead};

    /// A working counter on a healthy PMU: fully scheduled for the whole
    /// window. Measured shape on bare metal (Threadripper 1950X): count 3.5e9,
    /// time_enabled == time_running.
    #[test]
    fn fully_scheduled_counter_is_live() {
        assert_eq!(
            classify_perf_read(3_500_068_433, 986_862_857, 986_862_857, None),
            PerfRead::Live(3_500_068_433)
        );
    }

    /// A `pinned` event that could not be placed. Measured shape for `cycles`
    /// on both a KVM guest and bare metal: everything zero. Publishing this
    /// would present "no free PMU counter" as a real measurement of zero.
    #[test]
    fn never_scheduled_counter_is_suppressed() {
        assert_eq!(classify_perf_read(0, 0, 0, None), PerfRead::NeverRan);
    }

    /// time_running == 0 means never placed even if the event was enabled for a
    /// long time and somehow carries a non-zero count.
    #[test]
    fn enabled_but_never_running_is_suppressed() {
        assert_eq!(
            classify_perf_read(1234, 1_000_000_000, 0, None),
            PerfRead::NeverRan
        );
    }

    /// A counter that worked and then died: time_enabled keeps advancing while
    /// time_running is frozen. The count is still true, so it is published, but
    /// the condition is reported.
    #[test]
    fn counter_that_stops_advancing_is_stalled() {
        let prev = Some((1_000_000_000, 1_000_000_000));
        assert_eq!(
            classify_perf_read(42, 2_000_000_000, 1_000_000_000, prev),
            PerfRead::Stalled(42)
        );
    }

    /// Both clocks advancing is the normal case and must not be mistaken for a
    /// stall, including when the count itself does not move (a genuinely idle
    /// CPU retires nothing but the counter is still measuring).
    #[test]
    fn idle_but_scheduled_counter_is_live_not_stalled() {
        let prev = Some((1_000_000_000, 1_000_000_000));
        assert_eq!(
            classify_perf_read(7, 2_000_000_000, 2_000_000_000, prev),
            PerfRead::Live(7)
        );
    }

    /// A guest whose PMU is advertised but does not count reports itself fully
    /// scheduled while retiring 395 instructions per second. This rule cannot
    /// detect that -- it is recorded here so the limitation is explicit rather
    /// than assumed covered. Detecting it needs a busy-time reference (#1036).
    #[test]
    fn fake_vpmu_is_not_detected_by_time_alone() {
        assert_eq!(
            classify_perf_read(395, 1_027_487_719, 1_027_487_719, None),
            PerfRead::Live(395)
        );
    }
}
