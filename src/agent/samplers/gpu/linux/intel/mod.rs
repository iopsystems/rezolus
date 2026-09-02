//! Collects Intel GPU telemetry from the i915/xe PMU via `perf_event_open(2)`.
//!
//! This covers both integrated GPUs and discrete cards (Arc), which register
//! separate PMUs under `/sys/bus/event_source/devices/` (see [`pmu`]). It is the
//! same data source `intel_gpu_top` uses: that tool performs no GPU-specific
//! magic, it is an ordinary perf client that reads cumulative busy-nanosecond
//! counters and differentiates them in userspace. `perf stat` on the same events
//! produces identical numbers.
//!
//! ## What is collected
//!
//! From the PMU: per-engine (`rcs`/`bcs`/`vcs`/`vecs`/`ccs`) busy nanoseconds,
//! and per-GPU actual/requested frequency. The engine set is **discovered from
//! sysfs**, not assumed — an integrated GPU typically exposes no compute engine,
//! while a discrete Arc card does, and engine instances are renumbered densely
//! by the driver.
//!
//! Every PMU value is monotonically cumulative, so those metrics are counters
//! and the viewer derives utilization/frequency as rates (see [`stats`]).
//!
//! Alongside them, VRAM usage comes from the DRM query ioctl — the PMU has no
//! memory counter — and is a gauge in bytes (see [`drm_memory`]).
//!
//! ## Reading model
//!
//! All events for one GPU are opened as a single **perf group** (the first event
//! is the group leader, the rest join it), so one `read(2)` returns every counter
//! sampled atomically with no skew between engines. The i915 PMU is a system-wide
//! PMU with `cpumask=0`, so the events are opened on CPU 0 with `any_pid`.
//!
//! No GPU work is submitted and no kernel state is perturbed by a read.
//!
//! ## Cadence — a documented departure from principle 10
//!
//! Reading this PMU is **not** an mmap load. A grouped `read(2)` on an i915 perf
//! fd takes a driver lock and samples each event: measured at 6.4 us for a single
//! event, 12.1 us for a 26-event group, plus ~5.4 us for the VRAM ioctl. Letting
//! consumers drive that at the snapshot-TTL rate (up to ~100 Hz) would burn CPU
//! and hammer the driver for values that move on the order of seconds.
//!
//! So this sampler reads on its **own bounded cadence**
//! (`[samplers.gpu_intel_pmu] interval`, default 1s) and serves the cached values
//! in between, with the read dispatched via `spawn_blocking` so a slow driver
//! read cannot stall the async runtime (principle 17). `refresh()` itself only
//! does a time comparison. The counters are cumulative, so a 1s cadence costs
//! `rate()` nothing.
//!
//! ## Windows
//!
//! The per-GPU sweep is one read section (principle 18): the grouped perf read
//! and the VRAM ioctl for a device are reached together through that device's
//! own fds. It is bracketed by two acquisition groups rather than one, because
//! the metrics it fills live in two index spaces — see the groups' doc comment
//! in [`stats`] for why one shared group published phantom series.
//!
//! ## Permissions
//!
//! Opening these counters needs `CAP_PERFMON` (or `kernel.perf_event_paranoid`
//! <= 2, though hosts commonly ship with a stricter value such as 4). Without
//! it the sampler disables itself at init rather than failing the agent.
//!
//! ## Limits
//!
//! The PMU reports engine **occupancy, not efficiency**: a busy engine had work
//! queued, which does not mean the EUs were saturated. Pair busy time with
//! frequency to tell "busy but downclocked" from genuinely saturated. EU-level
//! detail requires the separate i915 perf/OA interface (`DRM_IOCTL_I915_PERF_OPEN`),
//! a different subsystem despite the shared word "perf".
//!
//! There is no VRAM bandwidth counter in the i915 PMU. `intel_gpu_top` shows
//! `uncore_imc` bandwidth, but that is the CPU package's memory controller —
//! host DRAM traffic, not the card's GDDR6 — so it is not attributed to the GPU
//! here.

const NAME: &str = "gpu_intel_pmu";

use crate::agent::*;

use perf_event::events::Event;
use perf_event::ReadFormat;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

