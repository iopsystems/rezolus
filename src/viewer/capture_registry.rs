//! Holds the two capture stores (baseline + optional experiment) plus
//! their per-capture metadata. All cross-capture composition lives
//! outside this module; the registry is intentionally dumb about
//! comparison.
//!
//! A capture slot is either SQL-backed (`SqlCapture`, used by file /
//! upload / A-B paths) or live-backed (`LiveCapture` wrapping a
//! `LiveSource` for the live-agent ingest path). The two paths are
//! mutually exclusive per slot — see [`CaptureBackend`].

use std::sync::Arc;

use parking_lot::RwLock;

use super::live_capture::LiveCapture;
use super::sql_capture::SqlCapture;

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

/// Per-slot data store. Each slot is either SQL-backed (file / upload
/// / A-B captures) or live-backed (the live-agent ingest path
/// wrapping a `LiveSource`).
pub enum CaptureBackend {
    Sql(Arc<RwLock<SqlCapture>>),
    Live(Arc<RwLock<LiveCapture>>),
}

impl CaptureBackend {
    pub fn as_live(&self) -> Option<Arc<RwLock<LiveCapture>>> {
        match self {
            CaptureBackend::Live(live) => Some(live.clone()),
            CaptureBackend::Sql(_) => None,
        }
    }

    pub fn as_sql(&self) -> Option<Arc<RwLock<SqlCapture>>> {
        match self {
            CaptureBackend::Sql(cap) => Some(cap.clone()),
            CaptureBackend::Live(_) => None,
        }
    }
}

pub struct CaptureSlot {
    /// Data store. Immutable for the lifetime of the slot — variant
    /// swaps (live→sql via upload, sql→sql via re-upload) rebuild
    /// the whole slot via `RwLock<Option<CaptureSlot>>` on the
    /// registry, so the inner variant doesn't need its own lock.
    /// `as_live`/`as_sql` just clone the inner `Arc<RwLock<...>>`
    /// out — the variant data is locked at the next level down.
    pub backend: CaptureBackend,
    pub systeminfo: RwLock<Option<String>>,
    pub file_metadata: RwLock<Option<String>>,
    /// Optional display alias for this capture (e.g. "redis", "valkey").
    /// Purely cosmetic — internal identifiers stay "baseline"/"experiment".
    pub alias: RwLock<Option<String>>,
}

pub struct CaptureRegistry {
    /// Baseline slot. `None` for upload-only mode pre-upload; gets
    /// populated by the first `replace_baseline_with_sql` (or set at
    /// construction for file/live-mode inits).
    baseline: RwLock<Option<CaptureSlot>>,
    experiment: RwLock<Option<CaptureSlot>>,
}

impl CaptureRegistry {
    /// Unified factory. `baseline = None` initialises an upload-only
    /// registry; `Some(backend)` wraps it in a fresh `CaptureSlot`
    /// with empty metadata (set systeminfo / file_metadata / alias
    /// afterwards via the dedicated setters). The experiment slot
    /// starts `None` regardless — call `attach_experiment_sql` to
    /// populate.
    pub fn new(baseline: Option<CaptureBackend>) -> Self {
        Self {
            baseline: RwLock::new(baseline.map(|backend| CaptureSlot {
                backend,
                systeminfo: RwLock::new(None),
                file_metadata: RwLock::new(None),
                alias: RwLock::new(None),
            })),
            experiment: RwLock::new(None),
        }
    }

    /// Returns the baseline/experiment slot's LiveCapture handle, if
    /// the slot is live-backed. SQL-backed slots and an unpopulated
    /// baseline (upload-only pre-upload) return `None`.
    pub fn get_live(&self, id: CaptureId) -> Option<Arc<RwLock<LiveCapture>>> {
        match id {
            CaptureId::Baseline => self.baseline.read().as_ref()?.backend.as_live(),
            CaptureId::Experiment => self.experiment.read().as_ref()?.backend.as_live(),
        }
    }

    /// Returns the baseline/experiment slot's SqlCapture handle, if
    /// the slot is SQL-backed. Live-backed slots and unpopulated
    /// slots return `None`.
    pub fn get_sql(&self, id: CaptureId) -> Option<Arc<RwLock<SqlCapture>>> {
        match id {
            CaptureId::Baseline => self.baseline.read().as_ref()?.backend.as_sql(),
            CaptureId::Experiment => self.experiment.read().as_ref()?.backend.as_sql(),
        }
    }

