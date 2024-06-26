use crate::{Arc, Config, PERCENTILES};
use metriken::{AtomicHistogram, Counter, Gauge, RwLockHistogram};
use metriken_exposition::SnapshotterBuilder;
use std::time::UNIX_EPOCH;
use warp::Filter;

/// HTTP exposition
pub async fn http(config: Arc<Config>) {
    let listen = config.general().listen();

    if config.general().compression() {
        warp::serve(filters::http(config).with(warp::filters::compression::gzip()))
            .run(listen)
            .await;
    } else {
        warp::serve(filters::http(config)).run(listen).await;
    }
}

mod filters {
    use super::*;

    fn with_config(
        config: Arc<Config>,
    ) -> impl Filter<Extract = (Arc<Config>,), Error = std::convert::Infallible> + Clone {
        warp::any().map(move || config.clone())
    }

    /// The combined set of http endpoint filters
    pub fn http(
        config: Arc<Config>,
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        prometheus_stats(config.clone())
            .or(human_stats())
            .or(json_stats())
            .or(hardware_info())
            .or(msgpack())
    }

    /// Serves Prometheus / OpenMetrics text format metrics. All metrics have
    /// type information, some have descriptions as well. Percentiles read from
    /// histograms are exposed with a `percentile` label where the value
    /// corresponds to the percentile in the range of 0.0 - 100.0.
    ///
    /// Since we serve histogram metrics from a snapshot, they have a timestamp
    /// that indicates when the histogram was snapshotted. All other metrics are
    /// read at the time of the request and rely on the scraper to timestamp the
    /// readings. This allows backends like Prometheus to attribute the readings
    /// from all sources to the moment in time they were read.
    ///
    /// See: https://github.com/OpenObservability/OpenMetrics/blob/main/specification/OpenMetrics.md
    ///
    /// ```text
    /// # TYPE example_a counter
    /// # HELP example_a An unsigned 64bit monotonic counter.
    /// example_a 0
    /// # TYPE example_b gauge
    /// # HELP example_b A signed 64bit gauge.
    /// example_b [i64]
    /// # TYPE example_c{percentile="50.0"} gauge
    /// example_c{percentile="50.0"} [i64] [UNIX TIME]
    /// ```
    ///
    /// If histogram exposition is enabled, the histogram buckets are also
    /// exported and will look like this:
    ///
    /// ```text
    /// # TYPE example_c_distribution histogram
    /// example_c_distribution_bucket{le="0"} [u64] [UNIX TIME]
    /// ...
    /// example_c_distribution_bucket{le="+Inf"} [u64] [UNIX TIME]
    /// example_c_distribution_count [u64] [UNIX TIME]
    /// example_c_distribution_sum [u64] [UNIX TIME]
    /// ```
    ///
    /// GET /metrics
    pub fn prometheus_stats(
        config: Arc<Config>,
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("metrics")
            .and(warp::get())
            .and(with_config(config))
            .and_then(handlers::prometheus_stats)
    }

    /// Serves human readable stats. One metric per line with a `LF` as the
    /// newline character (Unix-style). Percentiles will have percentile labels
    /// appended with a `/` as a separator.
    ///
    /// ```
    /// example/counter: 0
    /// example/gauge: 0,
    /// example/histogram/p999: 0,
    /// ```
    ///
    /// GET /vars
    pub fn human_stats(
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("vars")
            .and(warp::get())
            .and_then(handlers::human_stats)
    }

    /// JSON metrics exposition. Multiple paths are provided for enhanced
    /// compatibility with metrics collectors.
    ///
    /// Percentiles read from heatmaps will have a percentile label appended to
    /// to the metric name in the form `/p999` which would be the 99.9th
    /// percentile.
    ///
    /// ```text
    /// {"example/counter": 0,"example/gauge": 0, example/histogram/p999": 0, ... }
    /// ```
    ///
    /// GET /metrics.json
    /// GET /vars.json
    /// GET /admin/metrics.json
    pub fn json_stats(
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("metrics.json")
            .and(warp::get())
            .and_then(handlers::json_stats)
            .or(warp::path!("vars.json")
                .and(warp::get())
                .and_then(handlers::json_stats))
            .or(warp::path!("admin" / "metrics.json")
                .and(warp::get())
                .and_then(handlers::json_stats))
    }

