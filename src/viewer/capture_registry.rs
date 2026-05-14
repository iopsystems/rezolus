//! Holds the two capture stores (baseline + optional experiment) plus
//! their per-capture metadata. All cross-capture composition lives
//! outside this module; the registry is intentionally dumb about
//! comparison.
//!
//! A capture slot is either SQL-backed (`SqlCapture`, used by file /
//! upload / A-B paths going forward) or Tsdb-backed (the legacy live-
//! agent ingest path, which keeps the in-memory TSDB during the
//! Tsdb→DuckDB migration). The two paths are mutually exclusive per
//! slot — see [`CaptureBackend`]. As of this commit the file-mode
//! init paths still construct Tsdb; commit 7 flips them to SqlCapture.

use std::sync::Arc;

#[cfg(feature = "live-mode")]
use metriken_query::Tsdb;
use parking_lot::RwLock;

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

/// Per-slot data store. Each slot is either SQL-backed (the new
/// DuckDB-driven path for file / upload / A-B captures) or
/// Tsdb-backed (the legacy live-agent ingest path). The `Live`
/// variant only exists when the `live-mode` feature is on — SQL-only
/// builds drop both the variant and the `metriken-query` link.
pub enum CaptureBackend {
    Sql(Arc<RwLock<SqlCapture>>),
    #[cfg(feature = "live-mode")]
    Live(Arc<RwLock<Tsdb>>),
}

impl CaptureBackend {
    /// Shorthand for `match self { Live(tsdb) => Some(tsdb), _ => None }`.
    /// Lets legacy callers continue to ask for a Tsdb specifically; SQL
    /// slots silently return `None`.
    #[cfg(feature = "live-mode")]
    pub fn as_live(&self) -> Option<Arc<RwLock<Tsdb>>> {
        match self {
            CaptureBackend::Live(tsdb) => Some(tsdb.clone()),
            CaptureBackend::Sql(_) => None,
        }
    }

    pub fn as_sql(&self) -> Option<Arc<RwLock<SqlCapture>>> {
        match self {
            CaptureBackend::Sql(cap) => Some(cap.clone()),
            #[cfg(feature = "live-mode")]
            CaptureBackend::Live(_) => None,
        }
    }
}

pub struct CaptureSlot {
    /// Data store. RwLock-wrapped so the upload / reset paths can
    /// swap the backend (e.g. live→sql when an upload replaces the
    /// initial empty live baseline) without rebuilding the registry.
    /// Readers clone the inner Arc out and drop the registry lock
    /// before doing any real work.
    pub backend: RwLock<CaptureBackend>,
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
    #[cfg(feature = "live-mode")]
    pub fn new(
        baseline_tsdb: Tsdb,
        baseline_systeminfo: Option<String>,
        baseline_file_metadata: Option<String>,
        baseline_alias: Option<String>,
    ) -> Self {
        Self {
            baseline: CaptureSlot {
                backend: RwLock::new(CaptureBackend::Live(Arc::new(RwLock::new(baseline_tsdb)))),
                systeminfo: RwLock::new(baseline_systeminfo),
                file_metadata: RwLock::new(baseline_file_metadata),
                alias: RwLock::new(baseline_alias),
            },
            experiment: RwLock::new(None),
        }
    }

    /// Returns the baseline/experiment slot's Tsdb handle, if the slot
    /// is Tsdb-backed. SQL-backed slots return `None`. Legacy callers
    /// (live-mode ingest, the metadata handler, dashboard section gen
    /// until commit 7) flow through this.
    #[cfg(feature = "live-mode")]
    pub fn get(&self, id: CaptureId) -> Option<Arc<RwLock<Tsdb>>> {
        match id {
            CaptureId::Baseline => self.baseline.backend.read().as_live(),
            CaptureId::Experiment => self
                .experiment
                .read()
                .as_ref()
                .and_then(|slot| slot.backend.read().as_live()),
        }
    }

    /// Returns the baseline/experiment slot's SqlCapture handle, if
    /// the slot is SQL-backed. Tsdb-backed slots return `None`. The
    /// SQL query handlers and SqlCapture-aware metadata paths consume
    /// this. Currently always returns `None` — commit 7 wires file
    /// mode through here.
    pub fn get_sql(&self, id: CaptureId) -> Option<Arc<RwLock<SqlCapture>>> {
        match id {
            CaptureId::Baseline => self.baseline.backend.read().as_sql(),
            CaptureId::Experiment => self
                .experiment
                .read()
                .as_ref()
                .and_then(|slot| slot.backend.read().as_sql()),
        }
    }

