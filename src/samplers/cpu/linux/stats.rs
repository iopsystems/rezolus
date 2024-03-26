use super::super::stats::*;
use crate::*;
use metriken::{metric, Counter, Gauge, LazyCounter, LazyGauge};

#[metric(
    name = "cpu/cores",
    description = "The total number of logical cores that are currently online"
)]
pub static CPU_CORES: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent waiting for IO to complete",
    formatter = cpu_metric_formatter,
    metadata = { state = "io_wait" }
)]
pub static CPU_USAGE_IO_WAIT: LazyCounter = LazyCounter::new(Counter::default);

histogram!(CPU_USAGE_IO_WAIT_HISTOGRAM, "cpu/usage/io_wait");

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent servicing interrupts",
    formatter = cpu_metric_formatter,
    metadata = { state = "irq" }
)]
pub static CPU_USAGE_IRQ: LazyCounter = LazyCounter::new(Counter::default);

histogram!(CPU_USAGE_IRQ_HISTOGRAM, "cpu/usage/irq");

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent servicing softirqs",
    formatter = cpu_metric_formatter,
    metadata = { state = "softirq" }
)]
pub static CPU_USAGE_SOFTIRQ: LazyCounter = LazyCounter::new(Counter::default);

histogram!(CPU_USAGE_SOFTIRQ_HISTOGRAM, "cpu/usage/softirq");

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time stolen by the hypervisor",
    formatter = cpu_metric_formatter,
    metadata = { state = "steal" }
)]
pub static CPU_USAGE_STEAL: LazyCounter = LazyCounter::new(Counter::default);

histogram!(CPU_USAGE_STEAL_HISTOGRAM, "cpu/usage/steal");

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent running a virtual CPU for a guest",
    formatter = cpu_metric_formatter,
    metadata = { state = "guest" }
)]
pub static CPU_USAGE_GUEST: LazyCounter = LazyCounter::new(Counter::default);

histogram!(CPU_USAGE_GUEST_HISTOGRAM, "cpu/usage/guest");

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent running a virtual CPU for a guest in low priority mode",
    formatter = cpu_metric_formatter,
    metadata = { state = "guest_nice" }
)]
pub static CPU_USAGE_GUEST_NICE: LazyCounter = LazyCounter::new(Counter::default);

histogram!(CPU_USAGE_GUEST_NICE_HISTOGRAM, "cpu/usage/guest_nice");

#[metric(
    name = "cpu/cycles",
    description = "The number of elapsed CPU cycles",
    formatter = cpu_metric_formatter
)]
pub static CPU_CYCLES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/instructions",
    description = "The number of instructions retired",
    formatter = cpu_metric_formatter
)]
pub static CPU_INSTRUCTIONS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/perf_groups/active",
    description = "The number of currently active perf event groups"
)]
pub static CPU_PERF_GROUPS_ACTIVE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "cpu/ipkc/average",
    description = "Average IPKC (instructions per thousand cycles): SUM(IPKC_CPU0...N)/N)"
)]
pub static CPU_IPKC_AVERAGE: LazyGauge = LazyGauge::new(Gauge::default);

histogram!(
    CPU_IPKC_HISTOGRAM,
    "cpu/ipkc",
    "distribution of per-CPU IPKC (Instructions Per Thousand Cycles)"
);

#[metric(
    name = "cpu/ipus/average",
    description = "Average IPUS (instructions per microsecond): SUM(IPUS_CPU0...N)/N)"
)]
pub static CPU_IPUS_AVERAGE: LazyGauge = LazyGauge::new(Gauge::default);

histogram!(
    CPU_IPUS_HISTOGRAM,
    "cpu/ipus",
    "Distribution of per-CPU IPUS (Instructions Per Microsecond)"
);

#[metric(
    name = "cpu/base_frequency/average",
    description = "Average base CPU frequency (MHz)"
)]
pub static CPU_BASE_FREQUENCY_AVERAGE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "cpu/frequency/average",
    description = "Average running CPU frequency (MHz): SUM(RUNNING_FREQUENCY_CPU0...N)/N"
)]
pub static CPU_FREQUENCY_AVERAGE: LazyGauge = LazyGauge::new(Gauge::default);

histogram!(
    CPU_FREQUENCY_HISTOGRAM,
    "cpu/frequency",
    "Distribution of the per-CPU running frequencies"
);
