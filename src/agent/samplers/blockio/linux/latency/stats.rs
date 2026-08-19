use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `blockio/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only. One group per histogram map read (each `Histogram` reads
// exactly one BPF map per refresh).
pub static READ_LATENCY_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("blockio_latency"),
    "blockio_latency_read_latency",
);
pub static WRITE_LATENCY_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("blockio_latency"),
    "blockio_latency_write_latency",
);
pub static FLUSH_LATENCY_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("blockio_latency"),
    "blockio_latency_flush_latency",
);
pub static DISCARD_LATENCY_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("blockio_latency"),
    "blockio_latency_discard_latency",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static READ_LATENCY_ACQ_REG: &'static AcquisitionGroup = &READ_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static WRITE_LATENCY_ACQ_REG: &'static AcquisitionGroup = &WRITE_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static FLUSH_LATENCY_ACQ_REG: &'static AcquisitionGroup = &FLUSH_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static DISCARD_LATENCY_ACQ_REG: &'static AcquisitionGroup = &DISCARD_LATENCY_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "blockio_latency"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "blockio_latency"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio operation latency in nanoseconds",
    metadata = { op = "read", unit = "nanoseconds", acq_group = "blockio_latency_read_latency" }
)]
pub static BLOCKIO_READ_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio operation latency in nanoseconds",
    metadata = { op = "write", unit = "nanoseconds", acq_group = "blockio_latency_write_latency" }
)]
pub static BLOCKIO_WRITE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio operation latency in nanoseconds",
    metadata = { op = "flush", unit = "nanoseconds", acq_group = "blockio_latency_flush_latency" }
)]
pub static BLOCKIO_FLUSH_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio operation latency in nanoseconds",
    metadata = { op = "discard", unit = "nanoseconds", acq_group = "blockio_latency_discard_latency" }
)]
pub static BLOCKIO_DISCARD_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
