//! Process-global registry of per-sampler status, populated during agent
//! initialization and read by the `/samplers` and `/status` HTTP endpoints.
//! A running sampler adds and clears conditions with
//! [`set_runtime_degraded`] and [`clear_runtime_degraded`].

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// Status of a single sampler.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SamplerStatus {
    pub name: String,
    #[serde(flatten)]
    pub state: SamplerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<SamplerHealth>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub programs: Vec<ProgramStatus>,
    /// Conditions the running sampler reports, sorted by condition name. Any
    /// entry makes `health` `degraded` in [`snapshot`], unless it is `failed`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_degraded: Vec<RuntimeCondition>,
}

/// A condition a running sampler reported through [`set_runtime_degraded`]
/// and has not cleared.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeCondition {
    /// The sampler's name for the condition, such as `stuck_sweep`.
    pub condition: String,
    pub reason: String,
}

/// Aggregate agent status returned by the `/status` endpoint: identity +
/// liveness header plus the full per-sampler health detail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub version: String,
    /// This run's producer epoch. Changes on restart and only on restart, so
    /// two readings that differ mean every cumulative counter in between
    /// restarted from zero. See `crate::agent::epoch`.
    #[serde(default)]
    pub producer_epoch: String,
    /// The wall clock this agent's timeline is anchored to, in nanoseconds
    /// since the Unix epoch.
    ///
    /// Every timestamp the agent produces is `anchor + monotonic elapsed` from
    /// here, so a consumer that has this can place a reading in wall time
    /// without trusting its own clock to agree. It is minted with the epoch
    /// and lasts exactly as long: the same restart resets both.
    ///
    /// Reported here as well as in a stream's handshake because a consumer
    /// that scrapes rather than subscribes never sees a handshake, and it
    /// needs the same anchor to interpret the same timestamps. Defaulted for
    /// an older agent that has no anchor to report.
    #[serde(default)]
    pub clock_anchor_wall_ns: i64,
    pub uptime_seconds: u64,
    pub ttl_seconds: u64,
    /// The sampling clock's current period in milliseconds, or `None` when the
    /// agent is **tickless** — nothing is subscribed, so nothing is sampled
    /// except when a scrape asks.
    ///
    /// Reported because a new subsystem nobody can see is a new subsystem
    /// nobody can debug: "is this agent sampling, and how often" is the first
    /// question when a subscriber is not getting what it expected, and the
    /// answer depends on who ELSE is subscribed (the clock runs at the fastest
    /// demand). Defaulted for an older agent that has no clock to report.
    #[serde(default)]
    pub sample_interval_ms: Option<u64>,
    /// How many subscriptions are currently driving the clock.
    #[serde(default)]
    pub subscribers: usize,
    pub samplers: Vec<SamplerStatus>,
}

/// Whether a sampler is running, disabled by config, or failed to initialize.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SamplerState {
    Active,
    Disabled,
    Failed {
        error: String,
    },
    /// Running, but only on some CPUs: the PMU reservation left too little on
    /// the reserved CPUs for this sampler's whole event set, and enough on the
    /// rest.
    ///
    /// A distinct state because partial coverage is neither of the two things
    /// it could be filed under. Reported as `Active` it reads as full coverage
    /// and the gap is invisible — which is the failure mode this whole area
    /// exists to remove. Reported as starved it reads as no data at all, when
    /// most of the machine has it.
    PmuLimited {
        cpus: usize,
        of: usize,
    },
    /// Not started because the PMU had too few free counters for its whole
    /// event set.
    ///
    /// Deliberately not `Failed`: nothing broke, and the health rollup counting
    /// it as a failure would send an operator looking for a fault that does not
    /// exist. Deliberately not `Disabled` either — nobody turned it off, and the
    /// operator may well want it, which is what `pmu_priority` is for. The
    /// numbers travel with it so the trade being made is visible rather than
    /// inferred.
    PmuStarved {
        wants: usize,
        free: usize,
    },
    /// Not started because this machine or kernel cannot support it — no L3
    /// cache domains, no GPU of that vendor, an architecture the probe does not
    /// cover — or because the platform gives the data only to a privilege the
    /// agent does not have, as macOS does with its TCP statistics.
    ///
    /// Deliberately not `Failed`, for the reason `PmuStarved` is not: nothing
    /// broke. A missing capability reported as a fault sends an operator
    /// looking for a problem that does not exist, and — because `rezolus
    /// status` exits non-zero on a failed sampler — fails a readiness probe on
    /// a machine that is working exactly as it can. Deliberately not `Disabled`
    /// either: nobody turned it off, and an operator reading `disabled` would
    /// reasonably go looking for the config that did.
    Unsupported {
        reason: String,
    },
}

