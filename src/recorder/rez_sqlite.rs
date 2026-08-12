//! The `.rez` v3 container: a single SQLite file. SQLite is used as a
//! transactional allocator with a queryable catalog, not as a query engine —
//! segments stay parquet BLOBs the database never looks inside. See
//! docs/journal/2026-08-12-rez-sqlite-container.md § "Why parquet blobs inside
//! a database".
//!
//! This is the ONLY module that knows SQL. Everything above it speaks in
//! recordings, segments, and WAL rows.
//!
//! The container is built before its writers: the `segments`, `wal`, and
//! `clock_offsets` tables exist here, but their accessors arrive with the
//! streaming writer, so the surface is wider than today's callers use.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

/// Fixed at file creation and NOT changeable afterwards without leaving WAL
/// mode and running a full `VACUUM`. 4096 was swept against {8192..65536} and
/// kept: larger pages win ≤26% on operations that are not the binding
/// constraint, while per-tick WAL amplification rises 3.14× → 8.20×.
/// See the journal, § "`page_size` selection".
pub(crate) const PAGE_SIZE: u32 = 4096;

/// Cap the `-wal` sidecar by BYTES, not pages. At `PAGE_SIZE` this is the
/// ~1000-page SQLite default, so nothing changes today — it stops the sidecar
/// (67 MB at 64 KiB pages) and the p99 from tracking any future page size.
const WAL_AUTOCHECKPOINT_BYTES: u32 = 4 * 1024 * 1024;

/// 256 MiB of page cache, as a negative (kibibyte-denominated) `cache_size`.
/// Worth +78% on segment reads (229.6 → 409.6 MB/s) and entirely reversible.
const CACHE_SIZE_KIB: i32 = -262_144;

/// v3. Written once at creation; a reader that finds anything else should
/// refuse the file rather than guess.
const SCHEMA_VERSION: i64 = 3;

/// One recording's identity: everything known when the recording starts.
pub(crate) struct RecordingMeta {
    pub labels: BTreeMap<String, String>,
    pub metadata: BTreeMap<String, String>,
    /// Wall-clock reading (ns since epoch) at recording start. Row timestamps
    /// are `anchor + monotonic elapsed`, so this pins the timeline to wall time.
    pub clock_anchor_wall_ns: u64,
}

/// A row of the `recordings` table.
pub(crate) struct RecordingRow {
    pub id: i64,
    pub meta: RecordingMeta,
    /// Whether the recording was cleanly finalized. This is what replaced the
    /// `.partial` filename convention: a `.rez` is a valid file from creation,
    /// so "was it finished" has to be a queryable property.
    pub complete: bool,
}

/// The catalog facts about one sealed segment. The segment's own bytes are an
/// opaque parquet BLOB the database never looks inside — this is everything
/// SQLite is asked to know about it.
pub(crate) struct SegmentMeta {
    pub rows: u64,
    pub first_ts: u64,
    pub last_ts: u64,
}

/// A row of the `segments` table for one `(recording, sampler)`.
pub(crate) struct SegmentRow {
    pub seq: u64,
    pub meta: SegmentMeta,
    pub bytes: Vec<u8>,
}

/// A row of the `wal` table: one sampler's values for one tick, keyed by
/// `(recording_id, sampler, ts)`. Values-only, not the raw msgpack snapshot —
/// see the journal § "The WAL is per-sampler rows, not raw snapshots" and
/// "WAL rows are values-only" (1,925 B vs 10,908 B per sampler per tick,
/// measured on a real fleet snapshot).
pub(crate) struct WalRow {
    pub sampler: String,
    pub ts: u64,
    pub wall_offset: i64,
    pub row: Vec<u8>,
}

/// An open handle on a `.rez` v3 file.
pub(crate) struct RezDb {
    conn: Connection,
}

impl RezDb {
    /// Create a new `.rez` at `path`, applying the pragmas that can only be set
    /// on a database that does not yet exist, then installing the schema.
    ///
    /// Fails if `path` already exists: a `.rez` is valid from creation, so there
    /// is no `.partial` staging file standing between a new recording and a
    /// previous one.
    pub(crate) fn create(path: &Path) -> Result<Self, String> {
        Self::create_with_page_size(path, PAGE_SIZE)
    }

    /// `create`, with the page size as a parameter. The parameter exists so a
    /// test can create at a NON-default page size: SQLite's own default happens
    /// to equal `PAGE_SIZE`, so asserting 4096 on a normally-created file passes
    /// even if the `page_size` pragma is never issued or is issued too late.
    /// Only `create` (and that test) should call this — the page size is not a
    /// caller's choice.
    fn create_with_page_size(path: &Path, page_size: u32) -> Result<Self, String> {
        // Claim the path atomically rather than testing `exists()` — this is
        // also what stops SQLite from silently adopting a file that appeared
        // between the check and the open. A zero-length file is a valid empty
        // database, so SQLite still treats what follows as a fresh creation.
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| format!("failed to create {}: {e}", path.display()))?;

