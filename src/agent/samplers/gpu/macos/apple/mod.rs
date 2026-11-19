//! The Apple silicon GPU, from IOReport, the private framework `powermetrics`
//! reads. It needs no privileges.
//!
//! IOReport's `GPU Stats` / `GPU Performance States` channel `GPUPH` counts
//! the 24 MHz system-counter ticks the GPU has spent in each performance
//! state since boot: `OFF`, then `P1` and up. The device tree's power manager
//! lists the states' frequencies in `voltage-states9`, with state `P<k>` at
//! entry `k` and `OFF` at entry 0. `Energy Model` / `GPU Energy` is the GPU's
//! energy since boot in nJ, the channel `cpu_power` publishes as
//! `cpu_igpu_energy`.
//!
//! A refresh compares its sample with the last one published from:
//!
//! - `gpu_utilization`: ticks in a performance state over ticks in any state;
//! - `gpu_clock`: ticks in each performance state times its frequency, over
//!   ticks in a performance state, or 0 when there were none;
//! - `gpu_power_usage`: energy over the system counter's elapsed time.
//!
//! `gpu_energy_consumption` is the running total. The GPU's residencies are
//! brought up to date when it changes state, so the ticks counted between two
//! samples can differ from the time between them; the first two gauges are
//! ratios of ticks and are not affected. `GPU Energy` advances about every
//! 15 ms on an M4 Max.
//!
//! The gauges are averages over an interval, which principle 10
//! (`docs/principles.md`) would have the consumer choose. A refresh less than
//! `MIN_INTERVAL` (200 ms) after the last published sample, or one that
//! counted no residency, publishes nothing and keeps that sample as the base,
//! so a consumer refreshing every few milliseconds cannot shorten another
//! consumer's interval below 200 ms, where the energy's 15 ms steps would be
//! 7.5% of it. The floor is below the common recording intervals of 250 ms
//! and up, since a consumer refreshing at the floor itself would publish only
//! when its timer ran late. A machine whose state count
//! does not match the table's length is not supported, and only the first
//! `GPUPH` channel is read.
//!
//! The subscription holds the two channels read. A sample of them took
//! about 0.1 ms in a release build on an M4 Max; the whole `Energy Model`
//! group's 328 channels take about 3.6 ms.

const NAME: &str = "gpu_apple";

use crate::agent::sampler_status::Unsupported;
use crate::agent::samplers::cpu::macos::topology;
use crate::agent::samplers::ioreport::{self, Channel, Subscription, Value};
use crate::agent::*;

use std::sync::Mutex;

mod stats;

use stats::*;

const STATE_GROUP: &str = "GPU Stats";
const STATE_SUBGROUP: &str = "GPU Performance States";
const STATE_CHANNEL: &str = "GPUPH";
const ENERGY_GROUP: &str = "Energy Model";
const ENERGY_CHANNEL: &str = "GPU Energy";
const TABLE: &str = "voltage-states9";

/// The system counter's rate, which residencies are counted in.
const TICK_HZ: u128 = 24_000_000;

/// The shortest interval the gauges are published over, in system-counter
/// ticks: 200 ms.
const MIN_INTERVAL: u64 = 4_800_000;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }
    Ok(Some(Box::new(GpuApple::new()?)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

/// The GPU's running totals at one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    /// The system counter, in 24 MHz ticks.
    pub tsc: u64,
    /// Ticks in a performance state.
    pub active: u64,
    /// Ticks in `OFF`.
    pub off: u64,
    /// Ticks in each performance state times its frequency in Hz.
    pub cycles: u128,
    /// Energy in nJ, when the machine reports it.
    pub energy: Option<u64>,
}

/// The gauges over the interval between two samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    /// Percent, rounded.
    pub utilization: i64,
    /// Hz.
    pub clock: i64,
    /// mW; `None` without energy or elapsed time.
    pub power: Option<i64>,
}

