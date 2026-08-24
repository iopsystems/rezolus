//! PMU counter budgeting: measure how many hardware counters are available,
//! then hand them to samplers in a declared order, whole sets at a time.
//!
//! The agent used to open more pinned CPU-wide hardware events than the PMU has
//! counters (#1053). Excess events open successfully, never get scheduled, and
//! read nothing. Which samplers won was decided by `linkme` slice order, which
//! is unspecified and can shift between builds — so the casualties were both
//! silent and arbitrary.
//!
//! Three properties, and all three are needed together: ranking alone only
//! makes the failure deterministic, and a budget alone only reorders it.
//!
//! * **Ranked** — [`DEFAULT_PRIORITY`] declares who claims counters first, and
//!   config can override it.
//! * **Budgeted** — the capacity is measured, not assumed, because the kernel
//!   does not report it (`/sys/bus/event_source/devices/cpu/caps/` carries no
//!   counter count on the hosts checked).
//! * **All-or-nothing** — a sampler that cannot get its whole event set gets
//!   none of it and is reported, rather than opening half and publishing a
//!   partial view.

/// Which PMU a sampler's events live on.
///
/// This distinction is load-bearing, not bookkeeping: a machine has several
/// independent PMUs with separate counters, and charging them all against one
/// budget starves samplers that were never competing. Measured on a host whose
/// core PMU was fully taken, `cpu_l3` still read 64 of 64 and `cpu_frequency`
/// 96 of 96, while core-PMU `cpu_branch` read 0 of 64.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmuKind {
    /// Core general-purpose counters — the scarce, contended pool, commonly 6
    /// per core, shared with every other perf consumer on the box including
    /// guest vPMUs.
    Core,
    /// An uncore PMU (AMD `amd_l3`, Intel CHA), with its own counters and its
    /// own cardinality — per cache domain, not per CPU.
    Uncore,
    /// Free-running MSRs (aperf/mperf/tsc). Not general-purpose counters and
    /// not allocated, so they cannot be starved.
    Msr,
}

/// The PMU samplers, in the order they claim counters, with the PMU each uses
/// and how many events it opens.
///
/// Only [`PmuKind::Core`] entries are budgeted; the count for those is **per
/// CPU**. The others are listed so the table is a complete picture of who opens
/// hardware events and where — and so that adding a budget for them later is a
/// matter of measuring their capacity, not of rediscovering which samplers they
/// are.
///
/// `cpu_perf` leads because cycles+instructions is the base IPC signal that
/// every other CPU metric is interpreted against. The order of the rest is a
/// judgment call, and is exactly what `[general] pmu_priority` exists to
/// override for someone whose investigation says otherwise.
///
/// Kept as one table rather than a field on every `SamplerEntry`: only these
/// samplers open PMU events, and a single list is where the whole allocation
/// policy can be read at once. `pmu_samplers_are_registered` pins the names
/// against the real sampler registry so this cannot drift into naming something
/// that no longer exists.
pub const DEFAULT_PRIORITY: &[(&str, PmuKind, usize)] = &[
    ("cpu_perf", PmuKind::Core, 2),
    ("cpu_branch", PmuKind::Core, 2),
    ("cpu_dtlb", PmuKind::Core, 2),
    // Uncore: its own counters, and per L3 domain rather than per CPU. Both
    // reasons it must not be charged against the core budget.
    ("cpu_l3", PmuKind::Uncore, 2),
    // MSR: aperf/mperf/tsc are free-running, not allocated counters.
    ("cpu_frequency", PmuKind::Msr, 3),
];

/// What the agent decided to do about one PMU sampler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub sampler: &'static str,
    /// Events per CPU this sampler wants.
    pub wants: usize,
    /// Whether it may open them.
    pub granted: bool,
    /// Counters still free when this sampler was considered — the useful half
    /// of "why not" when `granted` is false.
    pub free_at_decision: usize,
}

