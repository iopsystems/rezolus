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

use crate::agent::samplers::blocking::{recover_lock, BlockingWorker, Cadence};
use crate::agent::*;
use anyhow::Context;
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
    if !memory_enabled_for_children(Path::new(ROOT))? {
        warn!(
            "{NAME}: memory is not enabled in {ROOT}/cgroup.subtree_control; \
             child cgroup memory metrics will be empty until it is enabled"
        );
    }
    let interval = config.sampler_interval(NAME).unwrap_or(DEFAULT_INTERVAL);
    anyhow::ensure!(
        !interval.is_zero(),
        "{NAME} interval must be greater than zero"
    );
    Ok(Some(Box::new(CgroupMemory {
        state: Arc::new(Mutex::new(State::default())),
        worker: BlockingWorker::new(NAME, interval, Cadence::FromCompletion),
    })))
}

fn memory_enabled_for_children(root: &Path) -> anyhow::Result<bool> {
    let has_memory = |file: &str| -> std::io::Result<bool> {
        let contents = std::fs::read_to_string(root.join(file))?;
        Ok(contents.split_whitespace().any(|c| c == "memory"))
    };
    // No cgroup v2 hierarchy at the root, or no memory controller in it, is a
    // host this sampler cannot serve rather than a failure: `rezolus status`
    // must not report a v1 or hybrid host as broken.
    let unsupported =
        |reason: String| anyhow::Error::from(crate::agent::sampler_status::Unsupported(reason));
    let controllers = root.join("cgroup.controllers");
    let available = has_memory("cgroup.controllers").map_err(|e| {
        unsupported(format!(
            "no cgroup v2 hierarchy: reading {}: {e}",
            controllers.display()
        ))
    })?;
    if !available {
        return Err(unsupported(format!(
            "cgroup v2 memory controller is unavailable at {}",
            root.display()
        )));
    }
    let subtree_control = root.join("cgroup.subtree_control");
    has_memory("cgroup.subtree_control").with_context(|| {
        format!(
            "reading {} for cgroup v2 memory accounting",
            subtree_control.display()
        )
    })
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct CgroupMemory {
    state: Arc<Mutex<State>>,
    worker: BlockingWorker,
}

#[derive(Default)]
struct State {
    slots: HashMap<PathBuf, usize>,
    warned_capacity: bool,
}

#[async_trait]
impl Sampler for CgroupMemory {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        self.dispatch(|state| state.sweep(Path::new(ROOT)));
    }
}

impl CgroupMemory {
    fn dispatch(
        &self,
        read: impl FnOnce(&mut State) -> std::io::Result<usize> + Send + 'static,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let permit = self.worker.try_acquire()?;
        let state = self.state.clone();
        Some(permit.spawn(move || {
            let start = Instant::now();
            match read(&mut recover_lock(NAME, &state)) {
                Ok(count) => debug!(
                    "{NAME}: {count} cgroups sampled in {} us",
                    start.elapsed().as_micros()
                ),
                Err(e) => warn!(
                    "{NAME}: sweep failed: {e}; retaining the previous observation and \
                     retrying after the configured interval"
                ),
            }
        }))
    }
}

impl State {
    fn sweep(&mut self, root: &Path) -> std::io::Result<usize> {
        let guard = CGROUP_MEMORY_ACQ.acquire_blocking();
        let result = self.sample(root);
        if result.is_ok() {
            guard.finish();
        }
        result
    }

    fn sample(&mut self, root: &Path) -> std::io::Result<usize> {
        // Failure to access the mount must not stamp a successful acquisition.
        std::fs::read_dir(root).map_err(|e| path_error(root, e))?;
        let mut paths = HashSet::new();
        for entry in WalkDir::new(root).min_depth(1) {
            let entry = match entry {
                Ok(entry) => entry,
                // A cgroup can disappear during discovery.
                Err(e)
                    if e.io_error()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    continue
                }
                Err(e) => return Err(std::io::Error::other(e)),
            };
            if entry.file_type().is_dir() {
                let probe = entry.path().join("memory.current");
                if probe.try_exists().map_err(|e| path_error(&probe, e))? {
                    paths.insert(entry.into_path());
                }
            }
        }

