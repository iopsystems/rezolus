//! Collects CPU DTLB (Data Translation Lookaside Buffer) miss counters:
//!
//! * `cpu_dtlb_miss` - DTLB misses (AMD/ARM combined)
//! * `cpu_dtlb_miss{op="load"}` - DTLB load misses (Intel only)
//! * `cpu_dtlb_miss{op="store"}` - DTLB store misses (Intel only)
//!
//! High DTLB miss rates indicate memory access patterns with poor locality
//! or working sets larger than the TLB can cache. Consider using huge pages
//! to reduce TLB pressure.
//!
//! Event codes are processor-specific:
//! - Intel: DTLB_LOAD_MISSES (0x08), DTLB_STORE_MISSES (0x49)
//! - AMD: L1_DTLB_MISS (0x45)
//! - ARM: L1D_TLB_REFILL (0x05)

const NAME: &str = "cpu_dtlb";

use crate::agent::*;

use perf_event::events::Event;
use perf_event::ReadFormat;
use tokio::sync::Mutex;
use walkdir::WalkDir;

use std::collections::BTreeSet;
use std::thread::JoinHandle;

mod stats;

use stats::*;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = DtlbInner::new()?;

    Ok(Some(Box::new(Dtlb {
        inner: inner.into(),
    })))
}

struct Dtlb {
    inner: Mutex<DtlbInner>,
}

#[async_trait]
impl Sampler for Dtlb {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh().await;
    }
}

struct DtlbInner {
    perf_threads: Vec<std::thread::JoinHandle<()>>,
    perf_sync: Vec<SyncPrimitive>,
}

impl DtlbInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let (perf_threads, perf_sync) = spawn_threads()?;

        Ok(Self {
            perf_threads,
            perf_sync,
        })
    }

    pub async fn refresh(&mut self) -> Result<(), std::io::Error> {
        // check that no perf threads have exited
        for thread in self.perf_threads.iter() {
            if thread.is_finished() {
                panic!("{} perf thread exited early", NAME);
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

        Ok(())
    }
}

pub struct LowLevelEvent {
    event_type: u32,
    config: u64,
}

impl LowLevelEvent {
    pub fn new(event_type: u32, config: u64) -> Self {
        Self { event_type, config }
    }
}

impl Event for LowLevelEvent {
    fn update_attrs(self, attr: &mut perf_event_open_sys::bindings::perf_event_attr) {
        attr.type_ = self.event_type;
        attr.config = self.config;
    }
}

/// A struct that represents a CPU core and contains the DTLB counters
struct Core {
    /// perf events for this core
    load_miss: perf_event::Counter,
    store_miss: Option<perf_event::Counter>,
    /// the core id
    id: usize,
}

impl Core {
    pub fn new(id: usize) -> Result<Self, ()> {
        let (load_event, store_event) = if let Some(events) = get_events() {
            events
        } else {
            return Err(());
        };

        match perf_event::Builder::new(load_event)
            .one_cpu(id)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .pinned(true)
            .read_format(
                ReadFormat::TOTAL_TIME_ENABLED | ReadFormat::TOTAL_TIME_RUNNING | ReadFormat::GROUP,
            )
            .build()
        {
            Ok(mut load_miss) => {
                // Store event is optional (ARM doesn't have a separate store event)
                let store_miss = if let Some(store_evt) = store_event {
                    match perf_event::Builder::new(store_evt)
                        .one_cpu(id)
                        .any_pid()
                        .exclude_hv(false)
                        .exclude_kernel(false)
                        .build_with_group(&mut load_miss)
                    {
                        Ok(counter) => Some(counter),
                        Err(e) => {
                            debug!("failed to enable the dtlb_store_miss counter on CPU{id}: {e}");
                            None
                        }
                    }
                } else {
                    None
                };

                match load_miss.enable_group() {
                    Ok(_) => Ok(Core {
                        load_miss,
                        store_miss,
                        id,
                    }),
                    Err(e) => {
                        error!("failed to enable the perf group on CPU{id}: {e}");
                        Err(())
                    }
                }
            }
            Err(e) => {
                debug!("failed to enable the dtlb_load_miss counter on CPU{id}: {e}");
                Err(())
            }
        }
    }

    pub fn refresh(&mut self) {
        if let Ok(group) = self.load_miss.read_group() {
            if let Some(load) = group.get(&self.load_miss) {
                let load_val = load.value();

                // Check if we have separate load/store events (Intel) or combined (AMD/ARM)
                if let Some(ref store_counter) = self.store_miss {
                    // Intel: use labeled metrics for load and store
                    let _ = CPU_DTLB_MISS_LOAD.set(self.id, load_val);

                    if let Some(store) = group.get(store_counter) {
                        let store_val = store.value();
                        let _ = CPU_DTLB_MISS_STORE.set(self.id, store_val);
                    }
                } else {
                    // AMD/ARM: use unlabeled metric for combined misses
                    let _ = CPU_DTLB_MISS.set(self.id, load_val);
                }
            }
        }
    }
}

fn get_events() -> Option<(LowLevelEvent, Option<LowLevelEvent>)> {
    const PERF_TYPE_RAW: u32 = 4;

    if let Ok(uarch) = archspec::cpu::host().map(|u| u.name().to_owned()) {
        let events = match uarch.as_str() {
            // AMD Zen family
            // L1_DTLB_MISS: Event 0x45, Umask 0xFF (all page sizes)
            // AMD doesn't have a separate store miss event, uses same counter
            "zen" | "zen2" | "zen3" | "zen4" | "zen5" => {
                let load_config = (0xFF << 8) | 0x45; // L1_DTLB_MISS all
                (
                    LowLevelEvent::new(PERF_TYPE_RAW, load_config),
                    None, // AMD uses same event for loads and stores
                )
            }

            // Intel server CPUs
            // DTLB_LOAD_MISSES.MISS_CAUSES_A_WALK: Event 0x08, Umask 0x01
            // DTLB_STORE_MISSES.MISS_CAUSES_A_WALK: Event 0x49, Umask 0x01
            "skylake" | "skylake_avx512" | "cascadelake" | "icelake" | "sapphirerapids"
            | "emeraldrapids" | "graniterapids" | "haswell" | "broadwell" => {
                let load_config = (0x01 << 8) | 0x08; // DTLB_LOAD_MISSES.MISS_CAUSES_A_WALK
                let store_config = (0x01 << 8) | 0x49; // DTLB_STORE_MISSES.MISS_CAUSES_A_WALK
                (
                    LowLevelEvent::new(PERF_TYPE_RAW, load_config),
                    Some(LowLevelEvent::new(PERF_TYPE_RAW, store_config)),
                )
            }

            // ARM Graviton / Neoverse
            // L1D_TLB_REFILL: Event 0x05 (L1 DTLB refill)
            // ARM doesn't distinguish load vs store TLB misses at this level
            "neoverse_n1" | "neoverse_v1" | "neoverse_n2" | "neoverse_v2" | "graviton2"
            | "graviton3" | "graviton4" | "cortex_a72" => (
                LowLevelEvent::new(PERF_TYPE_RAW, 0x05), // L1D_TLB_REFILL
                None,                                    // No separate store event
            ),

            _ => {
                debug!("unsupported microarchitecture for DTLB metrics: {uarch}");
                return None;
            }
        };

        Some(events)
    } else {
        None
    }
}

fn logical_cores() -> Result<Vec<usize>, std::io::Error> {
    let mut cores: BTreeSet<usize> = BTreeSet::new();

    // walk the cpu devices directory
    for entry in WalkDir::new("/sys/devices/system/cpu")
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let filename = path.file_name().and_then(|v| v.to_str()).unwrap_or("");

        // check if this is a cpu directory
        if filename.starts_with("cpu") && filename[3..].chars().all(char::is_numeric) {
            if let Ok(core_id) = filename[3..].parse() {
                cores.insert(core_id);
            }
        }
    }

    Ok(cores.iter().copied().collect())
}

