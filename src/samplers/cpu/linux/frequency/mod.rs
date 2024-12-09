//! Collects CPU frequency using a combination of MSRs.
//!
//! Initializes perf events to collect TSC, APERF, MPERF.
//!
//! Produces:
//! * `cpu/base_frequency`
//! * `cpu/running_frequency`

const NAME: &str = "cpu_frequency";

mod group;

use group::*;

use crate::common::*;
use crate::samplers::cpu::linux::stats::*;
use crate::samplers::Sampler;
use crate::*;

use parking_lot::Mutex;

use tokio::task::spawn_blocking;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = PerfInner::new()?;

    Ok(Some(Box::new(Perf {
        inner: Arc::new(Mutex::new(inner)),
    })))
}

pub struct Perf {
    inner: Arc<Mutex<PerfInner>>,
}

struct PerfInner {
    groups: Vec<PerfGroup>,
    gauges: ScopedGauges,
}

impl PerfInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let cpus = common::linux::cpus()?;

        let mut groups = Vec::with_capacity(cpus.len());
        let mut gauges = ScopedGauges::new();

        for cpu in cpus {
            gauges.push(
                cpu,
                DynamicGaugeBuilder::new("cpu/frequency")
                    .metadata("id", format!("{}", cpu))
                    .formatter(cpu_metric_percore_formatter)
                    .build(),
            );

            match PerfGroup::new(cpu) {
                Ok(g) => groups.push(g),
                Err(_) => {
                    warn!("Failed to create the perf group on CPU {}", cpu);
                    // we want to continue because it's possible that this CPU is offline
                    continue;
                }
            };
        }

        if groups.is_empty() {
            return Err(std::io::Error::other(
                "Failed to create perf group on any CPU",
            ));
        }

        Ok(Self { groups, gauges })
    }

    /// Refreshes the metrics from the underlying perf counter groups.
    ///
    /// *Note:* the reading returned by `get_metrics()` returns delta'd counters
    /// so instead of setting our counters, we will add the delta to them.
    pub fn refresh(&mut self) {
        let mut nr_active_groups: u64 = 0;

        let mut avg_base_frequency = 0;
        let mut avg_running_frequency = 0;

        for group in &mut self.groups {
            if let Ok(reading) = group.get_metrics() {
                nr_active_groups += 1;

                avg_base_frequency += reading.base_frequency_mhz.unwrap_or(0);
                avg_running_frequency += reading.running_frequency_mhz.unwrap_or(0);

                if let Some(g) = reading.running_frequency_mhz {
                    let _ = self.gauges.set(reading.cpu, 0, g as _);
                }
            }

            // we can only update averages if at least one group of perf
            // counters was active in the period
            if nr_active_groups > 0 {
                CPU_BASE_FREQUENCY_AVERAGE.set((avg_base_frequency / nr_active_groups) as i64);
                CPU_FREQUENCY_AVERAGE.set((avg_running_frequency / nr_active_groups) as i64);
            }
        }
    }
}

#[async_trait]
impl Sampler for Perf {
    async fn refresh(&self) {
        let inner = self.inner.clone();

        // we spawn onto a blocking thread because this can take on the order of
        // tens of milliseconds on large systems

        let _ = spawn_blocking(move || {
            let mut inner = inner.lock();
            inner.refresh();
        })
        .await;
    }
}