    /// GET /hardware_info
    pub fn hardware_info(
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("hardware_info")
            .and(warp::get())
            .and_then(handlers::hwinfo)
    }

    /// GET /metrics/binary
    pub fn msgpack() -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone
    {
        warp::path!("metrics" / "binary")
            .and(warp::get())
            .and_then(handlers::msgpack)
    }
}

mod handlers {
    use super::*;
    use crate::common::HISTOGRAM_GROUPING_POWER;
    use crate::SNAPSHOTS;
    use core::convert::Infallible;

    pub async fn prometheus_stats(config: Arc<Config>) -> Result<impl warp::Reply, Infallible> {
        let mut data = Vec::new();

        let snapshots = SNAPSHOTS.read().await;

        let timestamp = snapshots
            .timestamp
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();

        for metric in &metriken::metrics() {
            let any = match metric.as_any() {
                Some(any) => any,
                None => {
                    continue;
                }
            };

            let name = metric.name();

            if name.starts_with("log_") {
                continue;
            }

            if name == "cpu/usage" && metric.metadata().get("state") == Some("busy") {
                continue;
            }

            if let Some(counter) = any.downcast_ref::<Counter>() {
                if metric.metadata().is_empty() {
                    data.push(format!(
                        "# TYPE {name}_total counter\n{name}_total {}",
                        counter.value()
                    ));
                } else {
                    data.push(format!(
                        "# TYPE {name} counter\n{} {}",
                        metric.formatted(metriken::Format::Prometheus),
                        counter.value()
                    ));
                }
            } else if let Some(gauge) = any.downcast_ref::<Gauge>() {
                data.push(format!(
                    "# TYPE {name} gauge\n{} {}",
                    metric.formatted(metriken::Format::Prometheus),
                    gauge.value()
                ));
            } else if any.downcast_ref::<AtomicHistogram>().is_some()
                || any.downcast_ref::<RwLockHistogram>().is_some()
            {
                let simple_name = metric.formatted(metriken::Format::Simple);

                if let Some(delta) = snapshots.delta.get(&simple_name) {
                    let percentiles: Vec<f64> = PERCENTILES.iter().map(|(_, p)| *p).collect();

                    if let Ok(Some(result)) = delta.value.percentiles(&percentiles) {
                        for (percentile, value) in result.iter().map(|(p, b)| (p, b.end())) {
                            data.push(format!(
                                "# TYPE {name} gauge\n{name}{{percentile=\"{:02}\"}} {value} {timestamp}",
                                percentile,
                            ));
                        }
                    }
                }
                if config.prometheus().histograms() {
                    if let Some(histogram) = snapshots.previous.get(metric.name()) {
                        let current = HISTOGRAM_GROUPING_POWER;
                        let target = config.prometheus().histogram_grouping_power();

                        // downsample the histogram if necessary
                        let downsampled: Option<histogram::Histogram> = if current == target {
                            // the powers matched, we don't need to downsample
                            None
                        } else {
                            Some(histogram.value.downsample(target).unwrap())
                        };

                        // reassign to either use the downsampled histogram or the original
                        let histogram = if let Some(histogram) = downsampled.as_ref() {
                            histogram
                        } else {
                            &histogram.value
                        };

                        // we need to export a total count (free-running)
                        let mut count = 0;
                        // we also need to export a total sum of all observations
                        // which is also free-running
                        let mut sum = 0;

                        let mut entry = format!("# TYPE {name}_distribution histogram\n");
                        for bucket in histogram {
                            // add this bucket's sum of observations
                            sum += bucket.count() * bucket.end();

                            // add the count to the aggregate
                            count += bucket.count();

                            entry += &format!(
                                "{name}_distribution_bucket{{le=\"{}\"}} {count} {timestamp}\n",
                                bucket.end()
                            );
                        }

                        entry += &format!(
                            "{name}_distribution_bucket{{le=\"+Inf\"}} {count} {timestamp}\n"
                        );
                        entry += &format!("{name}_distribution_count {count} {timestamp}\n");
                        entry += &format!("{name}_distribution_sum {sum} {timestamp}\n");

                        data.push(entry);
                    }
                }
            }
        }

        data.sort();
        data.dedup();
        let mut content = data.join("\n");
        content += "\n";
        let parts: Vec<&str> = content.split('/').collect();
        Ok(parts.join("_"))
    }

