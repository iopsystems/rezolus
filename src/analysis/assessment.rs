//! The assessment data model — the structured, actionable conclusion an agent
//! (or fine-tuned model) emits over an `OverviewRecord`.
//!
//! The whole assessment is `Finding`s in three buckets (`overall`, `findings`,
//! `ruled_out`) — one type, three roles, uniform grounding and validation.

/// Schema version for `Assessment`. Bump on any change to the assessment shape.
pub const ASSESSMENT_SCHEMA_VERSION: u32 = 1;
