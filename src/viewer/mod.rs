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

    info!("Loading data from parquet file...");
    let data = Tsdb::load(&config.input)
        .map_err(|e| {
            eprintln!("failed to load data from parquet: {e}");
            std::process::exit(1);
        })
        .unwrap();

    info!("Generating dashboards...");

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
        Section {
            name: "cgroups".to_string(),
            route: "/cgroups".to_string(),
        },
        Section {
            name: "Service".to_string(),
            route: "/service".to_string(),
        },
    ];

    // define views for each section
    let mut overview = View::new(&data, sections.clone());
    let mut cpu = View::new(&data, sections.clone());
    let mut network = View::new(&data, sections.clone());
    let mut scheduler = View::new(&data, sections.clone());
    let mut syscall = View::new(&data, sections.clone());
    let mut softirq = View::new(&data, sections.clone());
    let mut blockio = View::new(&data, sections.clone());
    let mut cgroups = View::new(&data, sections.clone());
    let mut service = View::new(&data, sections.clone());

    // CPU

    // CPU Utilization

    let mut cpu_overview = Group::new("CPU", "cpu");
    let mut utilization = Group::new("Utilization", "utilization");

    let plot = Plot::line(
        "Busy %",
        "busy-pct",
        Unit::Percentage,
        data.cpu_avg("cpu_usage", Labels::default())
            .map(|v| (v / 1000000000.0)),
    );
    cpu_overview.push(plot.clone());
    utilization.push(plot);

    let plot = Plot::heatmap(
        "Busy %",
        "busy-pct-heatmap",
        Unit::Percentage,
        data.cpu_heatmap("cpu_usage", Labels::default())
            .map(|v| v / 1000000000.0),
    );
    cpu_overview.push(plot.clone());
    utilization.push(plot);

    for state in &["User", "System", "SoftIRQ"] {
        let plot = Plot::line(
            format!("{state} %"),
            format!("{}-pct", state.to_lowercase()),
            Unit::Percentage,
            data.cpu_avg("cpu_usage", [("state", state.to_lowercase())])
                .map(|v| (v / 1000000000.0)),
        );
        utilization.push(plot);

        let plot = Plot::heatmap(
            format!("{state} %"),
            format!("{}-pct-heatmap", state.to_lowercase()),
            Unit::Percentage,
            data.cpu_heatmap("cpu_usage", [("state", state.to_lowercase())])
                .map(|v| (v / 1000000000.0)),
        );
        utilization.push(plot);
    }

    overview.groups.push(cpu_overview);
    cpu.groups.push(utilization);

    // CPU Performance

    let mut performance = Group::new("Performance", "performance");

    let opts = PlotOpts::line("Instructions per Cycle (IPC)", "ipc", Unit::Count);
    if let (Some(cycles), Some(instructions)) = (
        data.counters("cpu_cycles", Labels::default())
            .map(|v| v.rate().sum()),
        data.counters("cpu_instructions", ())
            .map(|v| v.rate().sum()),
    ) {
        let ipc = instructions / cycles;
        performance.plot(opts, Some(ipc));
    }

    let opts = PlotOpts::heatmap("Instructions per Cycle (IPC)", "ipc-heatmap", Unit::Count);
    if let (Some(cycles), Some(instructions)) = (
        data.cpu_heatmap("cpu_cycles", Labels::default()),
        data.cpu_heatmap("cpu_instructions", Labels::default()),
    ) {
        let ipc = instructions / cycles;
        performance.heatmap_echarts(opts, Some(ipc));
    }

    let opts = PlotOpts::line("Instructions per Nanosecond (IPNS)", "ipns", Unit::Count);
    if let (Some(cycles), Some(instructions), Some(aperf), Some(mperf), Some(tsc), Some(cores)) = (
        data.counters("cpu_cycles", ()).map(|v| v.rate().sum()),
        data.counters("cpu_instructions", ())
            .map(|v| v.rate().sum()),
        data.counters("cpu_aperf", ()).map(|v| v.rate().sum()),
        data.counters("cpu_mperf", ()).map(|v| v.rate().sum()),
        data.counters("cpu_tsc", ()).map(|v| v.rate().sum()),
        data.gauges("cpu_cores", ()).map(|v| v.sum()),
    ) {
        let ipns = instructions / cycles * tsc * aperf / mperf / 1000000000.0 / cores;
        performance.plot(opts, Some(ipns));
    }

    let opts = PlotOpts::heatmap(
        "Instructions per Nanosecond (IPNS)",
        "ipns-heatmap",
        Unit::Count,
    );
    if let (Some(cycles), Some(instructions), Some(aperf), Some(mperf), Some(tsc)) = (
        data.cpu_heatmap("cpu_cycles", Labels::default()),
        data.cpu_heatmap("cpu_instructions", Labels::default()),
        data.cpu_heatmap("cpu_aperf", Labels::default()),
        data.cpu_heatmap("cpu_mperf", Labels::default()),
        data.cpu_heatmap("cpu_tsc", Labels::default()),
    ) {
        let ipns = instructions / cycles * tsc * aperf / mperf / 1000000000.0;
        performance.heatmap_echarts(opts, Some(ipns));
    }

    let opts = PlotOpts::line("Frequency", "frequency", Unit::Frequency);
    if let (Some(aperf), Some(mperf), Some(tsc), Some(cores)) = (
        data.counters("cpu_aperf", ()).map(|v| v.rate().sum()),
        data.counters("cpu_mperf", ()).map(|v| v.rate().sum()),
        data.counters("cpu_tsc", ()).map(|v| v.rate().sum()),
        data.gauges("cpu_cores", ()).map(|v| v.sum()),
    ) {
        let frequency = tsc * aperf / mperf / cores;
        performance.plot(opts, Some(frequency));
    }

    let opts = PlotOpts::heatmap("Frequency", "frequency-heatmap", Unit::Frequency)
        .with_unit_system("frequency");
    if let (Some(aperf), Some(mperf), Some(tsc)) = (
        data.cpu_heatmap("cpu_aperf", Labels::default()),
        data.cpu_heatmap("cpu_mperf", Labels::default()),
        data.cpu_heatmap("cpu_tsc", Labels::default()),
    ) {
        let frequency = tsc * aperf / mperf;
        performance.heatmap_echarts(opts, Some(frequency));
    }

    cpu.groups.push(performance);

    // CPU TLB

    let mut tlb = Group::new("TLB", "tlb");

    let opts = PlotOpts::line("Total", "tlb-total", Unit::Rate);
    tlb.plot(
        opts,
        data.counters("cpu_tlb_flush", ()).map(|v| v.rate().sum()),
    );

    let opts = PlotOpts::heatmap("Total", "tlb-total-heatmap", Unit::Rate);
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

        let opts = PlotOpts::line(*label, &id, Unit::Rate);
        tlb.plot(
            opts,
            data.counters("cpu_tlb_flush", [("reason", reason.clone())])
                .map(|v| v.rate().sum()),
        );

        let opts = PlotOpts::heatmap(*label, format!("{id}-heatmap"), Unit::Rate);
        tlb.heatmap_echarts(
            opts,
            data.cpu_heatmap("cpu_tlb_flush", [("reason", reason)]),
        );
    }

    cpu.groups.push(tlb);

    // Network overview

    let mut network_overview = Group::new("Network", "network");
    let mut traffic = Group::new("Traffic", "traffic");

    let opts = PlotOpts::line("Bandwidth Transmit", "bandwidth-tx", Unit::Bitrate)
        .with_unit_system("bitrate");
    let series = data
        .counters("network_bytes", [("direction", "transmit")])
        .map(|v| v.rate().sum())
        .map(|v| v * 8.0);

    network_overview.plot(opts.clone(), series.clone());
    traffic.plot(opts, series);

    let opts = PlotOpts::line("Bandwidth Receive", "bandwidth-rx", Unit::Bitrate)
        .with_unit_system("bitrate");
    let series = data
        .counters("network_bytes", [("direction", "receive")])
        .map(|v| v.rate().sum())
        .map(|v| v * 8.0);

    network_overview.plot(opts.clone(), series.clone());
    traffic.plot(opts, series);

    let opts = PlotOpts::line("Packets Transmit", "packets-tx", Unit::Rate);
    let series = data
        .counters("network_packets", [("direction", "transmit")])
        .map(|v| v.rate().sum());

    network_overview.plot(opts.clone(), series.clone());
    traffic.plot(opts, series);

    let opts = PlotOpts::line("Packets Receive", "packets-rx", Unit::Rate);
    let series = data
        .counters("network_packets", [("direction", "receive")])
        .map(|v| v.rate().sum());

    network_overview.plot(opts.clone(), series.clone());
    traffic.plot(opts, series);

    overview.groups.push(network_overview);
    network.groups.push(traffic);

    // Scheduler

    let mut scheduler_overview = Group::new("Scheduler", "scheduler");
    let mut scheduler_group = Group::new("Scheduler", "scheduler");

    let opts = PlotOpts::scatter("Runqueue Latency", "scheduler-runqueue-latency", Unit::Time)
        .with_axis_label("Latency")
        .with_unit_system("time")
        .with_log_scale(true);
    let series = data.percentiles("scheduler_runqueue_latency", Labels::default(), PERCENTILES);
    scheduler_overview.scatter(opts.clone(), series.clone());
    scheduler_group.scatter(opts, series);

    let opts = PlotOpts::scatter("Off CPU Time", "scheduler-offcpu-time", Unit::Time)
        .with_axis_label("Time")
        .with_unit_system("time")
        .with_log_scale(true);
    let series = data.percentiles("scheduler_offcpu", Labels::default(), PERCENTILES);
    scheduler_overview.scatter(opts.clone(), series.clone());
    scheduler_group.scatter(opts, series);

    let opts = PlotOpts::scatter("Running Time", "scheduler-running-time", Unit::Time)
        .with_axis_label("Time")
        .with_unit_system("time")
        .with_log_scale(true);
    let series = data.percentiles("scheduler_running", Labels::default(), PERCENTILES);
    scheduler_group.scatter(opts, series);

    overview.groups.push(scheduler_overview);
    scheduler.groups.push(scheduler_group);

    // Syscall Overview

    let mut syscall_overview = Group::new("Syscall", "syscall");
    let mut syscall_group = Group::new("Syscall", "syscall");

    let opts = PlotOpts::line("Total", "syscall-total", Unit::Rate);

    let series = data
        .counters("syscall", Labels::default())
        .map(|v| v.rate().sum());
    syscall_overview.plot(opts.clone(), series.clone());
    syscall_group.plot(opts, series);

    let percentiles = data.percentiles("syscall_latency", Labels::default(), PERCENTILES);
    syscall_group.scatter(
        PlotOpts::scatter("Total", "syscall-total-latency", Unit::Time),
        percentiles,
    );

    for op in &[
        "Read", "Write", "Lock", "Yield", "Poll", "Socket", "Time", "Sleep", "Other",
    ] {
        let series = data
            .counters("syscall", [("op", op.to_lowercase())])
            .map(|v| v.rate().sum());
        syscall_group.plot(
            PlotOpts::line(*op, format!("syscall-{op}"), Unit::Rate),
            series,
        );

        let percentiles = data.percentiles("syscall_latency", [("op", op.to_lowercase())], PERCENTILES);
        syscall_group.scatter(
            PlotOpts::scatter(*op, format!("syscall-{op}-latency"), Unit::Time),
            percentiles,
        );
    }

    overview.groups.push(syscall_overview);
    syscall.groups.push(syscall_group);

    // Softirq

    let mut softirq_overview = Group::new("Softirq", "softirq");
    let mut softirq_total = Group::new("Total", "total");

    let opts = PlotOpts::line("Rate", "softirq-total-rate", Unit::Rate);
    let series = data.counters("softirq", ()).map(|v| v.rate().sum());
    softirq_overview.plot(opts.clone(), series.clone());
    softirq_total.plot(opts.clone(), series.clone());

    let opts = PlotOpts::heatmap("Rate", "softirq-total-rate-heatmap", Unit::Rate);
    let series = data.cpu_heatmap("softirq", Labels::default());
    softirq_overview.heatmap_echarts(opts.clone(), series.clone());
    softirq_total.heatmap_echarts(opts.clone(), series.clone());

    let opts = PlotOpts::line("CPU %", "softirq-total-time", Unit::Percentage);
    let series = data
        .cpu_avg("softirq_time", Labels::default())
        .map(|v| v / 1000000000.0);
    softirq_total.plot(opts, series);

    let opts = PlotOpts::heatmap("CPU %", "softirq-total-time-heatmap", Unit::Percentage);
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

        let opts = PlotOpts::line("Rate", format!("softirq-{kind}-rate"), Unit::Rate);
        let series = data
            .counters("softirq", [("kind", kind)])
            .map(|v| v.rate().sum());
        group.plot(opts, series);

        let opts = PlotOpts::heatmap("Rate", format!("softirq-{kind}-rate-heatmap"), Unit::Rate);
        let series = data.cpu_heatmap("softirq", [("kind", kind)]);
        group.heatmap_echarts(opts, series);

        let opts = PlotOpts::line("CPU %", format!("softirq-{kind}-time"), Unit::Percentage);
        let series = data
            .cpu_avg("softirq_time", [("kind", kind)])
            .map(|v| v / 1000000000.0);
        group.plot(opts, series);

        let opts = PlotOpts::heatmap(
            "CPU %",
            format!("softirq-{kind}-time-heatmap"),
            Unit::Percentage,
        );
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

    let opts = PlotOpts::line("Read Throughput", "blockio-throughput-read", Unit::Datarate);
    blockio_overview.plot(
        opts,
        data.counters("blockio_bytes", [("op", "read")])
            .map(|v| v.rate().sum()),
    );

    let opts = PlotOpts::line(
        "Write Throughput",
        "blockio-throughput-write",
        Unit::Datarate,
    );
    blockio_overview.plot(
        opts,
        data.counters("blockio_bytes", [("op", "write")])
            .map(|v| v.rate().sum()),
    );

    let opts = PlotOpts::line("Read IOPS", "blockio-iops-read", Unit::Count);
    blockio_overview.plot(
        opts,
        data.counters("blockio_operations", [("op", "read")])
            .map(|v| v.rate().sum()),
    );

    let opts = PlotOpts::line("Write IOPS", "blockio-iops-write", Unit::Count);
    blockio_overview.plot(
        opts,
        data.counters("blockio_operations", [("op", "write")])
            .map(|v| v.rate().sum()),
    );

    overview.groups.push(blockio_overview);

    for op in &["Read", "Write", "Flush", "Discard"] {
        let opts = PlotOpts::line(
            *op,
            format!("throughput-{}", op.to_lowercase()),
            Unit::Datarate,
        );
        blockio_throughput.plot(
            opts,
            data.counters("blockio_bytes", [("op", op.to_lowercase())])
                .map(|v| v.rate().sum()),
        );

        let opts = PlotOpts::line(*op, format!("iops-{}", op.to_lowercase()), Unit::Count);
        blockio_iops.plot(
            opts,
            data.counters("blockio_operations", [("op", op.to_lowercase())])
                .map(|v| v.rate().sum()),
        );

        let opts = PlotOpts::scatter(*op, format!("latency-{}", op.to_lowercase()), Unit::Time);
        blockio_latency.scatter(
            opts,
            data.percentiles("blockio_latency", [("op", op.to_lowercase())], PERCENTILES),
        );

        let opts = PlotOpts::scatter(*op, format!("size-{}", op.to_lowercase()), Unit::Bytes);
        blockio_size.scatter(
            opts,
            data.percentiles("blockio_size", [("op", op.to_lowercase())], PERCENTILES),
        );
    }

    blockio.groups.push(blockio_throughput);
    blockio.groups.push(blockio_iops);
    blockio.groups.push(blockio_latency);
    blockio.groups.push(blockio_size);

    /*
     * cgroup section
     */
    let mut cgroup_cpu = Group::new("CPU", "cpu");
    let mut cgroup_performance = Group::new("Performance", "performance");
    let mut cgroup_syscalls = Group::new("Syscalls", "syscalls");

    // cpu usage

    let opts = PlotOpts::multi("Total Cores", "cgroup-total-cores", Unit::Count);
    cgroup_cpu.multi(
        opts,
        data.counters("cgroup_cpu_usage", ())
            .map(|v| (v.rate().by_name() / 1000000000.0).top_n(5, average)),
    );

    let opts = PlotOpts::multi("User Cores", "cgroup-user-cores", Unit::Count);
    cgroup_cpu.multi(
        opts,
        data.counters("cgroup_cpu_usage", [("state", "user")])
            .map(|v| (v.rate().by_name() / 1000000000.0).top_n(5, average)),
    );

    // performance

    if let (Some(cycles), Some(instructions)) = (
        data.counters("cgroup_cpu_cycles", Labels::default())
            .map(|v| v.rate().by_name()),
        data.counters("cgroup_cpu_instructions", Labels::default())
            .map(|v| v.rate().by_name()),
    ) {
        let opts = PlotOpts::multi("Highest IPC", "cgroup-ipc-low", Unit::Count);
        cgroup_performance.multi(
            opts,
            Some((cycles.clone() / instructions.clone()).top_n(5, average)),
        );

        let opts = PlotOpts::multi("Lowest IPC", "cgroup-ipc-high", Unit::Count);
        cgroup_performance.multi(opts, Some((cycles / instructions).bottom_n(5, average)));
    }

    // syscalls

    let opts = PlotOpts::multi("Total", "cgroup-syscalls", Unit::Rate);
    cgroup_syscalls.multi(
        opts,
        data.counters("cgroup_syscall", Labels::default())
            .map(|v| (v.rate().by_name()).top_n(5, average)),
    );

    for op in &[
        "Read", "Write", "Lock", "Yield", "Poll", "Socket", "Time", "Sleep", "Other",
    ] {
        let opts = PlotOpts::multi(*op, format!("syscall-{op}"), Unit::Rate);
        cgroup_syscalls.multi(
            opts,
            data.counters("cgroup_syscall", [("op", op.to_lowercase())])
                .map(|v| (v.rate().by_name()).top_n(5, average)),
        );
    }

    // Add all groups to the cgroups view
    cgroups.groups.push(cgroup_cpu);
    cgroups.groups.push(cgroup_performance);
    cgroups.groups.push(cgroup_syscalls);

    // Service

    let mut service_overview = Group::new("Service", "service");

    let opts = PlotOpts::scatter("Frame Start Delay", "frame-start-delay", Unit::Time)
        .with_axis_label("FSD")
        .with_unit_system("time")
        .with_log_scale(true);
    let series = data.percentiles("frame_start_delay", Labels::default(), PERCENTILES);
    service_overview.scatter(opts.clone(), series.clone());

    let opts = PlotOpts::scatter("FSD - Test 1", "frame-start-delay", Unit::Time)
        .with_axis_label("FSD")
        .with_unit_system("time")
        .with_log_scale(true);
    let series = data.percentiles("frame_start_delay", [("name".to_string(), "test-1".to_string())], PERCENTILES);
    service_overview.scatter(opts.clone(), series.clone());

    let opts = PlotOpts::scatter("FSD - Test 2", "frame-start-delay", Unit::Time)
        .with_axis_label("FSD")
        .with_unit_system("time")
        .with_log_scale(true);
    let series = data.percentiles("frame_start_delay", [("name".to_string(), "test-2".to_string())], PERCENTILES);
    service_overview.scatter(opts.clone(), series.clone());

    service.groups.push(service_overview);

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
    state.sections.insert(
        "cgroups.json".to_string(),
        serde_json::to_string(&cgroups).unwrap(),
    );
    state.sections.insert(
        "service.json".to_string(),
        serde_json::to_string(&service).unwrap(),
    );

    // open in browser
    let c = config.clone();
    rt.spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;

        if open::that(format!("http://{}", c.listen)).is_err() {
            info!("Use your browser to view: http://{}", c.listen);
        } else {
            info!("Launched browser to view: http://{}", c.listen);
        }
    });

    // launch the HTTP listener
    let c = config.clone();
    rt.block_on(async move { serve(c, state).await });

    std::thread::sleep(Duration::from_millis(200));
}

