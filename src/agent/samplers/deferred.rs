//! Samplers whose kernel module loads after the agent starts.
//!
//! ext4 and jbd2 are modules on most distribution kernels and load at the
//! first ext4 mount. On a host whose root filesystem is not ext4 (Amazon
//! Linux 2023 and RHEL use XFS) that mount often comes after the agent has
//! started at boot. Initialising then finds neither the module's tracepoints
//! nor its BTF: `ext4_ops` reports unsupported, and `ext4_alloc` and
//! `ext4_journal`, which fall back to `raw_tp` when the BTF is missing, fail
//! to load with EINVAL because their CO-RE reads of ext4 and jbd2 types have
//! nothing to relocate against. Init is never retried, so the host records
//! nothing for them until the agent is restarted.
//!
//! [`until_loaded`] runs a sampler's init at once when its modules are built
//! in or loaded. Otherwise it reports the sampler unsupported, naming the
//! module it waits for, and returns a [`Deferred`] sampler that checks every
//! [`CHECK_INTERVAL`] and runs the init on its own thread once they load.

use super::{Sampler, SamplerResult, ACQUISITION_GROUPS};
use crate::agent::Config;
use crate::*;

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How often a waiting sampler looks for its modules: a few `stat` calls.
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Where module state is read from. Separate from the real paths so tests can
/// build a fake tree.
#[derive(Clone, Debug)]
struct ModuleTree {
    /// `/sys`: `module/<name>/initstate` for a loaded module.
    sys: PathBuf,
    /// `/lib/modules/<release>`: `modules.builtin` lists built-in modules.
    modules: PathBuf,
}

impl ModuleTree {
    fn running_kernel() -> Self {
        let release = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|r| r.trim().to_string())
            .unwrap_or_default();

        Self {
            sys: PathBuf::from("/sys"),
            modules: Path::new("/lib/modules").join(release),
        }
    }

    /// The module is loaded: a loadable module has `initstate` once loaded.
    fn loaded(&self, name: &str) -> bool {
        std::fs::read_to_string(self.sys.join("module").join(name).join("initstate"))
            .map(|s| s.trim() == "live")
            .unwrap_or(false)
    }

    /// The module is built in, or this cannot be told: `None` when
    /// `modules.builtin` is unreadable (a container without `/lib/modules`).
    fn builtin(&self, name: &str) -> Option<bool> {
        let list = std::fs::read_to_string(self.modules.join("modules.builtin")).ok()?;
        let file = format!("/{name}.ko");

        Some(list.lines().any(|line| line.trim_end().ends_with(&file)))
    }

    /// Modules from `names` that are neither loaded nor built in. Empty when
    /// all are present, or when built-in status cannot be read, in which case
    /// the caller initialises at once as it did before.
    fn waiting_for(&self, names: &[&'static str]) -> Vec<&'static str> {
        names
            .iter()
            .copied()
            .filter(|name| !self.loaded(name) && self.builtin(name) == Some(false))
            .collect()
    }
}

/// Initialise `name` with `init` now if `modules` are present, or return a
/// sampler that initialises it once they are. `init` must do its own
/// `config.enabled()` check; it runs exactly once either way.
pub(crate) fn until_loaded(
    name: &'static str,
    modules: &'static [&'static str],
    config: Arc<Config>,
    init: fn(Arc<Config>) -> SamplerResult,
) -> SamplerResult {
    until_loaded_in(
        ModuleTree::running_kernel(),
        CHECK_INTERVAL,
        name,
        modules,
        config,
        init,
    )
}

