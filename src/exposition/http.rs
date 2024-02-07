use crate::Arc;
use crate::Config;
use crate::PERCENTILES;
use chrono::{DateTime, Utc};
use metriken::histogram::Snapshot;
use metriken::{AtomicHistogram, Counter, Gauge, RwLockHistogram};
use rmp_serde::Serializer;
use serde::Deserialize;
use serde::Serialize;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use warp::Filter;

/// HTTP exposition
pub async fn http(config: Arc<Config>) {
    if config.general().compression() {
        warp::serve(filters::http(config).with(warp::filters::compression::gzip()))
            .run(([0, 0, 0, 0], 4242))
            .await;
    } else {
        warp::serve(filters::http(config))
            .run(([0, 0, 0, 0], 4242))
            .await;
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
            .or(hardware_info())
            .or(binary_metadata())
            .or(binary_readings())
    }

    /// GET /metrics
    pub fn prometheus_stats(
        config: Arc<Config>,
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("metrics")
            .and(warp::get())
            .and(with_config(config))
            .and_then(handlers::prometheus_stats)
    }

    /// GET /vars
    pub fn human_stats(
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("vars")
            .and(warp::get())
            .and_then(handlers::human_stats)
    }

    /// GET /hardware_info
    pub fn hardware_info(
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("hardware_info")
            .and(warp::get())
            .and_then(handlers::hwinfo)
    }

    /// GET /metrics/binary/metadata
    pub fn binary_metadata(
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("metrics" / "binary" / "metadata")
            .and(warp::get())
            .and_then(handlers::binary_metadata)
    }

    /// GET /metrics/binary/readings
    pub fn binary_readings(
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("metrics" / "binary" / "readings")
            .and(warp::get())
            .and_then(handlers::binary_readings)
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
                if let Some(delta) = snapshots.deltas.get(metric.name()) {
                    let percentiles: Vec<f64> = PERCENTILES.iter().map(|(_, p)| *p).collect();

                    if let Ok(result) = delta.percentiles(&percentiles) {
                        for (percentile, value) in result.iter().map(|(p, b)| (p, b.end())) {
                            data.push(format!(
                                "# TYPE {name} gauge\n{name}{{percentile=\"{:02}\"}} {value} {timestamp}",
                                percentile,
                            ));
                        }
                    }
                }
                if config.prometheus().histograms() {
                    if let Some(snapshot) = snapshots.previous.get(metric.name()) {
                        let current = HISTOGRAM_GROUPING_POWER;
                        let target = config.prometheus().histogram_grouping_power();

                        // downsample the snapshot if necessary
                        let downsampled: Option<Snapshot> = if current == target {
                            // the powers matched, we don't need to downsample
                            None
                        } else {
                            Some(snapshot.downsample(target).unwrap())
                        };

                        // reassign to either use the downsampled snapshot or the original
                        let snapshot = if let Some(snapshot) = downsampled.as_ref() {
                            snapshot
                        } else {
                            snapshot
                        };

                        // we need to export a total count (free-running)
                        let mut count = 0;
                        // we also need to export a total sum of all observations
                        // which is also free-running
                        let mut sum = 0;

                        let mut entry = format!("# TYPE {name}_distribution histogram\n");
                        for bucket in snapshot {
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
        let mut data = Vec::new();

        let snapshots = SNAPSHOTS.read().await;

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

            if let Some(counter) = any.downcast_ref::<Counter>() {
                data.push(format!(
                    "{}: {}",
                    metric.formatted(metriken::Format::Simple),
                    counter.value()
                ));
            } else if let Some(gauge) = any.downcast_ref::<Gauge>() {
                data.push(format!(
                    "{}: {}",
                    metric.formatted(metriken::Format::Simple),
                    gauge.value()
                ));
            } else if any.downcast_ref::<AtomicHistogram>().is_some()
                || any.downcast_ref::<RwLockHistogram>().is_some()
            {
                if let Some(delta) = snapshots.deltas.get(metric.name()) {
                    let percentiles: Vec<f64> = PERCENTILES.iter().map(|(_, p)| *p).collect();

                    if let Ok(result) = delta.percentiles(&percentiles) {
                        for (value, label) in result
                            .iter()
                            .map(|(_, b)| b.end())
                            .zip(PERCENTILES.iter().map(|(l, _)| l))
                        {
                            data.push(format!(
                                "{}/{}: {}",
                                metric.formatted(metriken::Format::Simple),
                                label,
                                value
                            ));
                        }
                    }
                }
            }
        }

        data.sort();
        let mut content = data.join("\n");
        content += "\n";
        Ok(content)
    }

    pub async fn binary_metadata() -> Result<impl warp::Reply, Infallible> {
        let mut metadata = Metadata::new();

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

            if let Some(_counter) = any.downcast_ref::<Counter>() {
                metadata.counters.push(MetricMetadata {
                    name: metric.formatted(metriken::Format::Simple),
                });
            } else if let Some(_gauge) = any.downcast_ref::<Gauge>() {
                metadata.gauges.push(MetricMetadata {
                    name: metric.formatted(metriken::Format::Simple),
                });
            } else if let Some(histogram) = any.downcast_ref::<RwLockHistogram>() {
                metadata.histograms.push((
                    MetricMetadata {
                        name: metric.formatted(metriken::Format::Simple),
                    },
                    histogram.config(),
                ));
            }
        }

        let mut buf = Vec::new();
        metadata.serialize(&mut Serializer::new(&mut buf)).unwrap();

        Ok(buf)
    }

    pub async fn binary_readings() -> Result<impl warp::Reply, Infallible> {
        let mut readings = Readings::new();

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

            if let Some(counter) = any.downcast_ref::<Counter>() {
                readings.counters.push(counter.value());
            } else if let Some(gauge) = any.downcast_ref::<Gauge>() {
                readings.gauges.push(gauge.value());
            } else if let Some(histogram) = any.downcast_ref::<RwLockHistogram>() {
                readings.histograms.push(histogram.snapshot());
            }
        }

        let mut buf = Vec::new();
        readings.serialize(&mut Serializer::new(&mut buf)).unwrap();

        Ok(buf)
    }

    pub async fn hwinfo() -> Result<impl warp::Reply, Infallible> {
        if let Ok(hwinfo) = crate::samplers::hwinfo::hardware_info() {
            Ok(warp::reply::json(hwinfo))
        } else {
            Ok(warp::reply::json(&false))
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Metadata {
    counters: Vec<MetricMetadata>,
    gauges: Vec<MetricMetadata>,
    histograms: Vec<(MetricMetadata, histogram::Config)>,
}

impl Metadata {
    pub fn new() -> Self {
        Self {
            counters: Vec::new(),
            gauges: Vec::new(),
            histograms: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct MetricMetadata {
    name: String,
}

#[derive(Serialize, Deserialize)]
pub enum MetricKind {
    Counter,
    Gauge,
    Histogram {
        grouping_power: u8,
        max_value_power: u8,
    },
}

#[derive(Serialize, Deserialize)]
pub struct Readings {
    datetime: DateTime<Utc>,
    unix_ns: u128,
    counters: Vec<u64>,
    gauges: Vec<i64>,
    histograms: Vec<Option<Snapshot>>,
}

impl Readings {
    pub fn new() -> Self {
        let datetime: DateTime<Utc> = Utc::now();

        let unix_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        Self {
            datetime,
            unix_ns,
            counters: Vec::new(),
            gauges: Vec::new(),
            histograms: Vec::new(),
        }
    }
}
