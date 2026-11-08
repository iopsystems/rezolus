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
//! in or loaded. A module that is neither, and is not in `modules.dep`
//! either, will never load: the sampler is reported unsupported at once.
//! Otherwise it reports the sampler unsupported, naming the module it waits
//! for, starts a thread that checks every [`CHECK_INTERVAL`] and runs the
//! init once they load, and returns a [`Deferred`] sampler whose refreshes
//! do nothing until then and forward afterwards. Without a readable
//! `modules.builtin` (a container without the host's `/lib/modules`, NixOS)
//! init runs at once, as it did before samplers waited; with
//! `modules.builtin` but no readable `modules.dep`, a module that is not
//! built in is waited for.
//!
//! While it waits and while init runs, the sampler's acquisition groups are
//! bounded to zero members, as for a sampler that did not start; after init
//! succeeds, the ones init did not bound itself are unbounded again. A panic
//! in a deferred init ends the agent, as it does at start (the panic hook in
//! `main` exits the process).

use super::{Sampler, SamplerResult, ACQUISITION_GROUPS};
use crate::agent::timing::AcquisitionGroup;
use crate::agent::Config;
use crate::*;

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

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

    /// The running kernel can load the module: `modules.dep` lists every
    /// loadable module by path, compressed or not (`kernel/fs/xfs/xfs.ko`,
    /// `xfs.ko.xz`, `.zst`, `.gz`). `None` when `modules.dep` is unreadable.
    fn available(&self, name: &str) -> Option<bool> {
        let dep = std::fs::read_to_string(self.modules.join("modules.dep")).ok()?;
        let file = format!("/{name}.ko");

        Some(dep.lines().any(|line| {
            let path = line.split(':').next().unwrap_or("");
            path.ends_with(&file)
                || [".xz", ".zst", ".gz"]
                    .iter()
                    .any(|z| path.strip_suffix(z).is_some_and(|p| p.ends_with(&file)))
        }))
    }

    /// Modules from `names` that are not loaded, for the waiting thread.
    /// Unlike [`waiting_for`](Self::waiting_for), an unreadable
    /// `modules.builtin` counts as not built in: the thread only runs once a
    /// module was found missing, and a read that fails later (a package
    /// operation replacing `/lib/modules/<release>`) must not start init
    /// with the module still absent.
    fn still_waiting_for(&self, names: &[&'static str]) -> Vec<&'static str> {
        names
            .iter()
            .copied()
            .filter(|name| !self.loaded(name) && self.builtin(name) != Some(true))
            .collect()
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
/// sampler that initialises it once they are. `init` runs exactly once
/// either way.
pub(crate) fn until_loaded(
    name: &'static str,
    modules: &'static [&'static str],
    config: Arc<Config>,
    init: fn(Arc<Config>) -> SamplerResult,
) -> SamplerResult {
    let groups = ACQUISITION_GROUPS
        .iter()
        .copied()
        .filter(|g| g.sampler == name)
        .collect();

    until_loaded_in(
        ModuleTree::running_kernel(),
        CHECK_INTERVAL,
        name,
        modules,
        groups,
        config,
        init,
    )
}

/// A sampler's identity and init, carried to the watcher thread.
struct Pending {
    name: &'static str,
    modules: &'static [&'static str],
    groups: Vec<&'static AcquisitionGroup>,
    config: Arc<Config>,
    init: fn(Arc<Config>) -> SamplerResult,
}

fn until_loaded_in(
    tree: ModuleTree,
    interval: Duration,
    name: &'static str,
    modules: &'static [&'static str],
    groups: Vec<&'static AcquisitionGroup>,
    config: Arc<Config>,
    init: fn(Arc<Config>) -> SamplerResult,
) -> SamplerResult {
    let waiting = tree.waiting_for(modules);

    if waiting.is_empty() {
        return init(config);
    }

    // Neither built in nor shipped for this kernel: nothing will ever load
    // it, so say so rather than wait for the life of the agent. An
    // unreadable `modules.dep` waits, as before.
    let absent: Vec<&str> = waiting
        .iter()
        .copied()
        .filter(|m| tree.available(m) == Some(false))
        .collect();
    if !absent.is_empty() {
        return Err(crate::agent::sampler_status::Unsupported(format!(
            "this kernel has no {} module (not built in, not in modules.dep)",
            absent.join(", ")
        ))
        .into());
    }

    info!(
        "{name}: kernel module {} not loaded; starting when it is",
        waiting.join(", ")
    );

    crate::agent::sampler_status::set_unsupported(name, waiting_reason(&waiting));

    for group in &groups {
        group.set_member_bound(0);
    }

    let inner: Arc<OnceLock<Box<dyn Sampler>>> = Arc::new(OnceLock::new());
    let pending = Pending {
        name,
        modules,
        groups,
        config,
        init,
    };
    let slot = inner.clone();

    // A thread rather than a check on refresh: refreshes happen only when
    // something scrapes or subscribes, and a host with nothing attached
    // would otherwise never notice the module load.
    std::thread::Builder::new()
        .name(format!("{name}-wait"))
        .spawn(move || watch(tree, interval, pending, &slot))
        .map_err(|e| {
            anyhow::anyhow!("could not start the thread that waits for the module: {e}")
        })?;

    Ok(Some(Box::new(Deferred { name, inner })))
}

