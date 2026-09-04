//! Holds the capture stores — a mandatory `baseline` anchor plus any number of
//! other captures — and their per-capture metadata. All cross-capture
//! composition lives outside this module; the registry is intentionally dumb
//! about comparison.
//!
//! **Baseline is the anchor, the rest are a collection.** `baseline` is always
//! present and keeps the wire-stable id `"baseline"` (absence of a `?capture=`
//! param resolves to it). Every other capture lives in `others`, keyed by a
//! string id; the first of them is conventionally `"experiment"`, so the
//! two-capture A/B path and every existing URL keep working unchanged while the
//! store itself holds N. See `docs/superpowers/plans/2026-09-03-n-way-compare.md`.

use std::sync::Arc;

use metriken_query::MetricsSource;
use parking_lot::RwLock;

/// The conventional id of the first non-anchor capture — the "experiment" of a
/// classic A/B, and what `CaptureId::Experiment` and legacy `?capture=experiment`
/// URLs resolve to.
pub const EXPERIMENT_ID: &str = "experiment";

/// The anchor's id. Absence of `?capture=` resolves here; never renamed.
pub const BASELINE_ID: &str = "baseline";

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
            BASELINE_ID => Some(CaptureId::Baseline),
            EXPERIMENT_ID => Some(CaptureId::Experiment),
            _ => None,
        }
    }

    /// Parse an optional `?capture=…` query param. Missing or unknown
    /// values resolve to the default (Baseline).
    pub fn parse_opt(s: Option<&str>) -> Self {
        s.and_then(Self::parse).unwrap_or_default()
    }

    /// The wire id for this capture.
    pub fn id(self) -> &'static str {
        match self {
            CaptureId::Baseline => BASELINE_ID,
            CaptureId::Experiment => EXPERIMENT_ID,
        }
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
    /// Purely cosmetic — internal identifiers stay id strings.
    pub alias: RwLock<Option<String>>,
}

impl CaptureSlot {
    fn new(
        data: Arc<dyn MetricsSource>,
        systeminfo: Option<String>,
        file_metadata: Option<String>,
        alias: Option<String>,
    ) -> Self {
        Self {
            data: RwLock::new(data),
            systeminfo: RwLock::new(systeminfo),
            file_metadata: RwLock::new(file_metadata),
            alias: RwLock::new(alias),
        }
    }
}

pub struct CaptureRegistry {
    baseline: CaptureSlot,
    /// Non-anchor captures, in attach order, keyed by wire id. The first is
    /// conventionally `EXPERIMENT_ID`. A `Vec` rather than a map because the
    /// order is the display order and N is small (a handful of arms).
    others: RwLock<Vec<(String, CaptureSlot)>>,
}

impl CaptureRegistry {
    pub fn new(
        baseline_data: Arc<dyn MetricsSource>,
        baseline_systeminfo: Option<String>,
        baseline_file_metadata: Option<String>,
        baseline_alias: Option<String>,
    ) -> Self {
        Self {
            baseline: CaptureSlot::new(
                baseline_data,
                baseline_systeminfo,
                baseline_file_metadata,
                baseline_alias,
            ),
            others: RwLock::new(Vec::new()),
        }
    }

    /// Run `f` against the slot for `id`, or return `None` if no such capture.
    /// The single read-locking primitive the id-keyed accessors share.
    fn with_slot<T>(&self, id: &str, f: impl FnOnce(&CaptureSlot) -> T) -> Option<T> {
        if id == BASELINE_ID {
            return Some(f(&self.baseline));
        }
        self.others
            .read()
            .iter()
            .find(|(other, _)| other == id)
            .map(|(_, slot)| f(slot))
    }

    /// Every attached capture's id, anchor first, in display order.
    ///
    /// Consumed by the `/api/v1/captures` listing (and the N-way frontend).
    pub fn capture_ids(&self) -> Vec<String> {
        let mut ids = vec![BASELINE_ID.to_string()];
        ids.extend(self.others.read().iter().map(|(id, _)| id.clone()));
        ids
    }

    pub fn get_by_id(&self, id: &str) -> Option<Arc<dyn MetricsSource>> {
        self.with_slot(id, |slot| slot.data.read().clone())
    }

    pub fn get(&self, id: CaptureId) -> Option<Arc<dyn MetricsSource>> {
        self.get_by_id(id.id())
    }

    /// Returns the display filename for the given capture.
    /// Reads it from the data source's `filename()` method — no separate
    /// storage needed since the reader/store carries the name.
    pub fn filename(&self, id: CaptureId) -> String {
        self.filename_by_id(id.id())
    }

    pub fn filename_by_id(&self, id: &str) -> String {
        self.with_slot(id, |slot| slot.data.read().filename())
            .flatten()
            .unwrap_or_default()
    }

    pub fn systeminfo(&self, id: CaptureId) -> Option<String> {
        self.systeminfo_by_id(id.id())
    }

    pub fn systeminfo_by_id(&self, id: &str) -> Option<String> {
        self.with_slot(id, |slot| slot.systeminfo.read().clone())
            .flatten()
    }

    pub fn file_metadata(&self, id: CaptureId) -> Option<String> {
        self.file_metadata_by_id(id.id())
    }

    pub fn file_metadata_by_id(&self, id: &str) -> Option<String> {
        self.with_slot(id, |slot| slot.file_metadata.read().clone())
            .flatten()
    }

    /// Display alias for the given capture, when one was provided on the
    /// command line (or via attach). None = fall back to the identifier
    /// name on the UI side.
    pub fn alias(&self, id: CaptureId) -> Option<String> {
        self.alias_by_id(id.id())
    }

