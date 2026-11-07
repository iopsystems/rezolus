//! Collects AMD APU telemetry from amdgpu's sysfs files.
//!
//! `gpu_amd_smi` needs the ROCm SMI library, which an APU host running a
//! Vulkan or graphics workload usually does not have. This sampler reads the
//! firmware's metrics from `/sys/class/drm/card*/device/gpu_metrics` and needs
//! only the kernel driver. These values have no BPF or perf source, so each
//! read parses a sysfs file. It does not read `hwmon`: `hw_sensors` already
//! publishes amdgpu's voltages, temperature and power from there.
//!
//! ## Scope
//!
//! A GPU is read only if its `gpu_metrics` is format v2.1, the only format
//! checked against a capture (see [`gpu_metrics`]). That is an APU format;
//! discrete cards report v1.x and are read by `gpu_amd_smi`.
//!
//! ## Why it is unsupported beside ROCm
//!
//! `gpu_amd_smi` and this sampler both number GPUs from 0 under
//! `vendor="amd"`, so running both on one GPU would publish two `gpu_clock`
//! series for one `(id, vendor)`, and the dashboard's `sum by (id, vendor)`
//! would double them. When `gpu_amd_smi` is enabled and ROCm SMI finds a GPU,
//! this sampler reports unsupported at init. Disabling `gpu_amd_smi` selects
//! this sampler on such a host.
//!
//! ## What the values mean
//!
//! The clock, activity and power fields are named `average_*`, but the driver
//! fills most of them from unfiltered firmware fields and the firmware's update
//! period is not exposed, so a limit that holds a clock down briefly can show
//! as a small dip or not at all. Socket power on an APU includes the CPU cores,
//! the same scope as `gpu_amd_smi` reports there; on SMU 13.0.4 this field is
//! the firmware's filtered value.
//!
//! The fabric clock is the marked level in `pp_dpm_fclk`, because SMU 13.0.4
//! fills `average_fclk_frequency` from the memory clock.
//!
//! ## Cadence
//!
//! A `gpu_metrics` read makes the driver fetch the firmware's metrics table and
//! a `pp_dpm_fclk` read sends a firmware message: measured on a Radeon 780M at
//! about 0.3 ms, 2.3 ms at worst, and about 0.04 ms, 0.5 ms at worst. That is
//! too slow for the async worker and too often at the snapshot rate, so this
//! sampler reads on its own interval (`[samplers.gpu_amd_sysfs] interval`,
//! default 1s) in `spawn_blocking`, as principle 17 requires, and serves the
//! last values in between.

const NAME: &str = "gpu_amd_sysfs";

use crate::agent::timing::AcquisitionGroup;
use crate::agent::*;

use metriken::GaugeGroup;

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

mod gpu_metrics;
mod stats;

use gpu_metrics::{current_level_mhz, FormatError, GpuMetrics};
use stats::*;

use super::MAX_GPUS;

const SYSFS_DRM: &str = "/sys/class/drm";

/// `/sys/class/drm/card*/device/vendor` for AMD.
const VENDOR_AMD: &str = "0x1002";

const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(1);

/// How early a scrape may arrive and still be served with a fresh read, so
/// that a consumer scraping at `interval` does not have every other scrape
/// rejected by timer jitter.
const SCRAPE_TOLERANCE: Duration = Duration::from_millis(50);

/// Larger than any `gpu_metrics` format, so that a format this sampler does
/// not accept still reads whole and is reported by its header.
const READ_BUFFER: usize = 4096;

const HZ_PER_MHZ: i64 = 1_000_000;