mod drm_memory;
mod pmu;
mod stats;

use drm_memory::DrmDevice;
use pmu::GpuPmu;
use stats::*;

/// Built-in read cadence when `[samplers.gpu_intel_pmu] interval` is unset.
///
/// One second matches `intel_gpu_top`'s own default sampling period
/// (`DEFAULT_PERIOD_MS`) and is far finer than the timescale these counters move
/// on, while keeping the driver read rate off the scrape cadence.
const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(1);

/// How early a scrape may arrive and still be served with a fresh read.
///
/// Scrape timers jitter by well under a millisecond in practice; 50ms is far
/// above that and still two orders of magnitude below the default interval, so
/// it cannot meaningfully raise the driver read rate. See the note in
/// `refresh` for the aliasing this prevents.
const SCRAPE_TOLERANCE: Duration = Duration::from_millis(50);

/// Maximum number of Intel GPUs tracked. Hosts with more are truncated (logged
/// once at init).
pub(crate) const MAX_GPUS: usize = 8;

/// Maximum engines tracked per GPU. A discrete Arc exposes 7 (rcs0, bcs0, vcs0,
/// vcs1, vecs0, vecs1, ccs0); this leaves headroom for parts with more compute
/// or video engines.
pub(crate) const MAX_ENGINES: usize = 16;

/// Entry count for the per-engine metric groups, indexed by [`engine_index`].
pub(crate) const ENGINE_ENTRIES: usize = MAX_GPUS * MAX_ENGINES;

/// Index of one GPU's engine within the per-engine metric groups.
///
/// Each GPU owns a `MAX_ENGINES`-wide block, so engine indices never collide
/// across GPUs and a GPU's engines stay contiguous. The blocks are sparse: a
/// GPU exposing 4 engines leaves the rest of its block unused, which is why the
/// acquisition group declares an explicit member set rather than a prefix bound.
fn engine_index(gpu: usize, engine: usize) -> usize {
    gpu * MAX_ENGINES + engine
}

/// The per-engine sample types collected, as sysfs name suffixes.
///
/// The i915 PMU also exposes `-wait` and `-sema` (stalled on `MI_WAIT_FOR_EVENT`
/// and blocked on a cross-engine semaphore respectively). Both are deliberately
/// not collected: they describe *why* an engine made no progress, which is a
/// debugging question, whereas this sampler publishes the occupancy and clock
/// needed to answer "how loaded is this GPU". Cumulative nanoseconds.
const ENGINE_SAMPLES: [(&str, &metriken::CounterGroup, Option<&str>); 1] =
    [("busy", &GPU_ENGINE_BUSY, Some("ns"))];

/// The per-GPU global events, as (sysfs name, metric, expected sysfs unit).
///
/// The expected unit is checked against sysfs at init: the metric names and
/// descriptions encode a unit, so a kernel that reported something else would
/// silently make the exported series wrong.
/// The PMU also exposes `rc6-residency`, `software-gt-awake-time` and
/// `interrupts`; they are power-state and IRQ accounting rather than load, and
/// are not collected.
const GLOBAL_EVENTS: [(&str, &metriken::CounterGroup, Option<&str>); 2] = [
    ("actual-frequency", &GPU_FREQUENCY_ACTUAL, Some("M")),
    ("requested-frequency", &GPU_FREQUENCY_REQUESTED, Some("M")),
];

