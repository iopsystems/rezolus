//! Holds the two capture stores (baseline + optional experiment) plus their
//! per-capture metadata. All cross-capture composition lives outside this
//! module; the registry is intentionally dumb about comparison.

use std::sync::Arc;

use metriken_query::MetricsSource;
use parking_lot::RwLock;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureId {
    #[default]
    Baseline,
    Experiment,
}

impl CaptureId {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "baseline" => Some(CaptureId::Baseline),
            "experiment" => Some(CaptureId::Experiment),
            _ => None,
        }
    }

    /// Parse an optional `?capture=…` query param. Missing or unknown
    /// values resolve to the default (Baseline).
    pub fn parse_opt(s: Option<&str>) -> Self {
        s.and_then(Self::parse).unwrap_or_default()
    }
}

pub struct CaptureSlot {
    /// The data source behind a RwLock so it can be replaced on upload.
    /// The display filename is stored on the data source itself via
    /// `MetricsSource::filename()` — no separate field needed.
    pub data: RwLock<Arc<dyn MetricsSource>>,
    pub systeminfo: RwLock<Option<String>>,
    pub file_metadata: RwLock<Option<String>>,
    /// Optional display alias for this capture (e.g. "redis", "valkey").
    /// Purely cosmetic — internal identifiers stay "baseline"/"experiment".
    pub alias: RwLock<Option<String>>,
}

pub struct CaptureRegistry {
    baseline: CaptureSlot,
    experiment: RwLock<Option<CaptureSlot>>,
}

impl CaptureRegistry {
    pub fn new(
        baseline_data: Arc<dyn MetricsSource>,
        baseline_systeminfo: Option<String>,
        baseline_file_metadata: Option<String>,
        baseline_alias: Option<String>,
    ) -> Self {
        Self {
            baseline: CaptureSlot {
                data: RwLock::new(baseline_data),
                systeminfo: RwLock::new(baseline_systeminfo),
                file_metadata: RwLock::new(baseline_file_metadata),
                alias: RwLock::new(baseline_alias),
            },
            experiment: RwLock::new(None),
        }
    }

    pub fn get(&self, id: CaptureId) -> Option<Arc<dyn MetricsSource>> {
        match id {
            CaptureId::Baseline => Some(self.baseline.data.read().clone()),
            CaptureId::Experiment => self
                .experiment
                .read()
                .as_ref()
                .map(|slot| slot.data.read().clone()),
        }
    }

    /// Returns the display filename for the given capture.
    /// Reads it from the data source's `filename()` method — no separate
    /// storage needed since the reader/store carries the name.
    pub fn filename(&self, id: CaptureId) -> String {
        match id {
            CaptureId::Baseline => self.baseline.data.read().filename().unwrap_or_default(),
            CaptureId::Experiment => self
                .experiment
                .read()
                .as_ref()
                .and_then(|slot| slot.data.read().filename())
                .unwrap_or_default(),
        }
    }

    pub fn systeminfo(&self, id: CaptureId) -> Option<String> {
        match id {
            CaptureId::Baseline => self.baseline.systeminfo.read().clone(),
            CaptureId::Experiment => self
                .experiment
                .read()
                .as_ref()
                .and_then(|slot| slot.systeminfo.read().clone()),
        }
    }

    pub fn file_metadata(&self, id: CaptureId) -> Option<String> {
        match id {
            CaptureId::Baseline => self.baseline.file_metadata.read().clone(),
            CaptureId::Experiment => self
                .experiment
                .read()
                .as_ref()
                .and_then(|slot| slot.file_metadata.read().clone()),
        }
    }

    /// Display alias for the given capture, when one was provided on the
    /// command line (or via attach). None = fall back to the identifier
    /// name on the UI side.
    pub fn alias(&self, id: CaptureId) -> Option<String> {
        match id {
            CaptureId::Baseline => self.baseline.alias.read().clone(),
            CaptureId::Experiment => self
                .experiment
                .read()
                .as_ref()
                .and_then(|slot| slot.alias.read().clone()),
        }
    }

    /// Overwrite the baseline slot's alias.
    pub fn set_baseline_alias(&self, alias: Option<String>) {
        *self.baseline.alias.write() = alias;
    }

    /// Overwrite the baseline slot's systeminfo.
    pub fn set_baseline_systeminfo(&self, systeminfo: Option<String>) {
        *self.baseline.systeminfo.write() = systeminfo;
    }

    /// Overwrite the baseline slot's file_metadata.
    pub fn set_baseline_file_metadata(&self, file_metadata: Option<String>) {
        *self.baseline.file_metadata.write() = file_metadata;
    }

    /// Replace the baseline data store. The display filename is carried on
    /// the data source itself via `MetricsSource::filename()`.
    pub fn set_baseline_data(&self, data: Arc<dyn MetricsSource>) {
        *self.baseline.data.write() = data;
    }

    pub fn attach_experiment(
        &self,
        data: Arc<dyn MetricsSource>,
        systeminfo: Option<String>,
        file_metadata: Option<String>,
        alias: Option<String>,
    ) {
        *self.experiment.write() = Some(CaptureSlot {
            data: RwLock::new(data),
            systeminfo: RwLock::new(systeminfo),
            file_metadata: RwLock::new(file_metadata),
            alias: RwLock::new(alias),
        });
    }

    pub fn detach_experiment(&self) {
        *self.experiment.write() = None;
    }

    pub fn experiment_attached(&self) -> bool {
        self.experiment.read().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_capture_id() {
        assert_eq!(CaptureId::parse("baseline"), Some(CaptureId::Baseline));
        assert_eq!(CaptureId::parse("experiment"), Some(CaptureId::Experiment));
        assert_eq!(CaptureId::parse("unknown"), None);
    }

    #[test]
    fn default_capture_is_baseline() {
        assert_eq!(CaptureId::default(), CaptureId::Baseline);
    }

    #[test]
    fn registry_experiment_attached_toggles() {
        // Compile-only smoke: the types exist and the method signatures are reachable.
        #[allow(dead_code)]
        fn _compile_only(reg: &CaptureRegistry) -> bool {
            reg.experiment_attached()
        }
    }
}
