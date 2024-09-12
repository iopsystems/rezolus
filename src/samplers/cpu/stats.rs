use metriken::*;

#[metric(
    name = "metadata/cpu_usage/collected_at",
    description = "The offset from the Unix epoch when cpu_usage sampler was last run",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_CPU_USAGE_COLLECTED_AT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/cpu_usage/runtime",
    description = "The total runtime of the cpu_usage sampler",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_CPU_USAGE_RUNTIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/cpu_usage/runtime",
    description = "Distribution of sampling runtime of the cpu_usage_sampler",
    metadata = { unit = "nanoseconds/second" }
)]
pub static METADATA_CPU_USAGE_RUNTIME_HISTOGRAM: AtomicHistogram = AtomicHistogram::new(4, 32);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent executing normal tasks is user mode",
    formatter = cpu_metric_formatter,
    metadata = { state = "user", unit = "nanoseconds" }
)]
pub static CPU_USAGE_USER: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent executing low priority tasks in user mode",
    formatter = cpu_metric_formatter,
    metadata = { state = "nice", unit = "nanoseconds" }
)]
pub static CPU_USAGE_NICE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent executing tasks in kernel mode",
    formatter = cpu_metric_formatter,
    metadata = { state = "system", unit = "nanoseconds" }
)]
pub static CPU_USAGE_SYSTEM: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent idle",
    formatter = cpu_metric_formatter,
    metadata = { state = "idle", unit = "nanoseconds" }
)]
pub static CPU_USAGE_IDLE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent busy",
    formatter = cpu_metric_formatter,
    metadata = { state = "busy", unit = "nanoseconds" }
)]
pub static CPU_USAGE_BUSY: LazyCounter = LazyCounter::new(Counter::default);

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