fn until_loaded_in(
    tree: ModuleTree,
    interval: Duration,
    name: &'static str,
    modules: &'static [&'static str],
    config: Arc<Config>,
    init: fn(Arc<Config>) -> SamplerResult,
) -> SamplerResult {
    let waiting = tree.waiting_for(modules);

    if waiting.is_empty() {
        return init(config);
    }

    info!(
        "{name}: kernel module {} not loaded; starting when it is",
        waiting.join(", ")
    );

    crate::agent::sampler_status::set_unsupported(name, waiting_reason(&waiting));

    // Until init runs, these groups have no members, as for a sampler that
    // did not start (`bound_groups_without_a_live_sampler`).
    for group in ACQUISITION_GROUPS.iter().filter(|g| g.sampler == name) {
        group.set_member_bound(0);
    }

    Ok(Some(Box::new(Deferred {
        name,
        modules,
        tree,
        config,
        init,
        shared: Arc::new(Shared::default()),
        interval,
        next_check: Mutex::new(Instant::now() + interval),
    })))
}

fn waiting_reason(waiting: &[&str]) -> String {
    format!(
        "waiting for kernel module {} to load; starts when it does",
        waiting.join(", ")
    )
}

#[derive(Default)]
struct Shared {
    /// The real sampler, once init succeeded.
    inner: OnceLock<Box<dyn Sampler>>,
    /// Init has been started (successfully or not); never started twice.
    started: AtomicBool,
}

/// A sampler waiting for its kernel modules. Refreshes are no-ops until the
/// modules load and init succeeds, then forward to the real sampler.
pub(crate) struct Deferred {
    name: &'static str,
    modules: &'static [&'static str],
    tree: ModuleTree,
    config: Arc<Config>,
    init: fn(Arc<Config>) -> SamplerResult,
    shared: Arc<Shared>,
    interval: Duration,
    next_check: Mutex<Instant>,
}

impl Deferred {
    /// Start init on its own thread if the modules have loaded. Loading the
    /// BPF programs takes hundreds of milliseconds, which a snapshot should
    /// not wait for.
    fn check(&self) {
        if self.shared.started.load(Ordering::Acquire) {
            return;
        }

        {
            let mut next = self.next_check.lock().unwrap();
            let now = Instant::now();
            if now < *next {
                return;
            }
            *next = now + self.interval;
        }

        let waiting = self.tree.waiting_for(self.modules);
        if !waiting.is_empty() {
            crate::agent::sampler_status::set_unsupported(self.name, waiting_reason(&waiting));
            return;
        }

        if self.shared.started.swap(true, Ordering::AcqRel) {
            return;
        }

        info!("{}: kernel modules loaded; initialising", self.name);

        let (name, init, config, shared) = (
            self.name,
            self.init,
            self.config.clone(),
            self.shared.clone(),
        );

        let spawned = std::thread::Builder::new()
            .name(format!("{name}-init"))
            .spawn(move || start(name, init, config, &shared));

        if let Err(e) = spawned {
            crate::agent::sampler_status::set_failed(
                name,
                format!("could not start the init thread: {e}"),
            );
        }
    }
}

/// Run `init` and record the outcome as the agent's init loop does.
fn start(
    name: &'static str,
    init: fn(Arc<Config>) -> SamplerResult,
    config: Arc<Config>,
    shared: &Shared,
) {
    // Lift the bound set while waiting before init runs, so a bound init
    // sets itself is the one that stands.
    for group in ACQUISITION_GROUPS.iter().filter(|g| g.sampler == name) {
        group.set_member_bound(usize::MAX);
    }
    crate::agent::sampler_status::clear(name);

    match init(config) {
        Ok(Some(sampler)) => {
            crate::agent::sampler_status::set_active_if_absent(name);
            let _ = shared.inner.set(sampler);
            info!("{name}: started after its kernel module loaded");
        }
        Ok(None) => crate::agent::sampler_status::set_disabled(name),
        Err(e) => {
            for group in ACQUISITION_GROUPS.iter().filter(|g| g.sampler == name) {
                group.set_member_bound(0);
            }
            match e.downcast_ref::<crate::agent::sampler_status::Unsupported>() {
                Some(u) => crate::agent::sampler_status::set_unsupported(name, u.0.clone()),
                None => crate::agent::sampler_status::set_failed(name, e.to_string()),
            }
        }
    }
}

