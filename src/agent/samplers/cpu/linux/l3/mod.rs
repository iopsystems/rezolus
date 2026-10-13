//! This sampler is used to measure CPU L3 cache access and misses. It does this
//! by using perf events that are processor dependent:
//!
//! - AMD Zen: Uses uncore L3 PMU (type 0xb) per CCX/CCD
//! - Intel: Uses CHA (Caching Home Agent) uncore PMU per core
//!
//! This requires that we identify each L3 cache domain and the correct raw perf
//! events to use which are processor dependent.

const NAME: &str = "cpu_l3";

use crate::agent::*;

use perf_event::events::Event;
use perf_event::ReadFormat;
use tokio::sync::Mutex;
use walkdir::WalkDir;

use std::collections::HashSet;
use std::thread::JoinHandle;

mod stats;

use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = CpuL3Inner::new()?;

    Ok(Some(Box::new(CpuL3 {
        inner: inner.into(),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct CpuL3 {
    inner: Mutex<CpuL3Inner>,
}

#[async_trait]
impl Sampler for CpuL3 {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh().await;
    }
}

struct CpuL3Inner {
    perf_threads: Vec<std::thread::JoinHandle<()>>,
    perf_sync: Vec<SyncPrimitive>,
}

impl CpuL3Inner {
    pub fn new() -> anyhow::Result<Self> {
        let (perf_threads, perf_sync) = spawn_threads()?;

        // Boot-fixed population bound, same rationale as `CpuCounters::new`
        // (principle 18): the real number of per-CPU slots this sweep will
        // ever populate, not each member `CounterGroup`'s `MAX_CPUS`-sized
        // backing array.
        CPU_L3_ACQ.set_member_set(&crate::agent::bpf::present_cpus());

        Ok(Self {
            perf_threads,
            perf_sync,
        })
    }

    // Brackets the perf-thread sweep: `acquire()` before triggering the
    // thread(s), `finish()` only after `join_all` confirms every thread has
    // notified back — strictly after that thread's `set()` calls (see
    // `L3Cache::refresh()`), so the bracket spans the real read span
    // (stamp-last, principle 18). A single domain's failed perf read is
    // individually, normally fallible, not a bulk sweep failure, so there
    // is no discard path here (same ruling as `cpu_dtlb`).
    pub async fn refresh(&mut self) -> Result<(), std::io::Error> {
        for thread in self.perf_threads.iter() {
            if thread.is_finished() {
                panic!("{} perf thread exited early", NAME);
            }
        }

        let guard = CPU_L3_ACQ.acquire();

        let perf_futures: Vec<_> = self
            .perf_sync
            .iter()
            .map(|s| {
                s.trigger();
                s.wait_notify()
            })
            .collect();

        futures::future::join_all(perf_futures).await;

        guard.finish();

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

/// A struct that contains the perf counters for each L3 cache as well as the
/// list of all CPUs in that L3 domain.
struct L3Cache {
    /// perf events for this cache
    access: perf_event::Counter,
    miss: perf_event::Counter,
    /// all cores which share this cache
    shared_cores: Vec<usize>,
}

/// Why an L3 domain's counters did not open.
#[derive(Debug)]
enum OpenError {
    /// This processor has no L3 events the sampler knows how to program.
    NoEvents,
    /// `perf_event_open` or enabling the group failed.
    Perf(std::io::Error),
}

impl L3Cache {
    pub fn new(shared_cores: Vec<usize>) -> Result<Self, OpenError> {
        let cpu = *shared_cores.first().expect("empty l3 domain");

        let (access_event, miss_event) = if let Some(events) = get_events() {
            events
        } else {
            debug!("no L3 cache events available for CPU{cpu}");
            return Err(OpenError::NoEvents);
        };

        match perf_event::Builder::new(access_event)
            .one_cpu(cpu)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .pinned(true)
            .read_format(
                ReadFormat::TOTAL_TIME_ENABLED | ReadFormat::TOTAL_TIME_RUNNING | ReadFormat::GROUP,
            )
            .build()
        {
            Ok(mut access) => {
                match perf_event::Builder::new(miss_event)
                    .one_cpu(cpu)
                    .any_pid()
                    .exclude_hv(false)
                    .exclude_kernel(false)
                    .build_with_group(&mut access)
                {
                    Ok(miss) => match access.enable_group() {
                        Ok(_) => Ok(L3Cache {
                            access,
                            miss,
                            shared_cores,
                        }),
                        Err(e) => {
                            error!("failed to enable the perf group on CPU{cpu}: {e}");
                            Err(OpenError::Perf(e))
                        }
                    },
                    Err(e) => {
                        debug!("failed to create L3 miss counter on CPU{cpu}: {e}");
                        Err(OpenError::Perf(e))
                    }
                }
            }
            Err(e) => {
                debug!("failed to create L3 access counter on CPU{cpu}: {e}");
                Err(OpenError::Perf(e))
            }
        }
    }

    pub fn refresh(&mut self) {
        if let Ok(group) = self.access.read_group() {
            if let (Some(access), Some(miss)) = (group.get(&self.access), group.get(&self.miss)) {
                let access = access.value();
                let miss = miss.value();

                for cpu in &self.shared_cores {
                    let _ = CPU_L3_ACCESS.set(*cpu, access);
                    let _ = CPU_L3_MISS.set(*cpu, miss);
                }
            }
        }
    }
}

fn l3_domains() -> Result<Vec<Vec<usize>>, std::io::Error> {
    let mut l3_domains = Vec::new();
    let mut processed = HashSet::new();

    for entry in WalkDir::new("/sys/devices/system/cpu")
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let filename = path.file_name().and_then(|v| v.to_str()).unwrap_or("");

        if filename.starts_with("cpu") && filename[3..].chars().all(char::is_numeric) {
            let cache_dir = path.join("cache");

            // look for the cache where level = 3
            if let Some(l3_index) = WalkDir::new(&cache_dir)
                .into_iter()
                .filter_map(|e| e.ok())
                .find(|entry| {
                    let index_path = entry.path();
                    index_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|name| {
                            name.starts_with("index")
                                && index_path.join("level").exists()
                                && std::fs::read_to_string(index_path.join("level"))
                                    .unwrap_or_default()
                                    .trim()
                                    == "3"
                        })
                })
            {
                let shared_cpu_list = l3_index.path().join("shared_cpu_list");

                if let Ok(shared_cpu_list) = std::fs::read_to_string(&shared_cpu_list) {
                    let shared_cores = parse_cpu_list(&shared_cpu_list);

                    // avoid duplicates
                    if !processed.contains(&shared_cores) {
                        processed.insert(shared_cores.clone());
                        l3_domains.push(shared_cores);
                    }
                }
            }
        }
    }

    Ok(l3_domains)
}

fn get_l3_caches() -> anyhow::Result<Vec<L3Cache>> {
    let mut l3_domains = l3_domains()?;

    if l3_domains.is_empty() {
        // Not an error: plenty of CPUs have no L3 at all, and reporting that as
        // a failure makes a healthy machine look broken (and, via `rezolus
        // status`, fail a readiness probe).
        debug!("no L3 cache domains found");
        return Err(crate::agent::sampler_status::Unsupported(
            "no L3 cache domains found".to_string(),
        )
        .into());
    }

    let mut l3_caches = Vec::new();
    let mut errors = Vec::new();

    for l3_domain in l3_domains.drain(..) {
        match L3Cache::new(l3_domain) {
            Ok(l3_cache) => l3_caches.push(l3_cache),
            Err(e) => errors.push(e),
        }
    }

    if l3_caches.is_empty() {
        debug!("failed to create perf counters for any L3 cache domain");
        return Err(classify_open_errors(&errors));
    }

    Ok(l3_caches)
}

/// The sampler's status when no L3 domain's counters opened. A guest commonly
/// sees the host's cache topology in sysfs and has no PMU, so the counters do
/// not exist. No known events for the processor, or an absent event or PMU
/// (`ENOENT`, `ENODEV`, `EOPNOTSUPP`), is unsupported, as is a permission
/// refusal, which names what to change. Any other errno (file descriptors
/// exhausted `EMFILE`, PMU held `EBUSY`, an invalid event `EINVAL`, ...) is a
/// failure.
fn classify_open_errors(errors: &[OpenError]) -> anyhow::Error {
    let errno = |e: &OpenError| match e {
        OpenError::NoEvents => None,
        OpenError::Perf(e) => e.raw_os_error(),
    };
    let unavailable = |e: &OpenError| {
        matches!(e, OpenError::NoEvents)
            || matches!(
                errno(e),
                Some(libc::ENOENT | libc::ENODEV | libc::EOPNOTSUPP)
            )
    };
    let refused = |e: &OpenError| matches!(errno(e), Some(libc::EACCES | libc::EPERM));

    if let Some(OpenError::Perf(e)) = errors.iter().find(|e| !unavailable(e) && !refused(e)) {
        return anyhow::anyhow!("failed to create L3 cache perf counters: {e}");
    }
    if errors.iter().any(refused) {
        return crate::agent::sampler_status::Unsupported(
            "L3 cache perf counters not permitted (perf_event_paranoid, CAP_PERFMON, or a seccomp profile)"
                .to_string(),
        )
        .into();
    }
    crate::agent::sampler_status::Unsupported(
        "no L3 cache perf counters for this processor (microarchitecture not supported, uncore PMU not loaded, or no PMU in this VM)"
            .to_string(),
    )
    .into()
}

fn parse_cpu_list(list: &str) -> Vec<usize> {
    let mut cores = Vec::new();

    for range in list.trim().split(',') {
        if let Some((start, end)) = range.split_once('-') {
            // Range of cores
            if let (Ok(start_num), Ok(end_num)) = (start.parse::<usize>(), end.parse::<usize>()) {
                cores.extend(start_num..=end_num);
            }
        } else {
            // Single core
            if let Ok(core) = range.parse::<usize>() {
                cores.push(core);
            }
        }
    }

    cores.sort_unstable();
    cores.dedup();
    cores
}

/// Discovers the Intel CHA uncore PMU type from sysfs.
/// Returns None if not available (not Intel or no CHA PMU).
fn get_intel_cha_pmu_type() -> Option<u32> {
    // Try to read the PMU type from the first CHA device
    let type_path = "/sys/bus/event_source/devices/uncore_cha_0/type";
    if let Ok(content) = std::fs::read_to_string(type_path) {
        if let Ok(pmu_type) = content.trim().parse::<u32>() {
            return Some(pmu_type);
        }
    }
    None
}

/// Discovers the AMD L3 uncore PMU type from sysfs.
/// Returns None if not available (not AMD or no L3 PMU).
fn get_amd_l3_pmu_type() -> Option<u32> {
    let type_path = "/sys/bus/event_source/devices/amd_l3/type";
    if let Ok(content) = std::fs::read_to_string(type_path) {
        if let Ok(pmu_type) = content.trim().parse::<u32>() {
            return Some(pmu_type);
        }
    }
    None
}

fn get_events() -> Option<(LowLevelEvent, LowLevelEvent)> {
    if let Ok(uarch) = archspec::cpu::host().map(|u| u.name().to_owned()) {
        debug!("archspec detected microarchitecture: {}", uarch);
        let events = match uarch.as_str() {
            // AMD Zen family - uses L3 uncore PMU (dynamic type discovery)
            // Event 0xFF04 = L3 cache access (all request types)
            // Event 0x0104 = L3 cache miss
            "zen" | "zen2" | "zen3" | "zen4" | "zen5" => {
                if let Some(l3_type) = get_amd_l3_pmu_type() {
                    (
                        LowLevelEvent::new(l3_type, 0xFF04),
                        LowLevelEvent::new(l3_type, 0x0104),
                    )
                } else {
                    debug!("AMD L3 PMU not available");
                    return None;
                }
            }

            // Intel server CPUs - uses CHA uncore PMU (dynamic type discovery)
            // LLC_LOOKUP: Event 0x34, Umask 0x0F (any lookup, all MESI states)
            // TOR_INSERTS.IA_MISS: Event 0x35, Umask 0x21 (misses from cores)
            // Config format: (umask << 8) | event_code
            "skylake_avx512" | "cascadelake" | "icelake" | "sapphirerapids" | "emeraldrapids"
            | "graniterapids" => {
                if let Some(cha_type) = get_intel_cha_pmu_type() {
                    let llc_lookup_config = (0x0F << 8) | 0x34; // LLC_LOOKUP.ANY
                    let tor_miss_config = (0x21 << 8) | 0x35; // TOR_INSERTS.IA_MISS
                    (
                        LowLevelEvent::new(cha_type, llc_lookup_config),
                        LowLevelEvent::new(cha_type, tor_miss_config),
                    )
                } else {
                    debug!("Intel CHA PMU not available");
                    return None;
                }
            }

            // ARM Graviton / Neoverse - L3 cache metrics not supported
            // The L3D_CACHE and L3D_CACHE_REFILL events are per-core, not per-L3-domain.
            // Sampling one core and attributing to all cores in the domain is misleading.
            // Proper support would require CMN (Coherent Mesh Network) PMU which has
            // limited availability on cloud instances.
            "neoverse_n1" | "neoverse_v1" | "neoverse_n2" | "neoverse_v2" | "graviton2"
            | "graviton3" | "graviton4" | "cortex_a72" => {
                debug!("ARM L3 cache metrics not supported: per-core events cannot provide accurate L3 domain metrics");
                return None;
            }

            _ => {
                debug!("unsupported microarchitecture for L3 cache metrics: {uarch}");
                return None;
            }
        };

        Some(events)
    } else {
        debug!("failed to detect CPU microarchitecture for L3 cache metrics");
        None
    }
}

fn spawn_threads() -> anyhow::Result<(Vec<JoinHandle<()>>, Vec<SyncPrimitive>)> {
    // on virtualized environments, it is typically better to use multiple
    // threads to read the perf counters to get more consistent snapshot latency
    if is_virt() {
        spawn_threads_multi()
    } else {
        spawn_threads_single()
    }
}

fn spawn_threads_single() -> anyhow::Result<(Vec<JoinHandle<()>>, Vec<SyncPrimitive>)> {
    debug!("using single-threaded perf counter collection");

    let mut caches = get_l3_caches()?;

    let mut perf_threads = Vec::new();
    let mut perf_sync = Vec::new();

    let psync = SyncPrimitive::new();
    let psync2 = psync.clone();

    perf_threads.push(std::thread::spawn(move || loop {
        psync.wait_trigger();

        for cache in caches.iter_mut() {
            cache.refresh();
        }

        psync.notify();
    }));

    perf_sync.push(psync2);

    Ok((perf_threads, perf_sync))
}

fn spawn_threads_multi() -> anyhow::Result<(Vec<JoinHandle<()>>, Vec<SyncPrimitive>)> {
    debug!("using multi-threaded perf counter collection");

    let mut caches = get_l3_caches()?;

    let pt_pending = Arc::new(AtomicUsize::new(caches.len()));

    let mut perf_threads = Vec::new();
    let mut perf_sync = Vec::new();

    for mut cache in caches.drain(..) {
        let psync = SyncPrimitive::new();
        let psync2 = psync.clone();

        let pt_pending = pt_pending.clone();

        perf_threads.push(std::thread::spawn(move || {
            if !core_affinity::set_for_current(core_affinity::CoreId {
                id: cache.shared_cores[0],
            }) {
                warn!(
                    "failed to pin perf thread for core: {}",
                    cache.shared_cores[0]
                );
            }

            pt_pending.fetch_sub(1, Ordering::Relaxed);

            loop {
                psync.wait_trigger();

                cache.refresh();

                psync.notify();
            }
        }));

        perf_sync.push(psync2);
    }

    debug!("{} waiting for perf threads to launch", NAME);

    while pt_pending.load(Ordering::Relaxed) > 0 {
        std::thread::sleep(Duration::from_millis(50));
    }

    debug!("all perf threads launched");

    Ok((perf_threads, perf_sync))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sampler_status::Unsupported;

    fn perf(errno: i32) -> OpenError {
        OpenError::Perf(std::io::Error::from_raw_os_error(errno))
    }

    fn unsupported(errors: &[OpenError]) -> Option<String> {
        classify_open_errors(errors)
            .downcast_ref::<Unsupported>()
            .map(|u| u.0.clone())
    }

    #[test]
    fn a_missing_pmu_or_event_is_unsupported() {
        for errors in [
            vec![perf(libc::ENOENT)],
            vec![perf(libc::ENODEV), perf(libc::EOPNOTSUPP)],
            vec![OpenError::NoEvents],
        ] {
            let reason = unsupported(&errors).expect("unsupported");
            assert!(reason.contains("no L3 cache perf counters"), "{reason}");
        }
    }

    #[test]
    fn a_permission_refusal_is_unsupported_and_names_the_setting() {
        let reason = unsupported(&[perf(libc::ENOENT), perf(libc::EACCES)]).expect("unsupported");
        assert!(reason.contains("perf_event_paranoid"), "{reason}");
    }

    #[test]
    fn other_errnos_are_a_failure() {
        for errno in [libc::EMFILE, libc::ENOSPC, libc::EBUSY, libc::EINVAL] {
            let error = classify_open_errors(&[perf(libc::ENOENT), perf(errno)]);
            assert!(error.downcast_ref::<Unsupported>().is_none(), "{error}");
            assert!(
                error
                    .to_string()
                    .starts_with("failed to create L3 cache perf counters: "),
                "{error}"
            );
        }
    }
}
