//! Structured feature extraction and assessment for recordings.
//!
//! Turns a recording into a deterministic, versioned *overview record* of
//! Rezolus-native features (`record`), and defines the *assessment* schema an
//! agent emits over that record (`assessment`). See
//! `docs/superpowers/specs/2026-07-24-recording-assessment-extraction-design.md`.

// Phases 1-2 ship schemas + extraction with no runtime consumers; the
// MCP/CLI front door (Phase 3) wires them in. Remove once that lands.
#![allow(dead_code)]

pub mod assessment;
pub mod extract;
pub mod record;

#[allow(unused_imports)]
pub use assessment::{
    Assessment, Confidence, DataQuality, EvidenceRef, Finding, FindingKind, Overall, OverallStatus,
    Priority, TieredFinding, ASSESSMENT_SCHEMA_VERSION,
};

#[allow(unused_imports)]
pub use record::{
    AnalysisStatus, AnomalyFeature, Consumer, Context, CorrelationFeature, Coverage, DetailTier,
    MetricFeatures, NoiseSummary, OverviewRecord, Promotion, Rankings, RegimeShiftFeature,
    Selection, Stats, UncertaintySummary, RECORD_SCHEMA_VERSION,
};

// No runtime caller until Phase 3 wires the MCP/CLI front door.
#[allow(unused_imports)]
pub use extract::extract;