/// Returned from a sampler's `init` to say "this machine cannot do this",
/// rather than "this broke".
///
/// Init returns `anyhow::Result`, and every error out of it used to become
/// `Failed`. This is the marker that lets the one meaningful distinction
/// survive that funnel: `run()` downcasts to it and records `Unsupported`
/// instead.
#[derive(Debug)]
pub struct Unsupported(pub String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unsupported {}

/// Status of a single BPF program within a sampler.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProgramStatus {
    pub name: String,
    pub attached: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<ProbeIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub expected: bool,
    #[serde(default = "default_verdict")]
    pub verdict: ProbeVerdict,
}

fn default_verdict() -> ProbeVerdict {
    ProbeVerdict::Ok
}

fn registry() -> &'static Mutex<BTreeMap<&'static str, SamplerStatus>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<&'static str, SamplerStatus>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

type RuntimeConditions = BTreeMap<(&'static str, &'static str), String>;

/// Runtime conditions by (sampler, condition). Kept apart from the init-time
/// records so that a `set_*` call replacing a record does not drop them.
fn runtime() -> &'static Mutex<RuntimeConditions> {
    static RUNTIME: OnceLock<Mutex<RuntimeConditions>> = OnceLock::new();
    RUNTIME.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Report that running sampler `name` is degraded by `condition`, with the
/// reason. Setting a condition already set replaces its reason. While any
/// condition is set, `/samplers` and `/status` report the sampler's health as
/// `degraded` (unless it is `failed`) and `rezolus status` exits 1. The caller
/// logs the condition; this records it.
pub fn set_runtime_degraded(name: &'static str, condition: &'static str, reason: String) {
    runtime()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert((name, condition), reason);
}

/// Clear `condition` for sampler `name`. Clearing a condition that is not set
/// does nothing.
pub fn clear_runtime_degraded(name: &'static str, condition: &'static str) {
    runtime()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(name, condition));
}

/// Mark a sampler active if its record still says unsupported. For a
/// sampler that initialises after start (`samplers::deferred`): its
/// "waiting" record is replaced once init succeeds, unless init already
/// wrote a richer one (a BPF sampler's builder does).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn set_active_if_unsupported(name: &'static str) {
    let mut registry = registry().lock().unwrap();
    let waiting = matches!(
        registry.get(name).map(|s| &s.state),
        None | Some(SamplerState::Unsupported { .. })
    );
    if waiting {
        registry.insert(
            name,
            SamplerStatus {
                name: name.to_string(),
                state: SamplerState::Active,
                health: None,
                programs: Vec::new(),
                runtime_degraded: Vec::new(),
            },
        );
    }
}

/// Record a sampler as config-disabled.
pub fn set_disabled(name: &'static str) {
    registry().lock().unwrap().insert(
        name,
        SamplerStatus {
            name: name.to_string(),
            state: SamplerState::Disabled,
            health: None,
            programs: Vec::new(),
            runtime_degraded: Vec::new(),
        },
    );
}

/// Record a sampler as running on only some CPUs, with the coverage.
pub fn set_pmu_limited(name: &'static str, cpus: usize, of: usize) {
    registry().lock().unwrap().insert(
        name,
        SamplerStatus {
            name: name.to_string(),
            state: SamplerState::PmuLimited { cpus, of },
            health: Some(SamplerHealth::Degraded),
            programs: Vec::new(),
            runtime_degraded: Vec::new(),
        },
    );
}

/// Record a sampler as skipped for want of PMU counters, with the numbers.
///
/// Health is `Unsupported` — informational, "this capability is unavailable
/// here" — for the same reason the state is not `Failed`.
pub fn set_pmu_starved(name: &'static str, wants: usize, free: usize) {
    registry().lock().unwrap().insert(
        name,
        SamplerStatus {
            name: name.to_string(),
            state: SamplerState::PmuStarved { wants, free },
            health: Some(SamplerHealth::Unsupported),
            programs: Vec::new(),
            runtime_degraded: Vec::new(),
        },
    );
}

