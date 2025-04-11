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

const PERCENTILES: &[f64] = &[50.0, 90.0, 99.0, 99.9, 99.99];

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
            Section {
                name: "Scheduler".to_string(),
                route: "/scheduler".to_string(),
            },
            Section {
                name: "Syscall".to_string(),
                route: "/syscall".to_string(),
            },
            Section {
                name: "Softirq".to_string(),
                route: "/softirq".to_string(),
            },
            Section {
                name: "BlockIO".to_string(),
                route: "/blockio".to_string(),
            },
        ];

        // define views for each section
        let mut overview = View::new(sections.clone());
        let mut cpu = View::new(sections.clone());
        let mut network = View::new(sections.clone());
        let mut scheduler = View::new(sections.clone());
        let mut syscall = View::new(sections.clone());
        let mut softirq = View::new(sections.clone());
        let mut blockio = View::new(sections.clone());

        // CPU

        // CPU Utilization

        let mut cpu_overview = Group::new("CPU", "cpu");
        let mut utilization = Group::new("Utilization", "utilization");

        let opts = PlotOpts::line("Busy %", "busy-pct")
            .with_axis_label("Utilization")
            .with_unit_system("percentage");
        let series = data
            .cpu_avg("cpu_usage", Labels::default())
            .map(|v| (v / 1000000000.0));
        cpu_overview.plot(opts.clone(), series.clone());
        utilization.plot(opts.clone(), series.clone());

        let opts = PlotOpts::heatmap("Busy %", "busy-pct-heatmap")
            .with_axis_label("CPU")
            .with_unit_system("percentage");
        let series = data
            .cpu_heatmap("cpu_usage", Labels::default())
            .map(|v| v / 1000000000.0);
        cpu_overview.heatmap_echarts(opts.clone(), series.clone());
        utilization.heatmap_echarts(opts.clone(), series.clone());

        for state in &["User", "System", "SoftIRQ"] {
            let opts = PlotOpts::line(
                format!("{state} %"),
                format!("{}-pct", state.to_lowercase()),
            );
            utilization.plot(
                opts,
                data.cpu_avg("cpu_usage", [("state", state.to_lowercase())])
                    .map(|v| (v / 1000000000.0)),
            );

            let opts = PlotOpts::heatmap(
                format!("{state} %"),
                format!("{}-pct-heatmap", state.to_lowercase()),
            );
            utilization.heatmap_echarts(
                opts,
                data.cpu_heatmap("cpu_usage", [("state", state.to_lowercase())])
                    .map(|v| v / 1000000000.0),
            );
        }

        overview.groups.push(cpu_overview);
        cpu.groups.push(utilization);

        // CPU Performance

        let mut performance = Group::new("Performance", "performance");

        let opts = PlotOpts::line("Instructions per Cycle (IPC)", "ipc");
        if let (Some(cycles), Some(instructions)) = (
            data.sum("cpu_cycles", Labels::default()),
            data.sum("cpu_instructions", Labels::default()),
        ) {
            let ipc = instructions / cycles;
            performance.plot(opts, Some(ipc));
        }

        let opts = PlotOpts::heatmap("Instructions per Cycle (IPC)", "ipc-heatmap");
        if let (Some(cycles), Some(instructions)) = (
            data.cpu_heatmap("cpu_cycles", Labels::default()),
            data.cpu_heatmap("cpu_instructions", Labels::default()),
        ) {
            let ipc = instructions / cycles;
            performance.heatmap_echarts(opts, Some(ipc));
        }

        let opts = PlotOpts::line("Instructions per Nanosecond (IPNS)", "ipns");
        if let (
            Some(cycles),
            Some(instructions),
            Some(aperf),
            Some(mperf),
            Some(tsc),
            Some(cores),
        ) = (
            data.sum("cpu_cycles", Labels::default()),
            data.sum("cpu_instructions", Labels::default()),
            data.sum("cpu_aperf", Labels::default()),
            data.sum("cpu_mperf", Labels::default()),
            data.sum("cpu_tsc", Labels::default()),
            data.sum("cpu_cores", Labels::default()),
        ) {
            let ipns = instructions / cycles * tsc * aperf / mperf / 1000000000.0 / cores;
            performance.plot(opts, Some(ipns));
        }

        let opts = PlotOpts::heatmap("Instructions per Nanosecond (IPNS)", "ipns-heatmap");
        if let (
            Some(cycles),
            Some(instructions),
            Some(aperf),
            Some(mperf),
            Some(tsc),
            Some(cores),
        ) = (
            data.cpu_heatmap("cpu_cycles", Labels::default()),
            data.cpu_heatmap("cpu_instructions", Labels::default()),
            data.cpu_heatmap("cpu_aperf", Labels::default()),
            data.cpu_heatmap("cpu_mperf", Labels::default()),
            data.cpu_heatmap("cpu_tsc", Labels::default()),
            data.sum("cpu_cores", Labels::default()),
        ) {
            let ipns = instructions / cycles * tsc * aperf / mperf / 1000000000.0 / cores;
            performance.heatmap_echarts(opts, Some(ipns));
        }

        let opts = PlotOpts::line("Frequency", "frequency");
        if let (Some(aperf), Some(mperf), Some(tsc), Some(cores)) = (
            data.sum("cpu_aperf", Labels::default()),
            data.sum("cpu_mperf", Labels::default()),
            data.sum("cpu_tsc", Labels::default()),
            data.sum("cpu_cores", Labels::default()),
        ) {
            let frequency = tsc * aperf / mperf / cores;
            performance.plot(opts, Some(frequency));
        }

        let opts = PlotOpts::heatmap("Frequency", "frequency-heatmap");
        if let (Some(aperf), Some(mperf), Some(tsc), Some(cores)) = (
            data.cpu_heatmap("cpu_aperf", Labels::default()),
            data.cpu_heatmap("cpu_mperf", Labels::default()),
            data.cpu_heatmap("cpu_tsc", Labels::default()),
            data.sum("cpu_cores", Labels::default()),
        ) {
            let frequency = tsc * aperf / mperf / cores;
            performance.heatmap_echarts(opts, Some(frequency));
        }

        cpu.groups.push(performance);

        // CPU TLB

        let mut tlb = Group::new("TLB", "tlb");

        let opts = PlotOpts::line("Total", "tlb-total");
        tlb.plot(opts, data.sum("cpu_tlb_flush", Labels::default()));

        let opts = PlotOpts::heatmap("Total", "tlb-total-heatmap");
        tlb.heatmap_echarts(opts, data.cpu_heatmap("cpu_tlb_flush", Labels::default()));

        for reason in &[
            "Local MM Shootdown",
            "Remote Send IPI",
            "Remote Shootdown",
            "Task Switch",
        ] {
            let label = reason;
            let id = format!(
                "tlb-{}",
                reason
                    .to_lowercase()
                    .split(' ')
                    .collect::<Vec<&str>>()
                    .join("-")
            );
            let reason = reason
                .to_lowercase()
                .split(' ')
                .collect::<Vec<&str>>()
                .join("_");

            let opts = PlotOpts::line(*label, &id);
            tlb.plot(
                opts,
                data.sum("cpu_tlb_flush", [("reason", reason.clone())]),
            );

            let opts = PlotOpts::heatmap(*label, format!("{id}-heatmap"));
            tlb.heatmap_echarts(
                opts,
                data.cpu_heatmap("cpu_tlb_flush", [("reason", reason)]),
            );
        }

        cpu.groups.push(tlb);

        // Network overview

        let mut network_overview = Group::new("Network", "network");
        let mut traffic = Group::new("Traffic", "traffic");

        let opts = PlotOpts::line("Bandwidth Transmit", "bandwidth-tx");
        let series = data
            .sum("network_bytes", [("direction", "transmit")])
            .map(|v| v * 8.0);

        network_overview.plot(opts.clone(), series.clone());
        traffic.plot(opts, series);

        let opts = PlotOpts::line("Bandwidth Receive", "bandwidth-rx");
        let series = data
            .sum("network_bytes", [("direction", "receive")])
            .map(|v| v * 8.0);

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

        // Scheduler

        let mut scheduler_overview = Group::new("Scheduler", "scheduler");
        let mut scheduler_group = Group::new("Scheduler", "scheduler");

        let opts = PlotOpts::scatter("Runqueue Latency", "scheduler-runqueue-latency")
            .with_axis_label("Latency")
            .with_unit_system("time")
            .with_log_scale(true);
        let series = data.percentiles("scheduler_runqueue_latency", Labels::default());
        scheduler_overview.scatter(opts.clone(), series.clone());
        scheduler_group.scatter(opts, series);

        let opts = PlotOpts::scatter("Off CPU Time", "scheduler-offcpu-time")
            .with_axis_label("Time")
            .with_unit_system("time")
            .with_log_scale(true);
        let series = data.percentiles("scheduler_offcpu", Labels::default());
        scheduler_overview.scatter(opts.clone(), series.clone());
        scheduler_group.scatter(opts, series);

        let opts = PlotOpts::scatter("Running Time", "scheduler-running-time")
            .with_axis_label("Time")
            .with_unit_system("time")
            .with_log_scale(true);
        let series = data.percentiles("scheduler_running", Labels::default());
        scheduler_group.scatter(opts, series);

        overview.groups.push(scheduler_overview);
        scheduler.groups.push(scheduler_group);

        // Syscall Overview

        let mut syscall_overview = Group::new("Syscall", "syscall");
        let mut syscall_group = Group::new("Syscall", "syscall");

        let opts = PlotOpts::line("Total", "syscall-total");

        let series = data.sum("syscall", Labels::default());
        syscall_overview.plot(opts.clone(), series.clone());
        syscall_group.plot(opts, series);

        let percentiles = data.percentiles("syscall_latency", Labels::default());
        syscall_group.scatter(
            PlotOpts::scatter("Total", "syscall-total-latency"),
            percentiles,
        );

        for op in &[
            "Read", "Write", "Lock", "Yield", "Poll", "Socket", "Time", "Sleep", "Other",
        ] {
            let series = data.sum("syscall", [("op", op.to_lowercase())]);
            syscall_group.plot(PlotOpts::line(*op, format!("syscall-{op}")), series);

            let percentiles = data.percentiles("syscall_latency", [("op", op.to_lowercase())]);
            syscall_group.scatter(
                PlotOpts::scatter(*op, format!("syscall-{op}-latency")),
                percentiles,
            );
        }

        overview.groups.push(syscall_overview);
        syscall.groups.push(syscall_group);

        // Softirq

        let mut softirq_overview = Group::new("Softirq", "softirq");
        let mut softirq_total = Group::new("Total", "total");

        let opts = PlotOpts::line("Rate", "softirq-total-rate");
        let series = data.sum("softirq", Labels::default());
        softirq_overview.plot(opts.clone(), series.clone());
        softirq_total.plot(opts.clone(), series.clone());

        let opts = PlotOpts::heatmap("Rate", "softirq-total-rate-heatmap");
        let series = data.cpu_heatmap("softirq", Labels::default());
        softirq_overview.heatmap_echarts(opts.clone(), series.clone());
        softirq_total.heatmap_echarts(opts.clone(), series.clone());

        let opts = PlotOpts::line("CPU %", "softirq-total-time");
        let series = data
            .cpu_avg("softirq_time", Labels::default())
            .map(|v| v / 1000000000.0);
        softirq_total.plot(opts, series);

        let opts = PlotOpts::heatmap("CPU %", "softirq-total-time-heatmap");
        let series = data
            .cpu_heatmap("softirq_time", Labels::default())
            .map(|v| v / 1000000000.0);
        softirq_total.heatmap_echarts(opts, series);

        overview.groups.push(softirq_overview);
        softirq.groups.push(softirq_total);

        for (label, kind) in [
            ("Hardware Interrupts", "hi"),
            ("IRQ Poll", "irq_poll"),
            ("Network Transmit", "net_tx"),
            ("Network Receive", "net_rx"),
            ("RCU", "rcu"),
            ("Sched", "sched"),
            ("Tasklet", "tasklet"),
            ("Timer", "timer"),
            ("HR Timer", "hrtimer"),
            ("Block", "block"),
        ] {
            let mut group = Group::new(label, format!("softirq-{kind}"));

            let opts = PlotOpts::line("Rate", format!("softirq-{kind}-rate"));
            let series = data.sum("softirq", [("kind", kind)]);
            group.plot(opts, series);

            let opts = PlotOpts::heatmap("Rate", format!("softirq-{kind}-rate-heatmap"));
            let series = data.cpu_heatmap("softirq", [("kind", kind)]);
            group.heatmap_echarts(opts, series);

            let opts = PlotOpts::line("CPU %", format!("softirq-{kind}-time"));
            let series = data
                .cpu_avg("softirq_time", [("kind", kind)])
                .map(|v| v / 1000000000.0);
            group.plot(opts, series);

            let opts = PlotOpts::heatmap("CPU %", format!("softirq-{kind}-time-heatmap"));
            let series = data
                .cpu_heatmap("softirq_time", [("kind", kind)])
                .map(|v| v / 1000000000.0);
            group.heatmap_echarts(opts, series);

            softirq.groups.push(group);
        }

        // BlockIO

        let mut blockio_overview = Group::new("BlockIO", "blockio");
        let mut blockio_throughput = Group::new("Throughput", "throughput");
        let mut blockio_iops = Group::new("IOPS", "iops");
        let mut blockio_latency = Group::new("Latency", "latency");
        let mut blockio_size = Group::new("Size", "size");

        let opts = PlotOpts::line("Read Throughput", "blockio-throughput-read");
        blockio_overview.plot(opts, data.sum("blockio_bytes", [("op", "read")]));

        let opts = PlotOpts::line("Write Throughput", "blockio-throughput-write");
        blockio_overview.plot(opts, data.sum("blockio_bytes", [("op", "write")]));

        let opts = PlotOpts::line("Read IOPS", "blockio-iops-read");
        blockio_overview.plot(opts, data.sum("blockio_operations", [("op", "read")]));

        let opts = PlotOpts::line("Write IOPS", "blockio-iops-write");
        blockio_overview.plot(opts, data.sum("blockio_operations", [("op", "write")]));

        overview.groups.push(blockio_overview);

        for op in &["Read", "Write", "Flush", "Discard"] {
            let opts = PlotOpts::line(*op, format!("throughput-{}", op.to_lowercase()));
            blockio_throughput.plot(opts, data.sum("blockio_bytes", [("op", op.to_lowercase())]));

            let opts = PlotOpts::line(*op, format!("iops-{}", op.to_lowercase()));
            blockio_iops.plot(
                opts,
                data.sum("blockio_operations", [("op", op.to_lowercase())]),
            );

            let opts = PlotOpts::scatter(*op, format!("latency-{}", op.to_lowercase()));
            blockio_latency.scatter(
                opts,
                data.percentiles("blockio_latency", [("op", op.to_lowercase())]),
            );

            let opts = PlotOpts::scatter(*op, format!("size-{}", op.to_lowercase()));
            blockio_size.scatter(
                opts,
                data.percentiles("blockio_size", [("op", op.to_lowercase())]),
            );
        }

        blockio.groups.push(blockio_throughput);
        blockio.groups.push(blockio_iops);
        blockio.groups.push(blockio_latency);
        blockio.groups.push(blockio_size);

        // Finalize

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
        state.sections.insert(
            "scheduler.json".to_string(),
            serde_json::to_string(&scheduler).unwrap(),
        );
        state.sections.insert(
            "syscall.json".to_string(),
            serde_json::to_string(&syscall).unwrap(),
        );
        state.sections.insert(
            "softirq.json".to_string(),
            serde_json::to_string(&softirq).unwrap(),
        );
        state.sections.insert(
            "blockio.json".to_string(),
            serde_json::to_string(&blockio).unwrap(),
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

    pub fn plot(&mut self, opts: PlotOpts, series: Option<TimeSeries>) {
        if let Some(data) = series.map(|v| v.as_data()) {
            self.plots.push(Plot {
                opts,
                data,
                min_value: None,
                max_value: None,
                time_data: None,
            })
        }
    }

    pub fn heatmap(&mut self, opts: PlotOpts, series: Option<Heatmap>) {
        if let Some(data) = series.map(|v| v.as_data()) {
            if data.len() > 1 {
                self.plots.push(Plot {
                    opts,
                    data,
                    min_value: None,
                    max_value: None,
                    time_data: None,
                })
            }
        }
    }

    // New method to use the ECharts optimized heatmap data format
    pub fn heatmap_echarts(&mut self, opts: PlotOpts, series: Option<Heatmap>) {
        if let Some(heatmap) = series {
            let echarts_data = heatmap.as_echarts_data();
            // Only add if there's data
            if !echarts_data.data.is_empty() {
                self.plots.push(Plot {
                    opts,
                    data: echarts_data.data,
                    min_value: Some(echarts_data.min_value),
                    max_value: Some(echarts_data.max_value),
                    time_data: Some(echarts_data.time),
                })
            }
        }
    }

    pub fn scatter(&mut self, opts: PlotOpts, data: Option<Vec<Vec<f64>>>) {
        if data.is_none() {
            return;
        }

        let data = data.unwrap();

        if data.len() < 2 {
            return;
        }

        for series in &data {
            if series.is_empty() {
                return;
            }
        }

        // Check if ANY data series has valid non-zero values
        let has_meaningful_data = data.iter().skip(1).any(|series| {
            series.iter().any(|&value| value.is_finite() && !value.is_nan() && value > 0.0001)
        });

        if !has_meaningful_data {
            return;
        }

        self.plots.push(Plot {
            opts,
            data,
            min_value: None,
            max_value: None,
            time_data: None,
        })
    }
}