fn init(config: Arc<Config>) -> SamplerResult {
    // Zero first, so every exit below leaves the group empty rather than at
    // backing capacity, which would declare phantom all-null members.
    GPU_AMD_SYSFS_ACQ.set_member_bound(0);

    if !config.enabled(NAME) {
        return Ok(None);
    }

    let (gpus, refused) = discover(Path::new(SYSFS_DRM));
    if gpus.is_empty() {
        let reason = if refused.is_empty() {
            "no AMD GPU with gpu_metrics found".to_string()
        } else {
            format!("no supported AMD GPU: {}", refused.join(", "))
        };
        return Err(crate::agent::sampler_status::Unsupported(reason).into());
    }

    // After `discover`, so a host this sampler cannot read never calls
    // `rsmi_init`.
    if config.enabled("gpu_amd_smi") && super::rocm_smi::reports_gpus() {
        return Err(crate::agent::sampler_status::Unsupported(
            "gpu_amd_smi reports this host's AMD GPUs".to_string(),
        )
        .into());
    }

    for card in &refused {
        debug!("{NAME}: left out {card}");
    }

    // `discover` numbers the GPUs it keeps from 0, so a prefix bound is exact.
    GPU_AMD_SYSFS_ACQ.set_member_bound(gpus.len());

    let interval = config
        .sampler_interval(NAME)
        .unwrap_or(DEFAULT_READ_INTERVAL);

    debug!("{NAME}: {} GPU(s), reading every {interval:?}", gpus.len());

    Ok(Some(Box::new(AmdSysfs {
        gpus: Arc::new(std::sync::Mutex::new(gpus)),
        interval,
        last_read: std::sync::Mutex::new(None),
        reading: Arc::new(AtomicBool::new(false)),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct AmdSysfs {
    /// Shared with the `spawn_blocking` task.
    gpus: Arc<std::sync::Mutex<Vec<Gpu>>>,
    /// Minimum time between reads (principle 17).
    interval: Duration,
    /// When the last read was dispatched.
    last_read: std::sync::Mutex<Option<Instant>>,
    /// Guards against overlapping reads if one runs long.
    reading: Arc<AtomicBool>,
}

#[async_trait]
impl Sampler for AmdSysfs {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        {
            let mut last = self.last_read.lock().unwrap();
            let due = self.interval.saturating_sub(SCRAPE_TOLERANCE);
            match *last {
                Some(t) if t.elapsed() < due => return,
                _ => *last = Some(Instant::now()),
            }
        }

        if self.reading.swap(true, Ordering::AcqRel) {
            return;
        }

        let gpus = self.gpus.clone();
        let reading = self.reading.clone();

        tokio::task::spawn_blocking(move || {
            if let Ok(mut gpus) = gpus.lock() {
                sweep(&mut gpus, &GPU_AMD_SYSFS_ACQ);
            }
            reading.store(false, Ordering::Release);
        });
    }
}

/// Read every GPU inside one acquisition window.
fn sweep(gpus: &mut [Gpu], group: &AcquisitionGroup) {
    let guard = group.acquire();
    let mut read_any = false;
    for gpu in gpus.iter_mut() {
        read_any |= gpu.refresh();
    }
    // With no GPU read, no value is from inside this window, and stamping it
    // would present the old values as fresh.
    if read_any {
        guard.finish();
    } else {
        guard.discard();
    }
}

/// One GPU's open sysfs files. Held open so a read is a single `pread`.
struct Gpu {
    id: usize,
    gpu_metrics: File,
    /// Absent if the driver does not expose it for this GPU.
    pp_dpm_fclk: Option<File>,
    buffer: Vec<u8>,
    /// Whether a read has already failed in a way a retry does not fix.
    warned: bool,
}

impl Gpu {
    /// Read and publish one GPU. Returns whether `gpu_metrics` read and
    /// parsed; on false this GPU keeps its previous values.
    fn refresh(&mut self) -> bool {
        let metrics = match read_metrics(&self.gpu_metrics, &mut self.buffer) {
            Ok(metrics) => metrics,
            Err(e) if e.is_transient() || self.warned => {
                debug!("{NAME}: GPU {}: {e}", self.id);
                return false;
            }
            Err(e) => {
                warn!("{NAME}: GPU {}: {e}", self.id);
                self.warned = true;
                return false;
            }
        };
        publish(self.id, &metrics);

        if let Some(file) = &self.pp_dpm_fclk {
            match file.read_at(&mut self.buffer, 0) {
                Ok(n) => {
                    let text = std::str::from_utf8(&self.buffer[..n]).unwrap_or("");
                    match current_level_mhz(text) {
                        Some(mhz) => {
                            let _ = GPU_CLOCK_FABRIC.set(self.id, mhz as i64 * HZ_PER_MHZ);
                        }
                        None => debug!("{NAME}: GPU {}: no marked fclk level", self.id),
                    }
                }
                Err(e) => debug!("{NAME}: GPU {}: pp_dpm_fclk read failed: {e}", self.id),
            }
        }

        true
    }
}

#[derive(Debug)]
enum ReadError {
    Io(std::io::Error),
    Format(FormatError),
}

impl ReadError {
    /// Whether the driver refused the read for now. Kernels before 6.18 return
    /// `EPERM` during a GPU reset or system suspend, and later ones `EBUSY`.
    /// From 6.13 a runtime-suspended device returns the same error; before
    /// 6.13 the read wakes it.
    fn is_transient(&self) -> bool {
        match self {
            Self::Io(e) => matches!(e.raw_os_error(), Some(libc::EBUSY | libc::EPERM)),
            Self::Format(_) => false,
        }
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "gpu_metrics read failed: {e}"),
            Self::Format(e) => write!(f, "{e}"),
        }
    }
}

fn read_metrics(file: &File, buffer: &mut [u8]) -> Result<GpuMetrics, ReadError> {
    let n = file.read_at(buffer, 0).map_err(ReadError::Io)?;
    GpuMetrics::parse(&buffer[..n]).map_err(ReadError::Format)
}

fn publish(id: usize, metrics: &GpuMetrics) {
    let clock = |group: &GaugeGroup, mhz: Option<u16>| {
        if let Some(mhz) = mhz {
            let _ = group.set(id, mhz as i64 * HZ_PER_MHZ);
        }
    };
    let gauge = |group: &GaugeGroup, value: Option<i64>| {
        if let Some(value) = value {
            let _ = group.set(id, value);
        }
    };

    clock(&GPU_CLOCK_GRAPHICS, metrics.gfxclk_mhz);
    clock(&GPU_CLOCK_COMPUTE, metrics.gfxclk_mhz);
    clock(&GPU_CLOCK_MEMORY, metrics.uclk_mhz);
    clock(&GPU_CLOCK_SOC, metrics.socclk_mhz);

    gauge(&GPU_POWER_USAGE, metrics.socket_power_mw.map(i64::from));
    gauge(&GPU_RAIL_POWER_VDD, metrics.vdd_power_mw.map(i64::from));
    gauge(&GPU_RAIL_POWER_SOC, metrics.soc_power_mw.map(i64::from));

    gauge(
        &GPU_TEMPERATURE_GRAPHICS,
        metrics.temperature_gfx_centi_c.map(whole_units),
    );
    gauge(
        &GPU_TEMPERATURE_SOC,
        metrics.temperature_soc_centi_c.map(whole_units),
    );
    gauge(
        &GPU_UTILIZATION,
        metrics.gfx_activity_centi_percent.map(whole_units),
    );
    gauge(&GPU_THROTTLE_STATUS, metrics.throttle_status.map(i64::from));
}

/// Round a value in hundredths to whole units, the unit the other samplers
/// publish `gpu_temperature` and `gpu_utilization` in.
fn whole_units(hundredths: u16) -> i64 {
    (hundredths as i64 + 50) / 100
}

/// Whether a GPU stays in the sweep after its first read failed. Discovery runs
/// once, so refusing a GPU that is resetting would leave it unread until the
/// agent restarts. A runtime-suspended device is refused instead: it refuses
/// reads with the same error until something else wakes it, and only a discrete
/// card, which this sampler does not read, suspends that way.
fn keeps_after_failed_read(error: &ReadError, device: &Path) -> bool {
    let suspended = std::fs::read_to_string(device.join("power/runtime_status"))
        .is_ok_and(|status| matches!(status.trim(), "suspended" | "suspending"));
    error.is_transient() && !suspended
}

/// The AMD GPUs under `drm` with a readable, accepted `gpu_metrics`, numbered
/// from 0 in card order, and a description of each one refused.
fn discover(drm: &Path) -> (Vec<Gpu>, Vec<String>) {
    let mut cards: Vec<(u32, std::path::PathBuf)> = std::fs::read_dir(drm)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            // `card0-DP-1` and `renderD128` are not cards.
            let name = entry.file_name();
            let number = name.to_str()?.strip_prefix("card")?.parse().ok()?;
            Some((number, entry.path()))
        })
        .collect();
    cards.sort();

    let (mut gpus, mut refused) = (Vec::new(), Vec::new());
    for (number, card) in cards {
        let device = card.join("device");
        let vendor = std::fs::read_to_string(device.join("vendor")).unwrap_or_default();
        if vendor.trim() != VENDOR_AMD {
            continue;
        }
        if gpus.len() == MAX_GPUS {
            refused.push(format!("card{number}: more than {MAX_GPUS} GPUs"));
            continue;
        }
        let Ok(file) = File::open(device.join("gpu_metrics")) else {
            refused.push(format!("card{number}: no readable gpu_metrics"));
            continue;
        };
        let mut buffer = vec![0; READ_BUFFER];
        if let Err(e) = read_metrics(&file, &mut buffer) {
            if !keeps_after_failed_read(&e, &device) {
                refused.push(format!("card{number}: {e}"));
                continue;
            }
        }
        gpus.push(Gpu {
            id: gpus.len(),
            gpu_metrics: file,
            pp_dpm_fclk: File::open(device.join("pp_dpm_fclk")).ok(),
            buffer,
            warned: false,
        });
    }
    (gpus, refused)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: &[u8] = include_bytes!("fixtures/gpu_metrics_v2_1_idle.bin");
    const LOAD: &[u8] = include_bytes!("fixtures/gpu_metrics_v2_1_load.bin");

    fn card(drm: &Path, name: &str, vendor: &str, metrics: Option<&[u8]>) {
        let device = drm.join(name).join("device");
        std::fs::create_dir_all(&device).unwrap();
        std::fs::write(device.join("vendor"), format!("{vendor}\n")).unwrap();
        if let Some(metrics) = metrics {
            std::fs::write(device.join("gpu_metrics"), metrics).unwrap();
        }
    }

    #[test]
    fn an_amd_apu_is_found_and_its_connectors_and_other_vendors_are_not() {
        let drm = tempfile::tempdir().unwrap();
        card(drm.path(), "card0", "0x10de", Some(LOAD));
        card(drm.path(), "card1", VENDOR_AMD, Some(LOAD));
        card(drm.path(), "card1-DP-1", VENDOR_AMD, Some(LOAD));
        card(drm.path(), "renderD128", VENDOR_AMD, Some(LOAD));

        let (gpus, refused) = discover(drm.path());

        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].id, 0);
        assert!(refused.is_empty(), "{refused:?}");
    }

    #[test]
    fn gpu_ids_stay_dense_past_a_refused_card() {
        let drm = tempfile::tempdir().unwrap();
        let mut other = LOAD.to_vec();
        other[2] = 1;
        card(drm.path(), "card0", VENDOR_AMD, Some(&other));
        card(drm.path(), "card1", VENDOR_AMD, None);
        card(drm.path(), "card2", VENDOR_AMD, Some(LOAD));
        card(drm.path(), "card10", VENDOR_AMD, Some(LOAD));

        let (gpus, refused) = discover(drm.path());

        assert_eq!(gpus.iter().map(|g| g.id).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(
            refused,
            [
                "card0: gpu_metrics format v1.1, 120 bytes",
                "card1: no readable gpu_metrics",
            ]
        );
    }

    #[test]
    fn gpus_are_numbered_in_card_number_order_not_name_order() {
        let drm = tempfile::tempdir().unwrap();
        card(drm.path(), "card2", VENDOR_AMD, Some(LOAD));
        card(drm.path(), "card10", VENDOR_AMD, Some(IDLE));

        let (mut gpus, _) = discover(drm.path());

        let Gpu {
            gpu_metrics,
            buffer,
            ..
        } = &mut gpus[0];
        let first = read_metrics(gpu_metrics, buffer).unwrap();
        assert_eq!(
            first.gfxclk_mhz,
            GpuMetrics::parse(LOAD).unwrap().gfxclk_mhz
        );
    }

    #[test]
    fn a_host_with_no_drm_class_finds_nothing() {
        let (gpus, refused) = discover(Path::new("/nonexistent/drm"));
        assert!(gpus.is_empty());
        assert!(refused.is_empty());
    }

    #[test]
    fn a_sweep_with_no_good_read_does_not_stamp_the_window() {
        let drm = tempfile::tempdir().unwrap();
        card(drm.path(), "card0", VENDOR_AMD, Some(LOAD));
        let (mut gpus, _) = discover(drm.path());
        let metrics = drm.path().join("card0/device/gpu_metrics");
        let group = AcquisitionGroup::new("test_sampler", "gpu_amd_sysfs_failed_read");

        // Twice: the first failure warns and later ones do not, and neither
        // may stamp.
        std::fs::write(&metrics, &LOAD[..10]).unwrap();
        sweep(&mut gpus, &group);
        sweep(&mut gpus, &group);
        assert!(group.window().is_none());

        std::fs::write(&metrics, LOAD).unwrap();
        sweep(&mut gpus, &group);
        assert!(group.window().is_some());
    }

    #[test]
    fn a_sweep_with_one_good_read_stamps_the_window() {
        let drm = tempfile::tempdir().unwrap();
        card(drm.path(), "card0", VENDOR_AMD, Some(LOAD));
        card(drm.path(), "card1", VENDOR_AMD, Some(LOAD));
        let (mut gpus, _) = discover(drm.path());
        let metrics = drm.path().join("card0/device/gpu_metrics");
        let group = AcquisitionGroup::new("test_sampler", "gpu_amd_sysfs_partial_read");

        std::fs::write(metrics, &LOAD[..10]).unwrap();
        sweep(&mut gpus, &group);
        assert!(group.window().is_some());
    }

    #[test]
    fn only_a_refusal_by_the_driver_is_transient() {
        let io = |errno| ReadError::Io(std::io::Error::from_raw_os_error(errno));
        assert!(io(libc::EBUSY).is_transient());
        assert!(io(libc::EPERM).is_transient());
        assert!(!io(libc::ENODEV).is_transient());
        assert!(!ReadError::Format(FormatError::Short(3)).is_transient());
    }

    #[test]
    fn a_resetting_gpu_is_kept_and_a_runtime_suspended_one_is_not() {
        let device = tempfile::tempdir().unwrap();
        let refused = ReadError::Io(std::io::Error::from_raw_os_error(libc::EPERM));
        let status = device.path().join("power/runtime_status");
        std::fs::create_dir_all(status.parent().unwrap()).unwrap();

        std::fs::write(&status, "active\n").unwrap();
        assert!(keeps_after_failed_read(&refused, device.path()));

        std::fs::write(&status, "suspended\n").unwrap();
        assert!(!keeps_after_failed_read(&refused, device.path()));
    }

    #[test]
    fn a_wrong_format_is_not_kept() {
        let device = tempfile::tempdir().unwrap();
        let wrong = ReadError::Format(FormatError::Unsupported(120, 1, 1));
        assert!(!keeps_after_failed_read(&wrong, device.path()));
    }

    #[test]
    fn a_read_failure_a_retry_does_not_fix_sets_warned() {
        let drm = tempfile::tempdir().unwrap();
        card(drm.path(), "card0", VENDOR_AMD, Some(LOAD));
        let (mut gpus, _) = discover(drm.path());
        assert!(!gpus[0].warned);

        let metrics = drm.path().join("card0/device/gpu_metrics");
        std::fs::write(metrics, &LOAD[..10]).unwrap();
        assert!(!gpus[0].refresh());
        assert!(gpus[0].warned);
    }

    /// Uses the last id, which no other test writes: the gauges are process
    /// globals and tests run in parallel.
    #[test]
    fn a_refresh_publishes_every_value_in_its_metric_unit() {
        const ID: usize = MAX_GPUS - 1;
        let drm = tempfile::tempdir().unwrap();
        let mut metrics = LOAD.to_vec();
        metrics[108..112].copy_from_slice(&0x800u32.to_le_bytes());
        card(drm.path(), "card0", VENDOR_AMD, Some(&metrics));
        let fclk = "0: 500Mhz \n1: 1400Mhz *\n2: 1960Mhz \n";
        std::fs::write(drm.path().join("card0/device/pp_dpm_fclk"), fclk).unwrap();
        let (mut gpus, _) = discover(drm.path());
        gpus[0].id = ID;

        assert!(gpus[0].refresh());

        let published = [
            GPU_CLOCK_GRAPHICS.value(ID),
            GPU_CLOCK_COMPUTE.value(ID),
            GPU_CLOCK_MEMORY.value(ID),
            GPU_CLOCK_SOC.value(ID),
            GPU_CLOCK_FABRIC.value(ID),
            GPU_POWER_USAGE.value(ID),
            GPU_RAIL_POWER_VDD.value(ID),
            GPU_RAIL_POWER_SOC.value(ID),
            GPU_TEMPERATURE_GRAPHICS.value(ID),
            GPU_TEMPERATURE_SOC.value(ID),
            GPU_UTILIZATION.value(ID),
            GPU_THROTTLE_STATUS.value(ID),
        ];
        assert_eq!(
            published.map(Option::unwrap),
            [
                2_700_000_000,
                2_700_000_000,
                2_800_000_000,
                1_028_000_000,
                1_400_000_000,
                33426,
                21038,
                4737,
                51,
                41,
                90,
                0x800,
            ]
        );
    }

    #[test]
    fn hundredths_round_to_the_nearest_whole_unit() {
        assert_eq!(whole_units(5050), 51);
        assert_eq!(whole_units(5049), 50);
        assert_eq!(whole_units(50), 1);
        assert_eq!(whole_units(49), 0);
    }
}