/// Record a sampler as unsupported on this machine, with the reason.
///
/// Health is `Unsupported` — informational, "this capability is unavailable
/// here" — so it does not count against the health rollup or the exit status.
pub fn set_unsupported(name: &'static str, reason: String) {
    registry().lock().unwrap().insert(
        name,
        SamplerStatus {
            name: name.to_string(),
            state: SamplerState::Unsupported { reason },
            health: Some(SamplerHealth::Unsupported),
            programs: Vec::new(),
            runtime_degraded: Vec::new(),
        },
    );
}

/// Record a sampler as failed to initialize, with the error message.
pub fn set_failed(name: &'static str, error: String) {
    registry().lock().unwrap().insert(
        name,
        SamplerStatus {
            name: name.to_string(),
            state: SamplerState::Failed { error },
            health: None,
            programs: Vec::new(),
            runtime_degraded: Vec::new(),
        },
    );
}

/// Record a sampler as active with its per-program detail and computed health.
/// Only called from the BPF builder, which is Linux-only.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn set_active_with_programs(
    name: &'static str,
    health: SamplerHealth,
    programs: Vec<ProgramStatus>,
) {
    registry().lock().unwrap().insert(
        name,
        SamplerStatus {
            name: name.to_string(),
            state: SamplerState::Active,
            health: Some(health),
            programs,
            runtime_degraded: Vec::new(),
        },
    );
}

/// Mark a sampler active only if it has no record yet. Used by the central
/// init loop so it does not clobber the richer record a BPF sampler already
/// wrote from the builder.
pub fn set_active_if_absent(name: &'static str) {
    registry()
        .lock()
        .unwrap()
        .entry(name)
        .or_insert_with(|| SamplerStatus {
            name: name.to_string(),
            state: SamplerState::Active,
            health: None,
            programs: Vec::new(),
            runtime_degraded: Vec::new(),
        });
}

/// Record that a sampler's hardware perf counters are not measuring.
///
/// Unlike a probe attach, this is only discoverable at read time -- a counter
/// that never got a PMU slot, or that stopped advancing after having worked,
/// looks identical to a healthy one until its `time_running` is inspected. The
/// perf read threads call this once per affected counter so the condition
/// reaches `/samplers` instead of only a log line, letting a consumer tell
/// *not measured* from a genuine zero.
///
/// Idempotent per sampler: repeated calls refresh the reason rather than
/// accumulating entries.
///
/// Called from the BPF builder's perf read threads (Linux-only); on other
/// platforms it is exercised only by unit tests, so suppress the dead-code lint
/// there, matching `classify_program`/`rollup_health` above.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn note_perf_unavailable(name: &'static str, reason: &str) {
    let mut registry = registry().lock().unwrap();

    let entry = registry.entry(name).or_insert_with(|| SamplerStatus {
        name: name.to_string(),
        state: SamplerState::Active,
        health: None,
        programs: Vec::new(),
        runtime_degraded: Vec::new(),
    });

    if let Some(existing) = entry.programs.iter_mut().find(|p| p.name == "pmu") {
        existing.error = Some(reason.to_string());
    } else {
        entry.programs.push(ProgramStatus {
            name: "pmu".to_string(),
            attached: false,
            error: Some(reason.to_string()),
            intent: None,
            label: Some("hardware perf counters".to_string()),
            expected: true,
            verdict: ProbeVerdict::Unsupported,
        });
    }

    let verdicts: Vec<ProbeVerdict> = entry.programs.iter().map(|p| p.verdict).collect();
    entry.health = Some(rollup_health(true, &verdicts));
}

impl SamplerStatus {
    /// The capability label (or program name, where unlabelled) of each of
    /// this sampler's unsupported programs, with the reason recorded for it,
    /// each label once, in program order. An entry and an exit program
    /// commonly share a label and a reason; the first program's reason is
    /// given.
    pub fn unsupported_hooks(&self) -> Vec<(&str, &str)> {
        let mut hooks: Vec<(&str, &str)> = Vec::new();
        for p in &self.programs {
            let label = p.label.as_deref().unwrap_or(&p.name);
            if p.verdict == ProbeVerdict::Unsupported && !hooks.iter().any(|(l, _)| *l == label) {
                hooks.push((label, p.error.as_deref().unwrap_or("no kernel support")));
            }
        }
        hooks
    }
}