/// Decide which PMU samplers may open their events.
///
/// Only [`PmuKind::Core`] samplers are budgeted. `available` is the measured
/// count of free CORE counters, so charging an uncore or MSR sampler against it
/// would starve a sampler that was never competing for that pool — and the
/// counters it does use would then sit idle, helping nobody.
///
/// Pure, so the policy can be tested without a PMU.
///
/// Reserving counters is what keeps the agent from taking a whole hypervisor's
/// core PMU: KVM backs a guest's virtual PMU with host perf events, so an
/// exhausted host PMU leaves guests with a vPMU that reports itself working and
/// counts nothing. Defaults to 0, which is the behaviour that shipped before
/// this existed; raising it trades some of the agent's own metrics for the
/// guests'.
pub fn plan(
    order: &[(&'static str, PmuKind, usize)],
    available: usize,
    reserved: usize,
) -> Vec<Grant> {
    let mut free = available.saturating_sub(reserved);
    let mut plan = Vec::with_capacity(order.len());

    for &(sampler, kind, wants) in order {
        if kind != PmuKind::Core {
            // Not from the contended pool, so not ours to ration.
            plan.push(Grant {
                sampler,
                wants,
                granted: true,
                free_at_decision: free,
            });
            continue;
        }

        let free_at_decision = free;
        // All-or-nothing. A sampler that takes some of its events publishes a
        // partial view of whatever it measures, and for a ratio like IPC a
        // partial view is worse than none: the counter it did get is pinned
        // and unavailable to anything that could use a whole set.
        let granted = wants > 0 && wants <= free;
        if granted {
            free -= wants;
        }
        plan.push(Grant {
            sampler,
            wants,
            granted,
            free_at_decision,
        });
    }

    plan
}

/// The per-CPU core-counter demand of an ordering — what the budget is compared
/// against. Excludes uncore and MSR samplers, which do not draw on it.
pub fn core_demand(order: &[(&'static str, PmuKind, usize)]) -> usize {
    order
        .iter()
        .filter(|(_, kind, _)| *kind == PmuKind::Core)
        .map(|(_, _, wants)| wants)
        .sum()
}

/// Resolve the claim order from config, falling back to [`DEFAULT_PRIORITY`].
///
/// Names the config lists but that are not PMU samplers are ignored rather
/// than rejected — a typo should not stop the agent from starting — and any
/// PMU sampler the config omits keeps its default rank, appended after the
/// listed ones, so a partial override cannot silently drop a sampler
/// altogether.
pub fn resolve_order(configured: &[String]) -> Vec<(&'static str, PmuKind, usize)> {
    if configured.is_empty() {
        return DEFAULT_PRIORITY.to_vec();
    }

    let mut order: Vec<(&'static str, PmuKind, usize)> = Vec::with_capacity(DEFAULT_PRIORITY.len());
    for name in configured {
        if let Some(&entry) = DEFAULT_PRIORITY.iter().find(|(n, _, _)| n == name) {
            if !order.iter().any(|(n, _, _)| *n == entry.0) {
                order.push(entry);
            }
        }
    }
    for &entry in DEFAULT_PRIORITY {
        if !order.iter().any(|(n, _, _)| *n == entry.0) {
            order.push(entry);
        }
    }
    order
}

#[cfg(target_os = "linux")]
pub use linux::probe_available;

#[cfg(not(target_os = "linux"))]
pub fn probe_available() -> usize {
    0
}

#[cfg(target_os = "linux")]
mod linux {
    use perf_event::events::Hardware;
    use perf_event::{Builder, Counter, ReadFormat};

    /// A ceiling on the probe loop. Real PMUs are far below this; it only
    /// bounds a pathological kernel that keeps handing out scheduled counters.
    const MAX_PROBE: usize = 64;

    /// How many pinned hardware counters can be placed on CPU 0 right now.
    ///
    /// Measured, not read: the kernel does not report the count. `caps/` on the
    /// hosts checked carries only `branches` and `max_precise`.
    ///
    /// This is AVAILABILITY, not the hardware maximum — anything else already
    /// holding counters is subtracted by construction, which is the number the
    /// allocation actually needs. It is also a snapshot: another consumer
    /// starting later can still take counters out from under us, which is why
    /// this budget complements the runtime `time_running == 0` suppression
    /// rather than replacing it.
    ///
    /// CPU 0 stands in for the machine. On a hybrid part whose core types have
    /// different counter counts that is an approximation, and the conservative
    /// direction is not guaranteed.
    pub fn probe_available() -> usize {
        let mut held: Vec<Counter> = Vec::new();

        for _ in 0..MAX_PROBE {
            let mut builder = Builder::new(Hardware::INSTRUCTIONS);
            builder
                .one_cpu(0)
                .any_pid()
                .exclude_hv(false)
                .exclude_kernel(false)
                .pinned(true)
                .read_format(ReadFormat::TOTAL_TIME_ENABLED | ReadFormat::TOTAL_TIME_RUNNING);

            let Ok(mut counter) = builder.build() else {
                break;
            };
            if counter.enable().is_err() {
                break;
            }
            // Opening succeeds well past the hardware limit — that is the whole
            // reason over-subscription is silent. A counter that was never
            // scheduled reports `time_running == 0`, and that is the real edge.
            let scheduled = counter
                .read_full()
                .ok()
                .and_then(|r| r.time_running())
                .is_some_and(|running| !running.is_zero());
            if !scheduled {
                break;
            }
            held.push(counter);
        }

        let n = held.len();
        // Released here: the samplers are about to want these.
        drop(held);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sampler_that_cannot_get_its_whole_set_gets_none_of_it() {
        // Three core counters: cpu_perf takes 2, leaving one — not enough for
        // cpu_branch's pair.
        let decided = plan(DEFAULT_PRIORITY, 3, 0);

        let branch = decided.iter().find(|g| g.sampler == "cpu_branch").unwrap();
        assert!(!branch.granted, "one free counter cannot satisfy a pair");
        assert_eq!(
            branch.free_at_decision, 1,
            "the report must say how many were free, not just that it was refused"
        );

        // And a refusal must not consume the counter it could not use: a later
        // sampler that DOES fit still gets it. Deducting on refusal would make
        // one unlucky sampler starve everything behind it.
        assert_eq!(
            decided
                .iter()
                .find(|g| g.sampler == "cpu_dtlb")
                .unwrap()
                .free_at_decision,
            1
        );
    }

    #[test]
    fn reserving_counters_takes_them_off_the_top() {
        // The hypervisor case: hold four back for guest vPMUs and only the
        // first sampler fits.
        let decided = plan(DEFAULT_PRIORITY, 6, 4);
        let core_granted: Vec<&str> = decided
            .iter()
            .filter(|g| g.granted)
            .map(|g| g.sampler)
            .filter(|n| *n == "cpu_perf" || *n == "cpu_branch" || *n == "cpu_dtlb")
            .collect();
        assert_eq!(core_granted, vec!["cpu_perf"]);

        // Reserving more than exist must not underflow into a huge budget.
        let none = plan(DEFAULT_PRIORITY, 2, 99);
        assert!(
            none.iter()
                .filter(|g| matches!(g.sampler, "cpu_perf" | "cpu_branch" | "cpu_dtlb"))
                .all(|g| !g.granted),
            "over-reserving must starve the core samplers, not wrap around to plenty"
        );
    }

    #[test]
    fn a_measured_budget_of_zero_grants_nothing() {
        // What a host whose CORE PMU is already fully taken looks like: every
        // core sampler refused, each saying so.
        let decided = plan(DEFAULT_PRIORITY, 0, 0);
        assert_eq!(decided.len(), DEFAULT_PRIORITY.len());
        assert!(decided
            .iter()
            .filter(|g| matches!(g.sampler, "cpu_perf" | "cpu_branch" | "cpu_dtlb"))
            .all(|g| !g.granted && g.free_at_decision == 0));

        // ...and the samplers on OTHER PMUs still run. An exhausted core PMU
        // says nothing about uncore counters or free-running MSRs, and starving
        // them would lose metrics for no gain — measured on a host whose core
        // PMU was full, cpu_l3 read 64 of 64 and cpu_frequency 96 of 96.
        for name in ["cpu_l3", "cpu_frequency"] {
            let g = decided.iter().find(|g| g.sampler == name).unwrap();
            assert!(g.granted, "{name} does not draw on the core budget");
        }
    }

    #[test]
    fn config_order_wins_and_omissions_keep_their_default_rank() {
        let order = resolve_order(&["cpu_l3".to_string(), "cpu_branch".to_string()]);
        let names: Vec<&str> = order.iter().map(|(n, _, _)| *n).collect();

        assert_eq!(
            &names[..2],
            &["cpu_l3", "cpu_branch"],
            "the configured samplers claim first, in the order given"
        );
        assert_eq!(
            names.len(),
            DEFAULT_PRIORITY.len(),
            "a partial override must not drop the samplers it did not mention"
        );
        assert!(names.contains(&"cpu_perf"));
    }

    #[test]
    fn unknown_and_duplicate_config_names_are_tolerated() {
        // A typo should not stop the agent from starting, and a name listed
        // twice should not claim twice.
        let order = resolve_order(&[
            "cpu_l3".to_string(),
            "not_a_sampler".to_string(),
            "cpu_l3".to_string(),
        ]);
        let names: Vec<&str> = order.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(names.len(), DEFAULT_PRIORITY.len());
        assert_eq!(names.iter().filter(|n| **n == "cpu_l3").count(), 1);
        assert!(!names.contains(&"not_a_sampler"));
    }

    /// Every name in the priority table is a real sampler.
    ///
    /// The table is keyed by name rather than being a field on `SamplerEntry`,
    /// so a rename elsewhere could leave it pointing at nothing — and the
    /// symptom would be a sampler quietly claiming counters outside the budget,
    /// which is the exact failure this module exists to prevent.
    #[cfg(target_os = "linux")]
    #[test]
    fn pmu_samplers_are_registered() {
        use crate::agent::samplers::SAMPLERS;
        let known: std::collections::HashSet<&str> = SAMPLERS.iter().map(|e| e.name).collect();
        let missing: Vec<&str> = DEFAULT_PRIORITY
            .iter()
            .map(|(n, _, _)| *n)
            .filter(|n| !known.contains(n))
            .collect();
        assert!(
            missing.is_empty(),
            "these names are in the PMU priority table but are not samplers, so \
             their budget would never be enforced: {missing:?}"
        );
    }
}
