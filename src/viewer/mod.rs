use super::*;
use axum::handler::Handler;
use clap::ArgMatches;
use http::StatusCode;
use http::Uri;
use notify::Watcher;
use serde::Serialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use tower_http::services::{ServeDir, ServeFile};
use tower_livereload::LiveReloadLayer;

use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::decompression::RequestDecompressionLayer;

mod queries;
mod tsdb;

use tsdb::*;

pub fn command() -> Command {
    Command::new("view")
        .about("View a Rezolus artifact")
        .arg(
            clap::Arg::new("INPUT")
                .help("Rezolus parquet file")
                .value_parser(value_parser!(PathBuf))
                .action(clap::ArgAction::Set)
                .required(true)
                .index(1),
        )
        .arg(
            clap::Arg::new("VERBOSE")
                .long("verbose")
                .short('v')
                .help("Increase the verbosity")
                .action(clap::ArgAction::Count),
        )
        .arg(
            clap::Arg::new("LISTEN")
                .help("Viewer listen address")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(SocketAddr))
                .required(true)
                .index(2),
        )
        .arg(
            clap::Arg::new("TESTING")
                .long("testing")
                .short('t')
                .help("Use testing data")
                .action(clap::ArgAction::SetTrue),
        )
}

pub struct Config {
    input: PathBuf,
    verbose: u8,
    listen: SocketAddr,
    testing: bool,
}

impl TryFrom<ArgMatches> for Config {
    type Error = String;

    fn try_from(
        args: ArgMatches,
    ) -> Result<Self, <Self as std::convert::TryFrom<clap::ArgMatches>>::Error> {
        Ok(Config {
            input: args.get_one::<PathBuf>("INPUT").unwrap().to_path_buf(),
            verbose: *args.get_one::<u8>("VERBOSE").unwrap_or(&0),
            listen: *args.get_one::<SocketAddr>("LISTEN").unwrap(),
            testing: *args.get_one::<bool>("TESTING").unwrap_or(&false),
        })
    }
}

