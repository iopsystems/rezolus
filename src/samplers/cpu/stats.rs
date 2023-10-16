use crate::*;
use metriken::{metric, Counter, Format, Gauge, LazyCounter, LazyGauge, MetricEntry};

#[metric(
    name = "cpu/cores",
    description = "The total number of logical cores that are currently online"
)]
pub static CPU_CORES: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent executing normal tasks is user mode",
    formatter = cpu_metric_formatter,
    metadata = { state = "user" }
)]
pub static CPU_USAGE_USER: LazyCounter = LazyCounter::new(Counter::default);

histogram!(CPU_USAGE_USER_HISTOGRAM, "cpu/usage/user");

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent executing low priority tasks in user mode",
    formatter = cpu_metric_formatter,
    metadata = { state = "nice" }
)]
pub static CPU_USAGE_NICE: LazyCounter = LazyCounter::new(Counter::default);

histogram!(CPU_USAGE_NICE_HISTOGRAM, "cpu/usage/nice");

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent executing tasks in kernel mode",
    formatter = cpu_metric_formatter,
    metadata = { state = "system" }
)]
pub static CPU_USAGE_SYSTEM: LazyCounter = LazyCounter::new(Counter::default);

histogram!(CPU_USAGE_SYSTEM_HISTOGRAM, "cpu/usage/system");

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent idle",
    formatter = cpu_metric_formatter,
    metadata = { state = "idle" }
)]
pub static CPU_USAGE_IDLE: LazyCounter = LazyCounter::new(Counter::default);

histogram!(CPU_USAGE_IDLE_HISTOGRAM, "cpu/usage/idle");

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

/// A function to format the cpu metrics that allows for export of both total
/// and per-CPU metrics.
///
/// For the `Simple` format, the metrics will be formatted according to the
/// a pattern which depends on the metric metadata:
/// `{name}/cpu{id}` eg: `cpu/frequency/cpu0`
/// `{name}/total` eg: `cpu/cycles/total`
/// `{name}/{state}/cpu{id}` eg: `cpu/usage/user/cpu0`
/// `{name}/{state}/total` eg: `cpu/usage/user/total`
///
/// For the `Prometheus` format, if the metric has an `id` set in the metadata,
/// the metric name is left as-is. Otherwise, `/total` is appended. Note: we
/// rely on the exposition logic to convert the `/`s to `_`s in the metric name.
pub fn cpu_metric_formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            let name = if let Some(state) = metric.metadata().get("state") {
                format!("{}/{state}", metric.name())
            } else {
                metric.name().to_string()
            };

            if metric.metadata().contains_key("id") {
                format!(
                    "{name}/cpu{}",
                    metric.metadata().get("id").unwrap_or("unknown"),
                )
            } else {
                format!("{name}/total",)
            }
        }
        Format::Prometheus => {
            let metadata: Vec<String> = metric
                .metadata()
                .iter()
                .map(|(key, value)| format!("{key}=\"{value}\""))
                .collect();
            let metadata = metadata.join(", ");

            let name = if metric.metadata().contains_key("id") {
                metric.name().to_string()
            } else {
                format!("{}/total", metric.name())
            };

            if metadata.is_empty() {
                name
            } else {
                format!("{}{{{metadata}}}", name)
            }
        }
        _ => metriken::default_formatter(metric, format),
    }
}
