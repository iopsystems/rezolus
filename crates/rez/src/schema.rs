//! The membership of one acquisition group, as the archive stores it: the
//! segment format's (`metriken_segment::schema`), re-exported under the path
//! rezolus uses. Its byte-for-byte agreement with the producer's type is
//! pinned in `metriken-exposition`.

pub use metriken_segment::schema::*;