/// Runs the Rezolus exporter tool which is a Rezolus client that pulls data
/// from the msgpack endpoint and exports summary metrics on a Prometheus
/// compatible metrics endpoint. This allows for direct collection of percentile
/// metrics and/or full histograms with counter and gauge metrics passed through
/// directly.
pub fn run(config: Config) {
    // load config from file
    let config: Arc<Config> = config.into();

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

    // code to load data from parquet will go here
    let mut state = AppState::new(config.clone());

    if !config.testing {
        let data = Tsdb::load(&config.input)
            .map_err(|e| {
                eprintln!("failed to load data from parquet: {e}");
                std::process::exit(1);
            })
            .unwrap();

        // define our sections
        let sections = vec![
            Section {
                name: "Overview".to_string(),
                route: "/overview".to_string(),
            },
            Section {
                name: "CPU".to_string(),
                route: "/cpu".to_string(),
            },
            Section {
                name: "Network".to_string(),
                route: "/network".to_string(),
            },
        ];

        // define views for each section
        let mut overview = View::new(sections.clone());
        let mut cpu = View::new(sections.clone());
        let mut network = View::new(sections.clone());

        // CPU

        // CPU Utilization

        let mut cpu_overview = Group::new("CPU", "cpu");
        let mut utilization = Group::new("Utilization", "utilization");

        let opts = PlotOpts::line("Busy %", "busy-pct");
        let series = data
            .sum("cpu_usage", Labels::default())
            .map(|v| v.divide_scalar(1000000000.0));
        cpu_overview.plot(opts.clone(), series.clone());
        utilization.plot(opts.clone(), series.clone());

        let plot = Plot::heatmap(
            "Busy %",
            "busy-pct-heatmap",
            queries::cpu_usage_heatmap(&data, Labels::default()),
        );
        cpu_overview.push(plot.clone());
        utilization.push(plot.clone());

        for (label, id, state) in &[
            ("User %", "user-pct", "user"),
            ("System %", "system-pct", "system"),
            ("Soft IRQ %", "softirq-pct", "softirq"),
        ] {
            let opts = PlotOpts::line(*label, *id);
            utilization.plot(
                opts,
                data.sum("cpu_usage", [("state", *state)])
                    .map(|v| v.divide_scalar(1000000000.0)),
            );

            utilization.push(Plot::heatmap(
                *label,
                format!("{id}-heatmap"),
                queries::cpu_usage_heatmap(&data, [("state", *state)]),
            ));
        }

        overview.groups.push(cpu_overview);
        cpu.groups.push(utilization);

        // CPU Performance

        let mut performance = Group::new("Performance", "performance");

        let opts = PlotOpts::line("Instructions per Cycle (IPC)", "ipc");
        if let (Some(cycles), Some(mut instructions)) = (
            data.sum("cpu_cycles", Labels::default()),
            data.sum("cpu_instructions", Labels::default()),
        ) {
            instructions.divide(&cycles);
            performance.plot(opts, Some(instructions));
        }

        performance.push(Plot::heatmap(
            "Instructions per Cycle (IPC)",
            "ipc-heatmap",
            queries::cpu_ipc_heatmap(&data),
        ));

        cpu.groups.push(performance);

        // CPU TLB

        let mut tlb = Group::new("TLB", "tlb");

        let opts = PlotOpts::line("Total", "tlb-total");
        tlb.plot(opts, data.sum("cpu_tlb_flush", Labels::default()));

        // tlb.push(Plot::line("Total", "tlb-total", queries::get_sum(&data, "cpu_tlb_flush", Labels::default())));

        let heatmap = queries::get_cpu_heatmap(&data, "cpu_tlb_flush", Labels::default());

        if !(heatmap.is_empty()) {
            tlb.plots.push(Plot::heatmap("Total", "tlb-total", heatmap));
        }

        for (label, id, reason) in &[
            (
                "Local MM Shootdown",
                "tlb-local-mm-shootdown",
                "local_mm_shootdown",
            ),
            ("Remote Send IPI", "tlb-remote-send-ipi", "remote_send_ipi"),
            (
                "Remote Shootdown",
                "tlb-remote-shootdown",
                "remote_shootdown",
            ),
            ("Task Switch", "tlb-task-switch", "task_switch"),
        ] {
            let opts = PlotOpts::line(*label, *id);
            tlb.plot(opts, data.sum("cpu_tlb_flush", [("reason", *reason)]));

            let heatmap = queries::get_cpu_heatmap(&data, "cpu_tlb_flush", [("reason", *reason)]);
            tlb.plots.push(Plot::heatmap(
                label.to_string(),
                format!("{id}-heatmap"),
                heatmap,
            ));
        }

        cpu.groups.push(tlb);

        // Network overview

        let mut network_overview = Group::new("Network", "network");
        let mut traffic = Group::new("Traffic", "traffic");

        let opts = PlotOpts::line("Bandwidth Transmit", "bandwidth-tx");
        let series = data
            .sum("network_bytes", [("direction", "transmit")])
            .map(|v| v.multiply_scalar(8.0));

        network_overview.plot(opts.clone(), series.clone());
        traffic.plot(opts, series);

        let opts = PlotOpts::line("Bandwidth Receive", "bandwidth-rx");
        let series = data
            .sum("network_bytes", [("direction", "receive")])
            .map(|v| v.multiply_scalar(8.0));

        network_overview.plot(opts.clone(), series.clone());
        traffic.plot(opts, series);

        let opts = PlotOpts::line("Packets Transmit", "packets-tx");
        let series = data.sum("network_packets", [("direction", "transmit")]);

        network_overview.plot(opts.clone(), series.clone());
        traffic.plot(opts, series);

        let opts = PlotOpts::line("Packets Receive", "packets-rx");
        let series = data.sum("network_packets", [("direction", "receive")]);

        network_overview.plot(opts.clone(), series.clone());
        traffic.plot(opts, series);

        overview.groups.push(network_overview);
        network.groups.push(traffic);

        state.sections.insert(
            "overview.json".to_string(),
            serde_json::to_string(&overview).unwrap(),
        );
        state
            .sections
            .insert("cpu.json".to_string(), serde_json::to_string(&cpu).unwrap());
        state.sections.insert(
            "network.json".to_string(),
            serde_json::to_string(&network).unwrap(),
        );
    }

    // launch the HTTP listener
    let c = config.clone();
    rt.block_on(async move { serve(c, state).await });

    std::thread::sleep(Duration::from_millis(200));
}