        // Stage at most the active capacity before changing any published
        // value or label. Total read failure must leave the previous window's
        // values intact, including groups that disappeared during this sweep.
        let retained = self
            .slots
            .keys()
            .filter(|path| paths.contains(*path))
            .count();
        let targets = paths
            .iter()
            .filter(|path| self.slots.contains_key(*path))
            .chain(
                paths
                    .iter()
                    .filter(|path| !self.slots.contains_key(*path))
                    .take(MAX_CGROUPS - retained),
            );
        let readings: Vec<_> = targets
            .map(|path| {
                let total = read_bytes(&path.join("memory.current"));
                let swap = read_bytes(&path.join("memory.swap.current"));
                let values = std::fs::read_to_string(path.join("memory.stat"))
                    .map(|data| parse_stat(&data))
                    .unwrap_or([None; KINDS.len()]);
                (path, total, swap, values)
            })
            .collect();
        let count = readings
            .iter()
            .filter(|(_, total, swap, values)| {
                total.is_some() || swap.is_some() || values.iter().any(Option::is_some)
            })
            .count();
        if count == 0 && !paths.is_empty() {
            return Err(std::io::Error::other("all cgroup memory reads failed"));
        }
        if paths.len() > MAX_CGROUPS && !self.warned_capacity {
            warn!("{NAME}: more than {MAX_CGROUPS} cgroups; excess groups are omitted");
            self.warned_capacity = true;
        }
        self.slots.retain(|path, slot| {
            if paths.contains(path) {
                return true;
            }
            clear_slot(*slot);
            false
        });
        let used: HashSet<usize> = self.slots.values().copied().collect();
        let mut free = (0..MAX_CGROUPS).filter(|slot| !used.contains(slot));
        for (path, total, swap, values) in readings {
            let slot = if let Some(slot) = self.slots.get(path) {
                *slot
            } else if total.is_none() && swap.is_none() && values.iter().all(Option::is_none) {
                // Discovered, then gone before it could be read: no slot and
                // no labels, so the stream never carries a phantom occupant.
                continue;
            } else if let Some(slot) = free.next() {
                self.slots.insert(path.clone(), slot);
                label_slot(slot, path.strip_prefix(root).unwrap());
                slot
            } else {
                unreachable!("staged readings fit the active slot capacity");
            };
            // Missing/failed reads are absent, never zero or a previous value
            // wearing the new sweep's window. Optional fields vary by kernel.
            set_optional(&CGROUP_MEMORY_USAGE, slot, total);
            set_optional(&CGROUP_MEMORY_SWAP_USAGE, slot, swap);
            for (kind, value) in values.into_iter().enumerate() {
                set_optional(CGROUP_MEMORY_STAT[kind], slot, value);
            }
        }
        Ok(count)
    }
}

