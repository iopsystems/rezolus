use super::*;
use axum::routing::get;
use axum::Router;
use clap::ArgMatches;
use metriken_exposition::Snapshot;
use metriken_exposition::SnapshotV2;
use metriken_exposition::*;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::SystemTime;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::decompression::RequestDecompressionLayer;

static SUMMARIZED: Mutex<Option<SnapshotV2>> = Mutex::new(None);

pub struct Config {
    interval: humantime::Duration,
    verbose: u8,
    url: Url,
    listen: SocketAddr,
}

impl TryFrom<ArgMatches> for Config {
    type Error = String;

    fn try_from(
        args: ArgMatches,
    ) -> Result<Self, <Self as std::convert::TryFrom<clap::ArgMatches>>::Error> {
        Ok(Config {
            url: args.get_one::<Url>("URL").unwrap().clone(),
            listen: *args.get_one::<SocketAddr>("LISTEN").unwrap(),
            verbose: *args.get_one::<u8>("VERBOSE").unwrap_or(&0),
            interval: *args
                .get_one::<humantime::Duration>("INTERVAL")
                .unwrap_or(&humantime::Duration::from_str("1s").unwrap()),
        })
    }
}

pub fn command() -> Command {
    Command::new("summarize")
        .about("Produce and expose summary metrics for a running Rezolus agent")
        .arg(
            clap::Arg::new("URL")
                .help("Rezolus HTTP endpoint")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(Url))
                .required(true)
                .index(1),
        )
        .arg(
            clap::Arg::new("LISTEN")
                .help("IP:Port pair to listen on")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(SocketAddr))
                .required(true)
                .index(2),
        )
        .arg(
            clap::Arg::new("VERBOSE")
                .long("verbose")
                .short('v')
                .help("Increase the verbosity")
                .action(clap::ArgAction::Count),
        )
        .arg(
            clap::Arg::new("INTERVAL")
                .long("interval")
                .short('i')
                .help("Sets the collection interval")
                .action(clap::ArgAction::Set)
                .default_value("1s")
                .value_parser(value_parser!(humantime::Duration)),
        )
}

/// Runs the Rezolus summary tool which is a Rezolus client that pulls data from
/// the msgpack endpoint and exports summary metrics on a Prometheus compatible
/// metrics endpoint. This allows for direct collection of percentile metrics.
/// It also passes counter and gauge metrics through directly.
///
/// This is intended to be run in environments where full histogram collection
/// is not feasible or desirable. By collecting percentiles instead of full
/// histograms, metrics storage requirements are greatly reduced. The compromise
/// is that percentiles generally cannot be meaningfully aggregated across
/// multiple hosts.
pub fn run(config: Config) {
    // configure debug log
    let debug_output: Box<dyn Output> = Box::new(Stderr::new());

    let level = match config.verbose {
        0 => Level::Info,
        1 => Level::Debug,
        _ => Level::Trace,
    };

    let debug_log = if level <= Level::Info {
        LogBuilder::new().format(ringlog::default_format)
    } else {
        LogBuilder::new()
    }
    .output(debug_output)
    .build()
    .expect("failed to initialize debug log");

    let mut log = MultiLogBuilder::new()
        .level_filter(level.to_level_filter())
        .default(debug_log)
        .build()
        .start();

    // initialize async runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    // spawn logging thread
    rt.spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let _ = log.flush();
        }
    });

    ctrlc::set_handler(move || {
        std::process::exit(2);
    })
    .expect("failed to set ctrl-c handler");

    // parse source address
    let mut url = config.url.clone();

    if url.path() != "/" {
        eprintln!("URL should not have an non-root path: {url}");
        std::process::exit(1);
    }

    url.set_path("/metrics/binary");

    // our http client
    let client = match Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error connecting to Rezolus: {e}");
            std::process::exit(1);
        }
    };

    let config = Arc::new(config);

    // launch the HTTP listener
    let c = config.clone();
    rt.spawn(async move { serve(c).await });

    // timed loop to calculate summary metrics
    rt.block_on(async move {
        // sampling interval
        let mut interval = crate::common::aligned_interval(config.interval.into());

        // previous snapshot
        let mut previous = None;

        // sample in a loop
        loop {
            // wait to sample
            interval.tick().await;

            let start = Instant::now();

            // sample rezolus
            if let Ok(response) = client.get(url.clone()).send() {
                if let Ok(body) = response.bytes() {
                    let latency = start.elapsed();

                    debug!("sampling latency: {} us", latency.as_micros());

                    let mut reader = std::io::Cursor::new(body.as_ref());

                    if let Ok(current) =
                        rmp_serde::from_read::<&mut std::io::Cursor<&[u8]>, Snapshot>(&mut reader)
                    {
                        if let Some(previous) = previous.take() {
                            let summarized = summarize(&previous, &current);

                            let mut s = SUMMARIZED.lock();
                            *s = Some(summarized);
                        }

                        previous = Some(current);
                    }
                } else {
                    error!("failed read response. terminating early");
                    break;
                }
            } else {
                error!("failed to get metrics. terminating early");
                break;
            }
        }
    })
}

fn systemtime(snapshot: &Snapshot) -> SystemTime {
    match snapshot {
        Snapshot::V1(s) => s.systemtime,
        Snapshot::V2(s) => s.systemtime,
    }
}

fn counters(snapshot: &Snapshot) -> &Vec<Counter> {
    match snapshot {
        Snapshot::V1(s) => &s.counters,
        Snapshot::V2(s) => &s.counters,
    }
}