fn waiting_reason(waiting: &[&str]) -> String {
    format!(
        "waiting for kernel module {} to load; starts when it does",
        waiting.join(", ")
    )
}

/// Wait for the modules, then run init once and record the outcome as the
/// agent's init loop does.
fn watch(tree: ModuleTree, interval: Duration, p: Pending, slot: &OnceLock<Box<dyn Sampler>>) {
    let name = p.name;

    loop {
        std::thread::sleep(interval);
        let waiting = tree.still_waiting_for(p.modules);
        if waiting.is_empty() {
            break;
        }
        // The list can shrink (ext4 loaded, jbd2 not yet).
        crate::agent::sampler_status::set_unsupported(name, waiting_reason(&waiting));
    }

    info!("{name}: kernel modules loaded; initialising");

    match (p.init)(p.config) {
        Ok(Some(sampler)) => {
            // Groups init bounded itself keep that bound; the rest, still at
            // the zero set while waiting, are unbounded as at a normal start.
            for group in &p.groups {
                group.lift_zero_bound();
            }
            // Stored before the status says active, so a reader that sees
            // active also sees refreshes forwarded.
            let _ = slot.set(sampler);
            crate::agent::sampler_status::set_active_if_unsupported(name);
            info!("{name}: started after its kernel module loaded");
        }
        // Groups stay bounded to zero: nothing will fill them.
        Ok(None) => crate::agent::sampler_status::set_disabled(name),
        Err(e) => match e.downcast_ref::<crate::agent::sampler_status::Unsupported>() {
            Some(u) => crate::agent::sampler_status::set_unsupported(name, u.0.clone()),
            None => crate::agent::sampler_status::set_failed(name, e.to_string()),
        },
    }
}

/// A sampler waiting for its kernel modules. Refreshes do nothing until the
/// watcher thread has initialised the real sampler, then forward to it.
pub(crate) struct Deferred {
    name: &'static str,
    inner: Arc<OnceLock<Box<dyn Sampler>>>,
}

#[async_trait]
impl Sampler for Deferred {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn refresh(&self) {
        if let Some(inner) = self.inner.get() {
            inner.refresh().await;
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

    fn write_dep(t: &ModuleTree, paths: &[&str]) {
        let list: String = paths
            .iter()
            .map(|p| format!("{p}: kernel/lib/libcrc32c.ko\n"))
            .collect();
        std::fs::write(t.modules.join("modules.dep"), list).unwrap();
    }

    #[test]
    fn available_matches_compressed_and_plain_module_files() {
        let (_d, t) = tree(&[], Some(&[]));
        write_dep(
            &t,
            &[
                "kernel/fs/xfs/xfs.ko.xz",
                "kernel/fs/ext4/ext4.ko",
                "kernel/fs/jbd2/jbd2.ko.zst",
            ],
        );
        assert_eq!(t.available("xfs"), Some(true));
        assert_eq!(t.available("ext4"), Some(true));
        assert_eq!(t.available("jbd2"), Some(true));
        assert_eq!(t.available("btrfs"), Some(false));
        // Whole file name: `fs` is not found by `xfs.ko`.
        assert_eq!(t.available("fs"), Some(false));
    }

    #[test]
    fn available_is_unknown_without_modules_dep() {
        let (_d, t) = tree(&[], Some(&[]));
        assert_eq!(t.available("xfs"), None);
    }

    fn never_init(_: Arc<Config>) -> SamplerResult {
        panic!("init must not run for a module this kernel does not have");
    }

    /// A module neither built in nor in modules.dep is reported unsupported
    /// at once, not waited for.
    #[test]
    fn a_module_this_kernel_does_not_have_is_unsupported() {
        let (_d, t) = tree(&[], Some(&[]));
        write_dep(&t, &["kernel/fs/ext4/ext4.ko.xz"]);
        let err = match until_loaded_in(
            t,
            Duration::from_millis(10),
            "deferred_test_absent",
            &["xfs"],
            vec![],
            Arc::new(toml::from_str("").unwrap()),
            never_init,
        ) {
            Err(e) => e,
            Ok(_) => panic!("expected unsupported"),
        };
        let u = err
            .downcast_ref::<crate::agent::sampler_status::Unsupported>()
            .expect("unsupported, not failed");
        assert!(u.0.contains("no xfs module"), "{}", u.0);
    }

    // Lifecycle tests. The status registry is process-global, so each test
    // uses its own sampler name, group and counters.

    use crate::agent::sampler_status::{snapshot, SamplerState};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const TICK: Duration = Duration::from_millis(10);

    fn state(name: &str) -> Option<SamplerState> {
        snapshot()
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.state)
    }