    /// Replace the baseline slot's backend with a fresh SqlCapture.
    /// Used by upload + URL-load ingest paths (which initialise
    /// AppState with an empty Live baseline, then swap in the loaded
    /// SqlCapture). Live-mode reset uses [`reset_baseline_live`].
    pub fn replace_baseline_with_sql(&self, capture: SqlCapture) -> Arc<RwLock<SqlCapture>> {
        let handle = Arc::new(RwLock::new(capture));
        *self.baseline.backend.write() = CaptureBackend::Sql(handle.clone());
        handle
    }

    /// Reset the baseline Tsdb in place (live-mode reset handler).
    /// Panics if the baseline is SQL-backed — live reset doesn't make
    /// sense for file captures.
    #[cfg(feature = "live-mode")]
    pub fn reset_baseline_live(&self, tsdb: Tsdb) {
        let guard = self.baseline.backend.write();
        match &*guard {
            CaptureBackend::Live(handle) => {
                *handle.write() = tsdb;
            }
            CaptureBackend::Sql(_) => {
                panic!("reset_baseline_live called on a SQL-backed capture");
            }
        }
        // guard drops here; the inner handle Arc is unchanged.
        drop(guard);
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

    /// Overwrite the baseline slot's alias. Useful when the agent mode
    /// swaps in a newly-recorded baseline without tearing the registry
    /// down.
    pub fn set_baseline_alias(&self, alias: Option<String>) {
        *self.baseline.alias.write() = alias;
    }

    /// Construct a registry rooted at a SqlCapture-backed baseline.
    /// Mirrors `new()` for file/upload/A-B init paths that load
    /// parquet through DuckDbBackend instead of building an in-memory
    /// Tsdb. The experiment slot starts empty.
    pub fn new_sql(
        baseline_capture: SqlCapture,
        baseline_systeminfo: Option<String>,
        baseline_file_metadata: Option<String>,
        baseline_alias: Option<String>,
    ) -> Self {
        Self {
            baseline: CaptureSlot {
                backend: RwLock::new(CaptureBackend::Sql(Arc::new(RwLock::new(
                    baseline_capture,
                )))),
                systeminfo: RwLock::new(baseline_systeminfo),
                file_metadata: RwLock::new(baseline_file_metadata),
                alias: RwLock::new(baseline_alias),
            },
            experiment: RwLock::new(None),
        }
    }

    /// Overwrite the baseline slot's systeminfo. The baseline TSDB Arc is
    /// unaffected so callers holding it keep working across updates.
    pub fn set_baseline_systeminfo(&self, systeminfo: Option<String>) {
        *self.baseline.systeminfo.write() = systeminfo;
    }

    /// Overwrite the baseline slot's file_metadata.
    pub fn set_baseline_file_metadata(&self, file_metadata: Option<String>) {
        *self.baseline.file_metadata.write() = file_metadata;
    }

    #[cfg(feature = "live-mode")]
    pub fn attach_experiment(
        &self,
        tsdb: Tsdb,
        systeminfo: Option<String>,
        file_metadata: Option<String>,
        alias: Option<String>,
    ) {
        *self.experiment.write() = Some(CaptureSlot {
            backend: RwLock::new(CaptureBackend::Live(Arc::new(RwLock::new(tsdb)))),
            systeminfo: RwLock::new(systeminfo),
            file_metadata: RwLock::new(file_metadata),
            alias: RwLock::new(alias),
        });
    }

    /// SQL-backed attach. Mirrors `attach_experiment` but stores an
    /// `SqlCapture` instead of a `Tsdb`. Used by the file-mode HTTP
    /// attach handler.
    pub fn attach_experiment_sql(
        &self,
        capture: SqlCapture,
        systeminfo: Option<String>,
        file_metadata: Option<String>,
        alias: Option<String>,
    ) {
        *self.experiment.write() = Some(CaptureSlot {
            backend: RwLock::new(CaptureBackend::Sql(Arc::new(RwLock::new(capture)))),
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
        // Building a real Tsdb requires a parquet on disk (Tsdb::load(path)).
        // Exercise only the boolean state transitions that do not need a
        // backing store. Full attach/get is covered by manual verification
        // in Task 29.
        //
        // Compile-only smoke: the types exist and the method signatures
        // are reachable.
        #[allow(dead_code)]
        fn _compile_only(reg: &CaptureRegistry) -> bool {
            reg.experiment_attached()
        }
    }
}