fn gauges(snapshot: &Snapshot) -> &Vec<Gauge> {
    match snapshot {
        Snapshot::V1(s) => &s.gauges,
        Snapshot::V2(s) => &s.gauges,
    }
}

fn histograms(snapshot: &Snapshot) -> &Vec<Histogram> {
    match snapshot {
        Snapshot::V1(s) => &s.histograms,
        Snapshot::V2(s) => &s.histograms,
    }
}

fn metadata(snapshot: &Snapshot) -> &HashMap<String, String> {
    match snapshot {
        Snapshot::V1(s) => &s.metadata,
        Snapshot::V2(s) => &s.metadata,
    }
}

fn summarize(previous: &Snapshot, current: &Snapshot) -> SnapshotV2 {
    let mut summarized = SnapshotV2 {
        systemtime: systemtime(current),
        duration: systemtime(current)
            .duration_since(systemtime(previous))
            .unwrap(),
        metadata: metadata(current).clone(),
        counters: Vec::new(),
        gauges: Vec::new(),
        histograms: Vec::new(),
    };

    for curr in counters(current) {
        let mut metadata = curr.metadata.clone();

        // the real metric name is encoded in the metadata
        let name = if let Some(name) = metadata.remove("metric") {
            name.to_string()
        } else {
            continue;
        };

        summarized.counters.push(Counter {
            name,
            value: curr.value,
            metadata,
        })
    }

    for curr in gauges(current) {
        let mut metadata = curr.metadata.clone();

        // the real metric name is encoded in the metadata
        let name = if let Some(name) = metadata.remove("metric") {
            name.to_string()
        } else {
            continue;
        };

        summarized.gauges.push(Gauge {
            name,
            value: curr.value,
            metadata,
        })
    }

    for (prev, curr) in histograms(previous).iter().zip(histograms(current)) {
        let mut metadata = curr.metadata.clone();

        // the real metric name is encoded in the metadata
        let name = if let Some(name) = metadata.remove("metric") {
            name
        } else {
            continue;
        };

        // histograms have extra metadata we should remove
        let _ = metadata.remove("grouping_power");
        let _ = metadata.remove("max_value_power");

        // calculate the delta histogram
        let delta = if let Ok(delta) = curr.value.wrapping_sub(&prev.value) {
            delta
        } else {
            continue;
        };

        if let Ok(Some(percentiles)) = delta.percentiles(&[50.0, 90.0, 99.0, 99.9, 99.99]) {
            for (percentile, value) in percentiles.into_iter().map(|(p, b)| (p, b.end())) {
                if let Ok(value) = value.try_into() {
                    let mut metadata = metadata.clone();
                    metadata.insert("percentile".to_string(), percentile.to_string());

                    summarized.gauges.push(Gauge {
                        name: name.clone(),
                        value,
                        metadata,
                    })
                }
            }
        }
    }

    summarized
}

async fn serve(config: Arc<Config>) {
    let app: Router = app();

    let listener = TcpListener::bind(config.listen)
        .await
        .expect("failed to listen");

    axum::serve(listener, app)
        .await
        .expect("failed to run http server");
}

fn app() -> Router {
    Router::new()
        .route("/", get(root))
        .route("/metrics", get(prometheus))
        .layer(
            ServiceBuilder::new()
                .layer(RequestDecompressionLayer::new())
                .layer(CompressionLayer::new()),
        )
}

async fn prometheus() -> String {
    let summarized = { SUMMARIZED.lock().clone() };

    let mut data = Vec::new();

    if summarized.is_none() {
        return "".to_owned();
    }

    let mut summarized = summarized.unwrap();

    let timestamp = summarized
        .systemtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis();

    for metric in summarized.counters.drain(..) {
        data.push(metric.format(timestamp));
    }

    for metric in summarized.gauges.drain(..) {
        data.push(metric.format(timestamp));
    }

    data.sort();
    data.dedup();
    data.join("\n") + "\n"
}

trait PrometheusFormat {
    fn name(&self) -> &str;
    fn kind(&self) -> &str;
    fn metadata(&self) -> String;
    fn value(&self) -> String;

    fn format(&self, timestamp: u128) -> String {
        let name = self.name();
        let metadata = self.metadata();

        let name_with_metadata = if metadata.is_empty() {
            name.to_string()
        } else {
            format!("{}{{{metadata}}}", name)
        };

        let value = self.value();
        let kind = self.kind();

        format!("# TYPE {name} {kind}\n{name_with_metadata} {value} {timestamp}")
    }
}

impl PrometheusFormat for Counter {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &str {
        "counter"
    }

    fn metadata(&self) -> String {
        format_metadata(&self.metadata)
    }

    fn value(&self) -> String {
        format!("{}", self.value)
    }
}

impl PrometheusFormat for Gauge {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &str {
        "gauge"
    }

    fn metadata(&self) -> String {
        format_metadata(&self.metadata)
    }

    fn value(&self) -> String {
        format!("{}", self.value)
    }
}

fn format_metadata(metadata: &HashMap<String, String>) -> String {
    let mut metadata: Vec<String> = metadata
        .iter()
        .map(|(key, value)| format!("{key}=\"{value}\""))
        .collect();
    metadata.sort();
    metadata.join(", ")
}

async fn root() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!("Rezolus {version}\nFor information, see: https://rezolus.com\n")
}
