//! The agent's half of building a V3 snapshot: which acquisition group each
//! registered metric belongs to, how that group's members are chosen, and
//! where its window comes from.
//!
//! The registry walk, the skeleton cache, member naming and the schema hash
//! are metriken's (`metriken_exposition::group_builder`). What stays here is
//! what only the agent knows: sampler attribution by module path, the
//! [`ACQUISITION_GROUPS`](crate::agent::samplers::ACQUISITION_GROUPS)
//! registry, and the reader-stamped bracket around a `PackedCounters` read.

use crate::agent::timing::{AcquisitionGroup, AcquisitionGuard};

use metriken::{MetricEntry, Window};
use metriken_exposition::group_builder::{
    Acquisition, GroupId, Membership, ReadGuard, Route, Router, GROUP_METADATA_KEY,
};

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};

/// Keyed by `(sampler, name)` and looked up for every routed metric twice a
/// pass, so it hashes with foldhash rather than the default SipHash.
type GroupRegistry =
    HashMap<(&'static str, &'static str), &'static AcquisitionGroup, foldhash::fast::RandomState>;

/// The `(sampler, name) -> AcquisitionGroup` registry, built once. Sound to
/// cache for the process lifetime: `ACQUISITION_GROUPS` is a `linkme`
/// distributed slice, populated at link time before `main` runs and never
/// mutated afterward.
pub(super) fn group_registry() -> &'static GroupRegistry {
    static REGISTRY: OnceLock<GroupRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry: GroupRegistry = HashMap::default();
        for group in crate::agent::samplers::ACQUISITION_GROUPS {
            // Keyed by the `(sampler, name)` parts `AcquisitionGroup` itself
            // stores, both `&'static str`, so neither building this map nor a
            // lookup against it allocates.
            let key = (group.sampler, group.name);
            let prev = registry.insert(key, group);
            debug_assert!(
                prev.is_none(),
                "duplicate acquisition-group registry key `{}/{}` — every registered group's \
                 (sampler, name) pair must stay globally unique, including on builds where a BPF \
                 sampler does not exist and `samplers::bpf_sampler_name` resolves its \
                 `stats.rs` to the shared \"unattributed\" bucket (stats.rs is `include!`d \
                 there for metric-identity continuity, with no matching SamplerEntry). Qualify the group's `name` with its sampler, e.g. \
                 `<sampler>_<shortname>` — see the naming rule documented on \
                 `samplers::ACQUISITION_GROUPS`.",
                group.sampler,
                group.name,
            );
        }
        registry
    })
}

/// Routes the agent's registry into acquisition groups.
///
/// # Routing
///
/// A metric whose static metadata carries `acq_group = "<name>"` goes to the
/// declared group `"<sampler>/<name>"`, provided `(sampler, name)` is
/// registered on `ACQUISITION_GROUPS`. Every other metric goes to its
/// sampler's default group, `"<sampler>/main"`. A `log_` metric is left out.
///
/// A metric naming an `acq_group` with no registry entry is a migration bug
/// (a typo, or a group renamed on one side only). It is routed to the default
/// group so the tick still produces a valid snapshot, and a `debug_assert!`
/// catches it in tests and debug builds. On a debug build that panics the
/// scrape task for one tick; the `tokio::sync::Mutex` around
/// `SnapshotBuilder` does not poison, so the next scrape retries.
///
/// A registered group no metric routes to is absent from the snapshot, not
/// empty.
///
/// # Membership
///
/// - reader-stamped group ([`AcquisitionGroup::is_reader_stamped`], the
///   mmap-direct `PackedCounters`): the slots with metadata
///   ([`Membership::Slots`]), walked from the metadata store, so a 4M-slot
///   `MAX_PID` array costs its live population and not its capacity;
/// - declared group with a member set: exactly those slots;
/// - declared group with a member bound: the first `bound` slots;
/// - any other declared group: every slot of the backing array;
/// - default group: membership follows values ([`Membership::Present`]), the
///   V2 sentinel rule, so an undeclared per-CPU or per-device array does not
///   publish a member for every slot it could ever hold.
///
/// Under every registered membership a slot with no reading is `None`, not
/// absent. See `docs/principles.md` principle 18.
///
/// # Windows
///
/// A reader-stamped group's window is the builder's own read of it
/// ([`ReaderGuard`]). A declared group's window is the one its sampler
/// stamped, read before and after the group's values and unioned. A default
/// group has none.
pub(crate) struct RezolusRouter {
    sampler_mods: Vec<(&'static str, &'static str)>,
    groups: &'static GroupRegistry,
    /// Each module path's sampler, resolved once. Attribution is a scan of
    /// every registered sampler module, and the builder routes every metric
    /// twice a pass; a module path's sampler never changes, since sampler
    /// modules are registered at link time.
    attribution: Mutex<HashMap<String, &'static str, foldhash::fast::RandomState>>,
}

