use super::*;

use axum::handler::Handler;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use clap::ArgMatches;
use http::{header, StatusCode, Uri};
use include_dir::{include_dir, Dir};
use serde::Serialize;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::decompression::RequestDecompressionLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_livereload::LiveReloadLayer;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;

#[cfg(feature = "developer-mode")]
use notify::Watcher;

static ASSETS: Dir<'_> = include_dir!("src/viewer/assets");

const PERCENTILES: &[f64] = &[50.0, 90.0, 99.0, 99.9, 99.99];

mod plot;
mod tsdb;

use plot::*;
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
}

pub struct Config {
    input: PathBuf,
    verbose: u8,
    listen: SocketAddr,
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
    let mut state = AppState::new();

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

    // CPU

    // CPU Utilization

    let mut cpu_overview = Group::new("CPU", "cpu");
    let mut utilization = Group::new("Utilization", "utilization");

    let plot = Plot::line(
        "Busy %",
        "busy-pct",
        Unit::Percentage,
        data.cpu_avg("cpu_usage", ()).map(|v| (v / 1000000000.0)),
    );
    cpu_overview.push(plot.clone());
    utilization.push(plot);

    let plot = Plot::heatmap(
        "Busy %",
        "busy-pct-heatmap",
        Unit::Percentage,
        data.cpu_heatmap("cpu_usage", ()).map(|v| v / 1000000000.0),
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

    overview.group(cpu_overview);
    cpu.group(utilization);

    // CPU Performance

    let mut performance = Group::new("Performance", "performance");

    let opts = PlotOpts::line("Instructions per Cycle (IPC)", "ipc", Unit::Count);
    if let (Some(cycles), Some(instructions)) = (
        data.counters("cpu_cycles", ()).map(|v| v.rate().sum()),
        data.counters("cpu_instructions", ())
            .map(|v| v.rate().sum()),
    ) {
        let ipc = instructions / cycles;
        performance.plot(opts, Some(ipc));
    }

    let opts = PlotOpts::heatmap("Instructions per Cycle (IPC)", "ipc-heatmap", Unit::Count);
    if let (Some(cycles), Some(instructions)) = (
        data.cpu_heatmap("cpu_cycles", ()),
        data.cpu_heatmap("cpu_instructions", ()),
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
        data.cpu_heatmap("cpu_cycles", ()),
        data.cpu_heatmap("cpu_instructions", ()),
        data.cpu_heatmap("cpu_aperf", ()),
        data.cpu_heatmap("cpu_mperf", ()),
        data.cpu_heatmap("cpu_tsc", ()),
    ) {
        let ipns = instructions / cycles * tsc * aperf / mperf / 1000000000.0;
        performance.heatmap_echarts(opts, Some(ipns));
    }

    let opts = PlotOpts::line("L3 Hit %", "ld-hit", Unit::Percentage);
    if let (Some(access), Some(miss)) = (
        data.counters("cpu_l3_access", ()).map(|v| v.rate().sum()),
        data.counters("cpu_l3_miss", ()).map(|v| v.rate().sum()),
    ) {
        let hitrate = miss / access;
        performance.plot(opts, Some(hitrate));
    }

    let opts = PlotOpts::heatmap("L3 Hit %", "l3-hit-heatmap", Unit::Percentage);
    if let (Some(access), Some(miss)) = (
        data.cpu_heatmap("cpu_l3_access", ()),
        data.cpu_heatmap("cpu_l3_miss", ()),
    ) {
        let hitrate = miss / access;
        performance.heatmap_echarts(opts, Some(hitrate));
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
        data.cpu_heatmap("cpu_aperf", ()),
        data.cpu_heatmap("cpu_mperf", ()),
        data.cpu_heatmap("cpu_tsc", ()),
    ) {
        let frequency = tsc * aperf / mperf;
        performance.heatmap_echarts(opts, Some(frequency));
    }

    cpu.group(performance);

    // CPU Migrations

    let mut migrations = Group::new("Migrations", "migrations");

    let opts = PlotOpts::line("To", "cpu-migrations-to", Unit::Rate);
    migrations.plot(
        opts,
        data.counters("cpu_migrations", [("direction", "to")])
            .map(|v| v.rate().sum()),
    );

    let plot = Plot::heatmap(
        "To",
        "cpu-migrations-to-heatmap",
        Unit::Rate,
        data.cpu_heatmap("cpu_migrations", [("direction", "to")]),
    );
    migrations.push(plot);

    let opts = PlotOpts::line("From", "cpu-migrations-from", Unit::Rate);
    migrations.plot(
        opts,
        data.counters("cpu_migrations", [("direction", "from")])
            .map(|v| v.rate().sum()),
    );

    let plot = Plot::heatmap(
        "From",
        "cpu-migrations-from-heatmap",
        Unit::Rate,
        data.cpu_heatmap("cpu_migrations", [("direction", "from")]),
    );
    migrations.push(plot);

    cpu.group(migrations);

    // CPU TLB

    let mut tlb = Group::new("TLB", "tlb");

    let opts = PlotOpts::line("Total", "tlb-total", Unit::Rate);
    tlb.plot(
        opts,
        data.counters("cpu_tlb_flush", ()).map(|v| v.rate().sum()),
    );

    let opts = PlotOpts::heatmap("Total", "tlb-total-heatmap", Unit::Rate);
    tlb.heatmap_echarts(opts, data.cpu_heatmap("cpu_tlb_flush", ()));

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

    cpu.group(tlb);

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

    overview.group(network_overview);
    network.group(traffic);

    // Scheduler

    let mut scheduler_overview = Group::new("Scheduler", "scheduler");
    let mut scheduler_group = Group::new("Scheduler", "scheduler");

    let opts = PlotOpts::scatter("Runqueue Latency", "scheduler-runqueue-latency", Unit::Time)
        .with_axis_label("Latency")
        .with_unit_system("time")
        .with_log_scale(true);
    let series = data.percentiles("scheduler_runqueue_latency", (), PERCENTILES);
    scheduler_overview.scatter(opts.clone(), series.clone());
    scheduler_group.scatter(opts, series);

    let opts = PlotOpts::scatter("Off CPU Time", "scheduler-offcpu-time", Unit::Time)
        .with_axis_label("Time")
        .with_unit_system("time")
        .with_log_scale(true);
    let series = data.percentiles("scheduler_offcpu", (), PERCENTILES);
    scheduler_overview.scatter(opts.clone(), series.clone());
    scheduler_group.scatter(opts, series);

    let opts = PlotOpts::scatter("Running Time", "scheduler-running-time", Unit::Time)
        .with_axis_label("Time")
        .with_unit_system("time")
        .with_log_scale(true);
    let series = data.percentiles("scheduler_running", (), PERCENTILES);
    scheduler_group.scatter(opts, series);

    overview.group(scheduler_overview);
    scheduler.group(scheduler_group);

    // Syscall Overview

    let mut syscall_overview = Group::new("Syscall", "syscall");
    let mut syscall_group = Group::new("Syscall", "syscall");

    let opts = PlotOpts::line("Total", "syscall-total", Unit::Rate);

    let series = data.counters("syscall", ()).map(|v| v.rate().sum());
    syscall_overview.plot(opts.clone(), series.clone());
    syscall_group.plot(opts, series);

    let percentiles = data.percentiles("syscall_latency", (), PERCENTILES);
    syscall_group.scatter(
        PlotOpts::scatter("Total", "syscall-total-latency", Unit::Time),
        percentiles,
    );

    for op in &[
        "Read",
        "Write",
        "Poll",
        "Socket",
        "Lock",
        "Time",
        "Sleep",
        "Yield",
        "Filesystem",
        "Memory",
        "Process",
        "Query",
        "IPC",
        "Timer",
        "Event",
        "Other",
    ] {
        let series = data
            .counters("syscall", [("op", op.to_lowercase())])
            .map(|v| v.rate().sum());
        syscall_group.plot(
            PlotOpts::line(*op, format!("syscall-{op}"), Unit::Rate),
            series,
        );

        let percentiles =
            data.percentiles("syscall_latency", [("op", op.to_lowercase())], PERCENTILES);
        syscall_group.scatter(
            PlotOpts::scatter(*op, format!("syscall-{op}-latency"), Unit::Time),
            percentiles,
        );
    }

    overview.group(syscall_overview);
    syscall.group(syscall_group);

    // Softirq

    let mut softirq_overview = Group::new("Softirq", "softirq");
    let mut softirq_total = Group::new("Total", "total");

    let opts = PlotOpts::line("Rate", "softirq-total-rate", Unit::Rate);
    let series = data.counters("softirq", ()).map(|v| v.rate().sum());
    softirq_overview.plot(opts.clone(), series.clone());
    softirq_total.plot(opts.clone(), series.clone());

    let opts = PlotOpts::heatmap("Rate", "softirq-total-rate-heatmap", Unit::Rate);
    let series = data.cpu_heatmap("softirq", ());
    softirq_overview.heatmap_echarts(opts.clone(), series.clone());
    softirq_total.heatmap_echarts(opts.clone(), series.clone());

    let opts = PlotOpts::line("CPU %", "softirq-total-time", Unit::Percentage);
    let series = data.cpu_avg("softirq_time", ()).map(|v| v / 1000000000.0);
    softirq_total.plot(opts, series);

    let opts = PlotOpts::heatmap("CPU %", "softirq-total-time-heatmap", Unit::Percentage);
    let series = data
        .cpu_heatmap("softirq_time", ())
        .map(|v| v / 1000000000.0);
    softirq_total.heatmap_echarts(opts, series);

    overview.group(softirq_overview);
    softirq.group(softirq_total);

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

        softirq.group(group);
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

    overview.group(blockio_overview);

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

    blockio.group(blockio_throughput);
    blockio.group(blockio_iops);
    blockio.group(blockio_latency);
    blockio.group(blockio_size);

    /*
     * cgroup section
     */
    let mut cgroup_cpu = Group::new("CPU", "cpu");
    let mut cgroup_performance = Group::new("Performance", "performance");
    let mut cgroup_tlb = Group::new("TLB Flush", "tlb");
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

    let opts = PlotOpts::multi("System Cores", "cgroup-system-cores", Unit::Count);
    cgroup_cpu.multi(
        opts,
        data.counters("cgroup_cpu_usage", [("state", "system")])
            .map(|v| (v.rate().by_name() / 1000000000.0).top_n(5, average)),
    );

    let opts = PlotOpts::multi("CPU Migrations", "cgroup-cpu-migrations", Unit::Rate);
    cgroup_cpu.multi(
        opts,
        data.counters("cgroup_cpu_migrations", ())
            .map(|v| (v.rate().by_name()).top_n(5, average)),
    );

    // performance

    if let (Some(cycles), Some(instructions)) = (
        data.counters("cgroup_cpu_cycles", ())
            .map(|v| v.rate().by_name()),
        data.counters("cgroup_cpu_instructions", ())
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

    // TLB

    let opts = PlotOpts::multi("Total", "cgroup-tlb-flush", Unit::Count);
    cgroup_tlb.multi(
        opts,
        data.counters("cgroup_tlb_flush", ())
            .map(|v| (v.rate().by_name()).top_n(5, average)),
    );

    // syscalls

    let opts = PlotOpts::multi("Total", "cgroup-syscalls", Unit::Rate);
    cgroup_syscalls.multi(
        opts,
        data.counters("cgroup_syscall", ())
            .map(|v| (v.rate().by_name()).top_n(5, average)),
    );

    for op in &[
        "Read",
        "Write",
        "Poll",
        "Socket",
        "Lock",
        "Time",
        "Sleep",
        "Yield",
        "Filesystem",
        "Memory",
        "Process",
        "Query",
        "IPC",
        "Timer",
        "Event",
        "Other",
    ] {
        let opts = PlotOpts::multi(*op, format!("syscall-{op}"), Unit::Rate);
        cgroup_syscalls.multi(
            opts,
            data.counters("cgroup_syscall", [("op", op.to_lowercase())])
                .map(|v| (v.rate().by_name()).top_n(5, average)),
        );
    }

    // Add all groups to the cgroups view
    cgroups.group(cgroup_cpu);
    cgroups.group(cgroup_performance);
    cgroups.group(cgroup_tlb);
    cgroups.group(cgroup_syscalls);

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

    #[cfg(feature = "developer-mode")]
    {
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
    }

    let app = app(livereload, state);

    let listener = TcpListener::bind(config.listen)
        .await
        .expect("failed to listen");

    axum::serve(listener, app)
        .await
        .expect("failed to run http server");
}

struct AppState {
    sections: HashMap<String, String>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            sections: Default::default(),
        }
    }
}

// NOTE: we're going to want to include the assets in the binary for release
// builds. For now, we just serve from the assets folder
fn app(livereload: LiveReloadLayer, state: AppState) -> Router {
    let state = Arc::new(state);

    let router = Router::new()
        .route_service("/", ServeFile::new("src/viewer/assets/index.html"))
        .route("/about", get(about))
        .with_state(state.clone());

    let router = if cfg!(feature = "developer-mode") {
        router.nest_service("/lib", ServeDir::new(Path::new("src/viewer/assets/lib")))
    } else {
        router.nest_service("/lib", get(lib))
    };

    router
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

async fn lib(uri: Uri) -> impl IntoResponse {
    let path = uri.path();

    if let Some(asset) = ASSETS.get_file(format!("lib{path}")) {
        let body = asset.contents_utf8().unwrap();
        let content_type = if path.ends_with(".js") {
            "text/javascript"
        } else {
            "text/plain"
        };

        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            body.to_string(),
        )
    } else {
        error!("path: {path} does not map to a static resource");
        (
            StatusCode::from_u16(404).unwrap(),
            [(header::CONTENT_TYPE, "text/plain")],
            "404 Not Found".to_string(),
        )
    }
}