    fn test_config() -> Arc<Config> {
        Arc::new(toml::from_str("").expect("empty config parses"))
    }

    fn load(t: &ModuleTree, name: &str) {
        let m = t.sys.join("module").join(name);
        std::fs::create_dir_all(&m).unwrap();
        std::fs::write(m.join("initstate"), "live\n").unwrap();
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

    /// Poll until `f` holds or five seconds pass; init runs on its own thread.
    async fn eventually(f: impl Fn() -> bool) -> bool {
        for _ in 0..500 {
            if f() {
                return true;
            }
            tokio::time::sleep(TICK).await;
        }
        f()
    }

    static WAIT_INITS: AtomicUsize = AtomicUsize::new(0);
    static WAIT_REFRESHES: AtomicUsize = AtomicUsize::new(0);
    static WAIT_GROUP: AcquisitionGroup = AcquisitionGroup::new("deferred_test_waits", "plain");
    static WAIT_SELF_BOUND: AcquisitionGroup =
        AcquisitionGroup::new("deferred_test_waits", "bounded_by_init");
    /// The bound the waiting group read while init ran.
    static WAIT_BOUND_DURING_INIT: AtomicUsize = AtomicUsize::new(7);

    fn wait_init(_: Arc<Config>) -> SamplerResult {
        WAIT_INITS.fetch_add(1, Ordering::SeqCst);
        WAIT_BOUND_DURING_INIT.store(
            WAIT_GROUP.member_bound().unwrap_or(usize::MAX),
            Ordering::SeqCst,
        );
        WAIT_SELF_BOUND.set_member_bound(1);
        Ok(Some(Box::new(Fake(&WAIT_REFRESHES))))
    }

    #[tokio::test]
    async fn waits_then_initialises_once_and_forwards_refreshes() {
        const NAME: &str = "deferred_test_waits";
        let (_d, t) = tree(&[], Some(&[]));
        let sampler = until_loaded_in(
            t.clone(),
            TICK,
            NAME,
            &["ext4"],
            vec![&WAIT_GROUP, &WAIT_SELF_BOUND],
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
        assert_eq!(
            WAIT_GROUP.member_bound(),
            Some(0),
            "no members while waiting"
        );

        // No refresh drives the wait: the thread notices the load on its own.
        tokio::time::sleep(5 * TICK).await;
        sampler.refresh().await;
        assert_eq!(
            WAIT_INITS.load(Ordering::SeqCst),
            0,
            "no init before the module loads"
        );
        assert_eq!(WAIT_REFRESHES.load(Ordering::SeqCst), 0);

        load(&t, "ext4");
        assert!(
            eventually(|| WAIT_INITS.load(Ordering::SeqCst) == 1).await,
            "init ran"
        );
        assert!(
            eventually(|| matches!(state(NAME), Some(SamplerState::Active))).await,
            "{:?}",
            state(NAME)
        );

        assert_eq!(
            WAIT_BOUND_DURING_INIT.load(Ordering::SeqCst),
            0,
            "still no members while init runs"
        );
        assert_eq!(WAIT_GROUP.member_bound(), None, "unbounded after init");
        assert_eq!(
            WAIT_SELF_BOUND.member_bound(),
            Some(1),
            "init's own bound stands"
        );

        for _ in 0..5 {
            sampler.refresh().await;
        }
        assert_eq!(
            WAIT_REFRESHES.load(Ordering::SeqCst),
            5,
            "refreshes reach the real sampler"
        );
        tokio::time::sleep(5 * TICK).await;
        assert_eq!(
            WAIT_INITS.load(Ordering::SeqCst),
            1,
            "init runs exactly once"
        );
    }

    static NOW_INITS: AtomicUsize = AtomicUsize::new(0);
    static NOW_REFRESHES: AtomicUsize = AtomicUsize::new(0);
    static NOW_GROUP: AcquisitionGroup = AcquisitionGroup::new("deferred_test_now", "plain");

    fn now_init(_: Arc<Config>) -> SamplerResult {
        NOW_INITS.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(Fake(&NOW_REFRESHES))))
    }

