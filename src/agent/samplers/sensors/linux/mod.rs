//! Read-only Linux hardware sensors from thermal, hwmon and cooling sysfs.
//!
//! These gauges have no BPF or perf-event source, so this is a deliberate
//! principle-15 sysfs exception. Discovery is cached for 60 seconds and value
//! reads run family-major on a configurable cadence (5 seconds by default), in
//! one nonoverlapping `spawn_blocking` worker as required by principle 17.
//! `refresh()` only checks the throttle and dispatches that worker.
//!
//! Each metric family owns one acquisition group and the blocking worker is
//! its sole window writer. Successful family reads stamp after values; a
//! wholly failed family clears its live gauges but preserves the last window.
//! A removal-only pass stamps because publishing absence is a successful
//! membership event. Lifetime slots and their metadata are never reused or
//! relabeled, including after removal and rediscovery.

mod discovery;
mod stats;

use crate::agent::*;
use discovery::{Descriptor, Family};
use metriken::GaugeGroup;
use stats::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const NAME: &str = "sensors";
const SYSFS: &str = "/sys";
const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(5);
const REDISCOVERY_INTERVAL: Duration = Duration::from_secs(60);
const STUCK_INTERVALS: u32 = 3;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }
    let interval = config
        .sampler_interval(NAME)
        .unwrap_or(DEFAULT_READ_INTERVAL);
    Ok(Some(Box::new(Sensors::new(interval, PathBuf::from(SYSFS)))))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

#[derive(Debug, Default, Eq, PartialEq)]
struct Reconcile {
    added: Vec<usize>,
    removed: Vec<usize>,
    overflow: usize,
}

struct Slots {
    capacity: usize,
    by_sensor: HashMap<String, usize>,
    descriptors: Vec<Option<Descriptor>>,
    active: Vec<bool>,
    pending_removed: Vec<bool>,
    metadata_applied: Vec<bool>,
    next: usize,
}

impl Slots {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            by_sensor: HashMap::new(),
            descriptors: vec![None; capacity],
            active: vec![false; capacity],
            pending_removed: vec![false; capacity],
            metadata_applied: vec![false; capacity],
            next: 0,
        }
    }

    fn reconcile(&mut self, descriptors: Vec<Descriptor>) -> Reconcile {
        let previously_active = self.active.clone();
        self.active.fill(false);
        let mut result = Reconcile::default();

        for descriptor in descriptors {
            if let Some(slot) = self.by_sensor.get(&descriptor.sensor).copied() {
                // The identity is unchanged; update only the paths used by the
                // next read (hwmon class numbers can move on rediscovery).
                self.descriptors[slot] = Some(descriptor);
                self.active[slot] = true;
                self.pending_removed[slot] = false;
                continue;
            }
            if self.next == self.capacity {
                result.overflow += 1;
                continue;
            }
            let slot = self.next;
            self.next += 1;
            self.by_sensor.insert(descriptor.sensor.clone(), slot);
            self.descriptors[slot] = Some(descriptor);
            self.active[slot] = true;
            result.added.push(slot);
        }

        for (slot, was_active) in previously_active.into_iter().enumerate().take(self.next) {
            if was_active && !self.active[slot] {
                self.pending_removed[slot] = true;
                result.removed.push(slot);
            }
        }
        result
    }

    #[cfg(test)]
    fn slot_of(&self, sensor: &str) -> Option<usize> {
        self.by_sensor.get(sensor).copied()
    }

    fn bound(&self) -> usize {
        self.next
    }

    fn active(&self) -> usize {
        self.active.iter().take(self.next).filter(|v| **v).count()
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct Publish {
    succeeded: usize,
    failed: usize,
    removed: usize,
}

fn publish_family(family: Family, slots: &mut Slots) -> Publish {
    let metric = metric(family);
    let acquisition = acquisition(family);
    let guard = acquisition.acquire();
    let mut result = Publish::default();

    for slot in 0..slots.bound() {
        if slots.pending_removed[slot] {
            let _ = metric.set(slot, i64::MIN);
            slots.pending_removed[slot] = false;
            result.removed += 1;
        }
        if !slots.active[slot] {
            continue;
        }
        let descriptor = slots.descriptors[slot]
            .as_ref()
            .expect("an active slot has a descriptor");
        debug_assert_eq!(descriptor.family, family);
        if !slots.metadata_applied[slot] {
            apply_metadata(metric, slot, descriptor);
            slots.metadata_applied[slot] = true;
        }
        match descriptor.read() {
            Ok(value) => {
                let _ = metric.set(slot, value);
                result.succeeded += 1;
            }
            Err(_) => {
                // Missing, malformed, disabled and faulted channels are
                // absent, never manufactured zero or retained stale values.
                let _ = metric.set(slot, i64::MIN);
                result.failed += 1;
            }
        }
    }

    acquisition.set_member_bound(slots.bound());
    if result.succeeded > 0 || result.removed > 0 {
        guard.finish();
    } else {
        guard.discard();
    }
    result
}

fn apply_metadata(metric: &GaugeGroup, slot: usize, descriptor: &Descriptor) {
    metric.insert_metadata(slot, "sensor".to_string(), descriptor.sensor.clone());
    metric.insert_metadata(slot, "source".to_string(), descriptor.source.clone());
    metric.insert_metadata(slot, "chip".to_string(), descriptor.chip.clone());
    metric.insert_metadata(slot, "channel".to_string(), descriptor.channel.clone());
    if let Some(label) = &descriptor.label {
        metric.insert_metadata(slot, "label".to_string(), label.clone());
    }
    if let Some(scope) = &descriptor.scope {
        metric.insert_metadata(slot, "scope".to_string(), scope.clone());
    }
    if let Some(model) = &descriptor.board_model {
        metric.insert_metadata(slot, "board_model".to_string(), model.clone());
    }
    if let Some(compatible) = &descriptor.soc_compatible {
        metric.insert_metadata(slot, "soc_compatible".to_string(), compatible.clone());
    }
    if descriptor.derived {
        metric.insert_metadata(slot, "derived".to_string(), "voltage_x_current".to_string());
    }
}

fn acquisition(family: Family) -> &'static crate::agent::timing::AcquisitionGroup {
    match family {
        Family::Temperature => &SENSOR_TEMPERATURE_ACQ,
        Family::Power => &SENSOR_POWER_ACQ,
        Family::Voltage => &SENSOR_VOLTAGE_ACQ,
        Family::Current => &SENSOR_CURRENT_ACQ,
        Family::FanSpeed => &SENSOR_FAN_SPEED_ACQ,
        Family::FanPwm => &SENSOR_FAN_PWM_ACQ,
        Family::CoolingState => &SENSOR_COOLING_STATE_ACQ,
    }
}

