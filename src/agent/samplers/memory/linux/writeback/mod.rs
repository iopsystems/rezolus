//! Collects page-cache writeback stats using BPF and traces:
//! * `balance_dirty_pages` — every throttle evaluation of a task whose dirty
//!   pages were over the free-run ceiling, and the sleep it imposed (a raw
//!   argument in jiffies, converted with the measured tick; see `jiffy_ns`)
//! * `writeback_start` — every flusher pass, by the reason its work ran
//! * `writeback_pages_written` — pages each flusher pass wrote
//!
//! And produces these stats:
//! * `writeback_throttle_latency`, `writeback_throttle_checks`,
//!   `writeback_throttle_events`, `writeback_throttled_time`
//! * `writeback_runs` — labeled by `reason`
//! * `writeback_pages_written`
//!
//! Filesystem-agnostic: these are the mm layer's tracepoints. The throttle
//! is the mechanism behind "dirty pages accumulate and flush in bursts that
//! throttle writers"; the runs by reason say whether a periodic burst is the
//! flusher's cadence (`periodic`), memory pressure (`vmscan`, `background`)
//! or an explicit sync. `memory_dirty`/`memory_writeback` in `memory_meminfo`
//! are the gauges these rates act on.
//!
//! `balance_dirty_pages` has had two argument lists (see mod.bpf.c). The
//! sampler reads the tracepoint's argument count from BTF and loads the
//! program written for it; a kernel whose count matches neither, or that has
//! no BTF to ask, gets no throttle hook and reports it unsupported, since a
//! positional read of the wrong argument would be wrong without any error.
//! See docs/journal/2026-09-28-filesystem-telemetry-gaps.md, capability C1.

const NAME: &str = "memory_writeback";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/memory_writeback.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::agent::*;

use std::sync::Arc;

/// The tracepoints with one argument list, their twins, and the label
/// `rezolus status` shows if the active twin fails to attach.
const HOOKS: &[(&str, &str, &str, &str)] = &[
    (
        "writeback_start",
        "writeback_start_btf",
        "writeback_start_raw",
        "writeback runs by reason",
    ),
    (
        "writeback_pages_written",
        "writeback_pages_written_btf",
        "writeback_pages_written_raw",
        "pages written back",
    ),
];

/// `balance_dirty_pages` programs by argument count.
const BALANCE_TRACEPOINT: &str = "balance_dirty_pages";
const BALANCE_PROGRAMS: &[(u32, &str, &str)] = &[
    (
        12,
        "balance_dirty_pages_12_btf",
        "balance_dirty_pages_12_raw",
    ),
    (8, "balance_dirty_pages_8_btf", "balance_dirty_pages_8_raw"),
];
const BALANCE_LABEL: &str = "writeback throttling";