async fn serve(config: Arc<Config>, state: AppState) {
    let livereload = LiveReloadLayer::new();
    let reloader = livereload.reloader();

    let mut watcher = notify::recommended_watcher(move |_| reloader.reload())
        .expect("failed to initialize watcher");
    watcher
        .watch(
            Path::new("src/viewer/assets"),
            notify::RecursiveMode::Recursive,
        )
        .expect("failed to watch assets folder");

    let app = app(livereload, state);

    let listener = TcpListener::bind(config.listen)
        .await
        .expect("failed to listen");

    axum::serve(listener, app)
        .await
        .expect("failed to run http server");
}

struct AppState {
    config: Arc<Config>,
    sections: HashMap<String, String>,
}

impl AppState {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            sections: Default::default(),
        }
    }
}

// NOTE: we're going to want to include the assets in the binary for release
// builds. For now, we just serve from the assets folder
fn app(livereload: LiveReloadLayer, state: AppState) -> Router {
    let state = Arc::new(state);

    Router::new()
        .route_service("/", ServeFile::new("src/viewer/assets/index.html"))
        .route("/about", get(about))
        .with_state(state.clone())
        .nest_service("/lib", ServeDir::new(Path::new("src/viewer/assets/lib")))
        .nest_service("/data", data.with_state(state))
        .fallback_service(ServeFile::new("src/viewer/assets/index.html"))
        .layer(
            ServiceBuilder::new()
                .layer(RequestDecompressionLayer::new())
                .layer(CompressionLayer::new())
                .layer(livereload),
        )
}

// Basic /about page handler
async fn about() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!("Rezolus {version} Viewer\nFor information, see: https://rezolus.com\n")
}

async fn data(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    uri: Uri,
) -> (StatusCode, String) {
    let path = uri.path();
    let parts: Vec<&str> = path.split('/').collect();

    if state.config.testing {
        (
            StatusCode::OK,
            std::fs::read_to_string(format!("src/viewer/assets/data/{}", parts[1]))
                .unwrap_or("{ }".to_string()),
        )
    } else {
        (
            StatusCode::OK,
            state
                .sections
                .get(parts[1])
                .map(|v| v.to_string())
                .unwrap_or("{ }".to_string()),
        )
    }
}

#[derive(Default, Serialize)]
pub struct View {
    groups: Vec<Group>,
    sections: Vec<Section>,
}

impl View {
    pub fn new(sections: Vec<Section>) -> Self {
        Self {
            groups: Vec::new(),
            sections,
        }
    }
}

#[derive(Clone, Serialize)]
pub struct Section {
    name: String,
    route: String,
}

#[derive(Serialize)]
pub struct Group {
    name: String,
    id: String,
    plots: Vec<Plot>,
}

impl Group {
    pub fn new<T: Into<String>, U: Into<String>>(name: T, id: U) -> Self {
        Self {
            name: name.into(),
            id: id.into(),
            plots: Vec::new(),
        }
    }

    /// Adds a plot to a group if it is not empty
    pub fn push(&mut self, plot: Plot) {
        if !plot.data.is_empty() {
            self.plots.push(plot);
        }
    }

    pub fn plot(&mut self, opts: PlotOpts, series: Option<TimeSeries>) {
        if let Some(data) = series.map(|v| v.as_data()) {
            self.plots.push(Plot { opts, data })
        }
    }
}

#[derive(Serialize, Clone)]
pub struct Plot {
    data: Vec<Vec<f64>>,
    opts: PlotOpts,
}

impl Plot {
    pub fn heatmap<T: Into<String>, U: Into<String>>(title: T, id: U, data: Vec<Vec<f64>>) -> Self {
        Self {
            data,
            opts: PlotOpts::heatmap(title, id),
        }
    }
}

#[derive(Serialize, Clone)]
pub struct PlotOpts {
    title: String,
    id: String,
    style: String,
}

impl PlotOpts {
    pub fn heatmap<T: Into<String>, U: Into<String>>(title: T, id: U) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "heatmap".to_string(),
        }
    }

    pub fn line<T: Into<String>, U: Into<String>>(title: T, id: U) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "line".to_string(),
        }
    }
}
