//! Per-drive temperature for NVMe and SATA drives via read-only pass-through
//! ioctls — no kernel module required.
//!
//! Drive temperature originates in the drive's own health data; there is no BPF
//! or perf hook for it, so this sampler reads it from the device. It is the
//! deliberate principle-15 exception (see `docs/principles.md`): discovery is
//! one-time at startup (sysfs), and temperature is read with a read-only
//! command per drive — ATA `SMART READ DATA` via `SG_IO` for SATA
//! ([`ata`]), NVMe Get Log Page 0x02 for NVMe ([`nvme`]). No `drivetemp` or any
//! other module is loaded; `smartctl`/`hddtemp` use the same mechanism.
//!
//! Each read is a device command (measured ~ms), so reads are **not** driven on
//! the scrape/TTL sample cycle (principle 17). `refresh()` does a cheap time
//! check and, at most once per `interval` (`[samplers.drivehealth]`, default
//! 60s), dispatches the reads to Tokio's blocking pool (`spawn_blocking`, all
//! drives in parallel) and returns immediately. The gauge retains its last value
//! between reads.

const NAME: &str = "drivehealth";

/// Built-in read cadence when `[samplers.drivehealth] interval` is unset.
/// Chosen because drive temperature drifts slowly and each read costs a device
/// command; 60s keeps the amortized cost negligible on large JBODs.
const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(60);

use crate::agent::*;
use metriken::CounterGroup;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod ata;
mod device;
mod nvme;
mod stats;

use device::*;
use stats::*;

/// The NVMe-only throttle counter groups, in a fixed order, for per-drive label
/// application at discovery.
static NVME_COUNTER_GROUPS: &[&dyn GroupMetadata] = &[
    &DRIVE_TEMPERATURE_WARNING_TIME,
    &DRIVE_TEMPERATURE_CRITICAL_TIME,
    &DRIVE_THERMAL_THROTTLE_TIME_1,
    &DRIVE_THERMAL_THROTTLE_TIME_2,
    &DRIVE_THERMAL_THROTTLE_TRANSITIONS_1,
    &DRIVE_THERMAL_THROTTLE_TRANSITIONS_2,
];

