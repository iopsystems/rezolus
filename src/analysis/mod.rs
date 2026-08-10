//! Structured feature extraction and assessment for recordings.
//!
//! Turns a recording into a deterministic, versioned *overview record* of
//! Rezolus-native features (`record`), and defines the *assessment* schema an
//! agent emits over that record (`assessment`). See
//! `docs/superpowers/specs/2026-07-24-recording-assessment-extraction-design.md`.

// Assessment types have no runtime consumer until the Phase-4/5 label pipeline; validation is exercised by tests.
#[allow(dead_code)]
pub mod assessment;
pub mod extract;
pub mod record;

// External callers (src/mcp) reach these through the fully-qualified
// `crate::analysis::assessment::*` path, not this top-level re-export, so
// the alias itself is still unused outside this module.
#[allow(unused_imports)]
pub use assessment::{
    Assessment, Confidence, DataQuality, EvidenceRef, Finding, FindingKind, Overall, OverallStatus,
    Priority, TieredFinding, ASSESSMENT_SCHEMA_VERSION,
};

// Same as above: consumers use `crate::analysis::record::*` directly rather
// than this re-export.
#[allow(unused_imports)]
pub use record::{
    AnalysisStatus, AnomalyFeature, Consumer, Context, CorrelationFeature, Coverage, DetailTier,
    MetricFeatures, NoiseSummary, OverviewRecord, Promotion, Rankings, RegimeShiftFeature,
    Selection, Stats, UncertaintySummary, RECORD_SCHEMA_VERSION,
};

// The front door (src/mcp/mod.rs, src/mcp/server.rs) calls
// `crate::analysis::extract::extract` directly rather than through this
// re-export, so the alias itself is still unused.
#[allow(unused_imports)]
pub use extract::extract;
