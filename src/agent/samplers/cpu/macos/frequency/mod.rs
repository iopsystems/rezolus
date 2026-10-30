//! CPU frequency on Apple silicon, from IOReport's per-core
//! performance-state residency (`CPU Stats` / `CPU Core Performance States`),
//! published as the `cpu_tsc`, `cpu_mperf` and `cpu_aperf` counters the Linux
//! sampler reads from the MSRs, so `tsc × aperf / mperf` is the average
//! frequency while running on both.
//!
//! Apple silicon has no APERF/MPERF. IOReport counts, per core, the 24 MHz
//! system-counter ticks spent in each performance state (`V<k>P<n>`), idle
//! (`IDLE`) and powered down (`DOWN`). The device tree's power manager lists
//! each cluster type's state frequencies (`voltage-states1-sram` for E,
//! `voltage-states5-sram` for P), with state `V<k>` at entry `k`. Per core:
//!
//! - `cpu_tsc`: the system counter (`CLOCK_UPTIME_RAW` in 24 MHz ticks), the
//!   same for every core, so its rate is 24 MHz;
//! - `cpu_mperf`: ticks in a performance state, the core's running time;
//! - `cpu_aperf`: ticks in each performance state times that state's
//!   frequency over 24 MHz, the cycles the core would run at its states'
//!   nominal frequencies.
//!
//! `aperf` is therefore residency times nominal frequency, not a hardware
//! cycle count. A core's residencies are brought up to date when it changes
//! state, so between two reads `mperf` can advance by more or less than the
//! running time, by up to a second on a P core; `aperf` lags with it, and
//! their ratio is the frequency over the time counted. `cpu_tsc` is not a sum
//! of residencies for that reason. A cluster type whose table length does
//! not match its state count is not published.

const NAME: &str = "cpu_frequency";

use crate::agent::sampler_status::Unsupported;
use crate::agent::samplers::ioreport::{Channel, Subscription, Value};
use crate::agent::*;

use super::topology::{self, Core, Kind};

use std::collections::HashMap;
use std::sync::Mutex;

mod stats {
    include!("../../linux/frequency/stats.rs");
}

use stats::*;

/// The system counter's rate, which residencies are counted in.
const TICK_HZ: u128 = 24_000_000;

