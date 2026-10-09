//! NVMe drive health on macOS: the SMART / Health log page (0x02) read through
//! IOKit's NVMe SMART user client, which needs no privileges. It publishes the
//! same metrics as the Linux sampler: composite temperature and the
//! thermal-throttle counters.
//!
//! Drives are the I/O registry's block storage devices whose
//! `NVMe SMART Capable` property is true, found once at startup. SATA drives
//! are not read here. A device accepts one SMART client at a time, so each
//! sweep opens and releases it, and a sweep that finds it held by another
//! process (`smartctl`, for one) skips that drive until the next sweep.

const NAME: &str = "drivehealth";

const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(60);

use crate::agent::samplers::iokit::{CfString, NvmeSmart, Service};
use crate::agent::*;
use metriken::CounterGroup;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod stats {
    include!("../linux/stats.rs");
}

use super::nvme_log::parse_health;
use stats::*;

static NVME_COUNTER_GROUPS: &[&dyn metriken::group::SlotMetadata] = &[
    &DRIVE_TEMPERATURE_WARNING_TIME,
    &DRIVE_TEMPERATURE_CRITICAL_TIME,
    &DRIVE_THERMAL_THROTTLE_TIME_1,
    &DRIVE_THERMAL_THROTTLE_TIME_2,
    &DRIVE_THERMAL_THROTTLE_TRANSITIONS_1,
    &DRIVE_THERMAL_THROTTLE_TRANSITIONS_2,
];

static SWEEP_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(SWEEP_IDENTITY_GROUPS);

static SWEEP_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[&[&DRIVE_TEMPERATURE]];

static NVME_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(NVME_IDENTITY_GROUPS);

static NVME_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[NVME_COUNTER_GROUPS];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let interval = config
        .sampler_interval(NAME)
        .unwrap_or(DEFAULT_READ_INTERVAL);

    let mut drives = enumerate();
    drives.truncate(MAX_DRIVES);
    if drives.is_empty() {
        return Err(crate::agent::sampler_status::Unsupported(
            "no NVMe SMART capable drives found".to_string(),
        )
        .into());
    }

    // Every drive here is NVMe, so both groups have the same members.
    DRIVEHEALTH_SWEEP_ACQ.set_member_bound(drives.len());
    DRIVEHEALTH_NVME_ACQ.set_member_bound(drives.len());
    for (idx, drive) in drives.iter().enumerate() {
        SWEEP_IDENTITY.assign(idx, drive.labels.clone());
        NVME_IDENTITY.assign(idx, drive.labels.clone());
    }
    debug!(
        "{NAME}: discovered {} NVMe drive(s); reading every {:?}",
        drives.len(),
        interval
    );

    Ok(Some(Box::new(DriveHealth {
        drives: Arc::new(drives),
        interval,
        last_read: Mutex::new(None),
        reading: Arc::new(AtomicBool::new(false)),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Drive {
    service: Service,
    labels: BTreeMap<String, String>,
}

/// The NVMe SMART capable block storage devices, with the same labels the
/// Linux sampler uses: `device` (the BSD name of the whole disk, e.g.
/// `disk0`), `type`, and `model` and `serial` when the device reports them.
fn enumerate() -> Vec<Drive> {
    let (Some(capable), Some(characteristics), Some(product), Some(serial), Some(bsd)) = (
        CfString::new("NVMe SMART Capable"),
        CfString::new("Device Characteristics"),
        CfString::new("Product Name"),
        CfString::new("Serial Number"),
        CfString::new("BSD Name"),
    ) else {
        return Vec::new();
    };
    Service::matching("IOBlockStorageDevice")
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.boolean(&capable) == Some(true))
        .map(|service| {
            let mut labels = BTreeMap::new();
            labels.insert("type".to_string(), "nvme".to_string());
            if let Some(name) = service.search_string(&bsd) {
                labels.insert("device".to_string(), name);
            }
            if let Some(c) = service.dictionary(&characteristics) {
                for (key, label) in [(&product, "model"), (&serial, "serial")] {
                    if let Some(v) = c.get_string(key).map(|v| v.trim().to_string()) {
                        if !v.is_empty() {
                            labels.insert(label.to_string(), v);
                        }
                    }
                }
            }
            Drive { service, labels }
        })
        .collect()
}

struct DriveHealth {
    drives: Arc<Vec<Drive>>,
    interval: Duration,
    /// When the last sweep was dispatched.
    last_read: Mutex<Option<Instant>>,
    /// Set while a sweep runs, so sweeps never overlap and each one is its
    /// groups' only writer.
    reading: Arc<AtomicBool>,
}

#[async_trait]
impl Sampler for DriveHealth {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        {
            let mut last = self.last_read.lock().unwrap_or_else(|e| e.into_inner());
            if last.is_some_and(|t| t.elapsed() < self.interval) {
                return;
            }
            *last = Some(Instant::now());
        }
        if self.reading.swap(true, Ordering::AcqRel) {
            return;
        }
        // Each read is a device command, so the sweep runs on the blocking
        // pool and `refresh` returns without waiting for it, as on Linux.
        let drives = self.drives.clone();
        let reading = self.reading.clone();
        tokio::task::spawn_blocking(move || {
            // Cleared on drop, so a panic in the sweep does not stop every
            // later sweep.
            struct Clear(Arc<AtomicBool>);
            impl Drop for Clear {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _clear = Clear(reading);
            sweep(&drives);
        });
    }
}

/// Read every drive and publish what it returned. The groups' window is
/// stamped only if at least one drive returned a page; otherwise it keeps its
/// previous window, as the Linux sampler does for the sweep group. Returns
/// the number of drives read.
fn sweep(drives: &[Drive]) -> usize {
    let sweep = DRIVEHEALTH_SWEEP_ACQ.acquire();
    let nvme = DRIVEHEALTH_NVME_ACQ.acquire();
    let mut ok = 0;
    for (idx, drive) in drives.iter().enumerate() {
        let Some(h) = NvmeSmart::open(&drive.service)
            .and_then(|s| s.health_log())
            .and_then(|page| parse_health(&page))
        else {
            continue;
        };
        ok += 1;
        if let Some(celsius) = h.temperature_c {
            let _ = DRIVE_TEMPERATURE.set(idx, celsius);
        }
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
    if ok > 0 {
        sweep.finish();
        nvme.finish();
    } else {
        sweep.discard();
        nvme.discard();
    }
    debug!(
        "{NAME}: published readings for {ok}/{} drive(s)",
        drives.len()
    );
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The read needs no privileges, so this runs on any Mac with an NVMe
    /// SMART capable drive (every Apple silicon Mac's internal SSD is one),
    /// and skips elsewhere.
    #[test]
    fn reads_the_internal_ssd() {
        let _lock = crate::agent::samplers::iokit::NVME_SMART_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let drives = enumerate();
        if drives.is_empty() {
            eprintln!("no NVMe SMART capable drive on this host; skipping");
            return;
        }
        assert!(
            drives.iter().all(|d| d.labels.contains_key("device")),
            "a drive has no BSD name"
        );
        // A device accepts one SMART client at a time across processes.
        if drives.iter().any(|d| NvmeSmart::open(&d.service).is_none()) {
            eprintln!("an NVMe SMART device is held by another process; skipping");
            return;
        }
        assert_eq!(sweep(&drives), drives.len(), "a drive returned no page");
    }
}
