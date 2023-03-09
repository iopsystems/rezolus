use crate::PERCENTILES;
use warp::Filter;
use metriken::{Counter, Gauge, Heatmap};
use crate::samplers::hwinfo::Hwinfo;
use std::sync::Arc;

/// The HTTP admin server.
pub async fn http() {
    let admin = filters::admin();

    warp::serve(admin).run(([0, 0, 0, 0], 4242)).await;
}

mod filters {
    use super::*;

    /// The combined set of admin endpoint filters
    pub fn admin(
        // ratelimit: Option<Arc<Ratelimiter>>,
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        let hwinfo = Hwinfo::new().ok().map(Arc::new);

        prometheus_stats()
            .or(human_stats())
            .or(hardware_info(hwinfo))
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

    pub fn hardware_info(
        hwinfo: Option<Arc<Hwinfo>>,
    ) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
        warp::path!("hardware_info")
            .and(warp::get())
            .and(with_hwinfo(hwinfo))
            .and_then(handlers::hwinfo)
    }

    fn with_hwinfo(
        hwinfo: Option<Arc<Hwinfo>>,
    ) -> impl Filter<Extract = (Option<Arc<Hwinfo>>,), Error = std::convert::Infallible> + Clone
    {
        warp::any().map(move || hwinfo.clone())
    }
}

mod handlers {
    use super::*;
    use core::convert::Infallible;
    use warp::http::StatusCode;

    pub async fn prometheus_stats() -> Result<impl warp::Reply, Infallible> {
        let mut data = Vec::new();

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
                    "# TYPE {} counter\n{} {}",
                    metric.name(),
                    metric.name(),
                    counter.value()
                ));
            } else if let Some(gauge) = any.downcast_ref::<Gauge>() {
                data.push(format!(
                    "# TYPE {} gauge\n{} {}",
                    metric.name(),
                    metric.name(),
                    gauge.value()
                ));
            } else if let Some(heatmap) = any.downcast_ref::<Heatmap>() {
                for (_label, percentile) in PERCENTILES {
                    let value = heatmap
                        .percentile(*percentile)
                        .map(|b| b.high())
                        .unwrap_or(0);
                    data.push(format!(
                        "# TYPE {} gauge\n{}{{percentile=\"{:02}\"}} {}",
                        metric.name(),
                        metric.name(),
                        percentile,
                        value
                    ));
                }
            }
        }

        data.sort();
        let mut content = data.join("\n");
        content += "\n";
        let parts: Vec<&str> = content.split('/').collect();
        Ok(parts.join("_"))
    }

    pub async fn human_stats() -> Result<impl warp::Reply, Infallible> {
        let mut data = Vec::new();

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
                data.push(format!("{}: {}", metric.name(), counter.value()));
            } else if let Some(gauge) = any.downcast_ref::<Gauge>() {
                data.push(format!("{}: {}", metric.name(), gauge.value()));
            } else if let Some(heatmap) = any.downcast_ref::<Heatmap>() {
                for (label, p) in PERCENTILES {
                    let percentile = heatmap.percentile(*p).map(|b| b.high()).unwrap_or(0);
                    data.push(format!("{}/{}: {}", metric.name(), label, percentile));
                }
            }
        }

        data.sort();
        let mut content = data.join("\n");
        content += "\n";
        Ok(content)
    }

    pub async fn hwinfo(
        hwinfo: Option<Arc<Hwinfo>>,
    ) -> Result<impl warp::Reply, Infallible> {
        if let Some(hwinfo) = hwinfo {
            Ok(warp::reply::json(&*hwinfo))
        } else {
            // Ok(warp::reply::json(()))
            Ok(warp::reply::json(&false))
        }
    }
}

// pub async fn http() {
//     let root = warp::path::end().map(|| "rezolus");

//     let vars = warp::path("vars").map(human_stats);

//     let metrics = warp::path("metrics").map(prometheus_stats);

//     let routes = warp::get().and(
//         root
//             .or(vars)
//             .or(metrics),
//     );

//     warp::serve(routes).run(([0, 0, 0, 0], 4242)).await;

// }

// fn hwinfo() -> String {
//     let hwinfo = Hwinfo::new();
// }

// fn prometheus_stats() -> String {
//     let mut data = Vec::new();

//     for metric in &metriken::metrics() {
//         let any = match metric.as_any() {
//             Some(any) => any,
//             None => {
//                 continue;
//             }
//         };

//         if metric.name().starts_with("log_") {
//             continue;
//         }

//         if let Some(counter) = any.downcast_ref::<Counter>() {
//             data.push(format!("# TYPE {} counter\n{} {}", metric.name(), metric.name(), counter.value()));
//         } else if let Some(gauge) = any.downcast_ref::<Gauge>() {
//             data.push(format!("# TYPE {} gauge\n{} {}", metric.name(), metric.name(), gauge.value()));
//         } else if let Some(heatmap) = any.downcast_ref::<Heatmap>() {
//             for (_label, percentile) in PERCENTILES {
//                 let value = heatmap.percentile(*percentile).map(|b| b.high()).unwrap_or(0);
//                 data.push(format!("# TYPE {} gauge\n{}{{percentile=\"{:02}\"}} {}", metric.name(), metric.name(), percentile, value));
//             }
//         }
//     }

//     data.sort();
//     let mut content = data.join("\n");
//     content += "\n";
//     let parts: Vec<&str> = content.split('/').collect();
//     parts.join("_")
// }

// fn human_stats() -> String {
//     let mut data = Vec::new();

//     for metric in &metriken::metrics() {
//         let any = match metric.as_any() {
//             Some(any) => any,
//             None => {
//                 continue;
//             }
//         };

//         if metric.name().starts_with("log_") {
//             continue;
//         }

//         if let Some(counter) = any.downcast_ref::<Counter>() {
//             data.push(format!("{}: {}", metric.name(), counter.value()));
//         } else if let Some(gauge) = any.downcast_ref::<Gauge>() {
//             data.push(format!("{}: {}", metric.name(), gauge.value()));
//         } else if let Some(heatmap) = any.downcast_ref::<Heatmap>() {
//             for (label, p) in PERCENTILES {
//                 let percentile = heatmap.percentile(*p).map(|b| b.high()).unwrap_or(0);
//                 data.push(format!("{}/{}: {}", metric.name(), label, percentile));
//             }
//         }
//     }

//     data.sort();
//     let mut content = data.join("\n");
//     content += "\n";
//     content
// }