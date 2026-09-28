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
//! no BTF to ask, gets no throttle hook and reports degraded, since a
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

    let mut disabled: Vec<&'static str> = Vec::new();
    let mut required: Vec<(&'static str, &'static str)> = Vec::new();

    for (tracepoint, btf, raw, label) in HOOKS {
        if kernel_btf_has_tracepoints(&[tracepoint]) {
            disabled.push(raw);
            required.push((btf, label));
        } else {
            disabled.push(btf);
            required.push((raw, label));
        }
    }

    // One arity's twin stays enabled; every other balance program is disabled.
    // With no BTF there is no way to learn the arity, so every one is disabled
    // and the sampler runs without its throttle metrics rather than reading a
    // positional argument that may be the wrong one.
    let arity = kernel_btf_tracepoint_arg_count(BALANCE_TRACEPOINT);
    let btf_target = kernel_btf_has_tracepoints(&[BALANCE_TRACEPOINT]);
    let mut balance_selected = false;

    for (n, btf, raw) in BALANCE_PROGRAMS {
        if arity == Some(*n) {
            let (keep, drop) = if btf_target { (btf, raw) } else { (raw, btf) };
            disabled.push(drop);
            required.push((keep, BALANCE_LABEL));
            balance_selected = true;
        } else {
            disabled.push(btf);
            disabled.push(raw);
        }
    }

    match arity {
        Some(n) if balance_selected => debug!("{NAME} {BALANCE_TRACEPOINT} has {n} arguments"),
        Some(n) => warn!(
            "{NAME}: {BALANCE_TRACEPOINT} passes {n} arguments, which no program here is written \
             for; throttle metrics are disabled on this kernel"
        ),
        None => warn!(
            "{NAME}: no BTF describes {BALANCE_TRACEPOINT}, so its argument list is unknown; \
             throttle metrics are disabled on this kernel"
        ),
    }

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
    .disabled_programs(&disabled)
    .required_programs(&required)
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
}
