//! What the reader needs from an archive's container, so a `.rez` (v3,
//! SQLite) and a dendro archive read through the same `RezReader`.
//!
//! Both containers hold the same things under different names: a `.rez`
//! recording is a dendro source, a `.rez` table (a sampler, or
//! `sampler/group`) is a dendro stream, and both keep sealed parquet
//! segments, a WAL of unsealed rows and a store of caller rows. The trait is
//! the ten questions `RezReader` asks, answered in `.rez` terms: `u64`
//! timestamps, recordings, samplers. See
//! `docs/journal/2026-09-28-dendro-reader.md`.

use std::path::Path;

use crate::rez_sqlite::{RecordingMeta, RecordingRow, RezDb, SegmentMeta, Span, WalRow};

/// The read side of an archive's container.
///
/// `Send` so a catalog can sit behind the `Mutex` a byte-backed archive's
/// tables share: a SQLite connection is `Send` but not `Sync`.
pub trait Catalog: Send {
    /// Every recording, in id order.
    fn read_recordings(&self) -> Result<Vec<RecordingRow>, String>;

    /// Every table of a recording that has sealed segments or WAL rows.
    fn all_samplers(&self, recording_id: i64) -> Result<Vec<String>, String>;

    /// A table's sealed segments, `(seq, meta)`, oldest first. No bytes.
    fn read_segment_meta(
        &self,
        recording_id: i64,
        sampler: &str,
    ) -> Result<Vec<(u64, SegmentMeta)>, String>;

    /// One sealed segment's bytes; `None` when it no longer exists.
    fn read_segment_bytes(
        &self,
        recording_id: i64,
        sampler: &str,
        seq: u64,
    ) -> Result<Option<Vec<u8>>, String>;

    /// A table's WAL rows newer than its last sealed segment.
    fn live_wal(&self, recording_id: i64, sampler: &str) -> Result<Vec<WalRow>, String>;

    /// A table's sealed segment count and span, from the catalog.
    fn segment_span(&self, recording_id: i64, sampler: &str) -> Result<(u64, Span), String>;

    /// The span of [`live_wal`](Self::live_wal), from the catalog.
    fn live_wal_span(&self, recording_id: i64, sampler: &str) -> Result<Span, String>;

    /// Every stream a recording holds caller rows under.
    fn caller_row_streams(&self, recording_id: i64) -> Result<Vec<String>, String>;

    /// A stream's caller rows with `from <= ts <= to`, oldest first.
    fn read_caller_rows(
        &self,
        recording_id: i64,
        stream: &str,
        from: u64,
        to: u64,
    ) -> Result<Vec<(u64, Vec<u8>)>, String>;

    /// The timestamp of the newest caller row at or before `upto` that
    /// `pred` accepts.
    fn last_caller_row_at_or_before(
        &self,
        recording_id: i64,
        stream: &str,
        upto: u64,
        pred: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<Option<u64>, String>;
}

/// Which container a file is, decided by content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Rez,
    Dendro,
}

impl Container {
    /// A dendro archive by its header; anything else is left to the `.rez`
    /// checks.
    pub fn of_path(path: &Path) -> Result<Self, String> {
        match dendro::archive::sniff(path) {
            Ok(dendro::archive::Sniff::Stamped { .. }) => Ok(Container::Dendro),
            Ok(_) => Ok(Container::Rez),
            Err(e) => Err(e.to_string()),
        }
    }

    /// [`of_path`](Self::of_path) for an archive held as bytes.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        match dendro::archive::sniff_bytes(bytes) {
            dendro::archive::Sniff::Stamped { .. } => Container::Dendro,
            _ => Container::Rez,
        }
    }

    /// Open the file read-only as this container.
    pub fn open(self, path: &Path) -> Result<Box<dyn Catalog>, String> {
        Ok(match self {
            Container::Rez => Box::new(RezDb::open(path)?),
            Container::Dendro => Box::new(DendroCatalog(
                dendro::archive::Archive::open(path).map_err(|e| e.to_string())?,
            )),
        })
    }

    /// Open an archive held as bytes as this container.
    pub fn open_bytes(self, bytes: Vec<u8>) -> Result<Box<dyn Catalog>, String> {
        Ok(match self {
            Container::Rez => Box::new(RezDb::open_bytes(bytes)?),
            Container::Dendro => Box::new(DendroCatalog(
                dendro::archive::Archive::open_bytes(bytes).map_err(|e| e.to_string())?,
            )),
        })
    }
}

impl Catalog for RezDb {
    fn read_recordings(&self) -> Result<Vec<RecordingRow>, String> {
        RezDb::read_recordings(self)
    }

    fn all_samplers(&self, recording_id: i64) -> Result<Vec<String>, String> {
        RezDb::all_samplers(self, recording_id)
    }

    fn read_segment_meta(
        &self,
        recording_id: i64,
        sampler: &str,
    ) -> Result<Vec<(u64, SegmentMeta)>, String> {
        RezDb::read_segment_meta(self, recording_id, sampler)
    }

    fn read_segment_bytes(
        &self,
        recording_id: i64,
        sampler: &str,
        seq: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        RezDb::read_segment_bytes(self, recording_id, sampler, seq)
    }

    fn live_wal(&self, recording_id: i64, sampler: &str) -> Result<Vec<WalRow>, String> {
        RezDb::live_wal(self, recording_id, sampler)
    }

