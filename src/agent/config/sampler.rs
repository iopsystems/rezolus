use super::*;

#[derive(Deserialize, Default, Clone)]
pub struct Sampler {
    #[serde(default)]
    enabled: Option<bool>,
    /// AMD GPU performance level to set for this sampler (only meaningful for
    /// `gpu_amd_pmu`, which is Linux-only). `None` means leave the GPU power
    /// state untouched.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    gpu_perf_level: Option<String>,
    /// Read interval for samplers that poll a slow, expensive source on their
    /// own cadence rather than on the scrape/TTL sample cycle (currently
    /// `drivehealth` and `memory_cgroup`). A humantime string (e.g. `"60s"`).
    /// `None` means use the sampler's built-in default.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    interval: Option<String>,
    /// Whether a sampler that can attribute its events to the calling
    /// thread's cgroup does so (`ext4_ops`, `xfs_log`). Off by default: the
    /// serial check and two atomics it adds to a request-path hook were
    /// measured at half the hook's cost. `None` means the default.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    cgroup_attribution: Option<bool>,
    /// Whether a sampler that keeps per-task accounting also exports it as
    /// per-task series (`cpu_usage`'s `task_cpu_usage`). Off by default: the
    /// accounting stays (host and cgroup totals are computed from it), and
    /// what is dropped is the export — a task-metadata event per new task
    /// and per rename, a task-exit event per exit that is not a group's
    /// last, the `task_rename` program unless `process_attribution` is on,
    /// the walk of the per-pid map each time a snapshot is served
    /// (the slots with metadata for V3, every slot for V2), and one series
    /// per thread plus one per rename in every recording. `None` means the
    /// default.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    task_attribution: Option<bool>,
    /// Whether a sampler that keeps per-task accounting also keeps a total
    /// per process (`cpu_usage`'s `process_cpu_usage`). Off by default; on,
    /// it costs two per-pid arrays (64 MiB) and a 256 KiB ring, a
    /// process-metadata event per new process and per leader rename, a
    /// task-exit event per process exit when `task_attribution` is off (one
    /// from each last thread that exits concurrently), the `task_rename`
    /// program (one run per thread rename), a walk
    /// of the per-process map each time a snapshot is served, contention on
    /// a process's counter when its threads run on many CPUs (principle 7
    /// in `docs/principles.md`), and one series per process in every
    /// recording. Independent of `task_attribution`. `None` means the
    /// default.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    process_attribution: Option<bool>,
    /// The parts of a sampler that merged several older ones, each on by
    /// default (`syscall`: `counts`, `latency`; `blockio`: `requests`,
    /// `latency`; `scheduler`: `runqueue`, `migrations`). See `Config::part`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    counts: Option<bool>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    latency: Option<bool>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    requests: Option<bool>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    runqueue: Option<bool>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    migrations: Option<bool>,
    /// Which slab caches `memory_slabinfo` publishes `memory_slab_cache_bytes`
    /// for: `"all"` (every cache in `/proc/slabinfo`, the default) or
    /// `"fixed"` (the filesystem metadata and page-cache index caches whose
    /// object counts it also follows). `None` means the default.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    caches: Option<String>,
}

/// The values `caches` accepts.
const CACHES_VALUES: &[&str] = &["all", "fixed"];

/// The one section `caches` means something in.
const CACHES_SAMPLER: &str = "memory_slabinfo";

impl Sampler {
    pub fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn gpu_perf_level(&self) -> Option<&str> {
        self.gpu_perf_level.as_deref()
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn interval(&self) -> Option<&str> {
        self.interval.as_deref()
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn cgroup_attribution(&self) -> Option<bool> {
        self.cgroup_attribution
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn task_attribution(&self) -> Option<bool> {
        self.task_attribution
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn process_attribution(&self) -> Option<bool> {
        self.process_attribution
    }

    /// The part switches this section sets, by name.
    pub(super) fn parts_set(&self) -> Vec<&'static str> {
        let mut parts = Vec::new();
        if self.counts.is_some() {
            parts.push("counts");
        }
        if self.latency.is_some() {
            parts.push("latency");
        }
        if self.requests.is_some() {
            parts.push("requests");
        }
        if self.runqueue.is_some() {
            parts.push("runqueue");
        }
        if self.migrations.is_some() {
            parts.push("migrations");
        }
        parts
    }

    /// The switch for one part of a merged sampler, if the section sets it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn part(&self, part: &str) -> Option<bool> {
        match part {
            "counts" => self.counts,
            "latency" => self.latency,
            "requests" => self.requests,
            "runqueue" => self.runqueue,
            "migrations" => self.migrations,
            _ => None,
        }
    }

    pub(super) fn set_enabled(&mut self, on: bool) {
        self.enabled = Some(on);
    }

    pub(super) fn set_cgroup_attribution(&mut self, on: bool) {
        self.cgroup_attribution = Some(on);
    }

    pub(super) fn set_part(&mut self, part: &str, on: bool) {
        match part {
            "counts" => self.counts = Some(on),
            "latency" => self.latency = Some(on),
            "requests" => self.requests = Some(on),
            "runqueue" => self.runqueue = Some(on),
            "migrations" => self.migrations = Some(on),
            _ => unreachable!("no part named {part}"),
        }
    }

    /// `caches` is only meaningful in `[samplers.memory_slabinfo]`; anywhere
    /// else, including `[defaults]` (checked as `"default"`), it would be
    /// ignored, so it is refused.
    pub fn check_caches(&self, name: &str) -> Result<(), String> {
        let Some(ref caches) = self.caches else {
            return Ok(());
        };
        if name != CACHES_SAMPLER {
            return Err(format!(
                "sampler '{name}' does not take `caches`; it is a \
                 [samplers.{CACHES_SAMPLER}] setting"
            ));
        }
        if !CACHES_VALUES.contains(&caches.as_str()) {
            return Err(format!(
                "sampler '{name}' caches must be one of {CACHES_VALUES:?}, not {caches:?}"
            ));
        }
        Ok(())
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn caches(&self) -> Option<&str> {
        self.caches.as_deref()
    }

    pub fn check(&self, name: &str) {
        if let Err(e) = self.check_caches(name) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        if let Some(ref interval) = self.interval {
            if let Err(e) = interval.parse::<humantime::Duration>() {
                eprintln!("sampler '{name}' interval couldn't be parsed: {e}");
                std::process::exit(1);
            }
        }
    }
}