fn init(config: Arc<Config>) -> SamplerResult {
    // Zero FIRST, so every exit below leaves the group empty rather than at
    // backing capacity. An unset bound falls back to this sampler's backing
    // arrays (`ENGINE_ENTRIES` = 128), and the snapshot walk would then declare
    // that many members — phantom, all-null, and indistinguishable to a reader
    // from a real engine that simply had no reading. `Gpu::new` raises it to
    // the real engine set on success.
    GPU_INTEL_PMU_ENGINE_ACQ.set_member_bound(0);
    GPU_INTEL_PMU_DEVICE_ACQ.set_member_bound(0);

    if !config.enabled(NAME) {
        return Ok(None);
    }

    let mut pmus = pmu::discover();

    // No Intel GPU on this machine. Not a fault and not a config choice: the
    // capability is absent, so it is reported as unsupported rather than
    // disappearing into `Disabled` (which the health tally does not count).
    if pmus.is_empty() {
        debug!("{NAME}: no Intel GPU PMUs found");
        return Err(crate::agent::sampler_status::Unsupported(
            "no Intel GPU i915/xe PMU found".to_string(),
        )
        .into());
    }

    if pmus.len() > MAX_GPUS {
        warn!(
            "{NAME}: found {} Intel GPUs, only the first {MAX_GPUS} are recorded",
            pmus.len()
        );
        pmus.truncate(MAX_GPUS);
    }

    let mut gpus = Vec::new();

    for (id, pmu) in pmus.iter().enumerate() {
        match Gpu::new(id, pmu) {
            Ok(gpu) => {
                debug!(
                    "{NAME}: GPU {id} ({}, pmu {}, driver {}, perf type {}): {} engine(s) {:?}, \
                     global counters {:?}",
                    pmu.device_label(),
                    pmu.name,
                    pmu.driver,
                    pmu.perf_type,
                    gpu.engines.len(),
                    gpu.engines,
                    gpu.globals,
                );
                gpus.push(gpu);
            }
            Err(e) => {
                // A permission failure is the common case on hosts with a
                // restrictive perf_event_paranoid; it is not fatal.
                debug!("{NAME}: GPU {id} ({}) unavailable: {e}", pmu.device_label());
            }
        }
    }

    // The GPUs are here but none of their counters would open — almost always
    // a restrictive `perf_event_paranoid`. The machine cannot support this as
    // configured, so it is unsupported rather than disabled; the reason names
    // the fix.
    if gpus.is_empty() {
        return Err(crate::agent::sampler_status::Unsupported(
            "Intel GPU present but no PMU counters could be opened (needs CAP_PERFMON \
             or kernel.perf_event_paranoid <= 2)"
                .to_string(),
        )
        .into());
    }

    // Real membership, not backing capacity, and one set per index space (see
    // the groups' doc comment in `stats`).
    //
    // Engine entries are sparse: each GPU owns a `MAX_ENGINES`-wide block and
    // uses only the leading part of it, so a prefix bound would declare the
    // gaps as members. The explicit set does not.
    let mut engine_members: Vec<usize> = Vec::new();
    for gpu in gpus.iter() {
        for offset in 0..gpu.engines.len() {
            engine_members.push(engine_index(gpu.id, offset));
        }
    }
    engine_members.sort_unstable();
    GPU_INTEL_PMU_ENGINE_ACQ.set_member_set(&engine_members);

    // Per-GPU entries are indexed by GPU id, which `init` assigns densely from
    // 0, so a prefix bound is exactly right here.
    GPU_INTEL_PMU_DEVICE_ACQ.set_member_bound(gpus.len());

    let interval = config
        .sampler_interval(NAME)
        .unwrap_or(DEFAULT_READ_INTERVAL);

    debug!("{NAME}: reading every {interval:?}");

    Ok(Some(Box::new(IntelPmu {
        gpus: Arc::new(std::sync::Mutex::new(gpus)),
        interval,
        last_read: std::sync::Mutex::new(None),
        reading: Arc::new(AtomicBool::new(false)),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct IntelPmu {
    /// Shared with the blocking read task, which needs `&mut` on each `Gpu` to
    /// read its perf group.
    gpus: Arc<std::sync::Mutex<Vec<Gpu>>>,
    /// Minimum time between reads (principle 17).
    interval: Duration,
    /// When the last read was dispatched.
    last_read: std::sync::Mutex<Option<Instant>>,
    /// Guards against overlapping reads if one runs long.
    reading: Arc<AtomicBool>,
}

#[async_trait]
impl Sampler for IntelPmu {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        // Throttle: dispatch at most once per `interval`. This is the only work
        // done on the sample cycle; everything else happens off-worker.
        //
        // The comparison carries a tolerance, and it is load-bearing rather
        // than cosmetic. A consumer scraping at the same period as `interval`
        // — the common case, since both default to 1s — arrives each cycle a
        // few hundred microseconds early or late depending on jitter. A strict
        // `<` then rejects roughly every other scrape, and because these are
        // cumulative counters the exported series alternates between an
        // unchanged value and one that jumped two intervals' worth. Measured on
        // an Arc A770 at a steady 2400 MHz, the recorded `actual-frequency`
        // deltas read `0, 4799, 0, 4800, ...`, which differentiates to a
        // plausible-looking 4800 MHz — double the truth, with no gap to hint
        // that a sample was dropped.
        //
        // Admitting a read that is within `SCRAPE_TOLERANCE` of due costs at
        // most that much extra driver traffic and keeps one reading per scrape.
        {
            let mut last = self.last_read.lock().unwrap();
            let due = self.interval.saturating_sub(SCRAPE_TOLERANCE);
            match *last {
                Some(t) if t.elapsed() < due => return,
                _ => *last = Some(Instant::now()),
            }
        }

        // Never overlap reads.
        if self.reading.swap(true, Ordering::AcqRel) {
            return;
        }

        // Offload: every source here is a syscall against the driver (a perf
        // group read, a DRM ioctl), not an mmap load, so it must not run on the
        // async worker.
        let gpus = self.gpus.clone();
        let reading = self.reading.clone();

        tokio::task::spawn_blocking(move || {
            if let Ok(mut gpus) = gpus.lock() {
                // Acquisition-group bracket (principle 18): ONE group for the
                // whole device sweep, not one per GPU or per metric family.
                // An individual GPU's read can fail without invalidating the
                // section — that GPU keeps its stale values — so the group
                // always finishes once the sweep completes; there is no bulk
                // "read all failed" signal here that would warrant a discard.
                let engines = GPU_INTEL_PMU_ENGINE_ACQ.acquire();
                let devices = GPU_INTEL_PMU_DEVICE_ACQ.acquire();

                for gpu in gpus.iter_mut() {
                    gpu.refresh();
                }

                engines.finish();
                devices.finish();
            }
            reading.store(false, Ordering::Release);
        });
    }
}

/// A raw dynamic-PMU event: the i915 PMU type is assigned at boot, so events are
/// specified as a (type, config) pair rather than by a named perf event.
struct RawEvent {
    event_type: u32,
    config: u64,
}

impl Event for RawEvent {
    fn update_attrs(self, attr: &mut perf_event_open_sys::bindings::perf_event_attr) {
        attr.type_ = self.event_type;
        attr.config = self.config;
    }
}

/// One counter within a GPU's perf group, and where its value is published.
struct TrackedCounter {
    counter: perf_event::Counter,
    metric: &'static metriken::CounterGroup,
    /// Index into `metric` (a plain GPU id, or an [`engine_index`]).
    index: usize,
}

/// All counters for a single GPU, opened as one perf group.
struct Gpu {
    id: usize,
    label: String,
    /// The group leader, whose `read_group` returns every counter at once.
    leader: perf_event::Counter,
    /// The metric/index the leader itself publishes to.
    leader_target: (&'static metriken::CounterGroup, usize),
    /// Group members, in the order they were added.
    members: Vec<TrackedCounter>,
    /// Engine names in index order, for diagnostics.
    engines: Vec<String>,
    /// Global event names opened for this GPU, for diagnostics.
    globals: Vec<String>,
    /// DRM render node for VRAM accounting. `None` when the GPU has no
    /// device-local memory (integrated), the node could not be opened, or
    /// allocation accounting is unavailable to this process.
    drm: Option<DrmDevice>,
}

impl Gpu {
    fn new(id: usize, pmu: &GpuPmu) -> Result<Self, std::io::Error> {
        // Build the full list of (sysfs event name, metric, index) to open,
        // ordered so the group leader is a counter that always exists.
        let mut planned: Vec<(String, &'static metriken::CounterGroup, usize)> = Vec::new();

        let engines = discover_engines(pmu);

        for (offset, engine) in engines.iter().enumerate() {
            let metric_index = engine_index(id, offset);

            for (suffix, metric, expected_unit) in ENGINE_SAMPLES {
                let event_name = format!("{engine}-{suffix}");
                if unit_matches(pmu, &event_name, expected_unit, id) {
                    planned.push((event_name, metric, metric_index));
                }
            }
        }

        let mut globals = Vec::new();
        for (event_name, metric, expected_unit) in GLOBAL_EVENTS {
            if unit_matches(pmu, event_name, expected_unit, id) {
                planned.push((event_name.to_string(), metric, id));
                globals.push(event_name.to_string());
            }
        }

        if planned.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "PMU exposes no events we collect",
            ));
        }

        // Open the leader first, then attach the rest to its group so a single
        // read returns a coherent snapshot across all engines.
        let (leader_name, leader_metric, leader_index) = planned.remove(0);
        let leader_config = pmu
            .event(&leader_name)
            .expect("planned events exist in the PMU")
            .config;

        let mut leader = perf_event::Builder::new(RawEvent {
            event_type: pmu.perf_type,
            config: leader_config,
        })
        // The i915 PMU is system-wide with cpumask=0; it must be opened on
        // CPU 0 and counts work from every process.
        .one_cpu(0)
        .any_pid()
        .read_format(
            ReadFormat::TOTAL_TIME_ENABLED | ReadFormat::TOTAL_TIME_RUNNING | ReadFormat::GROUP,
        )
        .build()?;

        let mut members = Vec::new();

        for (event_name, metric, index) in planned {
            let config = pmu
                .event(&event_name)
                .expect("planned events exist in the PMU")
                .config;

            match perf_event::Builder::new(RawEvent {
                event_type: pmu.perf_type,
                config,
            })
            .one_cpu(0)
            .any_pid()
            .build_with_group(&mut leader)
            {
                Ok(counter) => members.push(TrackedCounter {
                    counter,
                    metric,
                    index,
                }),
                Err(e) => {
                    // Individual events can be absent on some parts (e.g. a GT1
                    // counter on a single-GT card); skip rather than give up.
                    debug!("{NAME}: GPU {id}: skipping event {event_name}: {e}");
                }
            }
        }

        leader.enable_group()?;

        let label = pmu.device_label();

        // Distinguishes an integrated GPU from a discrete card (e.g. Arc), which
        // is the first thing you want to filter on when a host has both.
        let gpu_type = if pmu.is_discrete() {
            "discrete"
        } else {
            "integrated"
        };

        // Publish the identity of every entry so the exported series carry the
        // device and engine they belong to, not just an opaque index.
        for (offset, engine) in engines.iter().enumerate() {
            let metric_index = engine_index(id, offset);
            for (_, metric, _) in ENGINE_SAMPLES {
                metric.insert_metadata(metric_index, "id".to_string(), id.to_string());
                metric.insert_metadata(metric_index, "device".to_string(), label.clone());
                metric.insert_metadata(metric_index, "type".to_string(), gpu_type.to_string());
                metric.insert_metadata(metric_index, "engine".to_string(), engine.clone());
                metric.insert_metadata(
                    metric_index,
                    "engine_class".to_string(),
                    engine_class(engine).to_string(),
                );
            }
        }

        for (_, metric, _) in GLOBAL_EVENTS {
            metric.insert_metadata(id, "id".to_string(), id.to_string());
            metric.insert_metadata(id, "device".to_string(), label.clone());
            metric.insert_metadata(id, "type".to_string(), gpu_type.to_string());
        }

        // VRAM comes from the DRM query ioctl, not the PMU. Only a GPU with a
        // device-local memory region reports it, so probe once here and keep the
        // node only if it yields real numbers — that way an integrated GPU (no
        // VRAM) and an unprivileged agent (no allocation accounting) both fall
        // back to publishing no memory series at all.
        let drm = pmu
            .pci_address
            .as_deref()
            .and_then(DrmDevice::for_pci_address)
            .filter(|drm| {
                let ok = drm.vram().is_some();
                if !ok {
                    debug!(
                        "{NAME}: GPU {id} ({label}): {} reports no device-local memory \
                         accounting; VRAM metrics disabled (needs CAP_PERFMON on a \
                         discrete GPU)",
                        drm.label()
                    );
                }
                ok
            });

        if drm.is_some() {
            for metric in [&GPU_MEMORY_USED, &GPU_MEMORY_FREE] {
                metric.insert_metadata(id, "id".to_string(), id.to_string());
                metric.insert_metadata(id, "device".to_string(), label.clone());
                metric.insert_metadata(id, "type".to_string(), gpu_type.to_string());
            }
        }

        Ok(Self {
            id,
            label,
            leader,
            leader_target: (leader_metric, leader_index),
            members,
            engines,
            globals,
            drm,
        })
    }

    /// Read every counter in the group with one syscall and publish the values.
    ///
    /// Values are set plain; the window is stamped once by the caller's
    /// acquisition bracket after every GPU has been visited (principle 18).
    fn refresh(&mut self) {
        let group = match self.leader.read_group() {
            Ok(group) => group,
            Err(e) => {
                // A failed read on one GPU leaves that GPU's values stale and
                // the rest of the sweep intact — this is the normal
                // partial-telemetry case, not a failure of the read section, so
                // the caller still finishes the group.
                debug!("{NAME}: GPU {} ({}) read failed: {e}", self.id, self.label);
                return;
            }
        };

        // These counters are cumulative, so the raw value is published directly
        // and the viewer differentiates it.
        if let Some(value) = group.get(&self.leader) {
            let (metric, index) = self.leader_target;
            let _ = metric.set(index, value.value());
        }

        for member in self.members.iter() {
            if let Some(value) = group.get(&member.counter) {
                let _ = member.metric.set(member.index, value.value());
            }
        }

        // VRAM is a separate ioctl on the DRM node, and unlike the PMU counters
        // it is an instantaneous gauge in bytes.
        if let Some(drm) = self.drm.as_ref() {
            if let Some(vram) = drm.vram() {
                let _ = GPU_MEMORY_USED.set(self.id, vram.used_bytes() as i64);
                let _ = GPU_MEMORY_FREE.set(self.id, vram.free_bytes as i64);
            }
        }
    }
}

/// True when `event_name` exists on this PMU and reports the unit we expect.
///
/// The metric definitions bake in a unit (nanoseconds, MHz), so an event whose
/// sysfs unit disagrees would be published under a wrong description. Rather
/// than emit a misleading series, skip the event and say why.
fn unit_matches(pmu: &GpuPmu, event_name: &str, expected: Option<&str>, id: usize) -> bool {
    let Some(event) = pmu.event(event_name) else {
        return false;
    };

    if event.unit.as_deref() == expected {
        return true;
    }

    warn!(
        "{NAME}: GPU {id}: skipping {event_name}: expected unit {:?} but sysfs reports {:?}",
        expected, event.unit
    );

    false
}

/// Find the engines this PMU exposes, in sysfs-name order.
///
/// Engines are discovered by looking for `<engine>-busy` events rather than
/// enumerating engine classes, because the set varies by part (an integrated GPU
/// has no `ccs`, and the driver renumbers instances densely so `vcs1` may be
/// hardware VCS2).
fn discover_engines(pmu: &GpuPmu) -> Vec<String> {
    let mut engines: Vec<String> = pmu
        .events
        .keys()
        .filter_map(|name| name.strip_suffix("-busy"))
        .map(|name| name.to_string())
        .collect();

    // Sort by class then instance so the index order is stable and readable.
    engines.sort_by_key(|engine| {
        let class = engine_class_rank(engine);
        let instance: u32 = engine
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .parse()
            .unwrap_or(0);
        (class, instance)
    });

    if engines.len() > MAX_ENGINES {
        warn!(
            "{NAME}: GPU {} exposes {} engines, only the first {MAX_ENGINES} are recorded",
            pmu.device_label(),
            engines.len()
        );
        engines.truncate(MAX_ENGINES);
    }

    engines
}

/// Map a sysfs engine name to its human-readable class, matching the grouping
/// `intel_gpu_top` displays.
fn engine_class(engine: &str) -> &'static str {
    match engine.trim_end_matches(|c: char| c.is_ascii_digit()) {
        "rcs" => "render",
        "bcs" => "copy",
        "vcs" => "video",
        "vecs" => "video-enhance",
        "ccs" => "compute",
        _ => "other",
    }
}

/// Sort rank for an engine class, following the engine-class enum order in
/// `i915_drm.h` (RENDER=0, COPY=1, VIDEO=2, VIDEO_ENHANCE=3, COMPUTE=4).
fn engine_class_rank(engine: &str) -> u32 {
    match engine.trim_end_matches(|c: char| c.is_ascii_digit()) {
        "rcs" => 0,
        "bcs" => 1,
        "vcs" => 2,
        "vecs" => 3,
        "ccs" => 4,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_engine_classes() {
        assert_eq!(engine_class("rcs0"), "render");
        assert_eq!(engine_class("bcs0"), "copy");
        assert_eq!(engine_class("vcs1"), "video");
        assert_eq!(engine_class("vecs1"), "video-enhance");
        assert_eq!(engine_class("ccs0"), "compute");
        assert_eq!(engine_class("mystery9"), "other");
    }

    #[test]
    fn orders_engines_by_class_then_instance() {
        use std::collections::HashMap;

        let names = [
            "vecs1",
            "rcs0",
            "ccs0",
            "vcs1",
            "bcs0",
            "vcs0",
            "vecs0",
            "interrupts",
        ];

        let events: HashMap<String, pmu::PmuEvent> = names
            .iter()
            .map(|n| {
                let key = if *n == "interrupts" {
                    n.to_string()
                } else {
                    format!("{n}-busy")
                };
                (
                    key,
                    pmu::PmuEvent {
                        config: 0,
                        unit: None,
                    },
                )
            })
            .collect();

        let gpu = GpuPmu {
            name: "i915_0000_04_00.0".to_string(),
            perf_type: 24,
            pci_address: Some("0000:04:00.0".to_string()),
            driver: "i915".to_string(),
            events,
        };

        // Matches the 7 engines a real Arc A770 exposes, in class order. The
        // `interrupts` global must not be mistaken for an engine.
        assert_eq!(
            discover_engines(&gpu),
            vec!["rcs0", "bcs0", "vcs0", "vcs1", "vecs0", "vecs1", "ccs0"]
        );
    }

    /// The two index spaces must not be conflated into one member set.
    ///
    /// Regression test for phantom series: engine entries are indexed by
    /// `engine_index`, per-GPU entries by plain GPU id. Unioning them made
    /// engine indices 2 and 3 of GPU 0 look like GPU ids 2 and 3, and the
    /// agent published all-zero `gpu_frequency_sample{id="2"}` and `{id="3"}`
    /// series on a two-GPU host. Observed on hardware.
    #[test]
    fn engine_and_device_index_spaces_stay_separate() {
        // Two GPUs, as on the discrete+integrated host this was caught on.
        let gpus = [4usize, 7]; // engine counts

        let mut engine_members: Vec<usize> = Vec::new();
        for (id, engines) in gpus.iter().enumerate() {
            for offset in 0..*engines {
                engine_members.push(engine_index(id, offset));
            }
        }

        // The device space is a dense prefix of GPU ids, and nothing more.
        let device_bound = gpus.len();
        assert_eq!(device_bound, 2);

        // The engine set must contain indices that are NOT valid GPU ids, which
        // is precisely why it cannot double as the device member set.
        assert!(engine_members.iter().any(|i| *i >= device_bound));

        // Each GPU's engines stay inside its own block.
        for (id, engines) in gpus.iter().enumerate() {
            for offset in 0..*engines {
                let index = engine_index(id, offset);
                assert!(index >= id * MAX_ENGINES);
                assert!(index < (id + 1) * MAX_ENGINES);
            }
        }
    }

    #[test]
    fn engine_metric_indices_stay_within_their_gpu() {
        // Entry layout must not let one GPU's engines collide with another's.
        for gpu in 0..MAX_GPUS {
            for engine in 0..MAX_ENGINES {
                let index = engine_index(gpu, engine);
                assert!(index < ENGINE_ENTRIES);
            }
        }

        // GPU 0's first engine sits at index 0, and GPU 1's first engine
        // starts a fresh block rather than overlapping GPU 0's last.
        assert_eq!(engine_index(0, 0), 0);
        assert_eq!(engine_index(1, 0), MAX_ENGINES);
        assert!(engine_index(0, MAX_ENGINES - 1) < engine_index(1, 0));
    }
}