fn metric(family: Family) -> &'static GaugeGroup {
    match family {
        Family::Temperature => &SENSOR_TEMPERATURE,
        Family::Power => &SENSOR_POWER,
        Family::Voltage => &SENSOR_VOLTAGE,
        Family::Current => &SENSOR_CURRENT,
        Family::FanSpeed => &SENSOR_FAN_SPEED,
        Family::FanPwm => &SENSOR_FAN_PWM,
        Family::CoolingState => &SENSOR_COOLING_STATE,
    }
}

struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    fn new() -> Self {
        Self { last: None }
    }

    fn due(&mut self, now: Instant, interval: Duration) -> bool {
        if self.last.is_some_and(|last| {
            now.checked_duration_since(last)
                .is_some_and(|d| d < interval)
        }) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiscoveryHealth {
    descriptors: usize,
    errors: Vec<String>,
    overflow: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReadHealth(Vec<(Family, usize, usize)>);

struct SweepState {
    slots: [Slots; 7],
    rediscovery: Throttle,
    discovery_health: Option<DiscoveryHealth>,
    read_health: Option<ReadHealth>,
}

impl SweepState {
    fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| Slots::new(MAX_SENSORS)),
            rediscovery: Throttle::new(),
            discovery_health: None,
            read_health: None,
        }
    }
}

fn family_index(family: Family) -> usize {
    match family {
        Family::Temperature => 0,
        Family::Power => 1,
        Family::Voltage => 2,
        Family::Current => 3,
        Family::FanSpeed => 4,
        Family::FanPwm => 5,
        Family::CoolingState => 6,
    }
}

fn report_discovery(last: &mut Option<DiscoveryHealth>, current: DiscoveryHealth) {
    if last.as_ref() == Some(&current) {
        return;
    }
    if current.descriptors == 0 {
        warn!("{NAME}: discovery found no readable sensor descriptors");
    } else {
        info!(
            "{NAME}: discovery found {} sensor descriptors",
            current.descriptors
        );
    }
    if !current.errors.is_empty() {
        warn!(
            "{NAME}: discovery had {} metadata/permission error(s): {}",
            current.errors.len(),
            current.errors.join("; ")
        );
    }
    if current.overflow > 0 {
        warn!(
            "{NAME}: {0} sensor identity/identities exceed a {MAX_SENSORS}-identity family lifetime cap",
            current.overflow
        );
    }
    *last = Some(current);
}

