use crate::common::*;
use crate::debug;
use crate::{Arc, Config, Sampler};
use axum::extract::State;
use axum::routing::get;
use axum::Router;
use metriken::{RwLockHistogram, Value};
use std::time::Instant;
use std::time::SystemTime;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::{compression::CompressionLayer, decompression::RequestDecompressionLayer};

mod snapshot;

struct AppState {
    config: Arc<Config>,
    samplers: Arc<Box<[Box<dyn Sampler>]>>,
}

impl AppState {
    async fn refresh(&self) {
        let s: Vec<_> = self.samplers.iter().map(|s| s.refresh()).collect();

        let start = Instant::now();
        futures::future::join_all(s).await;
        let duration = start.elapsed().as_micros();
        debug!("sampling latency: {duration} us");
    }
}

pub async fn serve(config: Arc<Config>, samplers: Arc<Box<[Box<dyn Sampler>]>>) {
    let state = Arc::new(AppState {
        config: config.clone(),
        samplers,
    });

    let app: Router = app(state);

    let listener = TcpListener::bind(config.general().listen())
        .await
        .expect("failed to listen");

    axum::serve(listener, app)
        .await
        .expect("failed to run http server");
}

fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/metrics", get(prometheus))
        .route("/metrics/binary", get(msgpack))
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(RequestDecompressionLayer::new())
                .layer(CompressionLayer::new()),
        )
}

async fn msgpack(State(state): State<Arc<AppState>>) -> Vec<u8> {
    let ts = SystemTime::now();
    let start = Instant::now();

    state.refresh().await;

    let duration = start.elapsed();

    let snapshot = snapshot::create(ts, duration);

    rmp_serde::encode::to_vec(&snapshot).expect("failed to serialize snapshot")
}