    /// Install or replace the baseline slot with a SqlCapture-backed
    /// one. Used by upload + URL-load ingest paths. Existing metadata
    /// (systeminfo / file_metadata / alias) is dropped; callers stamp
    /// fresh values afterward via the setters.
    pub fn replace_baseline_with_sql(&self, capture: SqlCapture) -> Arc<RwLock<SqlCapture>> {
        let handle = Arc::new(RwLock::new(capture));
        *self.baseline.write() = Some(CaptureSlot {
            backend: CaptureBackend::Sql(handle.clone()),
            systeminfo: RwLock::new(None),
            file_metadata: RwLock::new(None),
            alias: RwLock::new(None),
        });
        handle
    }

    /// Reset the baseline LiveCapture in place (live-mode reset
    /// handler). Panics if the baseline is SQL-backed or unpopulated
    /// — live reset doesn't make sense outside an established live
    /// session.
    pub fn reset_baseline_live(&self, live: LiveCapture) {
        let guard = self.baseline.read();
        let slot = guard
            .as_ref()
            .expect("reset_baseline_live called with no baseline");
        match &slot.backend {
            CaptureBackend::Live(handle) => {
                *handle.write() = live;
            }
            CaptureBackend::Sql(_) => {
                panic!("reset_baseline_live called on a SQL-backed capture");
            }
        }
        drop(guard);
    }

    pub fn systeminfo(&self, id: CaptureId) -> Option<String> {
        let guard = match id {
            CaptureId::Baseline => self.baseline.read(),
            CaptureId::Experiment => self.experiment.read(),
        };
        guard.as_ref().and_then(|slot| slot.systeminfo.read().clone())
    }

    pub fn file_metadata(&self, id: CaptureId) -> Option<String> {
        let guard = match id {
            CaptureId::Baseline => self.baseline.read(),
            CaptureId::Experiment => self.experiment.read(),
        };
        guard
            .as_ref()
            .and_then(|slot| slot.file_metadata.read().clone())
    }

    /// Display alias for the given capture, when one was provided on
    /// the command line (or via attach). `None` = fall back to the
    /// identifier name on the UI side (or no baseline loaded).
    pub fn alias(&self, id: CaptureId) -> Option<String> {
        let guard = match id {
            CaptureId::Baseline => self.baseline.read(),
            CaptureId::Experiment => self.experiment.read(),
        };
        guard.as_ref().and_then(|slot| slot.alias.read().clone())
    }

    /// Overwrite the baseline slot's alias. Silently no-ops if no
    /// baseline is loaded (the caller is expected to load one first).
    pub fn set_baseline_alias(&self, alias: Option<String>) {
        if let Some(slot) = self.baseline.read().as_ref() {
            *slot.alias.write() = alias;
        }
    }

    /// Overwrite the baseline slot's systeminfo. No-op when no
    /// baseline is loaded.
    pub fn set_baseline_systeminfo(&self, systeminfo: Option<String>) {
        if let Some(slot) = self.baseline.read().as_ref() {
            *slot.systeminfo.write() = systeminfo;
        }
    }

    /// Overwrite the baseline slot's file_metadata. No-op when no
    /// baseline is loaded.
    pub fn set_baseline_file_metadata(&self, file_metadata: Option<String>) {
        if let Some(slot) = self.baseline.read().as_ref() {
            *slot.file_metadata.write() = file_metadata;
        }
    }

    /// SQL-backed attach. Stores an `SqlCapture` in the experiment slot.
    /// Used by the file-mode HTTP attach handler.
    pub fn attach_experiment_sql(
        &self,
        capture: SqlCapture,
        systeminfo: Option<String>,
        file_metadata: Option<String>,
        alias: Option<String>,
    ) {
        *self.experiment.write() = Some(CaptureSlot {
            backend: CaptureBackend::Sql(Arc::new(RwLock::new(capture))),
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
        #[allow(dead_code)]
        fn _compile_only(reg: &CaptureRegistry) -> bool {
            reg.experiment_attached()
        }
    }
}