/// The GPU's totals from its residencies and energy in `channels`, read at
/// system counter `tsc`, or `None` when its states do not match `table`:
/// a state other than `OFF` and `P<k>`, a `P<k>` beyond the table, or a count
/// other than the table's length.
pub fn totals(channels: &[Channel], tsc: u64, table: &[u64]) -> Option<Totals> {
    let states = channels.iter().find_map(|c| match &c.value {
        Value::State(s)
            if c.group == STATE_GROUP
                && c.subgroup == STATE_SUBGROUP
                && c.name == STATE_CHANNEL =>
        {
            Some(s)
        }
        _ => None,
    })?;
    if states.len() != table.len() {
        return None;
    }
    let mut totals = Totals {
        tsc,
        active: 0,
        off: 0,
        cycles: 0,
        energy: None,
    };
    for (name, ticks) in states {
        let ticks = u64::try_from(*ticks).ok()?;
        if name == "OFF" {
            totals.off = totals.off.saturating_add(ticks);
            continue;
        }
        let k: usize = name.strip_prefix('P')?.parse().ok()?;
        let hz = *table.get(k)?;
        totals.active = totals.active.saturating_add(ticks);
        totals.cycles += u128::from(ticks) * u128::from(hz);
    }
    totals.energy = channels.iter().find_map(|c| match c.value {
        Value::Simple(v) if c.group == ENERGY_GROUP && c.name == ENERGY_CHANNEL => {
            nanojoules(v, &c.unit)
        }
        _ => None,
    });
    Some(totals)
}

/// `value` in `unit` as nanojoules, or `None` for a unit that is not energy
/// or a negative value.
fn nanojoules(value: i64, unit: &str) -> Option<u64> {
    let v = u64::try_from(value).ok()?;
    match unit {
        "mJ" => v.checked_mul(1_000_000),
        "uJ" => v.checked_mul(1000),
        "nJ" => Some(v),
        _ => None,
    }
}

/// The gauges from `prev` to `next`, or `None` when less than
/// `MIN_INTERVAL` passed or no residency was counted. A total that went
/// backwards counts as no change.
pub fn interval(prev: &Totals, next: &Totals) -> Option<Interval> {
    let elapsed = next.tsc.saturating_sub(prev.tsc);
    let active = next.active.saturating_sub(prev.active);
    let off = next.off.saturating_sub(prev.off);
    let cycles = next.cycles.saturating_sub(prev.cycles);
    let counted = u128::from(active) + u128::from(off);
    if elapsed < MIN_INTERVAL || counted == 0 {
        return None;
    }
    let utilization = (u128::from(active) * 200 + counted) / (counted * 2);
    let clock = cycles.checked_div(u128::from(active)).unwrap_or(0);
    let power = match (prev.energy, next.energy) {
        (Some(a), Some(b)) => {
            // nJ over ticks of 1/24 MHz s is nJ × 24e6 / ticks nW; mW is
            // that over 1e6.
            let mw = u128::from(b.saturating_sub(a)) * TICK_HZ / 1_000_000 / u128::from(elapsed);
            Some(i64::try_from(mw).unwrap_or(i64::MAX))
        }
        _ => None,
    };
    Some(Interval {
        utilization: utilization as i64,
        clock: i64::try_from(clock).unwrap_or(i64::MAX),
        power,
    })
}

/// The system counter now, in 24 MHz ticks.
fn system_ticks() -> u64 {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: `ts` is a valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &mut ts) };
    let ns = ts.tv_sec as u128 * 1_000_000_000 + ts.tv_nsec as u128;
    // A tick is 125/3 ns.
    u64::try_from(ns * 3 / 125).unwrap_or(u64::MAX)
}

struct GpuApple {
    state: Mutex<State>,
    table: Vec<u64>,
}

struct State {
    subscription: Subscription,
    /// The previous refresh's totals, which the next refresh's gauges are
    /// measured from.
    prev: Totals,
}

