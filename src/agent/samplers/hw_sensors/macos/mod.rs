//! Hardware sensors on macOS: temperatures from the IOHIDEventSystem, and
//! fan speeds, PMU rail voltage, current and power, and system power from the
//! System Management Controller (SMC). Neither needs root.
//!
//! Each read is a request to the SMC coprocessor: about 165 µs per SMC key
//! and 1 ms per HID temperature on an M4 Max, nearly all of it waiting. So,
//! as on Linux, this departs from principle 10 under principle 17: values are
//! read on the sampler's own interval (5 seconds by default) in one
//! nonoverlapping `spawn_blocking` worker, and `refresh()` only checks the
//! throttle and dispatches it.
//!
//! The sensors are found once, at startup: the HID temperature, voltage and
//! current services, the SMC fan count, and a probe of each candidate SMC key
//! (see `discovery`). Each family has one acquisition group, stamped after a
//! pass in which at least one of its values was read. A failed read is
//! absent, never zero or the previous value.

mod discovery;
mod hid;
mod smc;

mod stats {
    include!("../linux/stats.rs");
}

use crate::agent::*;
use discovery::{Candidate, Family, Origin};
use metriken::GaugeGroup;
use smc::{FloatKey, Smc};
use stats::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const NAME: &str = "hw_sensors";
const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(5);
const STUCK_INTERVALS: u32 = 3;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }
    let interval = config
        .sampler_interval(NAME)
        .unwrap_or(DEFAULT_READ_INTERVAL);
    let readers = discover();
    if readers.sensors.iter().all(|f| f.is_empty()) {
        return Err(crate::agent::sampler_status::Unsupported(
            "no SMC key or HID temperature service could be read".to_string(),
        )
        .into());
    }
    Ok(Some(Box::new(HwSensors::new(interval, readers))))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

#[derive(Clone, Copy, Debug)]
enum Source {
    Hid(usize),
    Smc(FloatKey),
}

#[derive(Debug)]
struct Sensor {
    sensor: String,
    name: String,
    source: Source,
}

/// The open SMC connection and HID temperature services, and the sensors
/// read through them, by family in `Family::ALL` order. A sensor's index is
/// its slot.
struct Readers {
    smc: Option<Smc>,
    temperatures: Option<hid::Services>,
    sensors: [Vec<Sensor>; 5],
    failed: [usize; 5],
}

fn family_index(family: Family) -> usize {
    Family::ALL.iter().position(|f| *f == family).unwrap()
}

fn discover() -> Readers {
    let smc = Smc::open();
    if smc.is_none() {
        debug!("{NAME}: could not open AppleSMC");
    }
    let temperatures = hid::Services::open(hid::TEMPERATURE);
    let voltage = hid::Services::open(hid::VOLTAGE).map(|(_, list)| list);
    let current = hid::Services::open(hid::CURRENT).map(|(_, list)| list);

    let mut candidates = Vec::new();
    if let Some((_, list)) = &temperatures {
        candidates.extend(discovery::temperatures(list));
    }
    if let Some(smc) = &smc {
        candidates.extend(discovery::fans(smc.read_u8("FNum").unwrap_or(0)));
        candidates.extend(discovery::totals());
        candidates.extend(discovery::rails(
            voltage.as_deref().unwrap_or_default(),
            current.as_deref().unwrap_or_default(),
        ));
    }

    let mut sensors: [Vec<Sensor>; 5] = std::array::from_fn(|_| Vec::new());
    let mut missing = 0;
    for Candidate {
        family,
        sensor,
        name,
        origin,
    } in candidates
    {
        let source = match origin {
            Origin::Hid(index) => Source::Hid(index),
            Origin::Smc => match smc.as_ref().and_then(|smc| smc.float_key(&sensor)) {
                Some(key) => Source::Smc(key),
                None => {
                    missing += 1;
                    continue;
                }
            },
        };
        let slots = &mut sensors[family_index(family)];
        if slots.len() == MAX_SENSORS {
            continue;
        }
        slots.push(Sensor {
            sensor,
            name,
            source,
        });
    }
    info!(
        "{NAME}: found {} sensors ({} candidate SMC keys absent or not a float)",
        sensors.iter().map(Vec::len).sum::<usize>(),
        missing
    );
    Readers {
        smc,
        temperatures: temperatures.map(|(services, _)| services),
        sensors,
        failed: [0; 5],
    }
}

