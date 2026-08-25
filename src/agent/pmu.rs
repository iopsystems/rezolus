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
    /// The RAPL energy PMU (`power`, and AMD's `power_core`). Free-running
    /// energy accumulators on their own PMU — type 14/31 here against the core
    /// PMU's 4 — so they never draw on the contended pool. Distinct from
    /// [`PmuKind::Msr`]: the `msr` PMU is a different device (type 13) whose
    /// fixed eight-entry allowlist holds no energy register, so RAPL is not
    /// reachable through it.
    Rapl,
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
    // RAPL: its own PMU, free-running energy accumulators, package- and
    // core-scope rather than per-CPU. The count is the most domains the
    // `power` PMU can expose (pkg, cores, gpu, ram, psys) rather than a
    // per-CPU figure, which would not mean anything for a package-scope PMU;
    // what a host actually opens depends on which domains it implements, and
    // ranges from one to three across the parts measured. Not budgeted, so
    // the number is documentary only.
    ("cpu_power", PmuKind::Rapl, 5),
];

/// What the agent decided to do about one PMU sampler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub sampler: &'static str,
    /// Events per CPU this sampler wants.
    pub wants: usize,
    /// Whether it may open them anywhere at all.
    pub granted: bool,
    /// Counters still free when this sampler was considered — the useful half
    /// of "why not" when `granted` is false.
    ///
    /// When a reservation applies to only some CPUs this is the figure on the
    /// RESERVED ones, which is where the shortage is.
    pub free_at_decision: usize,
    /// The CPUs this sampler may open events on, when that is not all of them.
    ///
    /// `None` means unrestricted — the common case, and what a sampler running
    /// outside the agent's normal startup sees.
    pub cpus: Option<Vec<usize>>,
    /// Whether the grant covers only part of the machine, and how much.
    ///
    /// `None` means every CPU. `Some((granted, total))` means the reservation
    /// left too little on the reserved CPUs but not on the rest, so this
    /// sampler runs on `granted` of `total` CPUs.
    ///
    /// Carried separately from `granted` because "running on 24 of 32 CPUs" is
    /// neither working nor refused, and reporting it as either would mislead:
    /// as `Active` it reads as full coverage, as starved it reads as no data
    /// when there is plenty.
    pub partial: Option<(usize, usize)>,
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
    cpus: &CpuSplit,
) -> Vec<Grant> {
    // Two budgets, because a reservation may cover only part of the machine:
    // the reserved CPUs give up counters, the rest do not. Both are walked in
    // lockstep so a sampler's rank means the same thing on either.
    //
    // They can genuinely diverge. A sampler that fits only on the unreserved
    // side consumes there and not here, after which a later sampler can fit on
    // the RESERVED side and not the other — so neither side is simply the
    // other's subset, and both directions of partial coverage are real.
    let mut free_reserved = available.saturating_sub(reserved);
    let mut free_open = available;
    let mut plan = Vec::with_capacity(order.len());

    for &(sampler, kind, wants) in order {
        if kind != PmuKind::Core {
            // Not from the contended pool, so not ours to ration.
            plan.push(Grant {
                sampler,
                wants,
                granted: true,
                free_at_decision: free_reserved,
                cpus: None,
                partial: None,
            });
            continue;
        }

        let free_at_decision = free_reserved;
        // All-or-nothing, PER CPU. A sampler that takes some of its events on a
        // CPU publishes a partial view there, and for a ratio like IPC that is
        // worse than none: the counter it did get is pinned and unavailable to
        // anything that could use a whole set. Per CPU rather than per machine
        // because the counters themselves are per CPU — the grain the hardware
        // actually has.
        let wanted = wants > 0;
        let on_reserved = wanted && !cpus.reserved.is_empty() && wants <= free_reserved;
        let on_open = wanted && cpus.open > 0 && wants <= free_open;

        if on_reserved {
            free_reserved -= wants;
        }
        if on_open {
            free_open -= wants;
        }

        let mut granted_cpus: Vec<usize> = Vec::new();
        if on_reserved {
            granted_cpus.extend(cpus.reserved.iter().copied());
        }
        if on_open {
            granted_cpus.extend((0..cpus.total()).filter(|c| !cpus.is_reserved(*c)));
        }
        granted_cpus.sort_unstable();

        let total = cpus.total();
        let granted = !granted_cpus.is_empty();
        let partial =
            (granted && granted_cpus.len() < total).then_some((granted_cpus.len(), total));

        plan.push(Grant {
            sampler,
            wants,
            granted,
            free_at_decision,
            // `None` means unrestricted, which keeps the common case free of a
            // list that would have to be kept in step with the machine.
            cpus: partial.is_some().then_some(granted_cpus),
            partial,
        });
    }

    plan
}

