//! Structured feature extraction and assessment for recordings.
//!
//! Turns a recording into a deterministic, versioned *overview record* of
//! Rezolus-native features (`record`), and defines the *assessment* schema an
//! agent emits over that record (`assessment`). See
//! `docs/superpowers/specs/2026-07-24-recording-assessment-extraction-design.md`.

pub mod assessment;
pub mod record;