    fn segment_span(&self, recording_id: i64, sampler: &str) -> Result<(u64, Span), String> {
        RezDb::segment_span(self, recording_id, sampler)
    }

    fn live_wal_span(&self, recording_id: i64, sampler: &str) -> Result<Span, String> {
        RezDb::live_wal_span(self, recording_id, sampler)
    }

    fn caller_row_streams(&self, recording_id: i64) -> Result<Vec<String>, String> {
        RezDb::caller_row_streams(self, recording_id)
    }

    fn read_caller_rows(
        &self,
        recording_id: i64,
        stream: &str,
        from: u64,
        to: u64,
    ) -> Result<Vec<(u64, Vec<u8>)>, String> {
        RezDb::read_caller_rows(self, recording_id, stream, from, to)
    }

    fn last_caller_row_at_or_before(
        &self,
        recording_id: i64,
        stream: &str,
        upto: u64,
        pred: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<Option<u64>, String> {
        RezDb::last_caller_row_at_or_before(self, recording_id, stream, upto, pred)
    }
}

/// A dendro archive, read as the `.rez` it was converted from or will
/// replace.
pub struct DendroCatalog(pub dendro::archive::Archive);

/// A dendro timestamp as a `.rez` one. dendro stores `i64`; rezolus writes
/// nanoseconds since the epoch, which are never negative, so a negative one
/// is an archive this reader does not understand.
fn ts(t: i64) -> Result<u64, String> {
    u64::try_from(t).map_err(|_| format!("negative timestamp {t} in a dendro archive"))
}

/// A `.rez` timestamp as a dendro bound. Above `i64::MAX` there is nothing
/// to find, so the bound saturates.
fn bound(t: u64) -> i64 {
    i64::try_from(t).unwrap_or(i64::MAX)
}

fn span(s: dendro::archive::Span) -> Result<Span, String> {
    Ok(Span {
        rows: s.rows,
        first_ts: s.first_ts.map(ts).transpose()?,
        last_ts: s.last_ts.map(ts).transpose()?,
    })
}

fn err(e: dendro::Error) -> String {
    e.to_string()
}

impl Catalog for DendroCatalog {
    fn read_recordings(&self) -> Result<Vec<RecordingRow>, String> {
        self.0
            .read_sources()
            .map_err(err)?
            .into_iter()
            .map(|s| {
                Ok(RecordingRow {
                    id: s.id,
                    meta: RecordingMeta {
                        labels: s.meta.labels,
                        metadata: s.meta.metadata,
                        clock_anchor_wall_ns: ts(s.meta.clock_anchor_wall_ns)?,
                    },
                    complete: s.complete,
                })
            })
            .collect()
    }

    fn all_samplers(&self, recording_id: i64) -> Result<Vec<String>, String> {
        self.0.all_streams(recording_id).map_err(err)
    }

    fn read_segment_meta(
        &self,
        recording_id: i64,
        sampler: &str,
    ) -> Result<Vec<(u64, SegmentMeta)>, String> {
        self.0
            .read_segment_meta(recording_id, sampler)
            .map_err(err)?
            .into_iter()
            .map(|(seq, m)| {
                Ok((
                    seq,
                    SegmentMeta {
                        rows: m.rows,
                        first_ts: ts(m.first_ts)?,
                        last_ts: ts(m.last_ts)?,
                    },
                ))
            })
            .collect()
    }

    fn read_segment_bytes(
        &self,
        recording_id: i64,
        sampler: &str,
        seq: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        self.0
            .read_segment_bytes(recording_id, sampler, seq)
            .map_err(err)
    }

    fn live_wal(&self, recording_id: i64, sampler: &str) -> Result<Vec<WalRow>, String> {
        self.0
            .live_wal(recording_id, sampler)
            .map_err(err)?
            .into_iter()
            .map(|r| {
                Ok(WalRow {
                    sampler: r.stream,
                    ts: ts(r.ts)?,
                    wall_offset: r.wall_offset,
                    row: r.row,
                })
            })
            .collect()
    }

    fn segment_span(&self, recording_id: i64, sampler: &str) -> Result<(u64, Span), String> {
        let (n, s) = self.0.segment_span(recording_id, sampler).map_err(err)?;
        Ok((n, span(s)?))
    }

    fn live_wal_span(&self, recording_id: i64, sampler: &str) -> Result<Span, String> {
        span(self.0.live_wal_span(recording_id, sampler).map_err(err)?)
    }

    fn caller_row_streams(&self, recording_id: i64) -> Result<Vec<String>, String> {
        self.0.caller_row_streams(recording_id).map_err(err)
    }

    fn read_caller_rows(
        &self,
        recording_id: i64,
        stream: &str,
        from: u64,
        to: u64,
    ) -> Result<Vec<(u64, Vec<u8>)>, String> {
        self.0
            .read_caller_rows(recording_id, stream, bound(from), bound(to))
            .map_err(err)?
            .into_iter()
            .map(|r| Ok((ts(r.ts)?, r.blob)))
            .collect()
    }

    fn last_caller_row_at_or_before(
        &self,
        recording_id: i64,
        stream: &str,
        upto: u64,
        pred: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<Option<u64>, String> {
        self.0
            .last_caller_row_at_or_before(recording_id, stream, bound(upto), pred)
            .map_err(err)?
            .map(|r| ts(r.ts))
            .transpose()
    }
}
