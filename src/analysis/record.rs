//! The overview-record data model — a deterministic, versioned summary of a
//! recording's Rezolus-native features. This is the input half of a training
//! example. Fields are a *curated* subset of the internal analysis structs, not
//! the heavy raw analyses.

/// Schema version for `OverviewRecord`. Bump on any change to extraction logic
/// or record shape so stored examples stay attributable to an extractor version.
pub const RECORD_SCHEMA_VERSION: u32 = 1;