#[derive(Serialize, Clone)]
pub struct Plot {
    data: Vec<Vec<f64>>,
    opts: PlotOpts,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_data: Option<Vec<f64>>,
}

#[derive(Serialize, Clone)]
pub struct PlotOpts {
    title: String,
    id: String,
    style: String,
    // Unified configuration for value formatting, axis labels, etc.
    format: Option<FormatConfig>,
}

#[derive(Serialize, Clone)]
pub struct FormatConfig {
    // Axis labels
    x_axis_label: Option<String>,
    y_axis_label: Option<String>,
    
    // Value formatting
    unit_system: Option<String>,  // e.g., "percentage", "time", "bitrate"
    precision: Option<u8>,       // Number of decimal places
    
    // Scale configuration
    log_scale: Option<bool>,      // Whether to use log scale for y-axis
    min: Option<f64>,            // Min value for y-axis
    max: Option<f64>,            // Max value for y-axis
    
    // Additional customization
    value_label: Option<String>,  // Label used in tooltips for the value
}

impl PlotOpts {
    // Basic constructors without formatting
    pub fn line<T: Into<String>, U: Into<String>>(title: T, id: U) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "line".to_string(),
            format: None,
        }
    }
    
    pub fn scatter<T: Into<String>, U: Into<String>>(title: T, id: U) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "scatter".to_string(),
            format: None,
        }
    }
    
    pub fn heatmap<T: Into<String>, U: Into<String>>(title: T, id: U) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "heatmap".to_string(),
            format: None,
        }
    }
    
    // Builder methods for configuring formatting
    pub fn with_format(mut self, format: FormatConfig) -> Self {
        self.format = Some(format);
        self
    }
    
    // Convenience methods
    pub fn with_unit_system<T: Into<String>>(mut self, unit_system: T) -> Self {
        if self.format.is_none() {
            self.format = Some(FormatConfig::default());
        }
        
        if let Some(ref mut format) = self.format {
            format.unit_system = Some(unit_system.into());
        }
        
        self
    }
    
    pub fn with_axis_label<T: Into<String>>(mut self, y_label: T) -> Self {
        if self.format.is_none() {
            self.format = Some(FormatConfig::default());
        }
        
        if let Some(ref mut format) = self.format {
            format.y_axis_label = Some(y_label.into());
        }
        
        self
    }
    
    pub fn with_log_scale(mut self, log_scale: bool) -> Self {
        if self.format.is_none() {
            self.format = Some(FormatConfig::default());
        }
        
        if let Some(ref mut format) = self.format {
            format.log_scale = Some(log_scale);
        }
        
        self
    }
}

// Implement Default for FormatConfig
impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            x_axis_label: None,
            y_axis_label: None,
            unit_system: None,
            precision: Some(2),
            log_scale: None,
            min: None,
            max: None,
            value_label: None,
        }
    }
}