#[async_trait]
impl Sampler for Deferred {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn refresh(&self) {
        match self.shared.inner.get() {
            Some(inner) => inner.refresh().await,
            None => self.check(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(loaded: &[&str], builtin: Option<&[&str]>) -> (tempfile::TempDir, ModuleTree) {
        let dir = tempfile::tempdir().unwrap();
        let sys = dir.path().join("sys");
        let modules = dir.path().join("lib/modules/6.18.0");

        for name in loaded {
            let m = sys.join("module").join(name);
            std::fs::create_dir_all(&m).unwrap();
            std::fs::write(m.join("initstate"), "live\n").unwrap();
        }

        if let Some(builtin) = builtin {
            std::fs::create_dir_all(&modules).unwrap();
            let list: String = builtin
                .iter()
                .map(|n| format!("kernel/fs/{n}/{n}.ko\n"))
                .collect();
            std::fs::write(modules.join("modules.builtin"), list).unwrap();
        }

        (dir, ModuleTree { sys, modules })
    }

    #[test]
    fn a_loadable_module_not_yet_loaded_is_waited_for() {
        let (_d, t) = tree(&[], Some(&["xfs"]));
        assert_eq!(t.waiting_for(&["ext4", "jbd2"]), vec!["ext4", "jbd2"]);
    }

    #[test]
    fn a_loaded_module_is_not_waited_for() {
        let (_d, t) = tree(&["ext4"], Some(&[]));
        assert_eq!(t.waiting_for(&["ext4", "jbd2"]), vec!["jbd2"]);
    }

    #[test]
    fn a_builtin_module_is_not_waited_for() {
        let (_d, t) = tree(&[], Some(&["ext4", "jbd2"]));
        assert!(t.waiting_for(&["ext4", "jbd2"]).is_empty());
    }

    /// No `modules.builtin` (a container without `/lib/modules`): nothing to
    /// go on, so init runs at once as before rather than waiting forever.
    #[test]
    fn unknown_builtin_status_does_not_wait() {
        let (_d, t) = tree(&[], None);
        assert!(t.waiting_for(&["ext4"]).is_empty());
    }

    /// A module whose initstate is not `live` (still `coming`, or `going`)
    /// is not loaded yet.
    #[test]
    fn a_module_still_loading_is_waited_for() {
        let (_d, t) = tree(&["ext4"], Some(&[]));
        std::fs::write(t.sys.join("module/ext4/initstate"), "coming\n").unwrap();
        assert_eq!(t.waiting_for(&["ext4"]), vec!["ext4"]);
    }

    /// The match is on the whole module file name: `jbd2` must not be found
    /// by a built-in `xjbd2.ko`.
    #[test]
    fn builtin_matches_the_whole_file_name() {
        let (_d, t) = tree(&[], Some(&["xjbd2"]));
        assert_eq!(t.waiting_for(&["jbd2"]), vec!["jbd2"]);
    }

    // Lifecycle tests. The status registry is process-global, so each test
    // uses its own sampler name and its own counters.

    use crate::agent::sampler_status::{snapshot, SamplerState};
    use std::sync::atomic::AtomicUsize;

    fn state(name: &str) -> Option<SamplerState> {
        snapshot()
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.state)
    }

    fn test_config() -> Arc<Config> {
        Arc::new(toml::from_str("").expect("empty config parses"))
    }

    struct Fake(&'static AtomicUsize);

    #[async_trait]
    impl Sampler for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn refresh(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Poll until `f` holds or two seconds pass; init runs on its own thread.
    async fn eventually(f: impl Fn() -> bool) -> bool {
        for _ in 0..200 {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        f()
    }

    static WAIT_INITS: AtomicUsize = AtomicUsize::new(0);
    static WAIT_REFRESHES: AtomicUsize = AtomicUsize::new(0);

    fn wait_init(_: Arc<Config>) -> SamplerResult {
        WAIT_INITS.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(Fake(&WAIT_REFRESHES))))
    }

    #[tokio::test]
    async fn waits_then_initialises_once_and_forwards_refreshes() {
        const NAME: &str = "deferred_test_waits";
        let (_d, t) = tree(&[], Some(&[]));
        let sampler = until_loaded_in(
            t.clone(),
            Duration::ZERO,
            NAME,
            &["ext4"],
            test_config(),
            wait_init,
        )
        .unwrap()
        .unwrap();

        match state(NAME) {
            Some(SamplerState::Unsupported { reason }) => {
                assert!(reason.contains("ext4"), "{reason}")
            }
            other => panic!("expected unsupported while waiting, got {other:?}"),
        }

        sampler.refresh().await;
        sampler.refresh().await;
        assert_eq!(
            WAIT_INITS.load(Ordering::SeqCst),
            0,
            "no init before the module loads"
        );

        let m = t.sys.join("module/ext4");
        std::fs::create_dir_all(&m).unwrap();
        std::fs::write(m.join("initstate"), "live\n").unwrap();

        sampler.refresh().await;
        assert!(
            eventually(|| WAIT_INITS.load(Ordering::SeqCst) == 1).await,
            "init ran"
        );

        // Forwarding starts once the init thread has stored the sampler.
        assert!(
            eventually(|| {
                futures::executor::block_on(sampler.refresh());
                WAIT_REFRESHES.load(Ordering::SeqCst) > 0
            })
            .await,
            "refreshes reach the real sampler"
        );
        for _ in 0..5 {
            sampler.refresh().await;
        }
        assert_eq!(
            WAIT_INITS.load(Ordering::SeqCst),
            1,
            "init runs exactly once"
        );
        assert!(
            matches!(state(NAME), Some(SamplerState::Active)),
            "{:?}",
            state(NAME)
        );
    }

    static NOW_INITS: AtomicUsize = AtomicUsize::new(0);
    static NOW_REFRESHES: AtomicUsize = AtomicUsize::new(0);

    fn now_init(_: Arc<Config>) -> SamplerResult {
        NOW_INITS.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(Fake(&NOW_REFRESHES))))
    }

