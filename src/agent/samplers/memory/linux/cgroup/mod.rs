//! Cgroup v2 memory accounting, including allocations made before agent startup.
//!
//! There is no stable BPF/perf interface that supplies these complete charged
//! byte gauges. Tracking allocation events would miss existing allocations and
//! require hooks across anonymous, file, slab and socket accounting hot paths.
//! Read the kernel's authoritative cgroupfs gauges instead (principle 15).
//! Walking the hierarchy and reading memory.stat can trigger kernel accounting
//! work, so throttle independently of scrapes and run off-cycle on the blocking
//! pool (principle 17). The 60s default follows drivehealth's slow-read model;
//! it captures memory trends, not short spikes. Cached series still cost CPU
//! and bandwidth at every scrape, so this sampler is opt-in (principle 13).
//! No memory controller settings are changed.

const NAME: &str = "memory_cgroup";
const ROOT: &str = "/sys/fs/cgroup";
const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);

use crate::agent::*;
use metriken::GaugeGroup;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use walkdir::WalkDir;

mod stats;
use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }
    let controllers = std::fs::read_to_string(Path::new(ROOT).join("cgroup.controllers"))?;
    anyhow::ensure!(
        controllers.split_whitespace().any(|c| c == "memory"),
        "cgroup v2 memory controller is unavailable at {ROOT}"
    );
    let interval = config.sampler_interval(NAME).unwrap_or(DEFAULT_INTERVAL);
    anyhow::ensure!(
        !interval.is_zero(),
        "{NAME} interval must be greater than zero"
    );
    Ok(Some(Box::new(CgroupMemory {
        state: Arc::new(Mutex::new(State::default())),
        interval,
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct CgroupMemory {
    state: Arc<Mutex<State>>,
    interval: Duration,
}

#[derive(Default)]
struct State {
    last_read: Option<Instant>,
    reading: bool,
    slots: HashMap<PathBuf, usize>,
    warned_capacity: bool,
}

#[async_trait]
impl Sampler for CgroupMemory {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        // Never wait for a sweep on the async worker, and never queue a second
        // sweep behind a slow first one. Cadence is measured from completion.
        let Ok(mut state) = self.state.try_lock() else {
            return;
        };
        if state.reading || state.last_read.is_some_and(|t| t.elapsed() < self.interval) {
            return;
        }
        state.reading = true;
        drop(state);
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = state.lock().unwrap();
            let start = Instant::now();
            let guard = CGROUP_MEMORY_ACQ.acquire();
            match state.sample(Path::new(ROOT)) {
                Ok(count) => {
                    if count > 0 || state.slots.is_empty() {
                        guard.finish();
                    }
                    debug!(
                        "{NAME}: {count} cgroups sampled in {} us",
                        start.elapsed().as_micros()
                    );
                }
                Err(e) => debug!("{NAME}: sweep failed: {e}"),
            }
            state.last_read = Some(Instant::now());
            state.reading = false;
        });
    }
}

impl State {
    fn sample(&mut self, root: &Path) -> std::io::Result<usize> {
        // Failure to access the mount must not stamp a successful acquisition.
        std::fs::read_dir(root)?;
        let paths: HashSet<PathBuf> = WalkDir::new(root)
            .min_depth(1)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_dir())
            .map(|entry| entry.into_path())
            .filter(|path| path.join("memory.current").exists())
            .collect();

        self.slots.retain(|path, slot| {
            if paths.contains(path) {
                return true;
            }
            clear_slot(*slot);
            false
        });
        let used: HashSet<usize> = self.slots.values().copied().collect();
        let mut free = (0..MAX_CGROUPS).filter(|slot| !used.contains(slot));
        let mut count = 0;
        for path in paths {
            let slot = if let Some(slot) = self.slots.get(&path) {
                *slot
            } else if let Some(slot) = free.next() {
                self.slots.insert(path.clone(), slot);
                let name = format!("/{}", path.strip_prefix(root).unwrap().to_string_lossy());
                label_slot(slot, &name);
                slot
            } else {
                if !self.warned_capacity {
                    warn!("{NAME}: more than {MAX_CGROUPS} cgroups; excess groups are omitted");
                    self.warned_capacity = true;
                }
                continue;
            };
            // Missing/failed reads are absent, never zero or a previous value
            // wearing the new sweep's window. Optional fields vary by kernel.
            let total = read_bytes(&path.join("memory.current"));
            let swap = read_bytes(&path.join("memory.swap.current"));
            set_optional(&CGROUP_MEMORY_USAGE, slot, total);
            set_optional(&CGROUP_MEMORY_SWAP_USAGE, slot, swap);
            let values = std::fs::read_to_string(path.join("memory.stat"))
                .map(|data| parse_stat(&data))
                .unwrap_or([None; KINDS.len()]);
            for (kind, value) in values.into_iter().enumerate() {
                set_optional(CGROUP_MEMORY_STAT[kind], slot, value);
            }
            count += usize::from(
                total.is_some() || swap.is_some() || values.iter().any(Option::is_some),
            );
        }
        Ok(count)
    }
}

