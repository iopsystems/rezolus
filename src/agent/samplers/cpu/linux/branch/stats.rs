use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::MAX_CPUS;
use linkme::distributed_slice;

// `cpu_branch` has no non-Linux fallback (see `cpu/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` stub, which does not include
// this file) — this whole sampler is Linux-only. Declared here anyway, next
// to the metrics it brackets, matching the family's convention.
//
/// Brackets the per-CPU perf-event sweep (`BranchInner::refresh`, which
/// triggers the dedicated perf-read thread(s) and awaits their completion —
/// see `cpu_dtlb`'s `CPU_DTLB_ACQ` doc comment for the shared "fourth
/// machinery" shape). Single writer: only `BranchInner::refresh` calls
/// `acquire()`/`finish()`. One group for both metrics: `Core::refresh()`
/// reads branch instructions + misses for one CPU via a single uninterrupted
/// `read_group()` call, with no phase boundary between the two.
pub static CPU_BRANCH_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("cpu_branch"),
    "cpu_branch_sweep",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CPU_BRANCH_ACQ_REG: &'static AcquisitionGroup = &CPU_BRANCH_ACQ;

// per-CPU metrics

#[metric(
    name = "cpu_branch_instructions",
    description = "The number of branch instructions retired",
    metadata = { acq_group = "cpu_branch_sweep" }
)]
pub static CPU_BRANCH_INSTRUCTIONS: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_branch_misses",
    description = "The number of branch mispredictions",
    metadata = { acq_group = "cpu_branch_sweep" }
)]
pub static CPU_BRANCH_MISSES: CounterGroup = CounterGroup::new(MAX_CPUS);
