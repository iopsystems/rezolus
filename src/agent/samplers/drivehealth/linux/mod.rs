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
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry =
    crate::agent::samplers::SamplerEntry { name: NAME, init };

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
        let drives = self.drives.clone();
        let reading = self.reading.clone();
        tokio::task::spawn_blocking(move || {
            let readings = read_all(&drives);
            let ok = readings
                .iter()
                .filter(|r| r.temperature_c.is_some())
                .count();
            for (idx, r) in readings.into_iter().enumerate() {
                // Bind the window before r.nvme is moved below.
                let win = r.window;
                if let Some(celsius) = r.temperature_c {
                    let _ = DRIVE_TEMPERATURE.set(idx, celsius);
                }
                if let Some(w) = win {
                    DRIVE_TEMPERATURE.set_window(idx, w.begin_ns, w.end_ns);
                }
                // NVMe thermal-throttle counters (from the same log-page read).
                if let Some(h) = r.nvme {
                    let _ = DRIVE_TEMPERATURE_WARNING_TIME.set(idx, h.warning_temp_time_s);
                    let _ = DRIVE_TEMPERATURE_CRITICAL_TIME.set(idx, h.critical_temp_time_s);
                    let _ = DRIVE_THERMAL_THROTTLE_TIME_1.set(idx, h.thermal_mgmt_time_s[0]);
                    let _ = DRIVE_THERMAL_THROTTLE_TIME_2.set(idx, h.thermal_mgmt_time_s[1]);
                    let _ = DRIVE_THERMAL_THROTTLE_TRANSITIONS_1
                        .set(idx, h.thermal_mgmt_transitions[0]);
                    let _ = DRIVE_THERMAL_THROTTLE_TRANSITIONS_2
                        .set(idx, h.thermal_mgmt_transitions[1]);
                    if let Some(w) = win {
                        DRIVE_TEMPERATURE_WARNING_TIME.set_window(idx, w.begin_ns, w.end_ns);
                        DRIVE_TEMPERATURE_CRITICAL_TIME.set_window(idx, w.begin_ns, w.end_ns);
                        DRIVE_THERMAL_THROTTLE_TIME_1.set_window(idx, w.begin_ns, w.end_ns);
                        DRIVE_THERMAL_THROTTLE_TIME_2.set_window(idx, w.begin_ns, w.end_ns);
                        DRIVE_THERMAL_THROTTLE_TRANSITIONS_1.set_window(idx, w.begin_ns, w.end_ns);
                        DRIVE_THERMAL_THROTTLE_TRANSITIONS_2.set_window(idx, w.begin_ns, w.end_ns);
                    }
                }
            }
            debug!("{NAME}: read {ok}/{} drive temperatures", drives.len());
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
            tokio::time::sleep(Duration::from_secs(2)).await; // let reads finish
        });

        let set: Vec<(usize, i64)> = (0..sampler.drives.len())
            .filter_map(|i| DRIVE_TEMPERATURE.value(i).map(|v| (i, v)))
            .collect();
        println!("gauge values populated: {}", set.len());
        for (i, v) in set.iter().take(5) {
            println!("  idx {i} = {v} C");
        }
        assert!(!set.is_empty(), "no gauge values populated after refresh");

        // Each populated drive must also carry a non-zero acquisition window.
        for (i, _) in set.iter().take(5) {
            let w = DRIVE_TEMPERATURE
                .load_window(*i)
                .unwrap_or_else(|| panic!("no window recorded for drive {i}"));
            println!(
                "  idx {i} window = [{}, {}] ({} ns)",
                w.begin_ns,
                w.end_ns,
                w.width_ns()
            );
            assert!(w.end_ns >= w.begin_ns);
            assert!(
                w.width_ns() > 0,
                "read window should be non-zero for drive {i}"
            );
        }
    }
}