    pub fn alias_by_id(&self, id: &str) -> Option<String> {
        self.with_slot(id, |slot| slot.alias.read().clone())
            .flatten()
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

    /// Attach (or replace) the conventional single experiment — the classic
    /// A/B second arm. Kept as its own method because it is the two-capture
    /// path every existing caller uses; `attach_capture` is the N-way form.
    pub fn attach_experiment(
        &self,
        data: Arc<dyn MetricsSource>,
        systeminfo: Option<String>,
        file_metadata: Option<String>,
        alias: Option<String>,
    ) {
        self.attach_capture(EXPERIMENT_ID, data, systeminfo, file_metadata, alias);
    }

    /// Attach (or replace, by id) a non-anchor capture. Replacing preserves the
    /// capture's position, so re-uploading one arm of a comparison does not
    /// reorder the rest.
    pub fn attach_capture(
        &self,
        id: &str,
        data: Arc<dyn MetricsSource>,
        systeminfo: Option<String>,
        file_metadata: Option<String>,
        alias: Option<String>,
    ) {
        let slot = CaptureSlot::new(data, systeminfo, file_metadata, alias);
        let mut others = self.others.write();
        match others.iter_mut().find(|(other, _)| other == id) {
            Some((_, existing)) => *existing = slot,
            None => others.push((id.to_string(), slot)),
        }
    }

    /// Remove the conventional single experiment, if present.
    pub fn detach_experiment(&self) {
        self.detach_capture(EXPERIMENT_ID);
    }

    /// Remove a non-anchor capture by id (no-op if absent; baseline cannot be
    /// detached — it is the anchor).
    pub fn detach_capture(&self, id: &str) {
        self.others.write().retain(|(other, _)| other != id);
    }

    pub fn experiment_attached(&self) -> bool {
        self.with_slot(EXPERIMENT_ID, |_| ()).is_some()
    }

    /// How many non-anchor captures are attached. Consumed by PR 2+ (see
    /// `capture_ids`).
    #[allow(dead_code)]
    pub fn other_count(&self) -> usize {
        self.others.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> Arc<dyn MetricsSource> {
        Arc::new(
            metriken_query::MemoryStore::builder()
                .filename(name)
                .build(),
        )
    }

    fn registry() -> CaptureRegistry {
        CaptureRegistry::new(store("base.parquet"), None, None, Some("base".into()))
    }

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
        let reg = registry();
        assert!(!reg.experiment_attached());
        reg.attach_experiment(store("exp.parquet"), None, None, Some("exp".into()));
        assert!(reg.experiment_attached());
        reg.detach_experiment();
        assert!(!reg.experiment_attached());
    }

    /// The two-capture path is unchanged: `CaptureId::Experiment` and the
    /// `experiment` string id resolve to the same slot, and detach clears it.
    #[test]
    fn the_experiment_id_and_enum_resolve_to_one_slot() {
        let reg = registry();
        reg.attach_experiment(store("exp.parquet"), None, None, Some("valkey".into()));
        assert_eq!(reg.alias(CaptureId::Experiment).as_deref(), Some("valkey"));
        assert_eq!(reg.alias_by_id(EXPERIMENT_ID).as_deref(), Some("valkey"));
        assert_eq!(reg.filename(CaptureId::Experiment), "exp.parquet");
        assert!(reg.get(CaptureId::Experiment).is_some());
    }

    /// The store holds N. Attaching beyond the experiment keeps each capture
    /// reachable by its own id, anchor first, in attach order.
    #[test]
    fn the_store_holds_more_than_two_captures() {
        let reg = registry();
        reg.attach_experiment(store("a.parquet"), None, None, Some("redis".into()));
        reg.attach_capture(
            "valkey",
            store("b.parquet"),
            None,
            None,
            Some("valkey".into()),
        );
        reg.attach_capture(
            "envoy",
            store("c.parquet"),
            None,
            None,
            Some("envoy".into()),
        );

        assert_eq!(
            reg.capture_ids(),
            vec![
                "baseline".to_string(),
                "experiment".to_string(),
                "valkey".to_string(),
                "envoy".to_string(),
            ],
        );
        // Each resolves to its own data, by value — not one shadowing another.
        assert_eq!(reg.filename_by_id("baseline"), "base.parquet");
        assert_eq!(reg.filename_by_id("valkey"), "b.parquet");
        assert_eq!(reg.filename_by_id("envoy"), "c.parquet");
        assert_eq!(reg.alias_by_id("envoy").as_deref(), Some("envoy"));
        assert_eq!(reg.other_count(), 3);
    }

    /// Re-attaching a capture by id replaces it in place, so re-uploading one
    /// arm does not reorder the others.
    #[test]
    fn re_attaching_by_id_replaces_in_place() {
        let reg = registry();
        reg.attach_capture("redis", store("old.parquet"), None, None, None);
        reg.attach_capture("valkey", store("v.parquet"), None, None, None);
        reg.attach_capture("redis", store("new.parquet"), None, None, None);

        assert_eq!(
            reg.capture_ids(),
            vec![
                "baseline".to_string(),
                "redis".to_string(),
                "valkey".to_string()
            ],
        );
        assert_eq!(reg.filename_by_id("redis"), "new.parquet");
    }

    /// An unknown id resolves to nothing rather than the anchor — the caller
    /// asked for a capture that is not attached.
    #[test]
    fn an_unknown_id_is_absent_not_the_anchor() {
        let reg = registry();
        assert!(reg.get_by_id("nope").is_none());
        assert!(reg.alias_by_id("nope").is_none());
        assert_eq!(reg.filename_by_id("nope"), "");
    }
}