async fn serve(config: Arc<Config>, state: AppState) {
    let livereload = LiveReloadLayer::new();
    let reloader = livereload.reloader();

    let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, _>| {
        if let Ok(event) = res {
            // Reload, unless it's just a read.
            if !matches!(event.kind, notify::EventKind::Access(_)) {
                reloader.reload();
            }
        }
    })
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

    (
        StatusCode::OK,
        state
            .sections
            .get(parts[1])
            .map(|v| v.to_string())
            .unwrap_or("{ }".to_string()),
    )
}

#[derive(Default, Serialize)]
pub struct View {
    // interval between consecutive datapoints as fractional seconds
    interval: f64,
    source: String,
    version: String,
    groups: Vec<Group>,
    sections: Vec<Section>,
}

impl View {
    pub fn new(data: &Tsdb, sections: Vec<Section>) -> Self {
        let interval = data.interval();
        let source = data.source();
        let version = data.version();

        Self {
            interval,
            source,
            version,
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

    pub fn push(&mut self, plot: Option<Plot>) {
        if let Some(plot) = plot {
            self.plots.push(plot);
        }
    }

    pub fn plot(&mut self, opts: PlotOpts, series: Option<UntypedSeries>) {
        if let Some(data) = series.map(|v| v.as_data()) {
            self.plots.push(Plot {
                opts,
                data,
                min_value: None,
                max_value: None,
                time_data: None,
                formatted_time_data: None,
                series_names: None,
            })
        }
    }

    // New method to use the ECharts optimized heatmap data format
    pub fn heatmap_echarts(&mut self, opts: PlotOpts, series: Option<Heatmap>) {
        if let Some(heatmap) = series {
            let echarts_data = heatmap.as_data();
            // Only add if there's data
            if !echarts_data.data.is_empty() {
                self.plots.push(Plot {
                    opts,
                    data: echarts_data.data,
                    min_value: Some(echarts_data.min_value),
                    max_value: Some(echarts_data.max_value),
                    time_data: Some(echarts_data.time),
                    formatted_time_data: Some(echarts_data.formatted_time),
                    series_names: None,
                })
            }
        }
    }

    pub fn scatter(&mut self, opts: PlotOpts, data: Option<Vec<UntypedSeries>>) {
        if data.is_none() {
            return;
        }

        let d = data.unwrap();

        let mut data = Vec::new();

        for series in &d {
            let d = series.as_data();

            if data.is_empty() {
                data.push(d[0].clone());
            }

            data.push(d[1].clone());
        }

        self.plots.push(Plot {
            opts,
            data,
            min_value: None,
            max_value: None,
            time_data: None,
            formatted_time_data: None,
            series_names: None,
        })
    }

    // New method to add a multi-series plot
    pub fn multi(&mut self, opts: PlotOpts, cgroup_data: Option<Vec<(String, UntypedSeries)>>) {
        if cgroup_data.is_none() {
            return;
        }

        let mut cgroup_data = cgroup_data.unwrap();

        let mut data = Vec::new();
        let mut labels = Vec::new();

        for (label, series) in cgroup_data.drain(..) {
            labels.push(label);
            let d = series.as_data();

            if data.is_empty() {
                data.push(d[0].clone());
            }

            data.push(d[1].clone());
        }

        self.plots.push(Plot {
            opts,
            data,
            min_value: None,
            max_value: None,
            time_data: None,
            formatted_time_data: None,
            series_names: Some(labels),
        });
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
    #[serde(skip_serializing_if = "Option::is_none")]
    formatted_time_data: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_names: Option<Vec<String>>,
}

impl Plot {
    pub fn line<T: Into<String>, U: Into<String>>(
        title: T,
        id: U,
        unit: Unit,
        series: Option<UntypedSeries>,
    ) -> Option<Self> {
        series.map(|series| Self {
            data: series.as_data(),
            opts: PlotOpts::line(title, id, unit),
            min_value: None,
            max_value: None,
            time_data: None,
            formatted_time_data: None,
            series_names: None,
        })
    }

    pub fn heatmap<T: Into<String>, U: Into<String>>(
        title: T,
        id: U,
        unit: Unit,
        series: Option<Heatmap>,
    ) -> Option<Self> {
        if let Some(heatmap) = series {
            let echarts_data = heatmap.as_data();
            if !echarts_data.data.is_empty() {
                return Some(Plot {
                    opts: PlotOpts::heatmap(title, id, unit),
                    data: echarts_data.data,
                    min_value: Some(echarts_data.min_value),
                    max_value: Some(echarts_data.max_value),
                    time_data: Some(echarts_data.time),
                    formatted_time_data: Some(echarts_data.formatted_time),
                    series_names: None,
                });
            }
        }

        None
    }
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
    unit_system: Option<String>, // e.g., "percentage", "time", "bitrate"
    precision: Option<u8>,       // Number of decimal places

    // Scale configuration
    log_scale: Option<bool>, // Whether to use log scale for y-axis
    min: Option<f64>,        // Min value for y-axis
    max: Option<f64>,        // Max value for y-axis

    // Additional customization
    value_label: Option<String>, // Label used in tooltips for the value
}

impl PlotOpts {
    // Basic constructors without formatting
    pub fn line<T: Into<String>, U: Into<String>>(title: T, id: U, unit: Unit) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "line".to_string(),
            format: Some(FormatConfig::new(unit)),
        }
    }

    pub fn multi<T: Into<String>, U: Into<String>>(title: T, id: U, unit: Unit) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "multi".to_string(),
            format: Some(FormatConfig::new(unit)),
        }
    }

    pub fn scatter<T: Into<String>, U: Into<String>>(title: T, id: U, unit: Unit) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "scatter".to_string(),
            format: Some(FormatConfig::new(unit)),
        }
    }

    pub fn heatmap<T: Into<String>, U: Into<String>>(title: T, id: U, unit: Unit) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "heatmap".to_string(),
            format: Some(FormatConfig::new(unit)),
        }
    }

    // Convenience methods
    pub fn with_unit_system<T: Into<String>>(mut self, unit_system: T) -> Self {
        if let Some(ref mut format) = self.format {
            format.unit_system = Some(unit_system.into());
        }

        self
    }

    pub fn with_axis_label<T: Into<String>>(mut self, y_label: T) -> Self {
        if let Some(ref mut format) = self.format {
            format.y_axis_label = Some(y_label.into());
        }

        self
    }

    pub fn with_log_scale(mut self, log_scale: bool) -> Self {
        if let Some(ref mut format) = self.format {
            format.log_scale = Some(log_scale);
        }

        self
    }
}

impl FormatConfig {
    pub fn new(unit: Unit) -> Self {
        Self {
            x_axis_label: None,
            y_axis_label: None,
            unit_system: Some(unit.to_string()),
            precision: Some(2),
            log_scale: None,
            min: None,
            max: None,
            value_label: None,
        }
    }
}

pub enum Unit {
    Count,
    Rate,
    Time,
    Bytes,
    Datarate,
    Bitrate,
    Percentage,
    Frequency,
}

impl std::fmt::Display for Unit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        let s = match self {
            Self::Count => "count",
            Self::Rate => "rate",
            Self::Time => "time",
            Self::Bytes => "bytes",
            Self::Datarate => "datarate",
            Self::Bitrate => "bitrate",
            Self::Percentage => "percentage",
            Self::Frequency => "frequency",
        };

        write!(f, "{s}")
    }
}