/// Which programs to load, from the kernel's BTF: `has_tracepoint` says
/// whether BTF describes a tracepoint (selecting its `tp_btf` twin over the
/// `raw_tp` one), and `balance_arity` is the argument count BTF gives for
/// `balance_dirty_pages`. The throttle hook is reported unavailable when that
/// count is unknown or matches no program, since reading an argument by
/// position on another argument list reads the wrong one.
fn select_programs(
    has_tracepoint: impl Fn(&str) -> bool,
    balance_arity: Option<u32>,
) -> crate::agent::bpf::ProgramSelection {
    let mut selection = crate::agent::bpf::ProgramSelection::default();

    for (tracepoint, btf, raw, label) in HOOKS {
        let (keep, drop) = if has_tracepoint(tracepoint) {
            (btf, raw)
        } else {
            (raw, btf)
        };
        selection.disabled.push(drop);
        selection.required.push((keep, label));
    }

    let btf_target = has_tracepoint(BALANCE_TRACEPOINT);
    let selected = BALANCE_PROGRAMS
        .iter()
        .any(|(n, _, _)| balance_arity == Some(*n));
    let reason = match balance_arity {
        Some(_) => "balance_dirty_pages passes an argument count no program here is written for",
        None => "BTF gives no argument count for balance_dirty_pages",
    };

    for (n, btf, raw) in BALANCE_PROGRAMS {
        let (active, twin) = if btf_target { (btf, raw) } else { (raw, btf) };
        selection.disabled.push(twin);
        if balance_arity == Some(*n) {
            selection.required.push((active, BALANCE_LABEL));
        } else if selected {
            // Written for the other argument list.
            selection.disabled.push(active);
        } else {
            selection.unavailable.push((active, BALANCE_LABEL, reason));
        }
    }

    selection
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Order MUST match the C_* indices in mod.bpf.c.
    let counters = vec![
        &WRITEBACK_THROTTLE_CHECKS,
        &WRITEBACK_THROTTLE_EVENTS,
        &WRITEBACK_THROTTLED_TIME,
        &WRITEBACK_PAGES_WRITTEN,
        &WRITEBACK_RUNS_BACKGROUND,
        &WRITEBACK_RUNS_VMSCAN,
        &WRITEBACK_RUNS_SYNC,
        &WRITEBACK_RUNS_PERIODIC,
        &WRITEBACK_RUNS_LAPTOP_TIMER,
        &WRITEBACK_RUNS_FS_FREE_SPACE,
        &WRITEBACK_RUNS_FORKER_THREAD,
        &WRITEBACK_RUNS_FOREIGN_FLUSH,
    ];

    let arity = kernel_btf_tracepoint_arg_count(BALANCE_TRACEPOINT);
    match arity {
        Some(n) if BALANCE_PROGRAMS.iter().any(|(m, _, _)| *m == n) => {
            debug!("{NAME} {BALANCE_TRACEPOINT} has {n} arguments")
        }
        Some(n) => info!(
            "{NAME}: {BALANCE_TRACEPOINT} passes {n} arguments, which no program here is written for"
        ),
        None => debug!("{NAME}: BTF gives no argument count for {BALANCE_TRACEPOINT}"),
    }
    let selection = select_programs(|tp| kernel_btf_has_tracepoints(&[tp]), arity);

    let tick = match jiffy_ns() {
        Some(ns) => ns,
        None => {
            warn!(
                "{NAME}: clock_getres(CLOCK_MONOTONIC_COARSE) gave no tick length; the throttle \
                 sleep histogram and writeback_throttled_time will stay empty"
            );
            0
        }
    };
    debug!("{NAME} jiffy = {tick} ns");

    let bpf = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .counters("counters", counters, &COUNTERS_ACQ)
    .histogram(
        "throttle_latency",
        &WRITEBACK_THROTTLE_LATENCY,
        &THROTTLE_LATENCIES_ACQ,
    )
    .map("jiffy_ns", vec![tick])
    .disabled_programs(&selection.disabled)
    .required_programs(&selection.required)
    .unavailable_programs(&selection.unavailable)
    .build()?;

    Ok(Some(Box::new(bpf)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map<'_> {
        match name {
            "counters" => &self.maps.counters,
            "throttle_latency" => &self.maps.throttle_latency,
            "jiffy_ns" => &self.maps.jiffy_ns,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} balance_dirty_pages_12_btf() BPF instruction count: {}",
            self.progs.balance_dirty_pages_12_btf.insn_cnt()
        );
        debug!(
            "{NAME} writeback_start_btf() BPF instruction count: {}",
            self.progs.writeback_start_btf.insn_cnt()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every program name is distinct across the fixed hooks and the arity
    /// variants: `disabled_programs` refuses a name not in the skeleton, and
    /// a duplicate would leave a twin autoloaded.
    #[test]
    fn program_names_are_distinct() {
        let mut names: Vec<&str> = HOOKS
            .iter()
            .flat_map(|(_, btf, raw, _)| [*btf, *raw])
            .chain(
                BALANCE_PROGRAMS
                    .iter()
                    .flat_map(|(_, btf, raw)| [*btf, *raw]),
            )
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
        assert_eq!(total, 2 * HOOKS.len() + 2 * BALANCE_PROGRAMS.len());
    }

    fn all_programs() -> Vec<&'static str> {
        HOOKS
            .iter()
            .flat_map(|(_, btf, raw, _)| [*btf, *raw])
            .chain(
                BALANCE_PROGRAMS
                    .iter()
                    .flat_map(|(_, btf, raw)| [*btf, *raw]),
            )
            .collect()
    }

    /// Every program is disabled, required or unavailable, and exactly one
    /// of those, on every combination of BTF and arity.
    #[test]
    fn every_program_is_selected_once() {
        for btf in [true, false] {
            for arity in [Some(12), Some(8), Some(10), None] {
                let s = select_programs(|_| btf, arity);
                for prog in all_programs() {
                    let n = s.disabled.iter().filter(|p| **p == prog).count()
                        + usize::from(s.requires(prog))
                        + usize::from(s.lacks(prog));
                    assert_eq!(
                        n, 1,
                        "{prog} selected {n} times (btf {btf}, arity {arity:?})"
                    );
                }
            }
        }
    }

    /// Each fixed hook and the balance hook take the twin their own
    /// tracepoint's BTF entry selects, not another's.
    #[test]
    fn each_twin_follows_its_own_tracepoint() {
        let s = select_programs(|tp| tp != BALANCE_TRACEPOINT, Some(12));
        assert!(s.requires("writeback_start_btf"));
        assert!(s.requires("writeback_pages_written_btf"));
        assert!(s.requires("balance_dirty_pages_12_raw"));

        let s = select_programs(|tp| tp == BALANCE_TRACEPOINT, Some(8));
        assert!(s.requires("writeback_start_raw"));
        assert!(s.requires("writeback_pages_written_raw"));
        assert!(s.requires("balance_dirty_pages_8_btf"));
    }

    /// A known arity loads its program in the attach type BTF allows; an
    /// unknown one loads no balance program and reports the throttle hook
    /// unavailable, naming the programs of the attach type in use and why,
    /// so the sampler does not read healthy without it.
    #[test]
    fn the_balance_program_follows_the_arity() {
        let s = select_programs(|_| true, Some(12));
        assert!(s.requires("balance_dirty_pages_12_btf"));
        assert!(s.unavailable.is_empty());

        let s = select_programs(|_| false, Some(8));
        assert!(s.requires("balance_dirty_pages_8_raw"));
        assert!(s.unavailable.is_empty());

        let s = select_programs(|_| true, Some(10));
        assert!(s.lacks("balance_dirty_pages_12_btf"));
        assert!(s.lacks("balance_dirty_pages_8_btf"));
        assert!(s
            .unavailable
            .iter()
            .all(|(_, label, reason)| *label == BALANCE_LABEL
                && reason.contains("argument count no program")));

        for btf in [true, false] {
            let s = select_programs(|_| btf, None);
            let suffix = if btf { "_btf" } else { "_raw" };
            assert_eq!(s.unavailable.len(), BALANCE_PROGRAMS.len());
            assert!(s
                .unavailable
                .iter()
                .all(|(p, label, reason)| p.ends_with(suffix)
                    && *label == BALANCE_LABEL
                    && reason.starts_with("BTF gives no argument count")));
            assert!(!s.required.iter().any(|(_, label)| *label == BALANCE_LABEL));
        }
    }
}
