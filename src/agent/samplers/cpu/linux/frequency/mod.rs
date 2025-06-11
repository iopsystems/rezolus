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

use std::collections::BTreeSet;
use std::sync::mpsc::sync_channel;
use std::thread::JoinHandle;

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
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh().await;
    }
}

struct FrequencyInner {
    perf_threads: Vec<std::thread::JoinHandle<()>>,
    perf_sync: Vec<SyncPrimitive>,
}

impl FrequencyInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let (perf_threads, perf_sync) = get_cores()?;

        Ok(Self {
            perf_threads,
            perf_sync,
        })
    }

    pub async fn refresh(&mut self) -> Result<(), std::io::Error> {
        // check that no perf threads have exited
        for thread in self.perf_threads.iter() {
            if thread.is_finished() {
                panic!("{} perf thread exited early", NAME);
            }
        }

        // trigger and wait on all perf threads
        let perf_futures: Vec<_> = self
            .perf_sync
            .iter()
            .map(|s| {
                s.trigger();
                s.wait_notify()
            })
            .collect();

        futures::future::join_all(perf_futures.into_iter()).await;

        Ok(())
    }
}

/// A struct that represents a physical core and contains the counters necessary
/// for calculating running frequency
struct Core {
    /// perf events for this core
    aperf: perf_event::Counter,
    mperf: perf_event::Counter,
    tsc: perf_event::Counter,
    /// the core id
    id: usize,
}

impl Core {
    pub fn new(id: usize) -> Result<Self, ()> {
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
            .one_cpu(id)
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
                    .one_cpu(id)
                    .any_pid()
                    .exclude_hv(false)
                    .exclude_kernel(false)
                    .build_with_group(&mut tsc)
                {
                    Ok(aperf) => {
                        match perf_event::Builder::new(mperf_event)
                            .one_cpu(id)
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
                                    id,
                                }),
                                Err(e) => {
                                    error!("failed to enable the perf group on CPU{id}: {e}");
                                    Err(())
                                }
                            },
                            Err(e) => {
                                debug!("failed to enable the mperf counter on CPU{id}: {e}");
                                Err(())
                            }
                        }
                    }
                    Err(e) => {
                        debug!("failed to enable the aperf counter on CPU{id}: {e}");
                        Err(())
                    }
                }
            }
            Err(e) => {
                debug!("failed to enable the tsc counter on CPU{id}: {e}");
                Err(())
            }
        }
    }

    pub fn refresh(&mut self) {
        if let Ok(group) = self.tsc.read_group() {
            if let (Some(aperf), Some(mperf), Some(tsc)) = (
                group.get(&self.aperf),
                group.get(&self.mperf),
                group.get(&self.tsc),
            ) {
                let aperf = aperf.value();
                let mperf = mperf.value();
                let tsc = tsc.value();

                let _ = CPU_APERF.set(self.id, aperf);
                let _ = CPU_MPERF.set(self.id, mperf);
                let _ = CPU_TSC.set(self.id, tsc);
            }
        }
    }
}

fn logical_cores() -> Result<Vec<usize>, std::io::Error> {
    let mut cores: BTreeSet<usize> = BTreeSet::new();

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
            if let Ok(core_id) = filename[3..].parse() {
                cores.insert(core_id);
            }
        }
    }

    Ok(cores.iter().copied().collect())
}

fn get_cores() -> Result<(Vec<JoinHandle<()>>, Vec<SyncPrimitive>), std::io::Error> {
    let mut logical_cores = logical_cores()?;

    let (unpinned_tx, unpinned_rx) = sync_channel(logical_cores.len());

    let pt_pending = Arc::new(AtomicUsize::new(logical_cores.len()));

    let mut perf_threads = Vec::new();
    let mut perf_sync = Vec::new();

    for core in logical_cores.drain(..) {
        if let Ok(mut core) = Core::new(core) {
            let psync = SyncPrimitive::new();
            let psync2 = psync.clone();

            let unpinned = unpinned_tx.clone();
            let pt_pending = pt_pending.clone();

            perf_threads.push(std::thread::spawn(move || {
                if !core_affinity::set_for_current(core_affinity::CoreId { id: core.id }) {
                    unpinned.send(core).expect("failed to send unpinned perf counters");
                    pt_pending.fetch_sub(1, Ordering::Relaxed);
                    return;
                }

                pt_pending.fetch_sub(1, Ordering::Relaxed);

                loop {
                    psync.wait_trigger();

                    core.refresh();

                    psync.notify();
                }
            }));

            perf_sync.push(psync2);
        }
    }

    debug!("{} waiting for perf threads to launch", NAME);

    while pt_pending.load(Ordering::Relaxed) > 0 {
        std::thread::sleep(Duration::from_millis(50));
    }

    debug!("{} checking for unpinned perf threads", NAME);

    let mut unpinned: Vec<Core> = unpinned_rx.try_iter().collect();

    debug!(
        "{} there are {} perf threads which could not be pinned",
        NAME,
        unpinned.len()
    );

    if !unpinned.is_empty() {
        let psync = SyncPrimitive::new();
        let psync2 = psync.clone();

        perf_threads.push(std::thread::spawn(move || loop {
            psync.wait_trigger();

            for core in unpinned.iter_mut() {
                core.refresh();
            }

            psync.notify();
        }));

        perf_sync.push(psync2);
    }

    Ok((perf_threads, perf_sync))
}