fn read_bytes(path: &Path) -> Option<i64> {
    parse_bytes(std::fs::read_to_string(path).ok()?.trim())
}

fn parse_bytes(value: &str) -> Option<i64> {
    value
        .parse::<u64>()
        .ok()
        .and_then(|v| i64::try_from(v).ok())
}

fn parse_stat(data: &str) -> [Option<i64>; KINDS.len()] {
    let mut values = [None; KINDS.len()];
    for line in data.lines() {
        let mut fields = line.split_whitespace();
        if let (Some(key), Some(value), None) = (fields.next(), fields.next(), fields.next()) {
            if let Some(idx) = KINDS.iter().position(|kind| *kind == key) {
                values[idx] = parse_bytes(value);
            }
        }
    }
    values
}

fn set_optional(metric: &GaugeGroup, slot: usize, value: Option<i64>) {
    // GaugeGroup's unset sentinel, returned by value() as None.
    metric.set(slot, value.unwrap_or(i64::MIN));
}

fn label_slot(slot: usize, name: &str) {
    for metric in [&CGROUP_MEMORY_USAGE, &CGROUP_MEMORY_SWAP_USAGE] {
        metric.insert_metadata(slot, "name".into(), name.into());
    }
    for metric in CGROUP_MEMORY_STAT {
        metric.insert_metadata(slot, "name".into(), name.into());
    }
}

