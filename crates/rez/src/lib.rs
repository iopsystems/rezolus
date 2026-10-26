//! Reading recordings: [`reader::RezReader`] and [`live::LiveReader`],
//! which implement `metriken_query::MetricsSource` over a dendro archive,
//! and [`container`], which names a file by content and refuses one that is
//! not a dendro archive. The `.rez` format modules are metriken-storage's
//! (feature `rez`), re-exported for `recording upgrade`, which converts the
//! formats earlier releases wrote.

pub mod container;
#[cfg(any(all(test, feature = "write"), feature = "test-support"))]
pub mod fixtures;
pub mod live;
pub mod reader;

#[cfg(feature = "write")]
pub use metriken_storage::rez::to_dendro;
pub use metriken_storage::rez::{catalog, rez, rez_sqlite, wal, wire};
pub use metriken_storage::{schema, window};

/// The occupant stream of a long table, and the relabel that applies it.
pub mod occupants {
    pub use metriken_storage::long::{OccupantLabels, OCCUPANT_LABEL};
    pub use metriken_storage::occupants::*;
}
