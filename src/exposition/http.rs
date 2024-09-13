use crate::common::HISTOGRAM_GROUPING_POWER;
use crate::Arc;
use crate::Config;
use crate::PERCENTILES;
use crate::SNAPSHOTS;
use axum::extract::State;
use axum::routing::get;
use axum::Router;
use histogram::AtomicHistogram;
use metriken::Counter;
use metriken::Gauge;
use metriken::RwLockHistogram;
use std::time::UNIX_EPOCH;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::{compression::CompressionLayer, decompression::RequestDecompressionLayer};

struct AppState {
    config: Arc<Config>,
}

pub async fn serve(config: Arc<Config>) {
    let state = Arc::new(AppState { config });

    let app: Router = app(state);

    let listener = TcpListener::bind("0.0.0.0:4242")
        .await
        .expect("failed to listen");

    axum::serve(listener, app)
        .await
        .expect("failed to run http server");
}

fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/admin/metrics.json", get(json))
        .route("/metrics", get(prometheus))
        .route("/metrics/binary", get(msgpack))
        .route("/vars", get(human_readable))
        .route("/vars.json", get(json))
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(RequestDecompressionLayer::new())
                .layer(CompressionLayer::new()),
        )
}

async fn human_readable(State(_state): State<Arc<AppState>>) -> String {
    let data = simple_stats(false).await;

    let mut content = data.join("\n");
    content += "\n";

    content
}

async fn json(State(_state): State<Arc<AppState>>) -> String {
    let data = simple_stats(true).await;

    let mut content = "{".to_string();
    content += &data.join(", ");
    content += "}";

    content
}

async fn msgpack(State(_state): State<Arc<AppState>>) -> Vec<u8> {
    let snapshot = metriken_exposition::SnapshotterBuilder::new()
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

    metriken_exposition::Snapshot::to_msgpack(&snapshot).unwrap()
}

async fn prometheus(State(state): State<Arc<AppState>>) -> String {
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
            if state.config.prometheus().histograms() {
                if let Some(histogram) = snapshots.previous.get(metric.name()) {
                    let current = HISTOGRAM_GROUPING_POWER;
                    let target = state.config.prometheus().histogram_grouping_power();

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

                    entry +=
                        &format!("{name}_distribution_bucket{{le=\"+Inf\"}} {count} {timestamp}\n");
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
    parts.join("_")
}

async fn root() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!("Rezolus {version}\nFor information, see: https://rezolus.com\n")
}

// gathers up the metrics into a simple format that can be presented as human
// readable metrics or transformed into JSON
async fn simple_stats(quoted: bool) -> Vec<String> {
    let mut data = Vec::new();

    let snapshots = SNAPSHOTS.read().await;

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