impl RezolusRouter {
    pub(crate) fn new() -> Self {
        Self {
            sampler_mods: crate::agent::samplers::sampler_modules(),
            groups: group_registry(),
            attribution: Mutex::new(HashMap::default()),
        }
    }

    fn sampler(&self, metric: &MetricEntry) -> &'static str {
        let module = metric.module();
        let mut cache = self.attribution.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(sampler) = cache.get(module) {
            return sampler;
        }
        let sampler = crate::agent::samplers::attribute_sampler(module, &self.sampler_mods);
        cache.insert(module.to_string(), sampler);
        sampler
    }
}

impl Router for RezolusRouter {
    type Guard = ReaderGuard;

    fn route<'a>(&'a self, metric: &'a MetricEntry) -> Option<Route<'a>> {
        if metric.name().starts_with("log_") {
            return None;
        }
        let sampler = self.sampler(metric);
        let Some(acq_group) = metric.metadata().get(GROUP_METADATA_KEY) else {
            return Some(Route {
                group: GroupId::new(sampler, "main"),
                membership: Membership::Present,
            });
        };
        let Some(ag) = self.groups.get(&(sampler, acq_group)).copied() else {
            debug_assert!(
                false,
                "metric `{}` declares acq_group=\"{acq_group}\" for sampler `{sampler}`, but no \
                 AcquisitionGroup (\"{sampler}\", \"{acq_group}\") is registered on \
                 ACQUISITION_GROUPS; routing to the default group instead",
                metric.name(),
            );
            return Some(Route {
                group: GroupId::new(sampler, "main"),
                membership: Membership::Present,
            });
        };
        // A set wins over a bound: a sampler that knows the indices knows
        // more than one that knows a count.
        let membership = if ag.is_reader_stamped() {
            Membership::Slots
        } else if let Some(set) = ag.member_set() {
            Membership::Set(set)
        } else if let Some(bound) = ag.member_bound() {
            Membership::Prefix(bound)
        } else {
            Membership::All
        };
        Some(Route {
            group: GroupId::new(ag.sampler, ag.name),
            membership,
        })
    }

    fn acquire(&self, group: GroupId<'_>) -> Acquisition<ReaderGuard> {
        match self.groups.get(&(group.namespace, group.name)).copied() {
            Some(ag) if ag.is_reader_stamped() => Acquisition::Reader(ReaderGuard {
                guard: ag.acquire(),
                group: ag,
            }),
            Some(ag) => Acquisition::Stamped(ag.window()),
            None => Acquisition::Windowless,
        }
    }

    fn window(&self, group: GroupId<'_>) -> Option<Window> {
        self.groups
            .get(&(group.namespace, group.name))
            .and_then(|ag| ag.window())
    }

    /// Every member carries the sampler it is attributed to. Inserted after
    /// the metric's static metadata, so it wins over a static `sampler` key,
    /// the same order V2's `metric_metadata` uses.
    fn annotate(&self, metric: &MetricEntry, metadata: &mut BTreeMap<String, String>) {
        metadata.insert("sampler".to_string(), self.sampler(metric).to_string());
    }
}

/// The bracket around the builder's own read of a reader-stamped group.
///
/// Opened at the group's first touch, its end marked after each of the
/// group's metrics is read, and published when the group is emitted. The
/// window returned is the one just published, read back from the group's
/// slot. A group that produced no values drops this unfinished and publishes
/// nothing.
pub(crate) struct ReaderGuard {
    guard: AcquisitionGuard<'static>,
    group: &'static AcquisitionGroup,
}

impl ReadGuard for ReaderGuard {
    fn mark_end(&mut self) {
        self.guard.mark_end();
    }

    fn finish(self) -> Option<Window> {
        self.guard.finish();
        self.group.window()
    }
}