    #[tokio::test]
    async fn initialises_at_once_when_the_module_is_loaded() {
        let (_d, t) = tree(&["ext4"], Some(&[]));
        let sampler = until_loaded_in(
            t,
            Duration::ZERO,
            "deferred_test_now",
            &["ext4"],
            test_config(),
            now_init,
        )
        .unwrap()
        .unwrap();
        assert_eq!(NOW_INITS.load(Ordering::SeqCst), 1);
        sampler.refresh().await;
        assert_eq!(
            NOW_REFRESHES.load(Ordering::SeqCst),
            1,
            "the real sampler, not a wrapper"
        );
    }

    static FAIL_INITS: AtomicUsize = AtomicUsize::new(0);

    fn failing_init(_: Arc<Config>) -> SamplerResult {
        FAIL_INITS.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("Invalid argument (os error 22)"))
    }

    /// An init that fails after the module loads is recorded as failed and
    /// not retried every interval.
    #[tokio::test]
    async fn a_failed_init_after_loading_is_recorded_and_not_retried() {
        const NAME: &str = "deferred_test_fails";
        let (_d, t) = tree(&[], Some(&[]));
        let sampler = until_loaded_in(
            t.clone(),
            Duration::ZERO,
            NAME,
            &["ext4"],
            test_config(),
            failing_init,
        )
        .unwrap()
        .unwrap();

        let m = t.sys.join("module/ext4");
        std::fs::create_dir_all(&m).unwrap();
        std::fs::write(m.join("initstate"), "live\n").unwrap();

        sampler.refresh().await;
        assert!(
            eventually(|| matches!(state(NAME), Some(SamplerState::Failed { .. }))).await,
            "{:?}",
            state(NAME)
        );
        for _ in 0..5 {
            sampler.refresh().await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(FAIL_INITS.load(Ordering::SeqCst), 1);
    }
}
