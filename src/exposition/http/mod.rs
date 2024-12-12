use crate::common::HISTOGRAM_GROUPING_POWER;
use crate::{Arc, Config, Sampler};
use axum::extract::State;
use axum::routing::get;
use axum::Router;
use metriken::{AtomicHistogram, RwLockHistogram, Value};
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::{compression::CompressionLayer, decompression::RequestDecompressionLayer};

struct AppState {
    config: Arc<Config>,
    samplers: Arc<Box<[Box<dyn Sampler>]>>,
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

async fn human_readable(State(state): State<Arc<AppState>>) -> String {
    refresh(&state.samplers).await;

    let data = simple_stats(false);

    let mut content = data.join("\n");
    content += "\n";

    content
}

async fn json(State(state): State<Arc<AppState>>) -> String {
    refresh(&state.samplers).await;

    let data = simple_stats(true);

    let mut content = "{".to_string();
    content += &data.join(", ");
    content += "}";

    content
}

async fn msgpack(State(state): State<Arc<AppState>>) -> Vec<u8> {
    refresh(&state.samplers).await;

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
    refresh(&state.samplers).await;

    let timestamp = clocksource::precise::UnixInstant::EPOCH
        .elapsed()
        .as_millis();

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

        if name == "cpu/usage" && metric.metadata().get("state") == Some("busy") {
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
                continue;
            }
            Some(Value::Gauge(value)) => {
                data.push(format!(
                    "# TYPE {name} gauge\n{name_with_metadata} {value} {timestamp}"
                ));
                continue;
            }
            Some(_) => {}
            None => {
                continue;
            }
        }

        let any = match metric.as_any() {
            Some(any) => any,
            None => {
                continue;
            }
        };

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
                    entry += &format!("{name}_distribution_sum {sum} {timestamp}");

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

async fn refresh(samplers: &[Box<dyn Sampler>]) {
    let s: Vec<_> = samplers.iter().map(|s| s.refresh()).collect();

    futures::future::join_all(s).await;
}

fn simple_stats(quoted: bool) -> Vec<String> {
    let mut data = Vec::new();

    let q = if quoted { "\"" } else { "" };

    for metric in &metriken::metrics() {
        let value = metric.value();

        if value.is_none() {
            continue;
        }

        if metric.name().starts_with("log_") {
            continue;
        }

        let simple_name = metric.formatted(metriken::Format::Simple);

        match value {
            Some(Value::Counter(value)) => {
                data.push(format!("{q}{simple_name}{q}: {value}"));
            }
            Some(Value::Gauge(value)) => {
                data.push(format!("{q}{simple_name}{q}: {value}"));
            }
            Some(_) | None => {}
        }
    }

    data.sort();
    data
}