impl GpuApple {
    /// Read the table, subscribe, and take the first sample, which the
    /// first refresh is measured from.
    fn new() -> Result<Self, anyhow::Error> {
        let table = topology::frequency_table(TABLE)
            .ok_or_else(|| Unsupported(format!("{NAME}: no {TABLE} in the device tree")))?;
        let api = ioreport::api().map_err(|e| Unsupported(format!("{NAME}: {e}")))?;
        let subscription = Subscription::filtered(
            api,
            &[(STATE_GROUP, Some(STATE_SUBGROUP)), (ENERGY_GROUP, None)],
            |name| name == STATE_CHANNEL || name == ENERGY_CHANNEL,
        )
        .ok_or_else(|| Unsupported(format!("{NAME}: IOReport has no {STATE_SUBGROUP} channels")))?;
        let channels = subscription
            .sample()
            .ok_or_else(|| anyhow::anyhow!("{NAME}: IOReport sample failed"))?;
        let prev = totals(&channels, system_ticks(), &table).ok_or_else(|| {
            Unsupported(format!(
                "{NAME}: the GPU's performance states do not match {TABLE}"
            ))
        })?;
        GPU_APPLE_ACQ.set_member_set(&[0]);
        Ok(GpuApple {
            state: Mutex::new(State { subscription, prev }),
            table,
        })
    }
}

#[async_trait]
impl Sampler for GpuApple {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut guard = GPU_APPLE_ACQ.acquire();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(channels) = state.subscription.sample() else {
            return;
        };
        let tsc = system_ticks();
        guard.mark_end();
        let Some(next) = totals(&channels, tsc, &self.table) else {
            return;
        };
        // Dropping the guard without finishing it publishes no window.
        let Some(interval) = interval(&state.prev, &next) else {
            return;
        };
        state.prev = next;
        drop(state);

        let _ = GPU_UTILIZATION.set(0, interval.utilization);
        let _ = GPU_CLOCK_GRAPHICS.set(0, interval.clock);
        if let Some(v) = interval.power {
            let _ = GPU_POWER_USAGE.set(0, v);
        }
        if let Some(nj) = next.energy {
            let _ = GPU_ENERGY_CONSUMPTION.set(0, nj / 1_000_000);
        }
        guard.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: [u64; 4] = [0, 338_000_000, 618_000_000, 1_578_000_000];

    fn channels(states: &[(&str, i64)], energy: Option<(i64, &str)>) -> Vec<Channel> {
        let mut out = vec![Channel {
            group: STATE_GROUP.into(),
            subgroup: STATE_SUBGROUP.into(),
            name: STATE_CHANNEL.into(),
            unit: String::new(),
            value: Value::State(states.iter().map(|(n, t)| (n.to_string(), *t)).collect()),
        }];
        if let Some((v, unit)) = energy {
            out.push(Channel {
                group: ENERGY_GROUP.into(),
                subgroup: String::new(),
                name: ENERGY_CHANNEL.into(),
                unit: unit.into(),
                value: Value::Simple(v),
            });
        }
        out
    }

    #[test]
    fn residencies_and_energy_become_totals() {
        let c = channels(
            &[("OFF", 30), ("P1", 10), ("P2", 0), ("P3", 20)],
            Some((5, "mJ")),
        );
        assert_eq!(
            totals(&c, 99, &TABLE),
            Some(Totals {
                tsc: 99,
                active: 30,
                off: 30,
                cycles: 10 * 338_000_000 + 20 * 1_578_000_000,
                energy: Some(5_000_000),
            })
        );
    }

    #[test]
    fn states_that_do_not_match_the_table_are_refused() {
        // One state short.
        let c = channels(&[("OFF", 1), ("P1", 1), ("P2", 1)], None);
        assert_eq!(totals(&c, 0, &TABLE), None);
        // A state past the table.
        let c = channels(&[("OFF", 1), ("P1", 1), ("P2", 1), ("P4", 1)], None);
        assert_eq!(totals(&c, 0, &TABLE), None);
        // A state that is not OFF or P<k>.
        let c = channels(&[("OFF", 1), ("P1", 1), ("P2", 1), ("ON", 1)], None);
        assert_eq!(totals(&c, 0, &TABLE), None);
        // No state channel.
        assert_eq!(totals(&[], 0, &TABLE), None);
    }