    #[tokio::test]
    async fn initialises_at_once_when_the_module_is_loaded() {
        let (_d, t) = tree(&["ext4"], Some(&[]));
        let sampler = until_loaded_in(
            t,
            TICK,
            "deferred_test_now",
            &["ext4"],
            vec![&NOW_GROUP],
            test_config(),
            now_init,
        )
        .unwrap()
        .unwrap();
        assert_eq!(NOW_INITS.load(Ordering::SeqCst), 1);
        assert_eq!(NOW_GROUP.member_bound(), None, "groups untouched");
        sampler.refresh().await;
        assert_eq!(
            NOW_REFRESHES.load(Ordering::SeqCst),
            1,
            "the real sampler, not a wrapper"
        );
    }

    static FAIL_INITS: AtomicUsize = AtomicUsize::new(0);
    static FAIL_GROUP: AcquisitionGroup = AcquisitionGroup::new("deferred_test_fails", "plain");

    fn failing_init(_: Arc<Config>) -> SamplerResult {
        FAIL_INITS.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("Invalid argument (os error 22)"))
    }

    /// An init that fails after the module loads is recorded as failed, not
    /// retried, and leaves its groups with no members.
    #[tokio::test]
    async fn a_failed_init_after_loading_is_recorded_and_not_retried() {
        const NAME: &str = "deferred_test_fails";
        let (_d, t) = tree(&[], Some(&[]));
        let sampler = until_loaded_in(
            t.clone(),
            TICK,
            NAME,
            &["ext4"],
            vec![&FAIL_GROUP],
            test_config(),
            failing_init,
        )
        .unwrap()
        .unwrap();

        load(&t, "ext4");
        assert!(
            eventually(|| matches!(state(NAME), Some(SamplerState::Failed { .. }))).await,
            "{:?}",
            state(NAME)
        );
        for _ in 0..5 {
            sampler.refresh().await;
        }
        tokio::time::sleep(5 * TICK).await;
        assert_eq!(FAIL_INITS.load(Ordering::SeqCst), 1);
        assert_eq!(FAIL_GROUP.member_bound(), Some(0));
    }

    /// The reason follows the modules still missing.
    #[tokio::test]
    async fn the_waiting_reason_names_the_modules_still_missing() {
        const NAME: &str = "deferred_test_partial";
        let (_d, t) = tree(&[], Some(&[]));
        let _sampler = until_loaded_in(
            t.clone(),
            TICK,
            NAME,
            &["ext4", "jbd2"],
            vec![],
            test_config(),
            now_init_unused,
        )
        .unwrap()
        .unwrap();

        load(&t, "jbd2");
        assert!(
            eventually(|| matches!(
                state(NAME),
                Some(SamplerState::Unsupported { ref reason }) if reason.contains("module ext4 to")
            ))
            .await,
            "{:?}",
            state(NAME)
        );
    }

    static GONE_INITS: AtomicUsize = AtomicUsize::new(0);

    fn gone_init(_: Arc<Config>) -> SamplerResult {
        GONE_INITS.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!(
            "must not run: ext4 never loads in this test"
        ))
    }

    /// `modules.builtin` becoming unreadable while waiting does not end the
    /// wait: ext4 is still not loaded.
    #[tokio::test]
    async fn losing_modules_builtin_while_waiting_keeps_waiting() {
        const NAME: &str = "deferred_test_builtin_gone";
        let (_d, t) = tree(&[], Some(&[]));
        let _sampler = until_loaded_in(
            t.clone(),
            TICK,
            NAME,
            &["ext4"],
            vec![],
            test_config(),
            gone_init,
        )
        .unwrap()
        .unwrap();

        std::fs::remove_file(t.modules.join("modules.builtin")).unwrap();
        tokio::time::sleep(20 * TICK).await;
        assert_eq!(GONE_INITS.load(Ordering::SeqCst), 0);
        assert!(
            matches!(state(NAME), Some(SamplerState::Unsupported { .. })),
            "{:?}",
            state(NAME)
        );
    }

    fn now_init_unused(_: Arc<Config>) -> SamplerResult {
        Err(anyhow::anyhow!(
            "must not run: ext4 never loads in this test"
        ))
    }
}
