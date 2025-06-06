use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

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
    description = "Distribution of blockio read operation latency in nanoseconds",
    metadata = { op = "read", unit = "nanoseconds" }
)]
pub static BLOCKIO_READ_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio write operation latency in nanoseconds",
    metadata = { op = "write", unit = "nanoseconds" }
)]
pub static BLOCKIO_WRITE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio flush operation latency in nanoseconds",
    metadata = { op = "flush", unit = "nanoseconds" }
)]
pub static BLOCKIO_FLUSH_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio discard operation latency in nanoseconds",
    metadata = { op = "discard", unit = "nanoseconds" }
)]
pub static BLOCKIO_DISCARD_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