    pub async fn human_stats() -> Result<impl warp::Reply, Infallible> {
        let data = simple_stats(false).await;

        let mut content = data.join("\n");
        content += "\n";

        Ok(content)
    }

    pub async fn json_stats() -> Result<impl warp::Reply, Infallible> {
        let data = simple_stats(true).await;

        let mut content = "{".to_string();
        content += &data.join(",");
        content += "}";

        Ok(content)
    }

    pub async fn msgpack() -> Result<impl warp::Reply, Infallible> {
        let snapshot = SnapshotterBuilder::new()
            .metadata("source".to_string(), env!("CARGO_BIN_NAME").to_string())
            .metadata("version".to_string(), env!("CARGO_PKG_VERSION").to_string())
            .filter(|metric| {
                if let Some(m) = metric.as_any() {
                    if m.downcast_ref::<AtomicHistogram>().is_some() {
                        false
                    } else {
                        !metric.name().starts_with("log_")
                    }
                } else {
                    false
                }
            })
            .build()
            .snapshot();

        Ok(metriken_exposition::Snapshot::to_msgpack(&snapshot).unwrap())
    }

    pub async fn hwinfo() -> Result<impl warp::Reply, Infallible> {
        if let Ok(hwinfo) = crate::samplers::hwinfo::hardware_info() {
            Ok(warp::reply::json(hwinfo))
        } else {
            Ok(warp::reply::json(&false))
        }
    }
}

// gathers up the metrics into a simple format that can be presented as human
// readable metrics or transformed into JSON
async fn simple_stats(quoted: bool) -> Vec<String> {
    let mut data = Vec::new();

    let snapshots = crate::SNAPSHOTS.read().await;

    let q = if quoted { "\"" } else { "" };

    for metric in &metriken::metrics() {
        let any = match metric.as_any() {
            Some(any) => any,
            None => {
                continue;
            }
        };

        if metric.name().starts_with("log_") {
            continue;
        }

        let simple_name = metric.formatted(metriken::Format::Simple);

        if let Some(counter) = any.downcast_ref::<Counter>() {
            data.push(format!("{q}{simple_name}{q}: {}", counter.value()));
        } else if let Some(gauge) = any.downcast_ref::<Gauge>() {
            data.push(format!("{q}{simple_name}{q}: {}", gauge.value()));
        } else if any.downcast_ref::<AtomicHistogram>().is_some()
            || any.downcast_ref::<RwLockHistogram>().is_some()
        {
            let simple_name = metric.formatted(metriken::Format::Simple);

            if let Some(delta) = snapshots.delta.get(&simple_name) {
                let percentiles: Vec<f64> = PERCENTILES.iter().map(|(_, p)| *p).collect();

                if let Ok(Some(result)) = delta.value.percentiles(&percentiles) {
                    for (value, label) in result
                        .iter()
                        .map(|(_, b)| b.end())
                        .zip(PERCENTILES.iter().map(|(l, _)| l))
                    {
                        data.push(format!("{q}{simple_name}/{label}{q}: {value}"));
                    }
                }
            }
        }
    }

    data.sort();
    data
}