/// The reason recorded for `condition` on sampler `name`, if set.
#[cfg(test)]
pub(crate) fn runtime_condition(name: &str, condition: &str) -> Option<String> {
    runtime()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|((s, c), _)| *s == name && *c == condition)
        .map(|(_, reason)| reason.clone())
}

/// Snapshot of all sampler statuses, sorted by name (BTreeMap order), with
/// each sampler's runtime conditions attached.
pub fn snapshot() -> Vec<SamplerStatus> {
    let mut statuses: Vec<SamplerStatus> = registry().lock().unwrap().values().cloned().collect();
    let runtime = runtime().lock().unwrap_or_else(|e| e.into_inner());
    for status in &mut statuses {
        apply_runtime(status, &runtime);
    }
    statuses
}

/// Attach the runtime conditions recorded for `status`'s sampler. A sampler
/// that is running (`active` or `pmu_limited`) with any condition reads
/// `degraded`, unless its health is `failed`. A sampler in any other state is
/// not running, so its conditions are attached and its health is left alone.
fn apply_runtime(status: &mut SamplerStatus, runtime: &RuntimeConditions) {
    status.runtime_degraded = runtime
        .iter()
        .filter(|((sampler, _), _)| *sampler == status.name)
        .map(|((_, condition), reason)| RuntimeCondition {
            condition: condition.to_string(),
            reason: reason.clone(),
        })
        .collect();
    let running = matches!(
        status.state,
        SamplerState::Active | SamplerState::PmuLimited { .. }
    );
    if running
        && !status.runtime_degraded.is_empty()
        && status.health != Some(SamplerHealth::Failed)
    {
        status.health = Some(SamplerHealth::Degraded);
    }
}

/// Author-declared meaning of a BPF program's attach.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ProbeIntent {
    /// Must attach on every supported kernel. Default for unclassified probes.
    #[default]
    Required,
    /// Per-device probe; expected to attach iff `driver` (the sysfs driver
    /// name, e.g. `virtio_net`, `mlx5_core`) is bound to a present device.
    Driver { driver: String },
}

/// Per-probe outcome after classification.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeVerdict {
    /// Attached as expected.
    Ok,
    /// Required probe absent due to ENOENT — kernel lacks the symbol.
    Unsupported,
    /// Should have attached but did not (non-ENOENT error, or a present-driver
    /// probe that failed).
    Broken,
    /// Driver probe whose driver is not bound to any present device — silent.
    NotApplicable,
}

/// Per-sampler health rollup.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplerHealth {
    Healthy,
    /// A capability is unavailable on this kernel (ENOENT-gated). Informational.
    Unsupported,
    /// Something that should work broke.
    Degraded,
    /// Load/verify error — completely non-functional.
    Failed,
}

/// Classify one probe. Pure function. `is_enoent` is meaningful only when
/// `attached` is false. `driver_present` is meaningful only for `Driver`.
///
/// Called from the BPF builder (Linux-only); on other platforms it is exercised
/// only by unit tests, so suppress the dead-code lint there.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn classify_program(
    intent: &ProbeIntent,
    attached: bool,
    is_enoent: bool,
    driver_present: bool,
) -> ProbeVerdict {
    if attached {
        return ProbeVerdict::Ok;
    }
    match intent {
        ProbeIntent::Required => {
            if is_enoent {
                ProbeVerdict::Unsupported
            } else {
                ProbeVerdict::Broken
            }
        }
        ProbeIntent::Driver { .. } => {
            if driver_present {
                ProbeVerdict::Broken
            } else {
                ProbeVerdict::NotApplicable
            }
        }
    }
}

