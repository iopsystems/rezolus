//! Collects CPU branch performance counters:
//!
//! * `cpu_branch_instructions` - Total branch instructions retired
//! * `cpu_branch_misses` - Branch mispredictions
//!
//! These stats can be used to calculate branch misprediction rate:
//! misprediction_rate = cpu_branch_misses / cpu_branch_instructions
//!
//! Uses portable hardware perf events that work across Intel, AMD, and ARM.

const NAME: &str = "cpu_branch";

use crate::agent::*;

use perf_event::events::Hardware;
use perf_event::ReadFormat;
use tokio::sync::Mutex;
use walkdir::WalkDir;

use std::collections::BTreeSet;
use std::thread::JoinHandle;

mod stats;

use stats::*;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = BranchInner::new()?;

    Ok(Some(Box::new(Branch {
        inner: inner.into(),
    })))
}

struct Branch {
    inner: Mutex<BranchInner>,
}

#[async_trait]
impl Sampler for Branch {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh().await;
    }
}

struct BranchInner {
    perf_threads: Vec<std::thread::JoinHandle<()>>,
    perf_sync: Vec<SyncPrimitive>,
}

impl BranchInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let (perf_threads, perf_sync) = spawn_threads()?;

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

/// A struct that represents a CPU core and contains the branch counters
struct Core {
    /// perf events for this core
    branch_instructions: perf_event::Counter,
    branch_misses: perf_event::Counter,
    /// the core id
    id: usize,
}

impl Core {
    pub fn new(id: usize) -> Result<Self, ()> {
        match perf_event::Builder::new(Hardware::BRANCH_INSTRUCTIONS)
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
            Ok(mut branch_instructions) => {
                match perf_event::Builder::new(Hardware::BRANCH_MISSES)
                    .one_cpu(id)
                    .any_pid()
                    .exclude_hv(false)
                    .exclude_kernel(false)
                    .build_with_group(&mut branch_instructions)
                {
                    Ok(branch_misses) => match branch_instructions.enable_group() {
                        Ok(_) => Ok(Core {
                            branch_instructions,
                            branch_misses,
                            id,
                        }),
                        Err(e) => {
                            error!("failed to enable the perf group on CPU{id}: {e}");
                            Err(())
                        }
                    },
                    Err(e) => {
                        debug!("failed to enable the branch_misses counter on CPU{id}: {e}");
                        Err(())
                    }
                }
            }
            Err(e) => {
                debug!("failed to enable the branch_instructions counter on CPU{id}: {e}");
                Err(())
            }
        }
    }

    pub fn refresh(&mut self) {
        match self.branch_instructions.read_group() {
            Ok(group) => {
                match (
                    group.get(&self.branch_instructions),
                    group.get(&self.branch_misses),
                ) {
                    (Some(instructions), Some(misses)) => {
                        let instructions = instructions.value();
                        let misses = misses.value();

                        trace!(
                            "CPU{} branch: instructions={}, misses={}",
                            self.id,
                            instructions,
                            misses
                        );

                        let _ = CPU_BRANCH_INSTRUCTIONS.set(self.id, instructions);
                        let _ = CPU_BRANCH_MISSES.set(self.id, misses);
                    }
                    (instr, miss) => {
                        debug!(
                            "CPU{} branch group.get() returned None: instructions={:?}, misses={:?}",
                            self.id,
                            instr.is_some(),
                            miss.is_some()
                        );
                    }
                }
            }
            Err(e) => {
                debug!("CPU{} branch read_group() failed: {}", self.id, e);
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

fn spawn_threads() -> Result<(Vec<JoinHandle<()>>, Vec<SyncPrimitive>), std::io::Error> {
    // on virtualized environments, it is typically better to use multiple
    // threads to read the perf counters to get more consistent snapshot latency
    if is_virt() {
        spawn_threads_multi()
    } else {
        spawn_threads_single()
    }
}

fn spawn_threads_single() -> Result<(Vec<JoinHandle<()>>, Vec<SyncPrimitive>), std::io::Error> {
    debug!("using single-threaded perf counter collection");

    let mut logical_cores = logical_cores()?;

    let mut perf_threads = Vec::new();
    let mut perf_sync = Vec::new();

    let psync = SyncPrimitive::new();
    let psync2 = psync.clone();

    let mut cores = Vec::new();

    for core in logical_cores.drain(..) {
        if let Ok(core) = Core::new(core) {
            cores.push(core);
        }
    }

    if cores.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "no cores available for branch sampling",
        ));
    }

    perf_threads.push(std::thread::spawn(move || loop {
        psync.wait_trigger();

        for core in cores.iter_mut() {
            core.refresh();
        }

        psync.notify();
    }));

    perf_sync.push(psync2);

    Ok((perf_threads, perf_sync))
}

fn spawn_threads_multi() -> Result<(Vec<JoinHandle<()>>, Vec<SyncPrimitive>), std::io::Error> {
    debug!("using multi-threaded perf counter collection");

    let mut logical_cores = logical_cores()?;

    let pt_pending = Arc::new(AtomicUsize::new(logical_cores.len()));

    let mut perf_threads = Vec::new();
    let mut perf_sync = Vec::new();

    for core in logical_cores.drain(..) {
        if let Ok(mut core) = Core::new(core) {
            let psync = SyncPrimitive::new();
            let psync2 = psync.clone();

            let pt_pending = pt_pending.clone();

            perf_threads.push(std::thread::spawn(move || {
                if !core_affinity::set_for_current(core_affinity::CoreId { id: core.id }) {
                    warn!("failed to pin perf thread for core: {}", core.id);
                }

                pt_pending.fetch_sub(1, Ordering::Relaxed);

                loop {
                    psync.wait_trigger();

                    core.refresh();

                    psync.notify();
                }
            }));

            perf_sync.push(psync2);
        } else {
            pt_pending.fetch_sub(1, Ordering::Relaxed);
        }
    }

    if perf_threads.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "no cores available for branch sampling",
        ));
    }

    debug!("{} waiting for perf threads to launch", NAME);

    while pt_pending.load(Ordering::Relaxed) > 0 {
        std::thread::sleep(Duration::from_millis(50));
    }

    debug!("all perf threads launched");

    Ok((perf_threads, perf_sync))
}
