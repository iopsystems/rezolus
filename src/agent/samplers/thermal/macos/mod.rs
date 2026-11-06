//! The thermal pressure level macOS reports to applications, from the
//! `notify(3)` state of `kOSThermalNotificationPressureLevelName`
//! (`com.apple.system.thermalpressurelevel`). Reading it needs no root.
//!
//! Each refresh calls `notify_check`, which `notify(3)` documents as a
//! shared-memory check (16 to 50 ns on an M4 Max, varying with the
//! machine's load). It calls `notify_get_state` (10 to 34 µs) only when
//! `notify_check` reports a post, or when the cache is empty: at startup and
//! after a failed check or read. `notify.h` allows the check false positives,
//! which cost one extra read. On an M4 Max over 1,200 one-second reads,
//! `notify_check` reported the level's rises from 0 to 1 and 1 to 2 and its
//! falls from 2 to 1 and 1 to 0, and the cached level always equalled a
//! fresh `notify_get_state`.
//!
//! The levels are `OSThermalPressureLevel` from `<libkern/OSThermalNotification.h>`:
//! 0 nominal, 1 moderate, 2 heavy, 3 trapping, 4 sleeping. A level is the
//! operating system's classification, not a measurement of throttling:
//! `cpu_frequency` shows whether the CPU ran slower.

const NAME: &str = "thermal_pressure";

use crate::agent::*;

use metriken::*;
use std::ffi::{c_char, c_int, CStr};
use std::sync::Mutex;

use crate::agent::timing::AcquisitionGroup;

pub static THERMAL_PRESSURE_ACQ: AcquisitionGroup = AcquisitionGroup::new(NAME, "thermal_pressure");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static THERMAL_PRESSURE_ACQ_REG: &'static AcquisitionGroup = &THERMAL_PRESSURE_ACQ;

#[metric(
    name = "thermal_pressure",
    description = "The thermal pressure level macOS reports to applications: 0 nominal, 1 moderate, 2 heavy, 3 trapping, 4 sleeping. It is the operating system's classification, not a measure of throttling. macOS only.",
    metadata = { acq_group = "thermal_pressure" }
)]
pub static THERMAL_PRESSURE: LazyGauge = LazyGauge::new(Gauge::default);

const NOTIFY_STATUS_OK: u32 = 0;

extern "C" {
    static kOSThermalNotificationPressureLevelName: *const c_char;
    fn notify_register_check(name: *const c_char, out_token: *mut c_int) -> u32;
    fn notify_check(token: c_int, check: *mut c_int) -> u32;
    fn notify_get_state(token: c_int, state: *mut u64) -> u32;
    fn notify_cancel(token: c_int) -> u32;
}

