//! Collects CPU perf counters and traces:
//! * `sched_switch`
//!
//! Initializes perf events to collect MSRs for APERF, MPERF, and TSC.
//!
//! And produces these stats:
//! * `cpu_aperf`
//! * `cpu_mperf`
//! * `cpu_tsc`
//!
//! These stats can be used to calculate the base frequency and running
//! frequency in post-processing or in an observability stack.

const NAME: &str = "cpu_frequency";

use crate::agent::*;

use perf_event::events::x86::{Msr, MsrId};
use perf_event::ReadFormat;
use tokio::sync::Mutex;
use walkdir::WalkDir;

use std::collections::HashSet;

mod stats;

use stats::*;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = FrequencyInner::new()?;

    Ok(Some(Box::new(Frequency {
        inner: inner.into(),
    })))
}

struct Frequency {
    inner: Mutex<FrequencyInner>,
}

#[async_trait]
impl Sampler for Frequency {
    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh().await;
    }
}

struct FrequencyInner {
    cores: Vec<Core>,
}

impl FrequencyInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let cores = get_cores()?;

        println!("initialized frequency counters for: {} cores", cores.len());

        Ok(Self { cores })
    }

    pub async fn refresh(&mut self) -> Result<(), std::io::Error> {
        for core in &mut self.cores {
            if let Ok(group) = core.tsc.read_group() {
                if let (Some(aperf), Some(mperf), Some(tsc)) = (
                    group.get(&core.aperf),
                    group.get(&core.mperf),
                    group.get(&core.tsc),
                ) {
                    let aperf = aperf.value();
                    let mperf = mperf.value();
                    let tsc = tsc.value();

                    for cpu in &core.siblings {
                        let _ = CPU_APERF.set(*cpu, aperf);
                        let _ = CPU_MPERF.set(*cpu, mperf);
                        let _ = CPU_TSC.set(*cpu, tsc);
                    }
                }
            }
        }

        Ok(())
    }
}

/// A struct that represents a physical core and contains the counters necessary
/// for calculating running frequency as well as the set of siblings (logical
/// cores)
struct Core {
    /// perf events for this core
    aperf: perf_event::Counter,
    mperf: perf_event::Counter,
    tsc: perf_event::Counter,
    /// all sibling cores
    siblings: Vec<usize>,
}

impl Core {
    pub fn new(siblings: Vec<usize>) -> Result<Self, ()> {
        let cpu = *siblings.first().expect("empty physical core");

        let aperf_event = match Msr::new(MsrId::APERF) {
            Ok(msr) => msr,
            Err(e) => {
                debug!("failed to initialize aperf msr: {e}");
                return Err(());
            }
        };

        let mperf_event = match Msr::new(MsrId::MPERF) {
            Ok(msr) => msr,
            Err(e) => {
                debug!("failed to initialize mperf msr: {e}");
                return Err(());
            }
        };

        let tsc_event = match Msr::new(MsrId::TSC) {
            Ok(msr) => msr,
            Err(e) => {
                debug!("failed to initialize tsc msr: {e}");
                return Err(());
            }
        };

        match perf_event::Builder::new(tsc_event)
            .one_cpu(cpu)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .pinned(true)
            .read_format(
                ReadFormat::TOTAL_TIME_ENABLED | ReadFormat::TOTAL_TIME_RUNNING | ReadFormat::GROUP,
            )
            .build()
        {
            Ok(mut tsc) => {
                match perf_event::Builder::new(aperf_event)
                    .one_cpu(cpu)
                    .any_pid()
                    .exclude_hv(false)
                    .exclude_kernel(false)
                    .build_with_group(&mut tsc)
                {
                    Ok(aperf) => {
                        match perf_event::Builder::new(mperf_event)
                            .one_cpu(cpu)
                            .any_pid()
                            .exclude_hv(false)
                            .exclude_kernel(false)
                            .build_with_group(&mut tsc)
                        {
                            Ok(mperf) => match tsc.enable_group() {
                                Ok(_) => Ok(Core {
                                    aperf,
                                    mperf,
                                    tsc,
                                    siblings,
                                }),
                                Err(e) => {
                                    error!("failed to enable the perf group on CPU{cpu}: {e}");
                                    Err(())
                                }
                            },
                            Err(e) => {
                                debug!("failed to enable the mperf counter on CPU{cpu}: {e}");
                                Err(())
                            }
                        }
                    }
                    Err(e) => {
                        debug!("failed to enable the aperf counter on CPU{cpu}: {e}");
                        Err(())
                    }
                }
            }
            Err(e) => {
                debug!("failed to enable the tsc counter on CPU{cpu}: {e}");
                Err(())
            }
        }
    }
}

fn physical_cores() -> Result<Vec<Vec<usize>>, std::io::Error> {
    let mut cores = Vec::new();
    let mut processed = HashSet::new();

    // walk the cpu devices directory
    for entry in WalkDir::new("/sys/devices/system/cpu")
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let filename = path.file_name().and_then(|v| v.to_str()).unwrap_or("");

        // check if this is a cpu directory
        if filename.starts_with("cpu") && filename[3..].chars().all(char::is_numeric) {
            let siblings_list = path.join("topology").join("thread_siblings_list");

            if let Ok(siblings_list) = std::fs::read_to_string(&siblings_list) {
                println!("parsing siblings list: {}", siblings_list);
                let siblings = parse_cpu_list(&siblings_list);

                // avoid duplicates
                if !processed.contains(&siblings) {
                    processed.insert(siblings.clone());
                    cores.push(siblings);
                }
            }
        }
    }

    Ok(cores)
}

fn get_cores() -> Result<Vec<Core>, std::io::Error> {
    let mut physical_cores = physical_cores()?;

    let mut cores = Vec::new();

    for siblings in physical_cores.drain(..) {
        if let Ok(core) = Core::new(siblings) {
            cores.push(core);
        }
    }

    Ok(cores)
}

fn parse_cpu_list(list: &str) -> Vec<usize> {
    let mut cores = Vec::new();

    for range in list.trim().split(',') {
        if let Some((start, end)) = range.split_once('-') {
            // Range of cores
            if let (Ok(start_num), Ok(end_num)) = (start.parse::<usize>(), end.parse::<usize>()) {
                cores.extend(start_num..=end_num);
            }
        } else {
            // Single core
            if let Ok(core) = range.parse::<usize>() {
                cores.push(core);
            }
        }
    }

    cores.sort_unstable();
    cores.dedup();
    cores
}
