// Copyright 2019 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

#[macro_use]
extern crate ringlog;

#[macro_use]
extern crate anyhow;

use ringlog::{LogBuilder, MultiLogBuilder, Stdout};
use std::sync::Arc;
use tokio::runtime::Builder;

mod common;
mod config;
mod exposition;
mod metrics;
mod samplers;

use common::*;
use config::Config;
use metrics::*;
use samplers::*;

pub type Instant = clocksource::Instant<Nanoseconds<u64>>;
pub type Duration = clocksource::Duration<Nanoseconds<u64>>;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // get config
    let config = Arc::new(Config::new());

    // initialize logging
    let log = LogBuilder::new()
        .output(Box::new(Stdout::new()))
        .log_queue_depth(4096)
        .single_message_size(4096)
        .build()
        .expect("failed to initialize debug log");

    let mut log = MultiLogBuilder::new()
        .level_filter(config.logging().to_level_filter())
        .default(log)
        .build()
        .start();

    info!("----------");
    info!("{} {}", common::NAME, common::VERSION);
    info!("----------");
    debug!("host cores: {}", hardware_threads().unwrap_or(1));

    let runnable = Arc::new(AtomicBool::new(true));
    let r = runnable.clone();

    // initialize signal handler
    debug!("initializing signal handler");
    ctrlc::set_handler(move || {
        r.store(false, Ordering::Relaxed);
    })
    .expect("Failed to set handler for SIGINT / SIGTERM");

    // initialize metrics
    debug!("initializing metrics");
    let metrics = Arc::new(Metrics::new());

    // initialize async runtime
    debug!("initializing async runtime");
    let runtime = Arc::new(
        Builder::new_multi_thread()
            .enable_all()
            .worker_threads(config.general().threads())
            .max_blocking_threads(config.general().threads())
            .thread_name("rezolus-worker")
            .build()
            .unwrap(),
    );

    // spawn samplers
    debug!("spawning samplers");
    let common = Common::new(config.clone(), metrics.clone(), runtime);


    Cpu::spawn(common.clone());
    Disk::spawn(common.clone());
    Ext4::spawn(common.clone());
    Http::spawn(common.clone());
    Interrupt::spawn(common.clone());
    Krb5kdc::spawn(common.clone());
    Memcache::spawn(common.clone());
    Memory::spawn(common.clone());
    PageCache::spawn(common.clone());
    Network::spawn(common.clone());
    Ntp::spawn(common.clone());
    Nvidia::spawn(common.clone());
    Process::spawn(common.clone());
    Rezolus::spawn(common.clone());

    let scheduler = Scheduler::new(common.clone());
    runtime.spawn(async move {
        loop {
            let _ = scheduler.sample().await;
        }
    });

    Scheduler::spawn(common.clone());
    Softnet::spawn(common.clone());
    Tcp::spawn(common.clone());
    Udp::spawn(common.clone());
    Usercall::spawn(common.clone());
    Xfs::spawn(common);

    #[cfg(feature = "push_kafka")]
    {
        if config.exposition().kafka().enabled() {
            let mut kafka_producer =
                exposition::KafkaProducer::new(config.clone(), metrics.clone());
            let _ = std::thread::Builder::new()
                .name("kafka".to_string())
                .spawn(move || loop {
                    kafka_producer.run();
                });
        }
    }

    debug!("beginning stats exposition");
    let mut http = exposition::Http::new(config.clone(), metrics);

    while runnable.load(Ordering::Relaxed) {
        clocksource::refresh_clock();
        http.run();
        let _ = log.flush();
    }

    Ok(())
}