/// How the machine splits between CPUs a reservation applies to and the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuSplit {
    /// CPUs where counters are held back for other consumers.
    pub reserved: Vec<usize>,
    /// How many CPUs are not covered by the reservation.
    pub open: usize,
}

impl CpuSplit {
    /// Every CPU reserved — what an unset mask means, and what the agent did
    /// before a mask existed.
    pub fn all(total: usize) -> Self {
        Self {
            reserved: (0..total).collect(),
            open: 0,
        }
    }

    pub fn total(&self) -> usize {
        self.reserved.len() + self.open
    }

    /// Whether `cpu` gives up counters.
    pub fn is_reserved(&self, cpu: usize) -> bool {
        self.reserved.contains(&cpu)
    }
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

/// Parse a CPU list like `"0-3,8,12-15"` into sorted, de-duplicated ids.
///
/// Returns `None` for anything unparseable rather than a partial set: a mask
/// that silently covered fewer CPUs than the operator wrote would hold back
/// counters in the wrong places, and the whole point of the mask is knowing
/// exactly where the agent is standing aside.
pub fn parse_cpu_list(spec: &str) -> Option<Vec<usize>> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Some(Vec::new());
    }

    let mut cpus = std::collections::BTreeSet::new();
    for part in spec.split(',') {
        let part = part.trim();
        match part.split_once('-') {
            Some((lo, hi)) => {
                let lo: usize = lo.trim().parse().ok()?;
                let hi: usize = hi.trim().parse().ok()?;
                if hi < lo {
                    return None;
                }
                cpus.extend(lo..=hi);
            }
            None => {
                cpus.insert(part.parse().ok()?);
            }
        }
    }
    Some(cpus.into_iter().collect())
}

/// Build the reserved/open split from the configured mask.
///
/// `None` reserves every CPU — the behaviour before a mask existed. A mask that
/// names CPUs this machine does not have keeps only the ones it does, so a
/// config shared across a heterogeneous fleet does not have to be exact.
pub fn cpu_split(spec: Option<&str>) -> CpuSplit {
    let total = online_cpu_count();
    let Some(spec) = spec else {
        return CpuSplit::all(total);
    };
    let Some(listed) = parse_cpu_list(spec) else {
        // `General::check` rejects an unparseable mask at startup, so reaching
        // here means something bypassed it; reserve everywhere rather than
        // silently reserving nowhere.
        return CpuSplit::all(total);
    };
    let reserved: Vec<usize> = listed.into_iter().filter(|c| *c < total).collect();
    let open = total.saturating_sub(reserved.len());
    CpuSplit { reserved, open }
}

