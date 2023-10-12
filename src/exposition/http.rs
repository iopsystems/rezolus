use crate::PERCENTILES;
use metriken::{AtomicHistogram, Counter, Gauge, RwLockHistogram};

use warp::Filter;

/// HTTP exposition
pub async fn http() {
    let http = filters::http();

    warp::serve(http).run(([0, 0, 0, 0], 4242)).await;
}

mod filters {
    use super::*;

    /// The combined set of http endpoint filters
    pub fn http() -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        prometheus_stats().or(human_stats()).or(hardware_info())
    }

    /// GET /metrics
    pub fn prometheus_stats(
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("metrics")
            .and(warp::get())
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
}

mod handlers {
    use super::*;
    use crate::SNAPSHOTS;
    use core::convert::Infallible;

    pub async fn prometheus_stats() -> Result<impl warp::Reply, Infallible> {
        let mut data = Vec::new();

        let snapshots = SNAPSHOTS.read().await;

        let previous = snapshots.front();
        let current = snapshots.back();

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
                if metric.metadata().is_empty() {
                    data.push(format!(
                        "# TYPE {}_total counter\n{}_total {}",
                        metric.name(),
                        metric.name(),
                        counter.value()
                    ));
                } else {
                    data.push(format!(
                        "# TYPE {} counter\n{} {}",
                        metric.name(),
                        metric.formatted(metriken::Format::Prometheus),
                        counter.value()
                    ));
                }
            } else if let Some(gauge) = any.downcast_ref::<Gauge>() {
                data.push(format!(
                    "# TYPE {} gauge\n{} {}",
                    metric.name(),
                    metric.formatted(metriken::Format::Prometheus),
                    gauge.value()
                ));
            } else if any.downcast_ref::<AtomicHistogram>().is_some()
                || any.downcast_ref::<RwLockHistogram>().is_some()
            {
                if current.is_none() {
                    continue;
                }

                let snapshot = match (
                    current.unwrap().get(metric.name()),
                    previous.map(|p| p.get(metric.name())),
                ) {
                    (Some(current), Some(Some(previous))) => {
                        if snapshots.len() < 61 {
                            current.clone()
                        } else {
                            current.wrapping_sub(previous).unwrap()
                        }
                    }
                    (Some(current), Some(None)) | (Some(current), None) => current.clone(),
                    _ => {
                        continue;
                    }
                };

                let percentiles: Vec<f64> = PERCENTILES.iter().map(|(_, p)| *p).collect();

                if let Ok(result) = snapshot.percentiles(&percentiles) {
                    for (percentile, value) in result.iter().map(|(p, b)| (p, b.end())) {
                        data.push(format!(
                            "# TYPE {} gauge\n{}{{percentile=\"{:02}\"}} {}",
                            metric.name(),
                            metric.name(),
                            percentile,
                            value,
                        ));
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

        let previous = snapshots.front();
        let current = snapshots.back();

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
                if current.is_none() {
                    continue;
                }

                let snapshot = match (
                    current.unwrap().get(metric.name()),
                    previous.map(|p| p.get(metric.name())),
                ) {
                    (Some(current), Some(Some(previous))) => {
                        if snapshots.len() < 61 {
                            current.clone()
                        } else {
                            current.wrapping_sub(previous).unwrap()
                        }
                    }
                    (Some(current), Some(None)) | (Some(current), None) => current.clone(),
                    _ => {
                        continue;
                    }
                };

                let percentiles: Vec<f64> = PERCENTILES.iter().map(|(_, p)| *p).collect();

                if let Ok(result) = snapshot.percentiles(&percentiles) {
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

        data.sort();
        let mut content = data.join("\n");
        content += "\n";
        Ok(content)
    }

    pub async fn hwinfo() -> Result<impl warp::Reply, Infallible> {
        if let Ok(hwinfo) = crate::samplers::hwinfo::hardware_info() {
            Ok(warp::reply::json(hwinfo))
        } else {
            Ok(warp::reply::json(&false))
        }
    }
}
