use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::MAX_CPUS;
use linkme::distributed_slice;

// `cpu_dtlb` has no non-Linux fallback (see `cpu/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` stub, which does not include
// this file) — this whole sampler is Linux-only. Declared here anyway, next
// to the metrics it brackets, matching the family's convention (e.g.
// `cpu_cores`'s `CPU_CORES_ACQ`).
//
/// Brackets the per-CPU perf-event sweep (`DtlbInner::refresh`, which
/// triggers the dedicated perf-read thread(s) and awaits their completion —
/// the fourth read-machinery shape alongside BPF `Counters`/`CpuCounters`/
/// `PackedCounters`: a sampler-owned OS thread reads `perf_event` counters
/// off the async task, synchronized by a trigger/notify handshake). Single
/// writer: only `DtlbInner::refresh` ever calls `acquire()`/`finish()` — the
/// perf thread(s) call `Core::refresh()`'s plain `set()`s but never touch
/// the group themselves, and the outer `Mutex<DtlbInner>` plus the sampler
/// scheduler's serial dispatch mean `refresh()` never runs concurrently with
/// itself. One group for all three metrics: `Core::refresh()` reads
/// load/store misses for one CPU in a single uninterrupted function call (no
/// phase boundary), and the sweep across CPUs is one triggered thread pass —
/// the same "read together, no gap" shape as `cpu_frequency`'s aperf/mperf/tsc
/// and `cpu_l3`'s access/miss, not `cpu_usage`'s three-separate-BPF-map case.
pub static CPU_DTLB_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("cpu_dtlb"),
    "cpu_dtlb_sweep",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CPU_DTLB_ACQ_REG: &'static AcquisitionGroup = &CPU_DTLB_ACQ;

// per-CPU metrics

/// DTLB misses without op label - used on AMD/ARM where load and store
/// misses are reported as a single combined event
#[metric(
    name = "cpu_dtlb_miss",
    description = "The number of DTLB misses",
    metadata = { acq_group = "cpu_dtlb_sweep" }
)]
pub static CPU_DTLB_MISS: CounterGroup = CounterGroup::new(MAX_CPUS);

/// DTLB load misses - Intel only
#[metric(
    name = "cpu_dtlb_miss",
    description = "The number of DTLB load misses",
    metadata = { op = "load", acq_group = "cpu_dtlb_sweep" }
)]
pub static CPU_DTLB_MISS_LOAD: CounterGroup = CounterGroup::new(MAX_CPUS);

/// DTLB store misses - Intel only
#[metric(
    name = "cpu_dtlb_miss",
    description = "The number of DTLB store misses",
    metadata = { op = "store", acq_group = "cpu_dtlb_sweep" }
)]
pub static CPU_DTLB_MISS_STORE: CounterGroup = CounterGroup::new(MAX_CPUS);
