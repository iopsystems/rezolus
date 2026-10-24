//! Reuse explicitly opted-in acquisitions across fast wide and long builds.
//! The sampler's revision covers values, membership and labels, including
//! unsuccessful reads. It is independent of the observation's window.
use super::router::{group_registry, RezolusRouter};
use metriken_exposition::group_builder::{ExtraGroup, GroupBuilder, Stamp, StreamGroup};
use metriken_exposition::{GroupSnapshot, Snapshot, SnapshotV3};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

type Key = (&'static str, &'static str);

struct Cached<T, Encoding = ()> {
    revision: u64,
    snapshot: Option<T>,
    encoding: Encoding,
}

#[derive(Default)]
struct Plan {
    readers: Vec<tokio::sync::RwLockReadGuard<'static, ()>>,
    skip: HashSet<Key>,
    refresh: HashMap<Key, u64>,
}

fn plan<T, E>(cache: &HashMap<String, Cached<T, E>>) -> Plan {
    let mut plan = Plan::default();
    for (&key, group) in group_registry() {
        if !group.caches_snapshots() {
            continue;
        }
        let name = format!("{}/{}", key.0, key.1);
        let reader = group.try_snapshot_read();
        match reader.as_ref().and_then(|_| group.snapshot_revision()) {
            Some(revision) if !cache.get(&name).is_some_and(|c| c.revision == revision) => {
                plan.refresh.insert(key, revision);
                plan.readers.push(reader.unwrap());
            }
            _ => {
                plan.skip.insert(key);
            }
        }
    }
    plan
}

/// Validate the revision and retain the last completed observation on a race.
/// Production callers hold read guards through the build, preventing races
/// inside the library's persistent schema and stream-occupant caches.
fn finish<T: Clone, E: Default>(
    plan: &Plan,
    cache: &mut HashMap<String, Cached<T, E>>,
    groups: &mut Vec<T>,
    name_of: impl Fn(&T) -> &str,
) -> bool {
    let mut raced = false;
    for (key, revision) in &plan.refresh {
        let name = format!("{}/{}", key.0, key.1);
        if group_registry()[key].snapshot_revision() == Some(*revision) {
            cache.insert(
                name.clone(),
                Cached {
                    revision: *revision,
                    snapshot: groups.iter().find(|g| name_of(g) == name).cloned(),
                    encoding: Default::default(),
                },
            );
        } else {
            raced = true;
            groups.retain(|g| name_of(g) != name);
            if let Some(previous) = cache.get(&name).and_then(|c| c.snapshot.as_ref()) {
                groups.push(previous.clone());
            }
        }
    }
    for key in &plan.skip {
        let name = format!("{}/{}", key.0, key.1);
        if let Some(previous) = cache.get(&name).and_then(|c| c.snapshot.as_ref()) {
            groups.push(previous.clone());
        }
    }
    groups.sort_by(|a, b| name_of(a).cmp(name_of(b)));
    raced
}

pub(crate) struct V3Builder {
    inner: GroupBuilder<RezolusRouter>,
    wide: HashMap<String, Cached<GroupSnapshot, OnceLock<Vec<u8>>>>,
    long: HashMap<String, Cached<StreamGroup>>,
}

impl V3Builder {
    pub(crate) fn new() -> Self {
        Self {
            inner: GroupBuilder::new(RezolusRouter::new()),
            wide: HashMap::new(),
            long: HashMap::new(),
        }
    }

    pub(crate) fn snapshot(
        &mut self,
        stamp: Stamp,
        duration: Duration,
        extra: Vec<ExtraGroup>,
        metadata: HashMap<String, String>,
    ) -> SnapshotV3 {
        let mut plan = plan(&self.wide);
        *self.inner.router().skipped.lock().unwrap() = std::mem::take(&mut plan.skip);
        let mut snapshot = self.inner.snapshot(stamp, duration, extra, metadata);
        plan.skip = std::mem::take(&mut *self.inner.router().skipped.lock().unwrap());
        let raced = finish(&plan, &mut self.wide, &mut snapshot.groups, |g| &g.name);
        debug_assert!(
            !raced,
            "read guards prevent acquisition changes during a build"
        );
        snapshot
    }