fn metric(family: Family) -> &'static GaugeGroup {
    match family {
        Family::Temperature => &SENSOR_TEMPERATURE,
        Family::Power => &SENSOR_POWER,
        Family::Voltage => &SENSOR_VOLTAGE,
        Family::Current => &SENSOR_CURRENT,
        Family::FanSpeed => &SENSOR_FAN_SPEED,
    }
}

fn acquisition(family: Family) -> &'static crate::agent::timing::AcquisitionGroup {
    match family {
        Family::Temperature => &SENSOR_TEMPERATURE_ACQ,
        Family::Power => &SENSOR_POWER_ACQ,
        Family::Voltage => &SENSOR_VOLTAGE_ACQ,
        Family::Current => &SENSOR_CURRENT_ACQ,
        Family::FanSpeed => &SENSOR_FAN_SPEED_ACQ,
    }
}

/// A reading in the metric's unit: degrees Celsius, watts, volts and amps
/// become milli-, micro-, milli- and milli-units, and RPM stays RPM.
/// Non-finite readings are absent.
fn scaled(family: Family, value: f64) -> Option<i64> {
    let scale = match family {
        Family::Temperature | Family::Voltage | Family::Current => 1e3,
        Family::Power => 1e6,
        Family::FanSpeed => 1.0,
    };
    let v = (value * scale).round();
    (v.is_finite() && v > i64::MIN as f64 && v < i64::MAX as f64).then_some(v as i64)
}

impl Readers {
    fn read(&self, source: Source) -> Option<f64> {
        match source {
            Source::Hid(index) => self.temperatures.as_ref()?.temperature(index),
            Source::Smc(key) => self.smc.as_ref()?.read_float(key).map(f64::from),
        }
    }

    fn sweep(&mut self) {
        let started = Instant::now();
        for family in Family::ALL {
            let i = family_index(family);
            let metric = metric(family);
            let acquisition = acquisition(family);
            let guard = acquisition.acquire();
            let mut succeeded = 0;
            let mut failed = 0;
            for (slot, sensor) in self.sensors[i].iter().enumerate() {
                match self.read(sensor.source).and_then(|v| scaled(family, v)) {
                    Some(v) => {
                        let _ = metric.set(slot, v);
                        succeeded += 1;
                    }
                    None => {
                        let _ = metric.set(slot, i64::MIN);
                        failed += 1;
                    }
                }
            }
            if succeeded > 0 {
                guard.finish();
            } else {
                guard.discard();
            }
            self.report(family, failed);
        }
        debug!(
            "{NAME}: read {} sensors in {} us",
            self.sensors.iter().map(Vec::len).sum::<usize>(),
            started.elapsed().as_micros()
        );
    }

    /// Log a family's failed reads when the count changes.
    fn report(&mut self, family: Family, failed: usize) {
        let i = family_index(family);
        let last = std::mem::replace(&mut self.failed[i], failed);
        if failed == last {
            return;
        }
        let total = self.sensors[i].len();
        if failed == 0 {
            info!("{NAME}: every {} reading is available again", family.name());
        } else if failed == total {
            warn!(
                "{NAME}: every {} reading failed ({failed}/{total}); the family window was not advanced",
                family.name()
            );
        } else {
            warn!(
                "{NAME}: {failed}/{total} {} readings failed; failed values are absent",
                family.name()
            );
        }
    }
}

struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct HwSensors {
    interval: Duration,
    last_read: Mutex<Option<Instant>>,
    reading: Arc<AtomicBool>,
    stuck_warned: AtomicBool,
    readers: Arc<Mutex<Readers>>,
}