fn name() -> &'static CStr {
    // SAFETY: libSystem exports the name as a constant NUL-terminated string
    // that lives for the life of the process.
    unsafe { CStr::from_ptr(kOSThermalNotificationPressureLevelName) }
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let mut token: c_int = 0;
    // SAFETY: `name()` is NUL-terminated and `token` is a valid out pointer.
    let status = unsafe { notify_register_check(name().as_ptr(), &mut token) };
    if status != NOTIFY_STATUS_OK {
        return Err(crate::agent::sampler_status::Unsupported(format!(
            "notify_register_check({:?}) returned {status}",
            name()
        ))
        .into());
    }

    let sampler = ThermalPressure {
        token,
        cached: Mutex::new(None),
    };
    if sampler.level().is_none() {
        return Err(crate::agent::sampler_status::Unsupported(format!(
            "notify_check or notify_get_state for {:?} failed",
            name()
        ))
        .into());
    }

    Ok(Some(Box::new(sampler)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct ThermalPressure {
    token: c_int,
    /// The level last read with `notify_get_state`, or `None` when the next
    /// refresh must read it.
    cached: Mutex<Option<i64>>,
}

/// The level, from `cached` unless `check` reports a change or `cached` is
/// empty, in which case `get` reads it. `check` returns `None` when it
/// failed, and `get` when the read failed; either leaves the level absent
/// and `cached` empty, so the next call reads it.
fn level(
    cached: &mut Option<i64>,
    check: impl FnOnce() -> Option<bool>,
    get: impl FnOnce() -> Option<i64>,
) -> Option<i64> {
    let Some(changed) = check() else {
        *cached = None;
        return None;
    };
    if changed || cached.is_none() {
        *cached = get();
    }
    *cached
}

/// Whether a notification was posted for `token` since the last check, or
/// `None` when the check failed. `token` must come from
/// `notify_register_check` and not yet be cancelled.
fn check(token: c_int) -> Option<bool> {
    let mut changed: c_int = 0;
    // SAFETY: the caller's contract on `token`; `changed` is a valid out
    // pointer.
    let status = unsafe { notify_check(token, &mut changed) };
    (status == NOTIFY_STATUS_OK).then_some(changed != 0)
}

/// The state posted for `token`, or `None` when the read failed. `token` as
/// for `check`.
fn get(token: c_int) -> Option<i64> {
    let mut state: u64 = 0;
    // SAFETY: the caller's contract on `token`; `state` is a valid out
    // pointer.
    let status = unsafe { notify_get_state(token, &mut state) };
    (status == NOTIFY_STATUS_OK).then(|| i64::try_from(state).ok())?
}

impl ThermalPressure {
    fn level(&self) -> Option<i64> {
        let mut cached = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        level(&mut cached, || check(self.token), || get(self.token))
    }
}

impl Drop for ThermalPressure {
    fn drop(&mut self) {
        // SAFETY: `token` came from `notify_register_check` and is not used
        // after this.
        unsafe { notify_cancel(self.token) };
    }
}

#[async_trait]
impl Sampler for ThermalPressure {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let guard = THERMAL_PRESSURE_ACQ.acquire();

        let Some(level) = self.level() else {
            return;
        };
        THERMAL_PRESSURE.set(level);

        guard.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_documented_notification_name() {
        assert_eq!(name(), c"com.apple.system.thermalpressurelevel");
    }

    #[test]
    fn stats_attribute_to_this_sampler() {
        let module = module_path!().trim_end_matches("::tests");
        let modules = crate::agent::samplers::sampler_modules();
        assert_eq!(
            crate::agent::samplers::attribute_sampler(module, &modules),
            NAME
        );
        assert_eq!(THERMAL_PRESSURE_ACQ.sampler, NAME);
    }

    #[tokio::test]
    async fn refresh_sets_a_documented_level_and_stamps_the_group() {
        let sampler = init(Arc::new(toml::from_str("").unwrap()))
            .unwrap()
            .unwrap();
        THERMAL_PRESSURE.set(-1);
        sampler.refresh().await;

        assert!(THERMAL_PRESSURE_ACQ.window().is_some());
        assert!((0..=4).contains(&Gauge::value(&THERMAL_PRESSURE)));
    }

    #[test]
    fn the_level_is_read_only_when_it_changed_or_is_not_cached() {
        let mut cached = None;
        assert_eq!(level(&mut cached, || Some(false), || Some(1)), Some(1));
        assert_eq!(
            level(
                &mut cached,
                || Some(false),
                || panic!("read while unchanged")
            ),
            Some(1)
        );
        assert_eq!(level(&mut cached, || Some(true), || Some(2)), Some(2));
        assert_eq!(cached, Some(2));
    }

    /// notify(3): the first check of a token reports a post, and a check
    /// with nothing posted since reports none.
    #[test]
    fn check_reports_a_post_once() {
        let mut token: c_int = 0;
        // SAFETY: `name()` is NUL-terminated and `token` is a valid out
        // pointer.
        let status = unsafe { notify_register_check(name().as_ptr(), &mut token) };
        assert_eq!(status, NOTIFY_STATUS_OK);
        assert_eq!(check(token), Some(true));
        assert_eq!(check(token), Some(false));
        // SAFETY: `token` came from `notify_register_check` and is not used
        // after this.
        unsafe { notify_cancel(token) };
    }

    #[test]
    fn a_failure_leaves_the_level_absent_and_reads_it_next_time() {
        let mut cached = Some(1);
        assert_eq!(level(&mut cached, || Some(true), || None), None);
        assert_eq!(level(&mut cached, || Some(false), || Some(2)), Some(2));

        assert_eq!(level(&mut cached, || None, || Some(3)), None);
        assert_eq!(level(&mut cached, || Some(false), || Some(3)), Some(3));
    }
}