const GROUP: &str = "CPU Stats";
const SUBGROUP: &str = "CPU Core Performance States";

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let cores = topology::cores()
        .ok_or_else(|| Unsupported(format!("{NAME}: no CPU topology in the device tree")))?;
    let (e, p) = topology::frequency_tables()
        .ok_or_else(|| Unsupported(format!("{NAME}: no performance-state frequency tables")))?;
    let subscription = Subscription::new(&[(GROUP, Some(SUBGROUP))])
        .ok_or_else(|| Unsupported(format!("{NAME}: IOReport has no {SUBGROUP} channels")))?;
    let channels = subscription
        .sample()
        .ok_or_else(|| anyhow::anyhow!("{NAME}: IOReport sample failed"))?;

    let tables = HashMap::from([(Kind::Efficiency, e), (Kind::Performance, p)]);
    let read = read(&channels, system_ticks(), &cores, &tables);
    if read.is_empty() {
        return Err(Unsupported(format!(
            "{NAME}: no core's performance states match its frequency table"
        ))
        .into());
    }
    let ids: Vec<usize> = read.iter().map(|r| r.id).collect();
    CPU_FREQUENCY_ACQ.set_member_set(&ids);
    debug!("{NAME}: {} core(s) of {}", ids.len(), cores.len());

    Ok(Some(Box::new(Frequency {
        subscription: Mutex::new(subscription),
        cores,
        tables,
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Frequency {
    subscription: Mutex<Subscription>,
    cores: Vec<Core>,
    tables: HashMap<Kind, Vec<u64>>,
}

/// One core's counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub id: usize,
    pub tsc: u64,
    pub mperf: u64,
    pub aperf: u64,
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

/// One core's counters from its residencies and the system counter `tsc`,
/// or `None` when the states do not match `table`: a performance state
/// beyond it, or a count other than its length.
pub fn reading(id: usize, tsc: u64, states: &[(String, i64)], table: &[u64]) -> Option<Reading> {
    let mut mperf: u64 = 0;
    let mut cycles: u128 = 0;
    let mut performance_states = 0;
    for (name, ticks) in states {
        let ticks = u64::try_from(*ticks).ok()?;
        if let Some(k) = topology::state_index(name) {
            let hz = *table.get(k)?;
            performance_states += 1;
            mperf = mperf.saturating_add(ticks);
            cycles += u128::from(ticks) * u128::from(hz);
        }
    }
    if performance_states != table.len() {
        return None;
    }
    Some(Reading {
        id,
        tsc,
        mperf,
        aperf: u64::try_from(cycles / TICK_HZ).unwrap_or(u64::MAX),
    })
}

/// Every mapped core's counters in `channels`, read at system counter `tsc`.
fn read(
    channels: &[Channel],
    tsc: u64,
    cores: &[Core],
    tables: &HashMap<Kind, Vec<u64>>,
) -> Vec<Reading> {
    channels
        .iter()
        .filter(|c| c.group == GROUP && c.subgroup == SUBGROUP)
        .filter_map(|c| {
            let Value::State(states) = &c.value else {
                return None;
            };
            let (kind, cluster, core) = topology::parse_state_channel(&c.name)?;
            let id = topology::id_of(cores, kind, cluster, core)?;
            reading(id, tsc, states, tables.get(&kind)?)
        })
        .collect()
}

#[async_trait]
impl Sampler for Frequency {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut guard = CPU_FREQUENCY_ACQ.acquire();
        let (channels, tsc) = {
            let subscription = self.subscription.lock().unwrap_or_else(|e| e.into_inner());
            (subscription.sample(), system_ticks())
        };
        let Some(channels) = channels else {
            return;
        };
        guard.mark_end();
        for r in read(&channels, tsc, &self.cores, &self.tables) {
            let _ = CPU_TSC.set(r.id, r.tsc);
            let _ = CPU_MPERF.set(r.id, r.mperf);
            let _ = CPU_APERF.set(r.id, r.aperf);
        }
        guard.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(pairs: &[(&str, i64)]) -> Vec<(String, i64)> {
        pairs.iter().map(|(n, t)| (n.to_string(), *t)).collect()
    }

    #[test]
    fn residency_becomes_tsc_mperf_and_aperf() {
        let table = [1_000_000_000, 2_000_000_000];
        let r = reading(
            3,
            72_000_040,
            &states(&[
                ("DOWN", 10),
                ("IDLE", 30),
                ("V0P1", 24_000_000),
                ("V1P0", 48_000_000),
            ]),
            &table,
        )
        .unwrap();
        assert_eq!(r.id, 3);
        assert_eq!(r.tsc, 72_000_040);
        assert_eq!(r.mperf, 72_000_000);
        // One second at 1 GHz and two at 2 GHz.
        assert_eq!(r.aperf, 5_000_000_000);
        // tsc rate * aperf / mperf: 24 MHz * 5e9 / 72e6 = 1.67 GHz.
        let hz = 24_000_000u128 * u128::from(r.aperf) / u128::from(r.mperf);
        assert_eq!(hz, 1_666_666_666);
    }

    #[test]
    fn states_that_do_not_match_the_table_are_refused() {
        let table = [1_000_000_000, 2_000_000_000];
        assert_eq!(
            reading(0, 1, &states(&[("IDLE", 1), ("V0P1", 1)]), &table),
            None
        );
        assert_eq!(
            reading(
                0,
                1,
                &states(&[("V0P2", 1), ("V1P1", 1), ("V2P0", 1)]),
                &table
            ),
            None
        );
    }

    /// This machine: every core is read, and its average running frequency
    /// over a short interval lies within its cluster's table.
    #[test]
    fn every_core_is_read_within_its_table() {
        let cores = topology::cores().unwrap();
        let (e, p) = topology::frequency_tables().unwrap();
        let tables = HashMap::from([(Kind::Efficiency, e), (Kind::Performance, p)]);
        let sub = Subscription::new(&[(GROUP, Some(SUBGROUP))]).unwrap();
        let first = read(&sub.sample().unwrap(), system_ticks(), &cores, &tables);
        assert_eq!(first.len(), cores.len());
        std::thread::sleep(std::time::Duration::from_millis(200));
        let second = read(&sub.sample().unwrap(), system_ticks(), &cores, &tables);
        // tsc is one clock for every core, and advances at 24 MHz.
        for read in [&first, &second] {
            assert!(read.iter().all(|r| r.tsc == read[0].tsc), "{read:?}");
        }
        let elapsed = second[0].tsc - first[0].tsc;
        assert!(
            (4_000_000..6_000_000).contains(&elapsed),
            "{elapsed} ticks in 200 ms"
        );
        for (a, b) in first.iter().zip(&second) {
            assert_eq!(a.id, b.id);
            let kind = cores.iter().find(|c| c.id == a.id).unwrap().kind;
            let table = &tables[&kind];
            let mperf = b.mperf - a.mperf;
            if mperf == 0 {
                continue;
            }
            let hz = TICK_HZ * u128::from(b.aperf - a.aperf) / u128::from(mperf);
            assert!(
                hz as u64 >= table[0] - 1 && hz as u64 <= *table.last().unwrap(),
                "cpu {} at {hz} Hz outside {table:?}",
                a.id
            );
        }
    }
}