impl HwSensors {
    fn new(interval: Duration, readers: Readers) -> Self {
        for family in Family::ALL {
            let sensors = &readers.sensors[family_index(family)];
            let metric = metric(family);
            for (slot, s) in sensors.iter().enumerate() {
                metric.insert_metadata(slot, "sensor".to_string(), s.sensor.clone());
                metric.insert_metadata(slot, "name".to_string(), s.name.clone());
                let source = match s.source {
                    Source::Hid(_) => "hid",
                    Source::Smc(_) => "smc",
                };
                metric.insert_metadata(slot, "source".to_string(), source.to_string());
            }
            acquisition(family).set_member_bound(sensors.len());
        }
        // Linux-only families: explicitly empty.
        SENSOR_FAN_PWM_ACQ.set_member_bound(0);
        SENSOR_COOLING_STATE_ACQ.set_member_bound(0);
        debug!("{NAME}: reading every {interval:?}");
        Self {
            interval,
            last_read: Mutex::new(None),
            reading: Arc::new(AtomicBool::new(false)),
            stuck_warned: AtomicBool::new(false),
            readers: Arc::new(Mutex::new(readers)),
        }
    }

    /// True, and the read time recorded, if `interval` has passed since the
    /// last read was dispatched.
    fn due(&self, now: Instant) -> bool {
        let mut last = self.last_read.lock().unwrap();
        if last.is_some_and(|l| {
            now.checked_duration_since(l)
                .is_some_and(|d| d < self.interval)
        }) {
            return false;
        }
        *last = Some(now);
        true
    }

    fn warn_if_stuck(&self) {
        let Some(dispatched) = *self.last_read.lock().unwrap() else {
            return;
        };
        let running = dispatched.elapsed();
        if running > self.interval * STUCK_INTERVALS
            && !self.stuck_warned.swap(true, Ordering::AcqRel)
        {
            warn!(
                "{NAME}: a sensor read has run for {running:?}, over {STUCK_INTERVALS} intervals; no second read was dispatched"
            );
        }
    }
}

#[async_trait]
impl Sampler for HwSensors {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        if self.reading.load(Ordering::Acquire) {
            self.warn_if_stuck();
            return;
        }
        if !self.due(Instant::now()) {
            return;
        }
        if self.reading.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stuck_warned.store(false, Ordering::Release);
        let reading = self.reading.clone();
        let readers = self.readers.clone();
        tokio::task::spawn_blocking(move || {
            let _in_flight = InFlight(reading);
            readers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .sweep();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_are_scaled_to_the_metric_units() {
        assert_eq!(scaled(Family::Temperature, 57.5689), Some(57569));
        assert_eq!(scaled(Family::Power, 1.06032), Some(1_060_320));
        assert_eq!(scaled(Family::Voltage, 0.97699), Some(977));
        assert_eq!(scaled(Family::Current, 1.0853), Some(1085));
        assert_eq!(scaled(Family::FanSpeed, 1361.94), Some(1362));
        assert_eq!(scaled(Family::Power, -0.5), Some(-500_000));
        assert_eq!(scaled(Family::Temperature, f64::NAN), None);
        assert_eq!(scaled(Family::Power, f64::INFINITY), None);
    }

    /// Discovers this Mac's sensors and reads them twice, from two threads,
    /// as successive blocking workers would. Prints every reading (run with
    /// `--nocapture` to see them).
    #[test]
    fn discovers_and_reads_this_macs_sensors() {
        let readers = discover();
        let count: usize = readers.sensors.iter().map(Vec::len).sum();
        if count == 0 {
            eprintln!("no SMC key or HID temperature service on this host; skipping");
            return;
        }
        let readers = Arc::new(Mutex::new(readers));
        for round in 0..2 {
            let readers = readers.clone();
            std::thread::spawn(move || {
                let r = readers.lock().unwrap();
                for family in Family::ALL {
                    let sensors = &r.sensors[family_index(family)];
                    let mut read = 0;
                    for s in sensors {
                        let v = r.read(s.source);
                        if round == 0 {
                            println!("{:12} {:5} {:24} {v:?}", family.name(), s.sensor, s.name);
                        }
                        let Some(v) = v else { continue };
                        read += 1;
                        let plausible = match family {
                            Family::Temperature => -40.0..150.0,
                            Family::Power => -500.0..500.0,
                            Family::Voltage => 0.0..60.0,
                            Family::Current => -50.0..50.0,
                            Family::FanSpeed => 0.0..20_000.0,
                        };
                        assert!(
                            plausible.contains(&v),
                            "{} {} = {v}",
                            family.name(),
                            s.sensor
                        );
                    }
                    assert_eq!(
                        read,
                        sensors.len(),
                        "round {round}: every discovered {} sensor reads",
                        family.name()
                    );
                }
            })
            .join()
            .unwrap();
        }
    }
}
