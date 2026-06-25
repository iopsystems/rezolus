//! Process-global registry of per-sampler status, populated during agent
//! initialization and read by the `/samplers` HTTP endpoint and the recorder.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// Status of a single sampler.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SamplerStatus {
    pub name: String,
    #[serde(flatten)]
    pub state: SamplerState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub programs: Vec<ProgramStatus>,
}

/// Whether a sampler is running, disabled by config, or failed to initialize.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SamplerState {
    Active,
    Disabled,
    Failed { error: String },
}

/// Status of a single BPF program within a sampler.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProgramStatus {
    pub name: String,
    pub attached: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn registry() -> &'static Mutex<BTreeMap<&'static str, SamplerStatus>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<&'static str, SamplerStatus>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Record a sampler as config-disabled.
pub fn set_disabled(name: &'static str) {
    registry().lock().unwrap().insert(
        name,
        SamplerStatus {
            name: name.to_string(),
            state: SamplerState::Disabled,
            programs: Vec::new(),
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
            programs: Vec::new(),
        },
    );
}

/// Record a sampler as active with its per-program attach detail.
/// Only called from the BPF builder, which is Linux-only.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn set_active_with_programs(name: &'static str, programs: Vec<ProgramStatus>) {
    registry().lock().unwrap().insert(
        name,
        SamplerStatus {
            name: name.to_string(),
            state: SamplerState::Active,
            programs,
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
            programs: Vec::new(),
        });
}

/// Snapshot of all sampler statuses, sorted by name (BTreeMap order).
pub fn snapshot() -> Vec<SamplerStatus> {
    registry().lock().unwrap().values().cloned().collect()
}

/// Author-declared meaning of a BPF program's attach.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "intent")]
pub enum ProbeIntent {
    /// Must attach on every supported kernel. Default for unclassified probes.
    Required,
    /// Per-device probe; expected to attach iff `module` (the sysfs module
    /// name, e.g. `virtio_net`) is bound to a present device.
    Driver { module: String },
}

impl Default for ProbeIntent {
    fn default() -> Self {
        ProbeIntent::Required
    }
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
    /// Driver probe for a module not present on this machine — silent.
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
/// `attached` is false. `module_present` is meaningful only for `Driver`.
pub fn classify_program(
    intent: &ProbeIntent,
    attached: bool,
    is_enoent: bool,
    module_present: bool,
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
            if module_present {
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
pub fn rollup_health(loaded_ok: bool, verdicts: &[ProbeVerdict]) -> SamplerHealth {
    if !loaded_ok {
        return SamplerHealth::Failed;
    }
    if verdicts.iter().any(|v| *v == ProbeVerdict::Broken) {
        return SamplerHealth::Degraded;
    }
    if verdicts.iter().any(|v| *v == ProbeVerdict::Unsupported) {
        return SamplerHealth::Unsupported;
    }
    SamplerHealth::Healthy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_serializes_with_flattened_state_and_programs() {
        let s = SamplerStatus {
            name: "cpu_usage".into(),
            state: SamplerState::Active,
            programs: vec![
                ProgramStatus {
                    name: "softirq_enter".into(),
                    attached: true,
                    error: None,
                },
                ProgramStatus {
                    name: "cpuacct_account_field_kprobe".into(),
                    attached: false,
                    error: Some("no kernel support (ENOENT)".into()),
                },
            ],
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
            programs: Vec::new(),
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
            programs: Vec::new(),
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
            module: "ena".into(),
        };
        assert_eq!(
            classify_program(&i, false, true, true),
            ProbeVerdict::Broken
        );
    }
    #[test]
    fn classify_driver_absent_not_attached_is_not_applicable() {
        let i = ProbeIntent::Driver {
            module: "ixgbe".into(),
        };
        assert_eq!(
            classify_program(&i, false, true, false),
            ProbeVerdict::NotApplicable
        );
    }
    #[test]
    fn classify_driver_attached_is_ok() {
        let i = ProbeIntent::Driver {
            module: "ena".into(),
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
}