    pub(crate) fn build_stream(&mut self, extra: Vec<ExtraGroup>) -> Vec<StreamGroup> {
        let mut plan = plan(&self.long);
        *self.inner.router().skipped.lock().unwrap() = std::mem::take(&mut plan.skip);
        let mut groups = self.inner.build_stream(extra);
        plan.skip = std::mem::take(&mut *self.inner.router().skipped.lock().unwrap());
        let raced = finish(&plan, &mut self.long, &mut groups, StreamGroup::name);
        debug_assert!(!raced, "read guards preserve stream occupant identity");
        groups
    }

    #[cfg(test)]
    pub(crate) fn rebuilds(&self) -> u64 {
        self.inner.rebuilds()
    }
}
/// Same self-contained V3 MessagePack as serde's canonical encoder. Reusing
/// encoded groups avoids walking thousands of unchanged label strings; it
/// does NOT omit schemas or require any per-client state. Old formats use the
/// original encoder. V3's four-field positional shape is frozen upstream.
pub(super) fn encode_snapshot(snapshot: &Snapshot, cache: &V3Builder) -> anyhow::Result<Vec<u8>> {
    let Snapshot::V3(s) = snapshot else {
        return Ok(rmp_serde::encode::to_vec(snapshot)?);
    };
    let mut body = Vec::new();
    rmp::encode::write_array_len(&mut body, 4)?;
    rmp_serde::encode::write(&mut body, &s.systemtime)?;
    rmp_serde::encode::write(&mut body, &s.duration)?;
    rmp_serde::encode::write(&mut body, &s.metadata)?;
    rmp::encode::write_array_len(&mut body, u32::try_from(s.groups.len())?)?;
    for group in &s.groups {
        if let Some(cached) = cache.wide.get(&group.name).filter(|c| c.matches(group)) {
            let encoded = cached.encoding.get_or_init(|| {
                rmp_serde::encode::to_vec(group).expect("failed to serialize acquisition group")
            });
            body.extend_from_slice(encoded);
        } else {
            rmp_serde::encode::write(&mut body, group)?;
        }
    }
    Ok(body)
}

