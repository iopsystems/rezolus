//! The `.rez` archive format, now `metriken-storage`'s (feature `rez`),
//! re-exported under the paths rezolus uses. What stays here is what
//! implements `metriken_query::MetricsSource`: [`reader::RezReader`] and
//! [`live::LiveReader`].

#[cfg(feature = "test-support")]
pub mod fixtures;
pub mod live;
pub mod reader;

#[cfg(any(test, feature = "test-support"))]
pub use metriken_storage::rez::rez_stream;
pub use metriken_storage::rez::{
    catalog, parquet_ingest, rez, rez_sqlite, rez_v3_rewrite, seal_policy, wal, wire,
};
#[cfg(feature = "write")]
pub use metriken_storage::rez::{rez_v3_writer, to_dendro};
pub use metriken_storage::{schema, window};

/// The occupant stream of a long table, and the relabel that applies it.
pub mod occupants {
    pub use metriken_storage::long::{OccupantLabels, OCCUPANT_LABEL};
    pub use metriken_storage::occupants::*;
}