fn spawn_threads() -> Result<(Vec<JoinHandle<()>>, Vec<SyncPrimitive>), std::io::Error> {
    // on virtualized environments, it is typically better to use multiple
    // threads to read the perf counters to get more consistent snapshot latency
    if is_virt() {
        spawn_threads_multi()
    } else {
        spawn_threads_single()
    }
}

fn spawn_threads_single() -> Result<(Vec<JoinHandle<()>>, Vec<SyncPrimitive>), std::io::Error> {
    debug!("using single-threaded perf counter collection");

    let mut logical_cores = logical_cores()?;

    let mut perf_threads = Vec::new();
    let mut perf_sync = Vec::new();

    let psync = SyncPrimitive::new();
    let psync2 = psync.clone();

    let mut cores = Vec::new();

    for core in logical_cores.drain(..) {
        if let Ok(core) = Core::new(core) {
            cores.push(core);
        }
    }

    if cores.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "no cores available for DTLB sampling",
        ));
    }

    perf_threads.push(std::thread::spawn(move || loop {
        psync.wait_trigger();

        for core in cores.iter_mut() {
            core.refresh();
        }

        psync.notify();
    }));

    perf_sync.push(psync2);

    Ok((perf_threads, perf_sync))
}

fn spawn_threads_multi() -> Result<(Vec<JoinHandle<()>>, Vec<SyncPrimitive>), std::io::Error> {
    debug!("using multi-threaded perf counter collection");

    let mut logical_cores = logical_cores()?;

    let pt_pending = Arc::new(AtomicUsize::new(logical_cores.len()));

    let mut perf_threads = Vec::new();
    let mut perf_sync = Vec::new();

    for core in logical_cores.drain(..) {
        if let Ok(mut core) = Core::new(core) {
            let psync = SyncPrimitive::new();
            let psync2 = psync.clone();

            let pt_pending = pt_pending.clone();

            perf_threads.push(std::thread::spawn(move || {
                if !core_affinity::set_for_current(core_affinity::CoreId { id: core.id }) {
                    warn!("failed to pin perf thread for core: {}", core.id);
                }

                pt_pending.fetch_sub(1, Ordering::Relaxed);

                loop {
                    psync.wait_trigger();

                    core.refresh();

                    psync.notify();
                }
            }));

            perf_sync.push(psync2);
        } else {
            pt_pending.fetch_sub(1, Ordering::Relaxed);
        }
    }

    if perf_threads.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "no cores available for DTLB sampling",
        ));
    }

    debug!("{} waiting for perf threads to launch", NAME);

    while pt_pending.load(Ordering::Relaxed) > 0 {
        std::thread::sleep(Duration::from_millis(50));
    }

    debug!("all perf threads launched");

    Ok((perf_threads, perf_sync))
}
