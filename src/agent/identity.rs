//! What a slot means: metriken's `SlotIdentity`, which writes an occupant's
//! labels and a minted `__uid__` onto every group its slot spans. The
//! snapshot builder reads them from there into each group's schema.
//!
//! Moved into metriken (`metriken::group::SlotIdentity`, phase 5a of
//! metriken's `docs/journal/2026-09-29-members-that-come-and-go.md`); this
//! module keeps the paths rezolus's call sites use.

pub use metriken::group::{SlotIdentity, UID_LABEL};
