//! Structured feature extraction and assessment for recordings.
//!
//! Turns a recording into a deterministic, versioned *overview record* of
//! Rezolus-native features (`record`), and defines the *assessment* schema an
//! agent emits over that record (`assessment`). See
//! `docs/superpowers/specs/2026-07-24-recording-assessment-extraction-design.md`.

// Phase 1 ships schemas with no runtime consumers; extraction (Phase 2) and the
// MCP/CLI front door (Phase 3) wire these in. Remove once Phase 2/3 add runtime consumers.
#![allow(dead_code)]

pub mod assessment;
pub mod record;

#[allow(unused_imports)]
pub use assessment::{
    Assessment, Confidence, DataQuality, EvidenceRef, Finding, FindingKind, Overall, OverallStatus,
    Priority, TieredFinding, ASSESSMENT_SCHEMA_VERSION,
};