/// Roll up per-probe verdicts into a sampler health, in strict precedence:
/// failed (load error) > degraded (any broken) > unsupported (any enoent) >
/// healthy.
///
/// Called from the BPF builder (Linux-only); on other platforms it is exercised
/// only by unit tests, so suppress the dead-code lint there.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn rollup_health(loaded_ok: bool, verdicts: &[ProbeVerdict]) -> SamplerHealth {
    if !loaded_ok {
        return SamplerHealth::Failed;
    }
    if verdicts.contains(&ProbeVerdict::Broken) {
        return SamplerHealth::Degraded;
    }
    if verdicts.contains(&ProbeVerdict::Unsupported) {
        return SamplerHealth::Unsupported;
    }
    SamplerHealth::Healthy
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The marker must survive being carried up through `init`.
    ///
    /// `run()` finds it with `downcast_ref`, which searches an `anyhow` error
    /// AND its context chain — so `.context(..)` is safe. What is not safe is
    /// `anyhow!("...: {e}")`: that builds a fresh error with the text
    /// interpolated, the type is gone, and an absent GPU stack silently
    /// becomes `failed` again. This bit twice while writing the fix, and the
    /// symptom is a correct-looking message under the wrong state.
    #[test]
    fn the_unsupported_marker_survives_context_but_not_reformatting() {
        let raw: anyhow::Error = Unsupported("no ROCm stack on this host".into()).into();
        assert!(raw.downcast_ref::<Unsupported>().is_some(), "bare");

        let with_context = raw.context("gpu_amd_smi: failed to initialize");
        assert!(
            with_context.downcast_ref::<Unsupported>().is_some(),
            "context() must preserve the marker"
        );

        // The trap, asserted so nobody reintroduces it believing otherwise.
        let reformatted = anyhow::anyhow!(
            "gpu_amd_smi: failed to initialize: {}",
            Unsupported("no ROCm stack on this host".to_string())
        );
        assert!(
            reformatted.downcast_ref::<Unsupported>().is_none(),
            "reformatting DOES destroy the marker — propagate or use context()"
        );
    }

    #[test]
    fn active_serializes_with_flattened_state_and_programs() {
        let s = SamplerStatus {
            name: "cpu_usage".into(),
            state: SamplerState::Active,
            health: None,
            programs: vec![
                ProgramStatus {
                    name: "softirq_enter".into(),
                    attached: true,
                    error: None,
                    intent: Some(ProbeIntent::Required),
                    label: None,
                    expected: false,
                    verdict: ProbeVerdict::Ok,
                },
                ProgramStatus {
                    name: "cpuacct_account_field_kprobe".into(),
                    attached: false,
                    error: Some("no kernel support (ENOENT)".into()),
                    intent: Some(ProbeIntent::Required),
                    label: None,
                    expected: false,
                    verdict: ProbeVerdict::Unsupported,
                },
            ],
            runtime_degraded: Vec::new(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""name":"cpu_usage""#));
        assert!(json.contains(r#""state":"active""#));
        assert!(json.contains(r#""attached":true"#));
        assert!(json.contains(r#""error":"no kernel support (ENOENT)""#));
        let back: SamplerStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn disabled_omits_programs_and_error() {
        let json = serde_json::to_string(&SamplerStatus {
            name: "gpu".into(),
            state: SamplerState::Disabled,
            health: None,
            programs: Vec::new(),
            runtime_degraded: Vec::new(),
        })
        .unwrap();
        assert_eq!(json, r#"{"name":"gpu","state":"disabled"}"#);
    }

    #[test]
    fn failed_includes_error() {
        let json = serde_json::to_string(&SamplerStatus {
            name: "blockio_latency".into(),
            state: SamplerState::Failed {
                error: "boom".into(),
            },
            health: None,
            programs: Vec::new(),
            runtime_degraded: Vec::new(),
        })
        .unwrap();
        assert!(json.contains(r#""state":"failed""#));
        assert!(json.contains(r#""error":"boom""#));
    }

    #[test]
    fn classify_required_attached_is_ok() {
        assert_eq!(
            classify_program(&ProbeIntent::Required, true, false, false),
            ProbeVerdict::Ok
        );
    }
    #[test]
    fn classify_required_enoent_is_unsupported() {
        assert_eq!(
            classify_program(&ProbeIntent::Required, false, true, false),
            ProbeVerdict::Unsupported
        );
    }
    #[test]
    fn classify_required_other_error_is_broken() {
        assert_eq!(
            classify_program(&ProbeIntent::Required, false, false, false),
            ProbeVerdict::Broken
        );
    }
    #[test]
    fn classify_driver_present_not_attached_is_broken() {
        let i = ProbeIntent::Driver {
            driver: "ena".into(),
        };
        assert_eq!(
            classify_program(&i, false, true, true),
            ProbeVerdict::Broken
        );
    }
    #[test]
    fn classify_driver_absent_not_attached_is_not_applicable() {
        let i = ProbeIntent::Driver {
            driver: "ixgbe".into(),
        };
        assert_eq!(
            classify_program(&i, false, false, false),
            ProbeVerdict::NotApplicable
        );
    }
    #[test]
    fn classify_driver_attached_is_ok() {
        let i = ProbeIntent::Driver {
            driver: "ena".into(),
        };
        assert_eq!(classify_program(&i, true, false, true), ProbeVerdict::Ok);
    }
    #[test]
    fn rollup_failed_when_load_error() {
        assert_eq!(rollup_health(false, &[]), SamplerHealth::Failed);
    }
    #[test]
    fn rollup_degraded_on_broken() {
        let v = vec![ProbeVerdict::Ok, ProbeVerdict::Broken];
        assert_eq!(rollup_health(true, &v), SamplerHealth::Degraded);
    }
    #[test]
    fn rollup_unsupported_on_enoent_only() {
        let v = vec![
            ProbeVerdict::Ok,
            ProbeVerdict::Unsupported,
            ProbeVerdict::NotApplicable,
        ];
        assert_eq!(rollup_health(true, &v), SamplerHealth::Unsupported);
    }
    #[test]
    fn rollup_healthy_when_all_ok_or_na() {
        let v = vec![ProbeVerdict::Ok, ProbeVerdict::NotApplicable];
        assert_eq!(rollup_health(true, &v), SamplerHealth::Healthy);
    }
    #[test]
    fn rollup_degraded_beats_unsupported() {
        let v = vec![ProbeVerdict::Unsupported, ProbeVerdict::Broken];
        assert_eq!(rollup_health(true, &v), SamplerHealth::Degraded);
    }
    #[test]
    fn rollup_healthy_when_loaded_and_empty() {
        assert_eq!(rollup_health(true, &[]), SamplerHealth::Healthy);
    }

    #[test]
    fn program_status_serializes_intent_and_verdict() {
        let p = ProgramStatus {
            name: "ena_tx_timeout".into(),
            attached: false,
            error: Some("no kernel support (ENOENT)".into()),
            intent: Some(ProbeIntent::Driver {
                driver: "ena".into(),
            }),
            label: Some("ENA tx timeout".into()),
            expected: false,
            verdict: ProbeVerdict::NotApplicable,
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains(r#""verdict":"not_applicable""#));
        assert!(json.contains(r#""intent":{"type":"driver""#));
        assert!(json.contains(r#""driver":"ena""#));
        let back: ProgramStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn old_program_status_without_new_fields_still_deserializes() {
        // Payload produced before this change (no intent/verdict/etc.).
        let json = r#"{"name":"softirq_enter","attached":true}"#;
        let p: ProgramStatus = serde_json::from_str(json).unwrap();
        assert_eq!(p.name, "softirq_enter");
        assert!(p.attached);
        assert_eq!(p.intent, None);
        assert_eq!(p.verdict, ProbeVerdict::Ok);
    }

    #[test]
    fn sampler_status_carries_health() {
        let s = SamplerStatus {
            name: "cpu_usage".into(),
            state: SamplerState::Active,
            health: Some(SamplerHealth::Unsupported),
            programs: Vec::new(),
            runtime_degraded: Vec::new(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""health":"unsupported""#));
        let back: SamplerStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn old_sampler_status_without_health_still_deserializes() {
        let json = r#"{"name":"gpu","state":"disabled"}"#;
        let s: SamplerStatus = serde_json::from_str(json).unwrap();
        assert_eq!(s.name, "gpu");
        assert_eq!(s.health, None);
        assert!(s.programs.is_empty());
    }

    #[test]
    fn agent_status_round_trips() {
        let s = AgentStatus {
            version: "5.15.1-alpha.2".into(),
            producer_epoch: "11111111-2222-4333-8444-555555555555".into(),
            clock_anchor_wall_ns: 0,
            uptime_seconds: 11532,
            ttl_seconds: 60,
            sample_interval_ms: None,
            subscribers: 0,
            samplers: vec![SamplerStatus {
                name: "cpu_usage".into(),
                state: SamplerState::Active,
                health: Some(SamplerHealth::Healthy),
                programs: Vec::new(),
                runtime_degraded: Vec::new(),
            }],
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""version":"5.15.1-alpha.2""#));
        assert!(json.contains(r#""uptime_seconds":11532"#));
        assert!(json.contains(r#""ttl_seconds":60"#));
        let back: AgentStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back.uptime_seconds, 11532);
        assert_eq!(back.samplers.len(), 1);
    }

    fn status_of(name: &str) -> SamplerStatus {
        snapshot().into_iter().find(|s| s.name == name).unwrap()
    }

    // Each test uses its own sampler name: the registry is process-global and
    // tests run in parallel.
    #[test]
    fn a_runtime_condition_degrades_a_running_sampler_until_cleared() {
        const NAME: &str = "test_runtime_degrades";
        set_active_if_absent(NAME);
        assert_eq!(status_of(NAME).health, None);

        set_runtime_degraded(NAME, "b_second", "second".into());
        set_runtime_degraded(NAME, "a_first", "first".into());
        let s = status_of(NAME);
        assert_eq!(s.health, Some(SamplerHealth::Degraded));
        let reasons: Vec<&str> = s
            .runtime_degraded
            .iter()
            .map(|c| c.reason.as_str())
            .collect();
        assert_eq!(reasons, ["first", "second"]);

        set_runtime_degraded(NAME, "a_first", "replaced".into());
        assert_eq!(status_of(NAME).runtime_degraded[0].reason, "replaced");

        clear_runtime_degraded(NAME, "a_first");
        assert_eq!(status_of(NAME).health, Some(SamplerHealth::Degraded));
        clear_runtime_degraded(NAME, "b_second");
        let s = status_of(NAME);
        assert_eq!(s.health, None);
        assert!(s.runtime_degraded.is_empty());
        // Clearing what is not set does nothing.
        clear_runtime_degraded(NAME, "b_second");
    }

    #[test]
    fn a_replaced_record_keeps_its_runtime_conditions() {
        const NAME: &str = "test_runtime_survives_replace";
        set_unsupported(NAME, "waiting".into());
        set_runtime_degraded(NAME, "stuck", "stuck".into());
        // Not running: the condition is attached and the health left alone.
        let s = status_of(NAME);
        assert_eq!(s.health, Some(SamplerHealth::Unsupported));
        assert_eq!(s.runtime_degraded.len(), 1);

        set_active_with_programs(NAME, SamplerHealth::Healthy, Vec::new());
        assert_eq!(status_of(NAME).health, Some(SamplerHealth::Degraded));
        set_pmu_limited(NAME, 2, 4);
        assert_eq!(status_of(NAME).health, Some(SamplerHealth::Degraded));
        clear_runtime_degraded(NAME, "stuck");
    }

    #[test]
    fn a_runtime_condition_does_not_mask_a_failed_health() {
        const NAME: &str = "test_runtime_failed";
        set_active_with_programs(NAME, SamplerHealth::Failed, Vec::new());
        set_runtime_degraded(NAME, "stuck", "stuck".into());
        assert_eq!(status_of(NAME).health, Some(SamplerHealth::Failed));
        clear_runtime_degraded(NAME, "stuck");
    }

    #[test]
    fn runtime_conditions_serialize_only_when_present() {
        let mut s = SamplerStatus {
            name: "filesystem".into(),
            state: SamplerState::Active,
            health: Some(SamplerHealth::Healthy),
            programs: Vec::new(),
            runtime_degraded: Vec::new(),
        };
        assert!(!serde_json::to_string(&s)
            .unwrap()
            .contains("runtime_degraded"));
        s.runtime_degraded.push(RuntimeCondition {
            condition: "stuck_sweep".into(),
            reason: "a sweep has been running for more than 3s".into(),
        });
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains(r#""runtime_degraded":[{"condition":"stuck_sweep","#),
            "{json}"
        );
        let back: SamplerStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