        // No `SQLITE_OPEN_CREATE`: the file above is the only one this may
        // adopt. No `SQLITE_OPEN_URI` either, so a path that happens to begin
        // with `file:` stays a filename.
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|e| format!("failed to open {}: {e}", path.display()))?;
        let db = RezDb { conn };

        // ORDER IS LOAD-BEARING, and a reordering here fails invisibly — the
        // file is written with the wrong geometry and only a full VACUUM of
        // every fleet file fixes it. The three tiers, in order:
        //
        //  1. `page_size` and `auto_vacuum` take effect only on a database with
        //     no pages yet: before `journal_mode=WAL` (which writes the header
        //     and welds the page size in) and before the first CREATE TABLE.
        //  2. `journal_mode=WAL` is PERSISTENT — stored in the file header, so
        //     it is set once here and never on open.
        //  3. `synchronous` and the cache/checkpoint knobs are PER-CONNECTION
        //     and not persistent, so they are applied on every connection,
        //     including this one. See `apply_connection_pragmas`.
        db.set_pragma("page_size", page_size)?;
        // INCREMENTAL, not NONE: eviction reuses freed pages, but the bound is
        // the high-water mark, so a burst would permanently inflate a hindsight
        // file. Free in steady state (8.230 vs 8.807 ms per cycle) and it
        // CANNOT be turned on later without a full VACUUM.
        db.set_pragma("auto_vacuum", "INCREMENTAL")?;
        db.set_journal_mode_wal()?;
        db.apply_connection_pragmas()?;

        db.conn
            .execute_batch(SCHEMA_SQL)
            .map_err(|e| format!("failed to create .rez schema: {e}"))?;
        db.conn
            .execute(
                "INSERT INTO schema_version(version) VALUES (?1)",
                [SCHEMA_VERSION],
            )
            .map_err(|e| format!("failed to record .rez schema version: {e}"))?;

        Ok(db)
    }

    /// Open an existing `.rez`, reapplying the per-connection pragmas.
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        // No `SQLITE_OPEN_CREATE`: opening a `.rez` that is not there is an
        // error, not an empty new recording.
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|e| format!("failed to open {}: {e}", path.display()))?;
        let db = RezDb { conn };
        // `page_size`, `auto_vacuum` and `journal_mode` persist in the file;
        // these do not, and forgetting them silently downgrades durability
        // (synchronous falls back to NORMAL) on every subsequent write.
        db.apply_connection_pragmas()?;
        Ok(db)
    }

    /// The pragmas that live on the connection, not in the file. Applied by
    /// both `create` and `open`.
    fn apply_connection_pragmas(&self) -> Result<(), String> {
        // FULL, not NORMAL: it survives power loss, not merely process death,
        // and on the combined workload it is no worse at any percentile that
        // threatens the tick budget — the tail is checkpoint and prune work,
        // not fsync.
        self.set_pragma("synchronous", "FULL")?;
        // Derived from the file's OWN page size rather than the constant, which
        // is what "denominated in bytes" has to mean: the cap then holds at
        // 4 MiB for any file this ever opens, not just ones written at
        // `PAGE_SIZE`.
        let pages = WAL_AUTOCHECKPOINT_BYTES / self.pragma_u32("page_size")?.max(1);
        self.set_pragma("wal_autocheckpoint", pages)?;
        // Applied to writers too: `cache_size` is an upper bound on the page
        // cache, not an allocation, and readers are the ones that benefit.
        self.set_pragma("cache_size", CACHE_SIZE_KIB)?;
        Ok(())
    }

    /// `PRAGMA journal_mode = WAL`. Separate because, unlike the others, it
    /// answers with a row, which `pragma_update` rejects.
    fn set_journal_mode_wal(&self) -> Result<(), String> {
        let mode: String = self
            .conn
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
            .map_err(|e| format!("failed to set journal_mode=WAL: {e}"))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(format!("journal_mode is {mode}, expected wal"));
        }
        Ok(())
    }

    fn set_pragma<V: rusqlite::ToSql>(&self, name: &str, value: V) -> Result<(), String> {
        self.conn
            .pragma_update(None, name, value)
            .map_err(|e| format!("failed to set pragma {name}: {e}"))
    }

    /// Start a recording, returning its id.
    pub(crate) fn insert_recording(&self, meta: &RecordingMeta) -> Result<i64, String> {
        let labels = serde_json::to_string(&meta.labels)
            .map_err(|e| format!("failed to encode recording labels: {e}"))?;
        let metadata = serde_json::to_string(&meta.metadata)
            .map_err(|e| format!("failed to encode recording metadata: {e}"))?;
        self.conn
            .execute(
                "INSERT INTO recordings(labels, metadata, complete, clock_anchor_wall_ns) \
                 VALUES (?1, ?2, 0, ?3)",
                rusqlite::params![labels, metadata, meta.clock_anchor_wall_ns as i64],
            )
            .map_err(|e| format!("failed to insert recording: {e}"))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Every recording in the file, in insertion order. A `.rez` may hold
    /// several (multi-host, or an A/B pair).
    pub(crate) fn read_recordings(&self) -> Result<Vec<RecordingRow>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, labels, metadata, complete, clock_anchor_wall_ns \
                 FROM recordings ORDER BY id",
            )
            .map_err(|e| format!("failed to query recordings: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(|e| format!("failed to query recordings: {e}"))?;

        let mut out = Vec::new();
        for row in rows {
            let (id, labels, metadata, complete, anchor) =
                row.map_err(|e| format!("failed to read recording: {e}"))?;
            out.push(RecordingRow {
                id,
                meta: RecordingMeta {
                    labels: serde_json::from_str(&labels)
                        .map_err(|e| format!("recording {id} has invalid labels: {e}"))?,
                    metadata: serde_json::from_str(&metadata)
                        .map_err(|e| format!("recording {id} has invalid metadata: {e}"))?,
                    // Round-trips through INTEGER; wall-clock nanoseconds stay
                    // inside i64 until the year 2262.
                    clock_anchor_wall_ns: anchor as u64,
                },
                complete: complete != 0,
            });
        }
        Ok(out)
    }

    /// Insert one sealed segment's bytes and catalog facts.
    ///
    /// A plain `INSERT` with a `&[u8]` parameter, NOT incremental BLOB I/O
    /// (`blob_open`): the gating run measured `blob_open` **15–18% slower**
    /// at 4–8 MiB, so the simpler API is also the faster one here. See the
    /// journal § "Insert cost".
    pub(crate) fn insert_segment(
        &self,
        recording_id: i64,
        sampler: &str,
        seq: u64,
        meta: &SegmentMeta,
        bytes: &[u8],
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO segments(recording_id, sampler, seq, rows, first_ts, last_ts, bytes) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    recording_id,
                    sampler,
                    seq as i64,
                    meta.rows as i64,
                    meta.first_ts as i64,
                    meta.last_ts as i64,
                    bytes,
                ],
            )
            .map_err(|e| format!("failed to insert segment {sampler}#{seq}: {e}"))?;
        Ok(())
    }

    /// Every segment for `(recording_id, sampler)`, in `seq` order.
    ///
    /// The `ORDER BY seq` is load-bearing, not cosmetic: the reader splices
    /// segment bytes together assuming they arrive in sequence order, and SQL
    /// makes no ordering guarantee without it. (On this SQLite, dropping the
    /// clause still happens to come out sorted, because the equality WHERE on
    /// `(recording_id, sampler)` is satisfied by a scan of the covering
    /// `PRIMARY KEY (recording_id, sampler, seq)` index — confirmed with
    /// `EXPLAIN QUERY PLAN` during review. That is a query-plan accident, not
    /// a contract, so the explicit `ORDER BY` stays.)
    pub(crate) fn read_segments(
        &self,
        recording_id: i64,
        sampler: &str,
    ) -> Result<Vec<SegmentRow>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT seq, rows, first_ts, last_ts, bytes FROM segments \
                 WHERE recording_id = ?1 AND sampler = ?2 ORDER BY seq",
            )
            .map_err(|e| format!("failed to query segments for {sampler}: {e}"))?;
        let rows = stmt
            .query_map(rusqlite::params![recording_id, sampler], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            })
            .map_err(|e| format!("failed to query segments for {sampler}: {e}"))?;

        let mut out = Vec::new();
        for row in rows {
            let (seq, n_rows, first_ts, last_ts, bytes) =
                row.map_err(|e| format!("failed to read segment row for {sampler}: {e}"))?;
            out.push(SegmentRow {
                // Round-trips through INTEGER, same as elsewhere in this
                // file: these stay inside i64 for any recording anyone will
                // ever make.
                seq: seq as u64,
                meta: SegmentMeta {
                    rows: n_rows as u64,
                    first_ts: first_ts as u64,
                    last_ts: last_ts as u64,
                },
                bytes,
            });
        }
        Ok(out)
    }

    /// Sum of `rows` across every segment for `(recording_id, sampler)`. Does
    /// not include WAL rows — callers combining sealed and unsealed row
    /// counts must add `live_wal().len()` themselves.
    pub(crate) fn total_rows(&self, recording_id: i64, sampler: &str) -> Result<u64, String> {
        let total: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(rows), 0) FROM segments WHERE recording_id = ?1 AND sampler = ?2",
                rusqlite::params![recording_id, sampler],
                |row| row.get(0),
            )
            .map_err(|e| format!("failed to sum rows for {sampler}: {e}"))?;
        Ok(total as u64)
    }

    /// Every distinct sampler with at least one segment for `recording_id`,
    /// alphabetically. A sampler with only unsealed WAL rows and no sealed
    /// segment yet will NOT appear here — callers that need "every sampler
    /// this recording has ever seen" must union with the WAL.
    pub(crate) fn samplers(&self, recording_id: i64) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT sampler FROM segments WHERE recording_id = ?1 ORDER BY sampler",
            )
            .map_err(|e| format!("failed to query samplers: {e}"))?;
        let rows = stmt
            .query_map([recording_id], |row| row.get::<_, String>(0))
            .map_err(|e| format!("failed to query samplers: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("failed to read sampler name: {e}"))?);
        }
        Ok(out)
    }

    /// Insert every WAL row for one tick — one sampler each, typically — in a
    /// single transaction. This is the per-tick commit the journal's insert
    /// gating measured at p50 3.6 ms / p99 12.1 ms (26 rows, fleet sizes),
    /// and it is what makes a tick atomic: either every sampler's row for
    /// this tick lands, or none does.
    pub(crate) fn insert_wal_rows(&self, recording_id: i64, rows: &[WalRow]) -> Result<(), String> {
        // `unchecked_transaction`, not `conn.transaction()`: the latter needs
        // `&mut Connection`, and every other accessor in this file — this one
        // included, to match the module's `&self` convention — takes `&self`.
        // Nesting is the hazard `transaction()` guards against at compile
        // time; this module never opens a transaction across an await point
        // or re-enters here, so that hazard does not apply.
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("failed to begin WAL transaction: {e}"))?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO wal(recording_id, sampler, ts, wall_offset, row) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(|e| format!("failed to prepare WAL insert: {e}"))?;
            for r in rows {
                stmt.execute(rusqlite::params![
                    recording_id,
                    r.sampler,
                    r.ts as i64,
                    r.wall_offset,
                    r.row,
                ])
                .map_err(|e| format!("failed to insert WAL row for {}: {e}", r.sampler))?;
            }
        }
        tx.commit()
            .map_err(|e| format!("failed to commit WAL transaction: {e}"))?;
        Ok(())
    }

    /// Every WAL row for `(recording_id, sampler)`, sealed or not, oldest
    /// first. Recovery should use `live_wal` instead — this is the raw table,
    /// kept for inspection and for the WAL tests to compare against.
    pub(crate) fn read_wal(&self, recording_id: i64, sampler: &str) -> Result<Vec<WalRow>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT sampler, ts, wall_offset, row FROM wal \
                 WHERE recording_id = ?1 AND sampler = ?2 ORDER BY ts",
            )
            .map_err(|e| format!("failed to query WAL for {sampler}: {e}"))?;
        Self::collect_wal_rows(&mut stmt, recording_id, sampler)
    }

    /// Rows not covered by any sealed segment — this filter IS the recovery
    /// rule, not just a helper for it: **`ts > COALESCE(MAX(last_ts) of that
    /// sampler's segments, 0)`.**
    ///
    /// The prune (`prune_wal`) deliberately runs OUTSIDE the seal
    /// transaction: doing it inside measured p90 78 ms / max 245 ms (a quiet
    /// sampler accumulates ~6,500 rows before sealing, deleting ~71 MB in one
    /// commit — see the journal § "Insert cost"). That means a crash between
    /// "segment committed" and "prune ran" can leave WAL rows whose `ts` is
    /// already covered by a sealed segment. Rather than prevent that
    /// straddle, recovery tolerates it: a row is live iff its `ts` is past
    /// the watermark of the sealed segments for its own sampler, full stop —
    /// one idempotent rule that needs no ordering guarantee between sealing
    /// and pruning.
    ///
    /// `COALESCE(..., 0)` is what makes the rule correct for a sampler with
    /// no segments at all, not just a straddling one: the subquery's `MAX`
    /// over zero rows is SQL `NULL`, which `COALESCE` turns into `0`, so
    /// `ts > 0` — every row with a real timestamp — is live. That is exactly
    /// the quiet-table case: a sampler that has never sealed keeps its WHOLE
    /// history live, which is the fix for the v2 finding (16 of 26 fleet
    /// tables recovered nothing at `kill -9` 120 s in, because kill-safety
    /// was per-segment and those tables had not sealed one yet).
    ///
    /// This turns the prune into a pure background optimisation with no
    /// correctness role — worth p90 212.7 → 44.4 ms on seal ticks.
    pub(crate) fn live_wal(&self, recording_id: i64, sampler: &str) -> Result<Vec<WalRow>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT sampler, ts, wall_offset, row FROM wal \
                 WHERE recording_id = ?1 AND sampler = ?2 \
                   AND ts > COALESCE( \
                         (SELECT MAX(last_ts) FROM segments \
                          WHERE recording_id = ?1 AND sampler = ?2), \
                         0) \
                 ORDER BY ts",
            )
            .map_err(|e| format!("failed to query live WAL for {sampler}: {e}"))?;
        Self::collect_wal_rows(&mut stmt, recording_id, sampler)
    }

    /// Shared row-materialization for `read_wal` and `live_wal` — they differ
    /// only in the `WHERE` clause of the prepared statement.
    fn collect_wal_rows(
        stmt: &mut rusqlite::Statement<'_>,
        recording_id: i64,
        sampler: &str,
    ) -> Result<Vec<WalRow>, String> {
        let rows = stmt
            .query_map(rusqlite::params![recording_id, sampler], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(|e| format!("failed to query WAL rows for {sampler}: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            let (sampler, ts, wall_offset, data) =
                row.map_err(|e| format!("failed to read WAL row for {sampler}: {e}"))?;
            out.push(WalRow {
                sampler,
                ts: ts as u64,
                wall_offset,
                row: data,
            });
        }
        Ok(out)
    }

    /// Delete WAL rows at or below `upto_ts` for `(recording_id, sampler)`.
    /// Runs OUTSIDE the seal transaction — see `live_wal` for why that is
    /// safe and has no correctness role. Returns the number of rows deleted,
    /// so callers/tests can assert idempotency (a second prune of the same
    /// watermark deletes 0).
    ///
    /// Bounded to one sampler by construction (the `sampler = ?2` filter):
    /// that is why WAL rows are per-sampler rather than whole snapshots — a
    /// slow-sealing table's prune must not touch, or be blocked by, any other
    /// sampler's tail.
    pub(crate) fn prune_wal(
        &self,
        recording_id: i64,
        sampler: &str,
        upto_ts: u64,
    ) -> Result<usize, String> {
        self.conn
            .execute(
                "DELETE FROM wal WHERE recording_id = ?1 AND sampler = ?2 AND ts <= ?3",
                rusqlite::params![recording_id, sampler, upto_ts as i64],
            )
            .map_err(|e| format!("failed to prune WAL for {sampler}: {e}"))
    }

    pub(crate) fn pragma_u32(&self, name: &str) -> Result<u32, String> {
        let value = self.pragma_i64(name)?;
        u32::try_from(value).map_err(|_| format!("pragma {name} is {value}, not a u32"))
    }

    /// Signed, because `cache_size` is negative when denominated in kibibytes.
    pub(crate) fn pragma_i64(&self, name: &str) -> Result<i64, String> {
        self.conn
            .pragma_query_value(None, name, |row| row.get(0))
            .map_err(|e| format!("failed to read pragma {name}: {e}"))
    }

    pub(crate) fn pragma_string(&self, name: &str) -> Result<String, String> {
        self.conn
            .pragma_query_value(None, name, |row| row.get(0))
            .map_err(|e| format!("failed to read pragma {name}: {e}"))
    }
}

/// The catalog. Segment and WAL payloads are opaque BLOBs; everything the
/// container needs to answer questions about them is a column.
const SCHEMA_SQL: &str = "
CREATE TABLE recordings(
  id INTEGER PRIMARY KEY,
  labels TEXT NOT NULL,               -- JSON
  metadata TEXT NOT NULL,             -- JSON
  complete INTEGER NOT NULL DEFAULT 0,
  clock_anchor_wall_ns INTEGER NOT NULL
);
CREATE TABLE segments(
  recording_id INTEGER NOT NULL REFERENCES recordings(id),
  sampler TEXT NOT NULL,
  seq INTEGER NOT NULL,
  rows INTEGER NOT NULL,
  first_ts INTEGER NOT NULL,
  last_ts INTEGER NOT NULL,
  bytes BLOB NOT NULL,
  PRIMARY KEY (recording_id, sampler, seq)
);
-- The catalog half of the design: it makes hindsight retention
-- (`WHERE last_ts < cutoff`) and range reads indexed lookups rather than
-- scans. Nothing queries it yet; that is not a reason to drop it.
CREATE INDEX segments_by_time ON segments(recording_id, sampler, last_ts);
CREATE TABLE wal(
  recording_id INTEGER NOT NULL,
  sampler TEXT NOT NULL,
  ts INTEGER NOT NULL,
  wall_offset INTEGER NOT NULL,
  row BLOB NOT NULL,
  PRIMARY KEY (recording_id, sampler, ts)
);
CREATE TABLE clock_offsets(
  recording_id INTEGER NOT NULL,
  ts INTEGER NOT NULL,
  offset_ns INTEGER NOT NULL
);
CREATE TABLE schema_version(version INTEGER NOT NULL);
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_applies_the_one_way_pragmas() {
        // JOB (a): prove OUR CODE sets these. That requires values SQLite would
        // not have arrived at by itself, so both assertions here differ from the
        // default (auto_vacuum NONE=0, journal_mode "delete") and go red if the
        // pragma is dropped or issued after the first table exists. They are
        // baked in at creation: a regression writes the wrong value into every
        // fleet file, and fixing it later means leaving WAL mode and VACUUMing
        // every one of them.
        //
        // `page_size` and `synchronous` are deliberately NOT asserted here —
        // ours coincide with SQLite's defaults, so they cannot do job (a). See
        // `create_honors_the_page_size_it_is_given` for whether we set the page
        // size, and `effective_config_matches_what_was_measured` for whether it
        // is still the value we benchmarked.
        let dir = tempfile::tempdir().unwrap();
        let db = RezDb::create(&dir.path().join("t.rez")).unwrap();
        assert_eq!(db.pragma_u32("auto_vacuum").unwrap(), 2, "INCREMENTAL");
        assert_eq!(db.pragma_string("journal_mode").unwrap(), "wal");
    }

    #[test]
    fn create_honors_the_page_size_it_is_given() {
        // JOB (a) for `page_size`, which the test above cannot do: SQLite's
        // compiled default is already 4096, so asserting 4096 on a normally
        // created file stays green even if the pragma is never issued — or is
        // issued AFTER journal_mode=WAL or a CREATE TABLE, at which point SQLite
        // silently ignores it. Creating at a non-default size is what makes that
        // reordering fail loudly here instead of invisibly in the fleet.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.rez");
        let db = RezDb::create_with_page_size(&path, 8192).unwrap();
        assert_eq!(db.pragma_u32("page_size").unwrap(), 8192);
        // And it survives the connection, i.e. it really is welded into the file.
        drop(db);
        assert_eq!(
            RezDb::open(&path).unwrap().pragma_u32("page_size").unwrap(),
            8192
        );
    }

    #[test]
    fn open_reapplies_the_per_connection_pragmas() {
        // JOB (a) for the per-connection tier. These pragmas are NOT persistent,
        // so an open() that forgets them silently downgrades durability on every
        // subsequent write — and only values that differ from SQLite's defaults
        // (1000 pages, -2000 KiB) can detect that. Asserting `synchronous` here
        // would not: SQLite's own default is already FULL(2), so it stays green
        // with the apply removed. It is asserted in
        // `effective_config_matches_what_was_measured` instead, for job (b).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.rez");
        drop(RezDb::create(&path).unwrap());
        let db = RezDb::open(&path).unwrap();
        assert_eq!(
            db.pragma_u32("wal_autocheckpoint").unwrap(),
            WAL_AUTOCHECKPOINT_BYTES / PAGE_SIZE,
            "byte-denominated cap, not SQLite's 1000-page default"
        );
        assert_eq!(
            db.pragma_i64("cache_size").unwrap(),
            CACHE_SIZE_KIB as i64,
            "256 MiB reader cache, not SQLite's -2000 default"
        );
    }

    #[test]
    fn effective_config_matches_what_was_measured() {
        // JOB (b), and it is NOT the same question as job (a). This asserts the
        // effective configuration regardless of who established it — including
        // where our value happens to equal SQLite's default, which is exactly
        // the case that looks tautological and is not.
        //
        // EVERY performance number in
        // docs/journal/2026-08-12-rez-sqlite-container.md was measured at
        // page_size=4096 and synchronous=FULL: the insert latencies, the
        // eviction plateau, the 3.14× WAL amplification, the tick-budget
        // analysis. Nothing in our code would notice if those changed under us.
        // We compile SQLite from source via `bundled`, so a `cargo update`
        // bumping libsqlite3-sys, a changed compile-time define
        // (SQLITE_DEFAULT_SYNCHRONOUS, SQLITE_DEFAULT_PAGE_SIZE), or a platform
        // difference are all live paths to silently invalidating them. This
        // test is where that fails instead.
        //
        // The values below are LITERALS on purpose. Written as `PAGE_SIZE` this
        // test would follow the constant and stay green when someone retunes it
        // without re-running the sweep — which is the other regression it is
        // here to catch.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.rez");
        let created = RezDb::create(&path).unwrap();
        let reopened = RezDb::open(&path).unwrap();

        assert_eq!(PAGE_SIZE, 4096, "the swept and measured page size");
        // Checked on both connections: page_size must also survive the reopen,
        // and synchronous must hold on a connection that did not create the file.
        for (which, db) in [("created", &created), ("reopened", &reopened)] {
            assert_eq!(db.pragma_u32("page_size").unwrap(), 4096, "{which}");
            assert_eq!(
                db.pragma_u32("synchronous").unwrap(),
                2,
                "{which}: FULL (3 is EXTRA)"
            );
        }
    }

    #[test]
    fn schema_round_trips_a_recording() {
        let dir = tempfile::tempdir().unwrap();
        let db = RezDb::create(&dir.path().join("t.rez")).unwrap();
        let id = db
            .insert_recording(&RecordingMeta {
                labels: [("host".to_string(), "h1".to_string())]
                    .into_iter()
                    .collect(),
                metadata: [("source".to_string(), "rezolus".to_string())]
                    .into_iter()
                    .collect(),
                clock_anchor_wall_ns: 1_700_000_000_000_000_000,
            })
            .unwrap();
        let got = db.read_recordings().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, id);
        assert_eq!(got[0].meta.labels["host"], "h1");
        assert_eq!(got[0].meta.metadata["source"], "rezolus");
        assert_eq!(got[0].meta.clock_anchor_wall_ns, 1_700_000_000_000_000_000);
        assert!(!got[0].complete, "a fresh recording is not complete");
    }

    #[test]
    fn segments_read_back_in_seq_order_not_insertion_order() {
        // Insert seq 1 BEFORE seq 0. A missing `ORDER BY seq` would still pass
        // if segments happened to be inserted in order, so this test inserts
        // out of order on purpose — it is the only way to make the absence of
        // the ORDER BY fail loudly instead of silently agreeing with rowid
        // order.
        let dir = tempfile::tempdir().unwrap();
        let db = RezDb::create(&dir.path().join("t.rez")).unwrap();
        let rid = db
            .insert_recording(&RecordingMeta {
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                clock_anchor_wall_ns: 0,
            })
            .unwrap();
        db.insert_segment(
            rid,
            "cpu_usage",
            1,
            &SegmentMeta {
                rows: 10,
                first_ts: 100,
                last_ts: 200,
            },
            b"seq-one-bytes",
        )
        .unwrap();
        db.insert_segment(
            rid,
            "cpu_usage",
            0,
            &SegmentMeta {
                rows: 5,
                first_ts: 0,
                last_ts: 99,
            },
            b"seq-zero-bytes",
        )
        .unwrap();

        let got = db.read_segments(rid, "cpu_usage").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0].seq, 0,
            "seq 0 must come first despite being inserted second"
        );
        assert_eq!(got[0].bytes, b"seq-zero-bytes");
        assert_eq!(got[1].seq, 1);
        assert_eq!(got[1].bytes, b"seq-one-bytes");
    }

    #[test]
    fn total_rows_sums_across_segments() {
        let dir = tempfile::tempdir().unwrap();
        let db = RezDb::create(&dir.path().join("t.rez")).unwrap();
        let rid = db
            .insert_recording(&RecordingMeta {
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                clock_anchor_wall_ns: 0,
            })
            .unwrap();
        db.insert_segment(
            rid,
            "cpu_usage",
            0,
            &SegmentMeta {
                rows: 3,
                first_ts: 0,
                last_ts: 29,
            },
            b"a",
        )
        .unwrap();
        db.insert_segment(
            rid,
            "cpu_usage",
            1,
            &SegmentMeta {
                rows: 2,
                first_ts: 30,
                last_ts: 49,
            },
            b"b",
        )
        .unwrap();

        assert_eq!(db.total_rows(rid, "cpu_usage").unwrap(), 5);
    }

    #[test]
    fn samplers_lists_each_sampler_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = RezDb::create(&dir.path().join("t.rez")).unwrap();
        let rid = db
            .insert_recording(&RecordingMeta {
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                clock_anchor_wall_ns: 0,
            })
            .unwrap();
        let meta = SegmentMeta {
            rows: 1,
            first_ts: 0,
            last_ts: 9,
        };
        db.insert_segment(rid, "cpu_usage", 0, &meta, b"a").unwrap();
        db.insert_segment(rid, "cpu_usage", 1, &meta, b"b").unwrap();
        db.insert_segment(rid, "blockio", 0, &meta, b"c").unwrap();

        assert_eq!(db.samplers(rid).unwrap(), vec!["blockio", "cpu_usage"]);
    }

    #[test]
    fn segments_are_scoped_per_recording() {
        // A .rez can hold several recordings (multi-host / A-B). Reading one
        // recording's segments must never see another recording's rows for a
        // sampler of the same name.
        let dir = tempfile::tempdir().unwrap();
        let db = RezDb::create(&dir.path().join("t.rez")).unwrap();
        let meta = |labels: &str| RecordingMeta {
            labels: [("host".to_string(), labels.to_string())]
                .into_iter()
                .collect(),
            metadata: BTreeMap::new(),
            clock_anchor_wall_ns: 0,
        };
        let r1 = db.insert_recording(&meta("h1")).unwrap();
        let r2 = db.insert_recording(&meta("h2")).unwrap();

        let sm = SegmentMeta {
            rows: 1,
            first_ts: 0,
            last_ts: 9,
        };
        db.insert_segment(r1, "cpu_usage", 0, &sm, b"r1-bytes")
            .unwrap();
        db.insert_segment(r2, "cpu_usage", 0, &sm, b"r2-bytes")
            .unwrap();

        let got1 = db.read_segments(r1, "cpu_usage").unwrap();
        assert_eq!(got1.len(), 1);
        assert_eq!(got1[0].bytes, b"r1-bytes");

        let got2 = db.read_segments(r2, "cpu_usage").unwrap();
        assert_eq!(got2.len(), 1);
        assert_eq!(got2[0].bytes, b"r2-bytes");

        assert_eq!(db.total_rows(r1, "cpu_usage").unwrap(), 1);
        assert_eq!(db.samplers(r1).unwrap(), vec!["cpu_usage"]);
    }

    #[test]
    fn create_refuses_an_existing_file() {
        // A .rez is valid from creation, so there is no .partial to protect a
        // previous recording — create must not clobber one.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.rez");
        drop(RezDb::create(&path).unwrap());
        let err = match RezDb::create(&path) {
            Ok(_) => panic!("create clobbered an existing .rez"),
            Err(e) => e,
        };

        // Pin the MECHANISM, not just the outcome: the refusal must come from
        // the atomic O_EXCL create, so that swapping in an `exists()` check —
        // which would reintroduce the TOCTOU window — fails here. Compared
        // against a live AlreadyExists rather than a hardcoded string, since the
        // OS wording differs per platform.
        let already_exists = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap_err();
        assert_eq!(already_exists.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(
            err.contains(&already_exists.to_string()),
            "{err:?} should carry the AlreadyExists error from create_new"
        );
    }

    /// Shared setup for the WAL tests: a fresh `.rez` with one recording.
    fn wal_test_db() -> (tempfile::TempDir, RezDb, i64) {
        let dir = tempfile::tempdir().unwrap();
        let db = RezDb::create(&dir.path().join("t.rez")).unwrap();
        let rid = db
            .insert_recording(&RecordingMeta {
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                clock_anchor_wall_ns: 0,
            })
            .unwrap();
        (dir, db, rid)
    }

    fn wal_row(sampler: &str, ts: u64) -> WalRow {
        WalRow {
            sampler: sampler.to_string(),
            ts,
            wall_offset: ts as i64,
            row: format!("row@{ts}").into_bytes(),
        }
    }

    #[test]
    fn live_wal_excludes_rows_already_covered_by_a_sealed_segment() {
        // THE recovery rule. Seal a segment covering ts<=30, then insert WAL
        // rows at 10/20/30/40 WITHOUT pruning — simulating a crash between
        // "segment committed" and "prune ran". live_wal must return only the
        // row past the watermark (ts=40); read_wal must still return all
        // four, because the raw table is untouched.
        let (_dir, db, rid) = wal_test_db();
        db.insert_segment(
            rid,
            "cpu_usage",
            0,
            &SegmentMeta {
                rows: 3,
                first_ts: 10,
                last_ts: 30,
            },
            b"sealed-bytes",
        )
        .unwrap();
        db.insert_wal_rows(
            rid,
            &[
                wal_row("cpu_usage", 10),
                wal_row("cpu_usage", 20),
                wal_row("cpu_usage", 30),
                wal_row("cpu_usage", 40),
            ],
        )
        .unwrap();

        let live = db.live_wal(rid, "cpu_usage").unwrap();
        assert_eq!(live.len(), 1, "only ts=40 is past the sealed watermark");
        assert_eq!(live[0].ts, 40);

        let all = db.read_wal(rid, "cpu_usage").unwrap();
        assert_eq!(all.len(), 4, "the raw WAL table is untouched by sealing");
    }

    #[test]
    fn live_wal_returns_everything_when_nothing_has_sealed() {
        // A quiet sampler that has never sealed a segment: every WAL row is
        // live. This is the case that recovered NOTHING under v2 (16 of 26
        // fleet tables at kill -9 120s in).
        let (_dir, db, rid) = wal_test_db();
        db.insert_wal_rows(
            rid,
            &[wal_row("drivehealth", 5), wal_row("drivehealth", 15)],
        )
        .unwrap();

        let live = db.live_wal(rid, "drivehealth").unwrap();
        assert_eq!(
            live.len(),
            2,
            "no segments sealed yet, so every row is live"
        );
        assert_eq!(live[0].ts, 5);
        assert_eq!(live[1].ts, 15);
    }

    #[test]
    fn prune_is_idempotent_and_bounded_to_one_sampler() {
        let (_dir, db, rid) = wal_test_db();
        db.insert_wal_rows(
            rid,
            &[
                wal_row("cpu_usage", 10),
                wal_row("cpu_usage", 20),
                wal_row("blockio", 10),
                wal_row("blockio", 20),
            ],
        )
        .unwrap();

        let deleted = db.prune_wal(rid, "cpu_usage", 10).unwrap();
        assert_eq!(deleted, 1, "only cpu_usage's ts<=10 row");

        // Idempotent: pruning the same watermark again deletes nothing.
        let deleted_again = db.prune_wal(rid, "cpu_usage", 10).unwrap();
        assert_eq!(deleted_again, 0);

        // Bounded to one sampler: blockio's rows, including one at the same
        // ts that was just pruned for cpu_usage, are untouched. This is why
        // WAL rows are per-sampler rather than whole snapshots — one slow
        // table's prune must not pin, or touch, every other sampler's tail.
        let blockio = db.read_wal(rid, "blockio").unwrap();
        assert_eq!(blockio.len(), 2, "blockio untouched by cpu_usage's prune");

        let cpu_usage = db.read_wal(rid, "cpu_usage").unwrap();
        assert_eq!(cpu_usage.len(), 1, "cpu_usage's ts=10 row is gone");
        assert_eq!(cpu_usage[0].ts, 20);
    }

    #[test]
    fn insert_wal_rows_is_one_transaction_for_the_whole_tick() {
        let (_dir, db, rid) = wal_test_db();
        let samplers: Vec<WalRow> = (0..26)
            .map(|i| wal_row(&format!("sampler_{i}"), 100))
            .collect();
        db.insert_wal_rows(rid, &samplers).unwrap();

        for i in 0..26 {
            let sampler = format!("sampler_{i}");
            let rows = db.read_wal(rid, &sampler).unwrap();
            assert_eq!(rows.len(), 1, "{sampler} should have its tick's row");
        }
    }

    #[test]
    fn wal_rows_are_scoped_per_recording() {
        // Same as segments: a .rez can hold several recordings, and reading
        // one must not see another's WAL rows for a same-named sampler.
        let dir = tempfile::tempdir().unwrap();
        let db = RezDb::create(&dir.path().join("t.rez")).unwrap();
        let meta = |host: &str| RecordingMeta {
            labels: [("host".to_string(), host.to_string())]
                .into_iter()
                .collect(),
            metadata: BTreeMap::new(),
            clock_anchor_wall_ns: 0,
        };
        let r1 = db.insert_recording(&meta("h1")).unwrap();
        let r2 = db.insert_recording(&meta("h2")).unwrap();

        db.insert_wal_rows(r1, &[wal_row("cpu_usage", 10)]).unwrap();
        db.insert_wal_rows(r2, &[wal_row("cpu_usage", 20)]).unwrap();

        let got1 = db.read_wal(r1, "cpu_usage").unwrap();
        assert_eq!(got1.len(), 1);
        assert_eq!(got1[0].ts, 10);

        let got2 = db.read_wal(r2, "cpu_usage").unwrap();
        assert_eq!(got2.len(), 1);
        assert_eq!(got2[0].ts, 20);

        // live_wal must also stay scoped: neither recording has sealed
        // anything, so each sees only its own row.
        let live1 = db.live_wal(r1, "cpu_usage").unwrap();
        assert_eq!(live1.len(), 1);
        assert_eq!(live1[0].ts, 10);
    }
}