async fn prometheus(State(state): State<Arc<AppState>>) -> String {
    let timestamp = clocksource::precise::UnixInstant::EPOCH
        .elapsed()
        .as_millis();

    state.refresh().await;

    let mut data = Vec::new();

    for metric in &metriken::metrics() {
        let value = metric.value();

        if value.is_none() {
            continue;
        }

        let name = metric.name();

        if name.starts_with("log_") {
            continue;
        }

        let metadata: Vec<String> = metric
            .metadata()
            .iter()
            .map(|(key, value)| format!("{key}=\"{value}\""))
            .collect();
        let metadata = metadata.join(", ");

        let name_with_metadata = if metadata.is_empty() {
            metric.name().to_string()
        } else {
            format!("{}{{{metadata}}}", metric.name())
        };

        match value {
            Some(Value::Counter(value)) => {
                data.push(format!(
                    "# TYPE {name} counter\n{name_with_metadata} {value} {timestamp}"
                ));
            }
            Some(Value::Gauge(value)) => {
                data.push(format!(
                    "# TYPE {name} gauge\n{name_with_metadata} {value} {timestamp}"
                ));
            }
            Some(Value::Other(any)) => {
                if let Some(histogram) = any.downcast_ref::<RwLockHistogram>() {
                    if state.config.prometheus().histograms() {
                        if let Some(histogram) = histogram.load() {
                            let current = HISTOGRAM_GROUPING_POWER;
                            let target = state.config.prometheus().histogram_grouping_power();

                            // downsample the histogram if necessary
                            let downsampled: Option<histogram::Histogram> = if current == target {
                                // the powers matched, we don't need to downsample
                                None
                            } else {
                                Some(histogram.downsample(target).unwrap())
                            };

                            // reassign to either use the downsampled histogram or the original
                            let histogram = if let Some(histogram) = downsampled.as_ref() {
                                histogram
                            } else {
                                &histogram
                            };

                            let metadata: Vec<String> = metric
                                .metadata()
                                .iter()
                                .map(|(key, value)| format!("{key}=\"{value}\""))
                                .collect();

                            let metadata = metadata.join(", ");

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

                                if metadata.is_empty() {
                                    entry += &format!(
                                        "{name}_distribution_bucket{{le=\"{}\"}} {count} {timestamp}\n",
                                        bucket.end()
                                    );
                                } else {
                                    entry += &format!(
                                        "{name}_distribution_bucket{{{metadata}, le=\"{}\"}} {count} {timestamp}\n",
                                        bucket.end()
                                    );
                                }
                            }

                            if metadata.is_empty() {
                                entry += &format!(
                                    "{name}_distribution_bucket{{le=\"+Inf\"}} {count} {timestamp}\n"
                                );
                                entry +=
                                    &format!("{name}_distribution_count {count} {timestamp}\n");
                                entry += &format!("{name}_distribution_sum {sum} {timestamp}");
                            } else {
                                entry += &format!(
                                    "{name}_distribution_bucket{{{metadata}, le=\"+Inf\"}} {count} {timestamp}\n"
                                );
                                entry += &format!(
                                    "{name}_distribution_count{{{metadata}}} {count} {timestamp}\n"
                                );
                                entry += &format!(
                                    "{name}_distribution_sum{{{metadata}}} {sum} {timestamp}"
                                );
                            }

                            data.push(entry);
                        }
                    }
                } else if let Some(counters) = any.downcast_ref::<CounterGroup>() {
                    if let Some(c) = counters.load() {
                        let mut entry = format!("# TYPE {name} counter");

                        let metadata: Vec<String> = metric
                            .metadata()
                            .iter()
                            .map(|(key, value)| format!("{key}=\"{value}\""))
                            .collect();

                        let metadata = metadata.join(", ");

                        for (id, value) in c.iter().enumerate() {
                            if *value == 0 {
                                continue;
                            }

                            let counter_metadata: Vec<String> =
                                if let Some(md) = counters.load_metadata(id) {
                                    md.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect()
                                } else {
                                    Vec::new()
                                };

                            let counter_metadata = counter_metadata.join(", ");

                            if metadata.is_empty() && counter_metadata.is_empty() {
                                entry += &format!("\n{name}{{id=\"{id}\"}} {value} {timestamp}");
                            } else if counter_metadata.is_empty() {
                                entry += &format!(
                                    "\n{name}{{{metadata}, id=\"{id}\"}} {value} {timestamp}"
                                );
                            } else if metadata.is_empty() {
                                entry += &format!(
                                    "\n{name}{{{counter_metadata}, id=\"{id}\"}} {value} {timestamp}"
                                );
                            } else {
                                entry += &format!(
                                    "\n{name}{{{metadata}, {counter_metadata}, id=\"{id}\"}} {value} {timestamp}"
                                );
                            }
                        }

                        data.push(entry);
                    }
                } else if let Some(gauges) = any.downcast_ref::<GaugeGroup>() {
                    if let Some(g) = gauges.load() {
                        let mut entry = format!("# TYPE {name} gauge");

                        let metadata: Vec<String> = metric
                            .metadata()
                            .iter()
                            .map(|(key, value)| format!("{key}=\"{value}\""))
                            .collect();

                        let metadata = metadata.join(", ");

                        for (id, value) in g.iter().enumerate() {
                            if *value == i64::MIN {
                                continue;
                            }

                            let counter_metadata: Vec<String> =
                                if let Some(md) = gauges.load_metadata(id) {
                                    md.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect()
                                } else {
                                    Vec::new()
                                };

                            let counter_metadata = counter_metadata.join(", ");

                            if metadata.is_empty() && counter_metadata.is_empty() {
                                entry += &format!("\n{name}{{id=\"{id}\"}} {value} {timestamp}");
                            } else if counter_metadata.is_empty() {
                                entry += &format!(
                                    "\n{name}{{{metadata}, id=\"{id}\"}} {value} {timestamp}"
                                );
                            } else if metadata.is_empty() {
                                entry += &format!(
                                    "\n{name}{{{counter_metadata}, id=\"{id}\"}} {value} {timestamp}"
                                );
                            } else {
                                entry += &format!(
                                    "\n{name}{{{metadata}, {counter_metadata}, id=\"{id}\"}} {value} {timestamp}"
                                );
                            }
                        }

                        data.push(entry);
                    }
                }
            }
            _ => {}
        }
    }

    data.sort();
    data.dedup();
    data.join("\n") + "\n"
}

async fn root() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!("Rezolus {version}\nFor information, see: https://rezolus.com\n")
}
