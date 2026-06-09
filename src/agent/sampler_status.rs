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
}
