use super::*;

mod config;
mod exposition;
mod metrics;
mod samplers;

use config::Config;
use samplers::{Sampler, SamplerResult, SAMPLERS};

#[allow(unused_imports)]
use metrics::{CounterGroup, CounterGroupError, GaugeGroup, GaugeGroupError, MetricGroup};

#[cfg(target_os = "linux")]
mod bpf;

#[cfg(target_os = "linux")]
use bpf::*;

#[cfg(target_os = "linux")]
pub use bpf::{process_cgroup_info, CgroupInfo};

/// Runs Rezolus in `agent` mode in which it gathers systems telemetry and
/// exposes metrics on an OTel/Prometheus compatible endpoint and a
/// Rezolus-specific msgpack endpoint.
///
/// This is the default mode for running Rezolus.
pub fn run(config: PathBuf) {
    // load config from file
    let config: Arc<Config> = {
        debug!("loading config: {:?}", config);
        match Config::load(&config) {
            Ok(c) => c.into(),
            Err(error) => {
                eprintln!("error loading config file: {:?}\n{error}", config);
                std::process::exit(1);
            }
        }
    };

    // configure scheduler
    #[cfg(target_os = "linux")]
    config.scheduler().apply();

    // configure debug log
    let debug_output: Box<dyn Output> = Box::new(Stderr::new());

    let level = config.log().level();

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

    let mut samplers = Vec::new();

    for init in SAMPLERS {
        if let Ok(Some(s)) = init(config.clone()) {
            samplers.push(s);
        }
    }

    let samplers = Arc::new(samplers.into_boxed_slice());

    rt.spawn(async move {
        exposition::http::serve(config, samplers).await;
    });

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
