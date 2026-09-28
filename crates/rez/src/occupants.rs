//! The occupant stream of a long table: its format lives in
//! `metriken-segment` and the relabel that applies it in `metriken-query`
//! (see metriken's `docs/journal/2026-09-28-high-cardinality-stack.md`).
//! Re-exported here under the path the reader uses.

pub use metriken_query::long::{OccupantLabels, OCCUPANT_LABEL};
pub use metriken_segment::occupants::*;