fn path_error(path: &Path, error: std::io::Error) -> std::io::Error {
    std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
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

/// Match BPF's last-three-component display name (63 bytes per component,
/// including its systemd \x2d decoding). The full path remains a separate label.
fn memory_name(relative: &Path) -> String {
    let parts: Vec<_> = relative
        .components()
        .map(|c| c.as_os_str().as_encoded_bytes())
        .collect();
    let part = |back: usize| {
        parts
            .len()
            .checked_sub(back + 1)
            .map(|i| &parts[i][..parts[i].len().min(63)])
            .unwrap_or(&[])
    };
    crate::agent::bpf::cgroup_name_label(part(0), part(1), part(2), parts.len() as i32)
}

fn label_slot(slot: usize, relative: &Path) {
    let metadata = HashMap::from([
        ("name".into(), memory_name(relative)),
        ("path".into(), format!("/{}", relative.to_string_lossy())),
    ]);
    for metric in [&CGROUP_MEMORY_USAGE, &CGROUP_MEMORY_SWAP_USAGE]
        .into_iter()
        .chain(CGROUP_MEMORY_STAT.iter().copied())
    {
        metric.set_metadata(slot, metadata.clone());
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
        state.sweep(root)
    }

    #[test]
    fn controller_diagnostics_distinguish_availability_and_child_enablement() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        use crate::agent::sampler_status::Unsupported;
        // No hierarchy and no memory controller are `Unsupported`, which the
        // agent records as such rather than as a failed sampler.
        let error = memory_enabled_for_children(root).unwrap_err();
        assert!(error
            .to_string()
            .contains(&root.join("cgroup.controllers").display().to_string()));
        assert!(error.downcast_ref::<Unsupported>().is_some());
        std::fs::write(root.join("cgroup.controllers"), "cpu io\n").unwrap();
        let error = memory_enabled_for_children(root).unwrap_err();
        assert!(error
            .to_string()
            .contains("memory controller is unavailable"));
        assert!(error.downcast_ref::<Unsupported>().is_some());
        // A v2 hierarchy with the controller but an unreadable subtree file is
        // a real failure.
        std::fs::write(root.join("cgroup.controllers"), "cpu memory io\n").unwrap();
        let error = memory_enabled_for_children(root).unwrap_err();
        assert!(error.to_string().contains("cgroup.subtree_control"));
        assert!(error.downcast_ref::<Unsupported>().is_none());
        std::fs::write(root.join("cgroup.subtree_control"), "cpu\n").unwrap();
        assert!(!memory_enabled_for_children(root).unwrap());
        // Enabling accounting later needs no change to controller availability.
        std::fs::write(root.join("cgroup.subtree_control"), "cpu memory\n").unwrap();
        assert!(memory_enabled_for_children(root).unwrap());
    }

    #[test]
    #[ignore = "creates 4097 cgroupfs fixtures to measure a capacity-sized sweep"]
    fn capacity_sweep_cost() {
        let _registry = crate::agent::exposition::http::test_registry();
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
        let guard = CGROUP_MEMORY_ACQ.acquire_blocking();
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

    #[test]
    fn names_match_bpf_depth_escaping_and_component_limits() {
        for (path, expected) in [
            ("one", "/one"),
            ("one/two", "/one/two"),
            ("one/two/three", "/one/two/three"),
            ("one/two/three/four", ".../two/three/four"),
            (r"a/b/c/foo\x2dbar.service", ".../b/c/foo-bar.service"),
        ] {
            assert_eq!(memory_name(Path::new(path)), expected);
        }
        let long = "x".repeat(90);
        assert_eq!(
            memory_name(Path::new(&long)),
            format!("/{}", "x".repeat(63))
        );
    }

    #[test]
    fn failed_sweep_preserves_values_membership_and_window_until_recovery() {
        let _registry = crate::agent::exposition::http::test_registry();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let existing = root.join("existing");
        let vanished = root.join("vanished");
        for path in [&existing, &vanished] {
            std::fs::create_dir(path).unwrap();
            std::fs::write(path.join("memory.current"), "4096").unwrap();
        }
        let mut state = State::default();
        assert_eq!(state.sweep(root).unwrap(), 2);
        let slots = state.slots.clone();
        let window = CGROUP_MEMORY_ACQ.window().unwrap();
        let metadata = CGROUP_MEMORY_USAGE.metadata_snapshot();
        // A broken subtree must report its path and preserve every published
        // group. A symlink loop injects a probe error even when tests run as root.
        let broken = root.join("broken");
        std::fs::create_dir(&broken).unwrap();
        let probe = broken.join("memory.current");
        std::os::unix::fs::symlink("memory.current", &probe).unwrap();
        std::fs::write(existing.join("memory.current"), "8192").unwrap();
        let error = state.sweep(root).unwrap_err();
        assert!(error.to_string().contains(&probe.display().to_string()));
        assert_eq!(state.slots, slots);
        assert_eq!(CGROUP_MEMORY_ACQ.window(), Some(window));
        assert_eq!(CGROUP_MEMORY_USAGE.metadata_snapshot(), metadata);
        for slot in slots.values() {
            assert_eq!(CGROUP_MEMORY_USAGE.value(*slot), Some(4096));
        }
        std::fs::remove_dir_all(&broken).unwrap();
        std::fs::remove_dir_all(&vanished).unwrap();
        std::fs::write(existing.join("memory.current"), "invalid").unwrap();
        let added = root.join("added");
        std::fs::create_dir(&added).unwrap();
        std::fs::write(added.join("memory.current"), "invalid").unwrap();
        assert!(state.sweep(root).is_err());
        assert_eq!(state.slots, slots);
        assert_eq!(CGROUP_MEMORY_ACQ.window(), Some(window));
        assert_eq!(CGROUP_MEMORY_USAGE.metadata_snapshot(), metadata);
        for slot in slots.values() {
            assert_eq!(CGROUP_MEMORY_USAGE.value(*slot), Some(4096));
        }
        // A successful sweep publishes both the new values and membership.
        std::fs::write(existing.join("memory.current"), "8192").unwrap();
        assert_eq!(state.sweep(root).unwrap(), 1);
        assert!(CGROUP_MEMORY_ACQ.window().unwrap().end_ns > window.end_ns);
        assert!(!state.slots.contains_key(&vanished));
        assert_eq!(
            CGROUP_MEMORY_USAGE.value(state.slots[&existing]),
            Some(8192)
        );
        // A path whose reads all failed, such as a cgroup deleted between
        // discovery and the read, gets no slot and no labels.
        assert!(!state.slots.contains_key(&added));
        assert_eq!(CGROUP_MEMORY_USAGE.metadata_snapshot().len(), 1);
        std::fs::remove_dir_all(&existing).unwrap();
        std::fs::remove_dir_all(&added).unwrap();
        assert_eq!(state.sweep(root).unwrap(), 0);
    }

    // Hold the registry lock across both mutations and stream assertions.
    #[test]
    fn discovers_nested_cgroups_and_clears_missing_values_and_deleted_groups() {
        let registry = crate::agent::exposition::http::test_registry();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let parent = root.join("outer.slice/user.slice/service.slice");
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
            ".../user.slice/service.slice/worker.service"
        );
        assert_eq!(
            CGROUP_MEMORY_USAGE.load_metadata(slot).unwrap()["path"],
            "/outer.slice/user.slice/service.slice/worker.service"
        );
        let anon = slot;
        assert_eq!(CGROUP_MEMORY_STAT[0].value(anon), Some(4096));
        assert_eq!(CGROUP_MEMORY_USAGE.value(state.slots[&parent]), Some(16384));

        // All thirty columns must join to the same two cgroup occupants in
        // the long stream. In particular, kinds belong to columns, not slots.
        use metriken_exposition::group_builder::StreamGroup;
        let groups = registry.stream_groups();
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
            .find(|o| o.labels["name"] == ".../user.slice/service.slice/worker.service")
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
            ".../user.slice/service.slice/replacement.service"
        );
        assert!(sample(&mut state, &root.join("missing")).is_err());
        std::fs::remove_dir_all(parent).unwrap();
        assert_eq!(sample(&mut state, root).unwrap(), 0);
        assert!(state.slots.is_empty());
    }
}
