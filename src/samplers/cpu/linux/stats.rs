use super::super::stats::*;
use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::{metric, AtomicHistogram, Counter, Gauge, LazyCounter, LazyGauge};

#[metric(
    name = "cpu/cores",
    description = "The total number of logical cores that are currently online"
)]
pub static CPU_CORES: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent waiting for IO to complete",
    formatter = cpu_metric_formatter,
    metadata = { state = "io_wait", unit = "nanoseconds" }
)]
pub static CPU_USAGE_IO_WAIT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage/io_wait",
    description = "Distribution of rate of CPU usage from sample to sample",
    metadata = { unit = "nanoseconds/second" }
)]
pub static CPU_USAGE_IO_WAIT_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent servicing interrupts",
    formatter = cpu_metric_formatter,
    metadata = { state = "irq", unit = "nanoseconds" }
)]
pub static CPU_USAGE_IRQ: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage/irq",
    description = "Distribution of rate of CPU usage from sample to sample",
    metadata = { unit = "nanoseconds/second" }
)]
pub static CPU_USAGE_IRQ_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent servicing softirqs",
    formatter = cpu_metric_formatter,
    metadata = { state = "softirq", unit = "nanoseconds" }
)]
pub static CPU_USAGE_SOFTIRQ: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage/softirq",
    description = "Distribution of rate of CPU usage from sample to sample",
    metadata = { unit = "nanoseconds/second" }
)]
pub static CPU_USAGE_SOFTIRQ_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time stolen by the hypervisor",
    formatter = cpu_metric_formatter,
    metadata = { state = "steal", unit = "nanoseconds" }
)]
pub static CPU_USAGE_STEAL: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage/steal",
    description = "Distribution of rate of CPU usage from sample to sample",
    metadata = { unit = "nanoseconds/second" }
)]
pub static CPU_USAGE_STEAL_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent running a virtual CPU for a guest",
    formatter = cpu_metric_formatter,
    metadata = { state = "guest", unit = "nanoseconds" }
)]
pub static CPU_USAGE_GUEST: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage/guest",
    description = "Distribution of rate of CPU usage from sample to sample",
    metadata = { unit = "nanoseconds/second" }
)]
pub static CPU_USAGE_GUEST_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent running a virtual CPU for a guest in low priority mode",
    formatter = cpu_metric_formatter,
    metadata = { state = "guest_nice", unit = "nanoseconds" }
)]
pub static CPU_USAGE_GUEST_NICE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage/guest_nice",
    description = "Distribution of rate of CPU usage from sample to sample",
    metadata = { unit = "nanoseconds/second" }
)]
pub static CPU_USAGE_GUEST_NICE_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "cpu/cycles",
    description = "The number of elapsed CPU cycles",
    formatter = cpu_metric_formatter,
    metadata = { unit = "cycles" }
)]
pub static CPU_CYCLES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/instructions",
    description = "The number of instructions retired",
    formatter = cpu_metric_formatter,
    metadata = { unit = "instructions" }
)]
pub static CPU_INSTRUCTIONS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/perf_groups/active",
    description = "The number of currently active perf event groups"
)]
pub static CPU_PERF_GROUPS_ACTIVE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "cpu/ipkc/average",
    description = "Average IPKC (instructions per thousand cycles): SUM(IPKC_CPU0...N)/N)",
    metadata = { unit = "instructions/kilocycle" }
)]
pub static CPU_IPKC_AVERAGE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "cpu/ipkc",
    description = "Distribution of instruction retirement rates from sample to sample",
    metadata = { unit = "instructions/kilocycle" }
)]
pub static CPU_IPKC_HISTOGRAM: AtomicHistogram = AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "cpu/ipus/average",
    description = "Average IPUS (instructions per microsecond): SUM(IPUS_CPU0...N)/N)",
    metadata = { unit = "instructions/microsecond" }
)]
pub static CPU_IPUS_AVERAGE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "cpu/ipus",
    description = "Distribution of instruction retirement rates from sample to sample",
    metadata = { unit = "instructions/microsecond" }
)]
pub static CPU_IPUS_HISTOGRAM: AtomicHistogram = AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "cpu/base_frequency/average",
    description = "Average base CPU frequency (MHz)",
    metadata = { unit = "megahertz" }
)]
pub static CPU_BASE_FREQUENCY_AVERAGE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "cpu/frequency/average",
    description = "Average running CPU frequency (MHz): SUM(RUNNING_FREQUENCY_CPU0...N)/N",
    metadata = { unit = "megahertz" }
)]
pub static CPU_FREQUENCY_AVERAGE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "cpu/frequency",
    description = "Distribution of CPU frequencies from sample to sample",
    metadata = { unit = "megahertz" }
)]
pub static CPU_FREQUENCY_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