/// Online CPU count, for sizing the split.
#[cfg(target_os = "linux")]
fn online_cpu_count() -> usize {
    std::fs::read_to_string("/sys/devices/system/cpu/online")
        .ok()
        .and_then(|s| parse_cpu_list(s.trim()))
        .map(|c| c.len())
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn online_cpu_count() -> usize {
    0
}

/// The per-sampler CPU allocation, published once at startup for the samplers
/// to consult as they open events.
///
/// A global rather than a parameter because the enforcement points are spread
/// across the two ways events get opened — the BPF builder for `cpu_perf`, and
/// each direct sampler's own per-CPU loop — and threading it through both would
/// mean changing signatures on paths that otherwise have nothing to do with
/// each other.
static ALLOCATION: std::sync::OnceLock<std::collections::BTreeMap<&'static str, Vec<usize>>> =
    std::sync::OnceLock::new();

/// Publish which CPUs each sampler may open events on. Called once, before any
/// sampler runs.
pub fn publish_allocation(plan: &[Grant]) {
    let mut map = std::collections::BTreeMap::new();
    for grant in plan {
        if !grant.granted {
            continue;
        }
        if let Some(cpus) = &grant.cpus {
            map.insert(grant.sampler, cpus.clone());
        }
        // A full grant is left out of the map entirely; `allowed_cpus` reads
        // absence as "no restriction", so the common case costs nothing and
        // cannot be narrowed by a stale entry.
    }
    let _ = ALLOCATION.set(map);
}

/// Narrow `cores` to the CPUs `sampler` may open events on.
///
/// Absence means unrestricted, which is both the common case and the state
/// before any allocation is published — so a sampler that runs outside the
/// agent's normal startup (a test, a tool) behaves exactly as it did before.
pub fn allowed_cpus(sampler: &str, cores: Vec<usize>) -> Vec<usize> {
    let Some(map) = ALLOCATION.get() else {
        return cores;
    };
    let Some(allowed) = map.get(sampler) else {
        return cores;
    };
    cores.into_iter().filter(|c| allowed.contains(c)).collect()
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
        let decided = plan(DEFAULT_PRIORITY, 3, 0, &CpuSplit::all(8));

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
        let decided = plan(DEFAULT_PRIORITY, 6, 4, &CpuSplit::all(8));
        let core_granted: Vec<&str> = decided
            .iter()
            .filter(|g| g.granted)
            .map(|g| g.sampler)
            .filter(|n| *n == "cpu_perf" || *n == "cpu_branch" || *n == "cpu_dtlb")
            .collect();
        assert_eq!(core_granted, vec!["cpu_perf"]);

        // Reserving more than exist must not underflow into a huge budget.
        let none = plan(DEFAULT_PRIORITY, 2, 99, &CpuSplit::all(8));
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
        let decided = plan(DEFAULT_PRIORITY, 0, 0, &CpuSplit::all(8));
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

    /// A mask reserves counters on some CPUs and not others, so a sampler can
    /// fit on part of the machine.
    ///
    /// The whole point of the mask: reserving uniformly costs the agent
    /// coverage on CPUs where nothing else wanted counters. Here six counters
    /// with four held back on half the machine leaves two there — enough for
    /// cpu_perf but not for cpu_branch behind it — while the unreserved half
    /// has all six and fits both.
    #[test]
    fn a_masked_reservation_can_grant_part_of_the_machine() {
        let split = CpuSplit {
            reserved: (0..4).collect(),
            open: 4,
        };
        let decided = plan(DEFAULT_PRIORITY, 6, 4, &split);

        let perf = decided.iter().find(|g| g.sampler == "cpu_perf").unwrap();
        assert!(perf.granted);
        assert_eq!(perf.partial, None, "cpu_perf fits in the 2 left everywhere");

        let branch = decided.iter().find(|g| g.sampler == "cpu_branch").unwrap();
        assert!(
            branch.granted,
            "it fits on the unreserved half, so it must not be refused outright"
        );
        assert_eq!(
            branch.partial,
            Some((4, 8)),
            "and must be reported as covering only that half"
        );
    }

    /// Reserving on every CPU is the old whole-machine behaviour: nothing is
    /// partial, because there is nowhere with more room.
    #[test]
    fn reserving_everywhere_is_never_partial() {
        let decided = plan(DEFAULT_PRIORITY, 6, 4, &CpuSplit::all(8));
        assert!(
            decided.iter().all(|g| g.partial.is_none()),
            "with no unreserved CPUs there is no partial case to report"
        );
    }

    #[test]
    fn cpu_lists_parse_or_are_refused_whole() {
        assert_eq!(
            parse_cpu_list("0-3,8,12-13"),
            Some(vec![0, 1, 2, 3, 8, 12, 13])
        );
        assert_eq!(parse_cpu_list(" 5 "), Some(vec![5]));
        assert_eq!(parse_cpu_list(""), Some(Vec::new()));
        // Overlaps collapse rather than double-count.
        assert_eq!(parse_cpu_list("0-2,1-3"), Some(vec![0, 1, 2, 3]));

        // Refused WHOLE, not partially: a mask that quietly covered fewer CPUs
        // than written would hold counters back in the wrong places, and the
        // point of the mask is knowing exactly where the agent stands aside.
        assert_eq!(parse_cpu_list("0-3,bogus"), None);
        assert_eq!(parse_cpu_list("3-1"), None, "a backwards range is a typo");
        assert_eq!(parse_cpu_list("-4"), None);
    }

    /// A mask naming CPUs this machine does not have keeps the ones it does.
    ///
    /// One config across a fleet of different core counts should not have to be
    /// exact, and the safe direction is to reserve on the CPUs that exist
    /// rather than to reserve nowhere.
    #[test]
    fn a_mask_beyond_this_machine_keeps_what_fits() {
        let split = cpu_split(Some("0-1"));
        // `online_cpu_count` is 0 off Linux and in a test environment without
        // the sysfs file, so assert the property that holds either way: no
        // reserved CPU is beyond the machine.
        assert!(split.reserved.iter().all(|c| *c < split.total().max(1)));
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