impl Cached<GroupSnapshot, OnceLock<Vec<u8>>> {
    fn matches(&self, group: &GroupSnapshot) -> bool {
        let Some(previous) = &self.snapshot else {
            return false;
        };
        // Do not key this by window alone: discarded reads can alter values
        // while retaining the window. Arc identity avoids deep label-map
        // comparisons; numeric vectors are cheap to compare. This check also
        // makes encoding an older Snapshot with a newer cache safe.
        previous.name == group.name
            && previous.schema_hash == group.schema_hash
            && previous.window == group.window
            && match (&previous.schema, &group.schema) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
            && previous.counters == group.counters
            && previous.gauges == group.gauges
            && previous.histograms == group.histograms
            && previous.gauge_histograms == group.gauge_histograms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::timing::AcquisitionGroup;
    use linkme::distributed_slice;
    use metriken::{metric, Window};
    use metriken_exposition::{GroupSchema, MetricDesc, SnapshotV2};
    use std::collections::BTreeMap;
    use std::time::SystemTime;
    static SLOW_CACHE_GROUP: AcquisitionGroup =
        AcquisitionGroup::new_cached_sparse("unattributed", "slow_cache_probe");
    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static SLOW_CACHE_GROUP_REG: &'static AcquisitionGroup = &SLOW_CACHE_GROUP;
    #[metric(name = "snapshot_slow_cache_gauge", metadata = { acq_group = "slow_cache_probe" })]
    static SLOW_CACHE_GAUGE: metriken::GaugeGroup = metriken::GaugeGroup::new(1024);

    #[test]
    fn slow_acquisition_cache_handles_updates_failures_churn_and_independent_clients() {
        let _serialize = super::super::snapshot::BUILDER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        const NAME: &str = "unattributed/slow_cache_probe";
        const KEY: (&str, &str) = ("unattributed", "slow_cache_probe");
        let take = |cache: &mut V3Builder| {
            Snapshot::V3(cache.snapshot(
                Stamp::now(),
                Duration::from_secs(1),
                vec![],
                HashMap::new(),
            ))
        };
        let extract = |snapshot: &Snapshot| -> Option<GroupSnapshot> {
            let Snapshot::V3(s) = snapshot else {
                panic!("expected V3")
            };
            s.groups.iter().find(|g| g.name == NAME).cloned()
        };
        let take_long = |cache: &mut V3Builder| {
            cache
                .build_stream(vec![])
                .into_iter()
                .find_map(|g| match g {
                    StreamGroup::Long(g) if g.name == NAME => Some(g),
                    _ => None,
                })
        };
        let mut cache = V3Builder::new();
        // A new client cannot see partially initialized data.
        let guard = SLOW_CACHE_GROUP.acquire_blocking();
        SLOW_CACHE_GAUGE.set_metadata(999, [("name".into(), "/old".into())].into());
        SLOW_CACHE_GAUGE.set(999, 0);
        assert!(extract(&take(&mut cache)).is_none());
        assert!(take_long(&mut cache).is_none());
        guard.finish();
        let first = take(&mut cache);
        let first_group = extract(&first).unwrap();
        let first_long = take_long(&mut cache).unwrap();
        assert_eq!(first_long.occupants[0].gauges, vec![Some(0)]);
        assert_eq!(first_long.window, first_group.window);
        assert_eq!(
            take_long(&mut cache).unwrap().occupants,
            first_long.occupants
        );
        assert_eq!(first_group.gauges, vec![Some(0)]);
        assert_eq!(first_group.validate(), Ok(()));
        assert_eq!(
            encode_snapshot(&first, &cache).unwrap(),
            rmp_serde::to_vec(&first).unwrap()
        );
        let encoded_ptr = cache.wide[NAME].encoding.get().unwrap().as_ptr();
        assert!(plan(&cache.wide).skip.contains(&KEY));
        let second = take(&mut cache);
        assert_eq!(extract(&second), Some(first_group.clone()));
        assert_eq!(
            encode_snapshot(&second, &cache).unwrap(),
            rmp_serde::to_vec(&second).unwrap()
        );
        assert_eq!(
            encoded_ptr,
            cache.wide[NAME].encoding.get().unwrap().as_ptr()
        );

        // Mid-update readers get the old complete observation, not new values
        // attached to old labels. A second builder has no previous data yet.
        let guard = SLOW_CACHE_GROUP.acquire_blocking();
        SLOW_CACHE_GAUGE.set(999, 42);
        SLOW_CACHE_GAUGE.set_metadata(999, [("name".into(), "/new".into())].into());
        assert_eq!(extract(&take(&mut cache)), Some(first_group.clone()));
        assert_eq!(
            take_long(&mut cache).unwrap().occupants,
            first_long.occupants
        );
        let mut other_client = V3Builder::new();
        assert!(extract(&take(&mut other_client)).is_none());
        assert!(take_long(&mut other_client).is_none());
        guard.finish();
        let changed = take(&mut cache);
        let changed_group = extract(&changed).unwrap();
        assert_eq!(changed_group.gauges, vec![Some(42)]);
        let changed_long = take_long(&mut cache).unwrap();
        assert_eq!(changed_long.occupants[0].gauges, vec![Some(42)]);
        assert_eq!(changed_long.occupants[0].labels["name"], "/new");
        assert_ne!(changed_long.occupants[0].key, first_long.occupants[0].key);
        assert_eq!(
            changed_group.schema.as_ref().unwrap().gauges[0].metadata["name"],
            "/new"
        );
        assert_ne!(changed_group.schema_hash, first_group.schema_hash);
        let other = take(&mut other_client);
        assert_eq!(extract(&other), Some(changed_group.clone()));
        assert_eq!(
            encode_snapshot(&other, &other_client).unwrap(),
            rmp_serde::to_vec(&other).unwrap()
        );
        // Encoding an older snapshot against a newer cache is also safe.
        assert_eq!(
            encode_snapshot(&first, &cache).unwrap(),
            rmp_serde::to_vec(&first).unwrap()
        );

        let window = changed_group.window;
        let guard = SLOW_CACHE_GROUP.acquire_blocking();
        SLOW_CACHE_GAUGE.set(999, i64::MIN);
        guard.discard();
        let failed = take(&mut cache);
        let failed_group = extract(&failed).unwrap();
        assert_eq!(failed_group.gauges, vec![None]);
        assert_eq!(failed_group.window, window);
        let failed_long = take_long(&mut cache).unwrap();
        assert_eq!(failed_long.occupants[0].gauges, vec![None]);
        assert_eq!(failed_long.window, window);
        assert_eq!(failed_long.occupants[0].key, changed_long.occupants[0].key);
        assert_eq!(
            encode_snapshot(&failed, &cache).unwrap(),
            rmp_serde::to_vec(&failed).unwrap()
        );

        // Deterministically schedule a writer between the two revision checks.
        let guard = SLOW_CACHE_GROUP.acquire_blocking();
        SLOW_CACHE_GAUGE.set(999, 10);
        guard.finish();
        let mut pending = plan(&cache.wide);
        pending.readers.clear();
        assert!(pending.refresh.contains_key(&KEY));
        let mut torn = vec![changed_group];
        let in_flight = SLOW_CACHE_GROUP.acquire_blocking();
        SLOW_CACHE_GAUGE.set(999, 20);
        assert!(finish(&pending, &mut cache.wide, &mut torn, |g| &g.name));
        torn.retain(|g| g.name == NAME);
        assert_eq!(torn, vec![failed_group]);
        in_flight.finish();
        assert_eq!(extract(&take(&mut cache)).unwrap().gauges, vec![Some(20)]);

        let guard = SLOW_CACHE_GROUP.acquire_blocking();
        SLOW_CACHE_GAUGE.clear_metadata(999);
        SLOW_CACHE_GAUGE.set(999, i64::MIN);
        guard.finish();
        assert!(extract(&take(&mut cache)).is_none());
        assert!(extract(&take(&mut cache)).is_none());
        assert!(cache.wide[NAME].snapshot.is_none());
        assert!(take_long(&mut cache).is_none());
        assert!(take_long(&mut cache).is_none());
        assert!(plan(&cache.wide).skip.contains(&KEY));
    }

    #[test]
    fn cached_group_encoder_matches_canonical_v3_bytes_at_array_boundaries() {
        let schema = Arc::new(GroupSchema {
            counters: vec![MetricDesc {
                name: "counter".into(),
                metadata: BTreeMap::new(),
            }],
            gauges: vec![MetricDesc {
                name: "gauge".into(),
                metadata: BTreeMap::new(),
            }],
            histograms: vec![],
            gauge_histograms: vec![MetricDesc {
                name: "gauge_histogram".into(),
                metadata: BTreeMap::new(),
            }],
        });
        let group = GroupSnapshot {
            name: "test/group".into(),
            schema_hash: schema.hash(),
            schema: Some(schema),
            window: Some(Window::new(10, 20)),
            counters: vec![Some(u64::MAX)],
            gauges: vec![Some(-42)],
            histograms: vec![],
            gauge_histograms: vec![None],
        };
        for count in [0, 1, 15, 16, 17, 255, 256] {
            let mut cache = V3Builder::new();
            cache.wide.insert(
                group.name.clone(),
                Cached {
                    revision: 2,
                    snapshot: Some(group.clone()),
                    encoding: Default::default(),
                },
            );
            let priming = Snapshot::V3(SnapshotV3 {
                systemtime: SystemTime::UNIX_EPOCH,
                duration: Duration::ZERO,
                metadata: HashMap::new(),
                groups: vec![group.clone()],
            });
            encode_snapshot(&priming, &cache).unwrap();
            let mut groups = vec![group.clone(); count];
            // Mix cached and uncached groups in the same response.
            if let Some(last) = groups.last_mut() {
                last.gauge_histograms[0] = Some(histogram::Histogram::new(3, 8).unwrap());
            }
            let snapshot = Snapshot::V3(SnapshotV3 {
                systemtime: SystemTime::UNIX_EPOCH,
                duration: Duration::from_millis(1),
                metadata: [("source".into(), "test".into())].into(),
                groups,
            });
            assert_eq!(
                encode_snapshot(&snapshot, &cache).unwrap(),
                rmp_serde::to_vec(&snapshot).unwrap(),
                "{count} groups"
            );
        }
        let legacy = Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::now(),
            duration: Duration::from_secs(1),
            metadata: HashMap::new(),
            counters: vec![],
            gauges: vec![],
            histograms: vec![],
            gauge_histograms: vec![],
        });
        assert_eq!(
            encode_snapshot(&legacy, &V3Builder::new()).unwrap(),
            rmp_serde::to_vec(&legacy).unwrap()
        );
    }
}