    #[test]
    fn the_interval_is_utilization_clock_and_power() {
        let prev = Totals {
            tsc: 0,
            active: 0,
            off: 0,
            cycles: 0,
            energy: Some(0),
        };
        // A quarter of a second at 338 MHz and three quarters off, over
        // 24e6 ticks (one second), using 100 mJ.
        let next = Totals {
            tsc: 24_000_000,
            active: 6_000_000,
            off: 18_000_000,
            cycles: 6_000_000 * 338_000_000,
            energy: Some(100_000_000),
        };
        assert_eq!(
            interval(&prev, &next),
            Some(Interval {
                utilization: 25,
                clock: 338_000_000,
                power: Some(100),
            })
        );
    }

    fn base() -> Totals {
        Totals {
            tsc: 0,
            active: 5,
            off: 5,
            cycles: 5 * 338_000_000,
            energy: None,
        }
    }

    #[test]
    fn an_idle_interval_reads_zero() {
        let prev = base();
        let off = Totals {
            off: 105,
            tsc: MIN_INTERVAL,
            ..prev
        };
        assert_eq!(
            interval(&prev, &off),
            Some(Interval {
                utilization: 0,
                clock: 0,
                power: None,
            })
        );
    }

    #[test]
    fn a_short_or_empty_interval_is_not_published() {
        let prev = base();
        let short = Totals {
            off: 105,
            tsc: MIN_INTERVAL - 1,
            ..prev
        };
        assert_eq!(interval(&prev, &short), None);
        let unchanged = Totals {
            tsc: MIN_INTERVAL,
            ..prev
        };
        assert_eq!(interval(&prev, &unchanged), None);
    }

    /// A consumer at 250 ms whose timer fires a little early still
    /// publishes.
    #[test]
    fn a_250_ms_consumer_running_early_is_published() {
        let prev = base();
        let early = Totals {
            off: 105,
            tsc: 5_880_000, // 245 ms
            ..prev
        };
        assert!(interval(&prev, &early).is_some());
    }

    #[test]
    fn utilization_rounds_to_the_nearest_percent() {
        let prev = Totals {
            tsc: 0,
            active: 0,
            off: 0,
            cycles: 0,
            energy: None,
        };
        let at = |active, off| {
            interval(
                &prev,
                &Totals {
                    tsc: MIN_INTERVAL,
                    active,
                    off,
                    ..prev
                },
            )
            .map(|i| i.utilization)
        };
        assert_eq!(at(1, 199), Some(1)); // 0.5% rounds up
        assert_eq!(at(1, 200), Some(0));
        assert_eq!(at(199, 1), Some(100)); // 99.5%
        assert_eq!(at(1, 0), Some(100));
    }

    /// This machine: the GPU's states match its table, the subscription
    /// holds only the two channels read, and a refresh after
    /// `MIN_INTERVAL` sets every metric and stamps the group.
    #[tokio::test]
    async fn new_and_refresh_set_every_metric_and_stamp_the_group() {
        crate::agent::samplers::ioreport::skip_in_vm!();
        let sampler = GpuApple::new().expect("supported on Apple silicon");
        let names: Vec<String> = {
            let state = sampler.state.lock().unwrap();
            let channels = state.subscription.sample().expect("sample");
            channels.into_iter().map(|c| c.name).collect()
        };
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().any(|n| n == STATE_CHANNEL), "{names:?}");
        assert!(names.iter().any(|n| n == ENERGY_CHANNEL), "{names:?}");
        std::thread::sleep(std::time::Duration::from_millis(300));
        sampler.refresh().await;
        assert_eq!(GPU_APPLE_ACQ.member_set(), Some(&[0usize][..]));
        assert!(GPU_APPLE_ACQ.window().is_some());
        let utilization = GPU_UTILIZATION.value(0).expect("utilization");
        assert!((0..=100).contains(&utilization), "{utilization}");
        let clock = GPU_CLOCK_GRAPHICS.value(0).expect("clock");
        let max = *sampler.table.iter().max().unwrap() as i64;
        assert!((0..=max).contains(&clock), "{clock} within 0..={max}");
        assert!(GPU_POWER_USAGE.value(0).is_some());
        assert!(GPU_ENERGY_CONSUMPTION.value(0).unwrap() > 0);
    }
}