/// Apply the per-drive labels (`device`, `type`, and `model`/`serial` when
/// present) to one metric group at index `idx`.
fn label_group(group: &dyn GroupMetadata, idx: usize, drive: &Drive) {
    group.insert_metadata(idx, "device".to_string(), drive.device.clone());
    group.insert_metadata(
        idx,
        "type".to_string(),
        drive.drive_type.as_str().to_string(),
    );
    if !drive.model.is_empty() {
        group.insert_metadata(idx, "model".to_string(), drive.model.clone());
    }
    if !drive.serial.is_empty() {
        group.insert_metadata(idx, "serial".to_string(), drive.serial.clone());
    }
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let interval = config
        .sampler_interval(NAME)
        .unwrap_or(DEFAULT_READ_INTERVAL);

    // Robust to absence: a host with no supported drive (or no privilege for the
    // ioctl) discovers zero drives / reads nothing and emits no series rather
    // than failing the agent.
    Ok(Some(Box::new(DriveHealth::new(interval))))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct DriveHealth {
    /// Drives found once at startup. `Arc` so a `spawn_blocking` read can borrow
    /// them without cloning the list each round.
    drives: Arc<Vec<Drive>>,
    /// Minimum spacing between reads.
    interval: Duration,
    /// Timestamp of the last dispatched read; `None` until the first read.
    last_read: Mutex<Option<Instant>>,
    /// True while a read is in flight, so we never overlap reads.
    reading: Arc<AtomicBool>,
}

impl DriveHealth {
    fn new(interval: Duration) -> Self {
        let mut drives = enumerate();
        drives.truncate(MAX_DRIVES);

        // Real population, not backing capacity (principle 18: "membership
        // comes from registration, not values"). `drives` is dense — every
        // index in `0..drives.len()` is a real, discovered drive — and every
        // per-drive set() (`refresh()`, below) indexes `DRIVE_TEMPERATURE`/
        // the NVMe counter groups by that same contiguous index, so a prefix
        // bound is correct here (not the metadata-presence mechanism, which
        // is for sparse/task-style membership). Set once at startup
        // discovery, before any snapshot walk reads it.
        DRIVEHEALTH_SWEEP_ACQ.set_member_bound(drives.len());

        // Per-index labels are read once at discovery and never change for the
        // life of the process (startup-only discovery; hotplug is out of scope
        // for Phase 1). Temperature is labeled for every drive; the NVMe-only
        // throttle counters are labeled only for NVMe drives.
        for (idx, drive) in drives.iter().enumerate() {
            label_group(&DRIVE_TEMPERATURE, idx, drive);
            if drive.drive_type == DriveType::Nvme {
                for group in NVME_COUNTER_GROUPS {
                    label_group(*group, idx, drive);
                }
            }
        }

        if drives.is_empty() {
            debug!("{NAME}: no NVMe or SATA drives found");
        } else {
            debug!(
                "{NAME}: discovered {} drive(s); reading temperature every {:?}",
                drives.len(),
                interval
            );
        }

        Self {
            drives: Arc::new(drives),
            interval,
            last_read: Mutex::new(None),
            reading: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait]
impl Sampler for DriveHealth {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        if self.drives.is_empty() {
            return;
        }

        // Throttle: dispatch a read at most once per `interval`. Cheap time
        // check on the scrape path.
        {
            let mut last = self.last_read.lock().unwrap();
            match *last {
                Some(t) if t.elapsed() < self.interval => return,
                _ => *last = Some(Instant::now()),
            }
        }

        // Never overlap reads.
        if self.reading.swap(true, Ordering::AcqRel) {
            return;
        }

        // Offload to the blocking pool and return immediately: each read is a
        // device command, so it must not run on the async worker. Drives are
        // read in parallel; the gauge is updated when the read completes.
        //
        // Acquisition-group bracket (principle 18): this `spawn_blocking`
        // task is `DRIVEHEALTH_SWEEP_ACQ`'s single writer — `refresh()`
        // itself never acquires/finishes the group, only dispatches this
        // task (and the `reading` flag above already prevents two sweeps
        // from overlapping, so there is structurally never a second task
        // that could race this one for the group). `acquire()` brackets
        // `read_all` plus the per-drive `set()` loop that follows it;
        // `finish()` publishes only if the sweep published at least one
        // value — a sweep where every drive's read failed to produce
        // ANYTHING publishable (`ok == 0`) discards (drops the guard)
        // instead, leaving the group's previous window standing rather than
        // pairing a fully-failed sweep with a fresh one. "Published
        // something" means either a temperature or an NVMe throttle-counter
        // reading, not temperature alone: a cleanly-parsed NVMe drive that
        // legitimately reports no temperature (composite temperature 0K —
        // see `nvme::read_health`) still yields six throttle counters from
        // the same log-page read, and a fleet of such drives must not go
        // permanently unstamped just because `temperature_c` is always
        // `None` for them. (See `stats::DRIVEHEALTH_SWEEP_ACQ`'s doc comment
        // for the walk-union window behavior this bracket inherits from
        // every declared group.)
        let drives = self.drives.clone();
        let reading = self.reading.clone();
        tokio::task::spawn_blocking(move || {
            let guard = DRIVEHEALTH_SWEEP_ACQ.acquire();

            let readings = read_all(&drives);
            let ok = readings
                .iter()
                .filter(|r| r.temperature_c.is_some() || r.nvme.is_some())
                .count();
            for (idx, r) in readings.into_iter().enumerate() {
                if let Some(celsius) = r.temperature_c {
                    let _ = DRIVE_TEMPERATURE.set(idx, celsius);
                }
                // NVMe thermal-throttle counters (from the same log-page read).
                if let Some(h) = r.nvme {
                    let counters: [(&CounterGroup, u64); 6] = [
                        (&DRIVE_TEMPERATURE_WARNING_TIME, h.warning_temp_time_s),
                        (&DRIVE_TEMPERATURE_CRITICAL_TIME, h.critical_temp_time_s),
                        (&DRIVE_THERMAL_THROTTLE_TIME_1, h.thermal_mgmt_time_s[0]),
                        (&DRIVE_THERMAL_THROTTLE_TIME_2, h.thermal_mgmt_time_s[1]),
                        (
                            &DRIVE_THERMAL_THROTTLE_TRANSITIONS_1,
                            h.thermal_mgmt_transitions[0],
                        ),
                        (
                            &DRIVE_THERMAL_THROTTLE_TRANSITIONS_2,
                            h.thermal_mgmt_transitions[1],
                        ),
                    ];
                    for (group, value) in counters {
                        let _ = group.set(idx, value);
                    }
                }
            }

            if ok > 0 {
                guard.finish();
            } else {
                guard.discard();
            }

            debug!(
                "{NAME}: published readings for {ok}/{} drive(s)",
                drives.len()
            );
            reading.store(false, Ordering::Release);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hardware integration test — requires root and at least one drive.
    /// Ignored by default. Exercises the real async dispatch: `refresh()` on a
    /// tokio runtime must populate the gauge via `spawn_blocking`. Run:
    ///   cargo test --bin rezolus --no-run
    ///   sudo ./target/debug/deps/rezolus-* drivehealth::linux::tests -- --ignored --nocapture
    #[test]
    #[ignore]
    fn hardware_refresh_populates_gauge() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();

        let sampler = DriveHealth::new(Duration::from_millis(1));
        println!("discovered {} drive(s)", sampler.drives.len());

        rt.block_on(async {
            sampler.refresh().await; // dispatches spawn_blocking
                                     // Reads run on the blocking pool; a full JBOD takes 0.2–2.3 s and the
                                     // latency swings run to run, so poll (up to ~10 s) rather than a
                                     // fragile fixed sleep.
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if (0..sampler.drives.len()).any(|i| DRIVE_TEMPERATURE.value(i).is_some()) {
                    break;
                }
            }
            // Let any straggler parallel reads land before we assert.
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        let set: Vec<(usize, i64)> = (0..sampler.drives.len())
            .filter_map(|i| DRIVE_TEMPERATURE.value(i).map(|v| (i, v)))
            .collect();
        println!("gauge values populated: {}", set.len());
        for (i, v) in set.iter().take(5) {
            println!("  idx {i} = {v} C");
        }
        assert!(!set.is_empty(), "no gauge values populated after refresh");

        // The sweep's single acquisition group must carry a non-zero window
        // (principle 18: one group for the whole sweep, not one per drive —
        // see `stats::DRIVEHEALTH_SWEEP_ACQ`'s doc comment).
        let w = DRIVEHEALTH_SWEEP_ACQ
            .window()
            .expect("no window recorded for the sweep");
        println!(
            "  sweep window = [{}, {}] ({} ns)",
            w.begin_ns,
            w.end_ns,
            w.width_ns()
        );
        assert!(w.end_ns >= w.begin_ns);
        assert!(w.width_ns() > 0, "read window should be non-zero");
    }

    // The async tear case this sampler used to be exposed to (a background
    // writer racing a reader over a per-entry `set_with_window`/
    // `load_with_window` pair) no longer applies: `DRIVE_TEMPERATURE` is a
    // plain `GaugeGroup` now, written with `set()` only, and its acquisition
    // window comes from `DRIVEHEALTH_SWEEP_ACQ`'s single-writer group slot
    // (see `crate::agent::timing::GroupWindowSlot`, which has its own
    // tear-freedom test) rather than a per-entry windowed cell. The
    // metriken-level torn-read guarantee for `WindowedGaugeGroup` itself is
    // still covered directly in that crate's own test suite.
}