fn clear_slot(slot: usize) {
    for metric in [&CGROUP_MEMORY_USAGE, &CGROUP_MEMORY_SWAP_USAGE] {
        set_optional(metric, slot, None);
        metric.clear_metadata(slot);
    }
    for metric in CGROUP_MEMORY_STAT {
        set_optional(metric, slot, None);
        metric.clear_metadata(slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(state: &mut State, root: &Path) -> std::io::Result<usize> {
        let guard = CGROUP_MEMORY_ACQ.acquire();
        let result = state.sample(root);
        if result.is_ok() {
            guard.finish();
        }
        result
    }

    #[test]
    #[ignore = "creates 4097 cgroupfs fixtures to measure a capacity-sized sweep"]
    fn capacity_sweep_cost() {
        let root = tempfile::tempdir().unwrap();
        let stat = KINDS
            .iter()
            .map(|kind| format!("{kind} 4096\n"))
            .collect::<String>();
        for i in 0..=MAX_CGROUPS {
            let path = root.path().join(format!("group-{i}"));
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("memory.current"), "8192").unwrap();
            std::fs::write(path.join("memory.swap.current"), "0").unwrap();
            std::fs::write(path.join("memory.stat"), &stat).unwrap();
        }
        let mut state = State::default();
        for _ in 0..5 {
            let start = Instant::now();
            assert_eq!(sample(&mut state, root.path()).unwrap(), MAX_CGROUPS);
            eprintln!(
                "fixture: {MAX_CGROUPS} cgroups sampled in {} us",
                start.elapsed().as_micros()
            );
        }
        assert_eq!(state.slots.len(), MAX_CGROUPS);
        assert!(state.warned_capacity);
        assert_eq!(CGROUP_MEMORY_USAGE.metadata_snapshot().len(), MAX_CGROUPS);
        let guard = CGROUP_MEMORY_ACQ.acquire();
        for slot in state.slots.values() {
            clear_slot(*slot);
        }
        guard.finish();
    }

    #[test]
    fn stat_parses_byte_gauges_by_key_and_ignores_events() {
        let values = parse_stat("file 4096\nanon 8192\npgfault 73\nfuture_field 99\nsock 0\nslab -1\nshmem invalid\nkernel 18446744073709551615\nfile_dirty 12 extra\n");
        let get = |key| values[KINDS.iter().position(|k| *k == key).unwrap()];
        assert_eq!(get("anon"), Some(8192));
        assert_eq!(get("file"), Some(4096));
        assert_eq!(get("sock"), Some(0));
        for key in ["slab", "shmem", "kernel", "file_dirty", "pagetables"] {
            assert_eq!(get(key), None, "{key}");
        }
        assert_eq!(values.iter().flatten().count(), 3);
    }

    // One test owns the process-global metric groups, so lifecycle operations
    // cannot race another test writing these statics.
    #[test]
    fn discovers_nested_cgroups_and_clears_missing_values_and_deleted_groups() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let parent = root.join("service.slice");
        let child = parent.join("worker.service");
        std::fs::create_dir_all(&child).unwrap();
        for (path, total, anon) in [(&parent, "16384", "8192"), (&child, "8192", "4096")] {
            std::fs::write(path.join("memory.current"), total).unwrap();
            std::fs::write(
                path.join("memory.stat"),
                format!("anon {anon}\nfile 4096\n"),
            )
            .unwrap();
        }
        std::fs::write(child.join("memory.swap.current"), "1024\n").unwrap();
        // The hierarchy root is intentionally not exported, even in fixtures
        // that provide memory.current there.
        std::fs::write(root.join("memory.current"), "32768").unwrap();
        let mut state = State::default();
        assert_eq!(sample(&mut state, root).unwrap(), 2);
        let slot = state.slots[&child];
        assert_eq!(CGROUP_MEMORY_USAGE.value(slot), Some(8192));
        assert_eq!(CGROUP_MEMORY_SWAP_USAGE.value(slot), Some(1024));
        assert_eq!(
            CGROUP_MEMORY_USAGE.load_metadata(slot).unwrap()["name"],
            "/service.slice/worker.service"
        );
        let anon = slot;
        assert_eq!(CGROUP_MEMORY_STAT[0].value(anon), Some(4096));
        assert_eq!(CGROUP_MEMORY_USAGE.value(state.slots[&parent]), Some(16384));

        // All thirty columns must join to the same two cgroup occupants in
        // the long stream. In particular, kinds belong to columns, not slots.
        use metriken_exposition::group_builder::StreamGroup;
        let groups = crate::agent::exposition::http::test_stream_groups();
        let group = groups
            .into_iter()
            .find_map(|g| match g {
                StreamGroup::Long(g) if g.name == "memory_cgroup/memory_cgroup_sweep" => Some(g),
                _ => None,
            })
            .unwrap();
        assert_eq!(group.occupants.len(), 2);
        assert_eq!(group.columns.gauges.len(), 2 + KINDS.len());
        let child_values = &group
            .occupants
            .iter()
            .find(|o| o.labels["name"] == "/service.slice/worker.service")
            .unwrap()
            .gauges;
        for (column, value) in group.columns.gauges.iter().zip(child_values) {
            let want = match column.metadata["metric"].as_str() {
                "cgroup_memory_usage" => Some(8192),
                "cgroup_memory_swap_usage" => Some(1024),
                "cgroup_memory_stat"
                    if matches!(column.metadata["kind"].as_str(), "anon" | "file") =>
                {
                    Some(4096)
                }
                "cgroup_memory_stat" => None,
                metric => panic!("unexpected column: {metric}"),
            };
            assert_eq!(*value, want, "{column:?}");
        }

        std::fs::remove_file(child.join("memory.swap.current")).unwrap();
        std::fs::write(child.join("memory.stat"), "file 0\n").unwrap();
        sample(&mut state, root).unwrap();
        assert_eq!(CGROUP_MEMORY_SWAP_USAGE.value(slot), None);
        assert_eq!(CGROUP_MEMORY_STAT[0].value(anon), None);
        assert_eq!(CGROUP_MEMORY_STAT[1].value(anon), Some(0));

        std::fs::remove_dir_all(&child).unwrap();
        sample(&mut state, root).unwrap();
        assert_eq!(CGROUP_MEMORY_USAGE.value(slot), None);
        assert!(CGROUP_MEMORY_USAGE.load_metadata(slot).is_none());
        assert!(CGROUP_MEMORY_STAT[0].load_metadata(anon).is_none());

        let replacement = parent.join("replacement.service");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(replacement.join("memory.current"), "128").unwrap();
        sample(&mut state, root).unwrap();
        assert_eq!(state.slots[&replacement], slot);
        assert_eq!(CGROUP_MEMORY_USAGE.value(slot), Some(128));
        assert_eq!(CGROUP_MEMORY_STAT[0].value(anon), None);
        assert_eq!(
            CGROUP_MEMORY_USAGE.load_metadata(slot).unwrap()["name"],
            "/service.slice/replacement.service"
        );
        assert!(sample(&mut state, &root.join("missing")).is_err());
        std::fs::remove_dir_all(parent).unwrap();
        assert_eq!(sample(&mut state, root).unwrap(), 0);
        assert!(state.slots.is_empty());
    }
}