fn report_reads(last: &mut Option<ReadHealth>, current: ReadHealth) {
    if last.as_ref() == Some(&current) {
        return;
    }
    for (family, active, failed) in &current.0 {
        if *failed == 0 {
            continue;
        }
        if *failed == *active {
            warn!(
                "{NAME}: every active {} reading failed ({failed}/{active}); the family window was not advanced",
                family.name()
            );
        } else {
            warn!(
                "{NAME}: {failed}/{active} active {} readings failed; failed values are absent",
                family.name()
            );
        }
    }
    if last
        .as_ref()
        .is_some_and(|old| old.0.iter().any(|(_, _, failed)| *failed > 0))
        && current.0.iter().all(|(_, _, failed)| *failed == 0)
    {
        info!("{NAME}: every active sensor reading is available again");
    }
    *last = Some(current);
}

fn sweep(state: &mut SweepState, sysfs: &Path) {
    let now = Instant::now();
    if state.rediscovery.due(now, REDISCOVERY_INTERVAL) {
        let started = Instant::now();
        let found = discovery::discover(sysfs);
        let descriptor_count = found.descriptors.len();
        let mut by_family: [Vec<Descriptor>; 7] = std::array::from_fn(|_| Vec::new());
        for descriptor in found.descriptors {
            by_family[family_index(descriptor.family)].push(descriptor);
        }
        let mut overflow = 0;
        for family in Family::ALL {
            overflow += state.slots[family_index(family)]
                .reconcile(std::mem::take(&mut by_family[family_index(family)]))
                .overflow;
        }
        report_discovery(
            &mut state.discovery_health,
            DiscoveryHealth {
                descriptors: descriptor_count,
                errors: found.errors,
                overflow,
            },
        );
        debug!(
            "{NAME}: discovery found {descriptor_count} descriptors in {} us",
            started.elapsed().as_micros()
        );
    }

    let started = Instant::now();
    let mut health = Vec::with_capacity(Family::ALL.len());
    let mut total_succeeded = 0;
    let mut total_failed = 0;
    for family in Family::ALL {
        let slots = &mut state.slots[family_index(family)];
        let active = slots.active();
        let published = publish_family(family, slots);
        total_succeeded += published.succeeded;
        total_failed += published.failed;
        health.push((family, active, published.failed));
    }
    report_reads(&mut state.read_health, ReadHealth(health));
    debug!(
        "{NAME}: read {total_succeeded} sensor values ({total_failed} failed) in {} us",
        started.elapsed().as_micros()
    );
}

struct Sensors {
    interval: Duration,
    last_read: Mutex<Throttle>,
    reading: Arc<AtomicBool>,
    stuck_warned: AtomicBool,
    state: Arc<Mutex<SweepState>>,
    sysfs: PathBuf,
}

impl Sensors {
    fn new(interval: Duration, sysfs: PathBuf) -> Self {
        // Initialization performs no device reads; the first blocking sweep
        // discovers and reads. Until it lands every family is explicitly empty.
        for family in Family::ALL {
            acquisition(family).set_member_bound(0);
        }
        debug!("{NAME}: reading every {interval:?}, rediscovering every {REDISCOVERY_INTERVAL:?}");
        Self {
            interval,
            last_read: Mutex::new(Throttle::new()),
            reading: Arc::new(AtomicBool::new(false)),
            stuck_warned: AtomicBool::new(false),
            state: Arc::new(Mutex::new(SweepState::new())),
            sysfs,
        }
    }

    fn warn_if_stuck(&self) {
        let Some(dispatched) = self.last_read.lock().unwrap().last else {
            return;
        };
        let running = dispatched.elapsed();
        if running > self.interval * STUCK_INTERVALS
            && !self.stuck_warned.swap(true, Ordering::AcqRel)
        {
            warn!(
                "{NAME}: a blocking sensor sweep has run for {running:?}, over {STUCK_INTERVALS} intervals; no second worker was dispatched"
            );
        }
    }
}

fn lock_state(state: &Mutex<SweepState>) -> std::sync::MutexGuard<'_, SweepState> {
    state.lock().unwrap_or_else(|poisoned| {
        // Slot ownership survives: discarding it would permit lifetime slot
        // reuse after a panic. Sweep mutations are bounded and internally
        // ordered, so retaining the allocation map is safer than relabeling.
        warn!("{NAME}: recovering sensor sweep state after a worker panic");
        state.clear_poison();
        poisoned.into_inner()
    })
}

#[async_trait]
impl Sampler for Sensors {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        if self.reading.load(Ordering::Acquire) {
            self.warn_if_stuck();
            return;
        }
        {
            let mut throttle = self.last_read.lock().unwrap();
            if !throttle.due(Instant::now(), self.interval) {
                return;
            }
        }
        if self.reading.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stuck_warned.store(false, Ordering::Release);
        let reading = self.reading.clone();
        let state = self.state.clone();
        let sysfs = self.sysfs.clone();
        tokio::task::spawn_blocking(move || {
            let _in_flight = InFlight(reading);
            sweep(&mut lock_state(&state), &sysfs);
        });
    }
}

#[cfg(test)]
mod tests;
