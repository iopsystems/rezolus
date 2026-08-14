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
/// mode and running a full `VACUUM`, so treat it as permanent.
///
/// Larger pages help operations that are not the binding constraint, and cost
/// where it hurts: every tick commits a small WAL row, so write amplification
/// scales with the page size. Optimize for the per-tick write, not the bulk
/// read.
pub(crate) const PAGE_SIZE: u32 = 4096;

/// Cap the `-wal` sidecar by BYTES, not pages. At `PAGE_SIZE` this is close to
/// SQLite's own ~1000-page default, so it changes nothing today — it exists so
/// that the sidecar's size, and the checkpoint pause it implies, cannot track a
/// future page size.
const WAL_AUTOCHECKPOINT_BYTES: u32 = 4 * 1024 * 1024;

/// Page cache for a connection that READS segments back, as a negative
/// (kibibyte-denominated) `cache_size`. Large because a reader replays whole
/// segments and benefits from holding them; see `WRITER_CACHE_SIZE_KIB` for why
/// a writing connection must not take this.
const READER_CACHE_SIZE_KIB: i32 = -262_144;

/// 16 MiB of page cache for a connection that only WRITES.
///
/// **Split from the reader's cache because a writer cannot use it.** The
/// reader's figure buys segment-read throughput; a recording writer inserts
/// opaque BLOBs and never reads one back, so the only pages it benefits from
/// caching are catalog b-trees, which are kilobytes.
///
/// It is not merely wasted headroom. `cache_size` is an upper bound rather than
/// an allocation, but a seal batch dirties every overflow page of every segment
/// it inserts inside one transaction — megabytes of BLOB is thousands of pages
/// — so a co-seal walks the writer's cache to whatever cap it is given, and
/// SQLite does not hand it back. On an always-on agent that is permanent
/// resident memory.
///
/// 16 MiB is sized from `SealPolicy::max_bytes`: two segments' worth, so a
/// single segment's insert fits with room to spare. Overrunning it is safe and
/// nearly free, which is what makes a small cache the right default — in WAL
/// mode a full cache spills dirty pages to the `-wal` before the commit, and
/// segment inserts are append-only pages that are never re-dirtied, so a
/// spilled page is written once either way.
const WRITER_CACHE_SIZE_KIB: i32 = -16_384;

/// These are NEGATIVE kibibytes, so the writer's cap is the GREATER number.
/// Easy to invert while retuning, and an inversion is silent — it hands the
/// always-on writer the big cache and the analysis reader the small one, which
/// is precisely backwards and costs only performance, so nothing else fails.
/// A compile error rather than a test, because there is no reason to let a
/// build with the two crossed over exist at all.
const _: () = assert!(
    WRITER_CACHE_SIZE_KIB > READER_CACHE_SIZE_KIB,
    "the writer's page cache must be smaller than the reader's"
);

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
/// `(recording_id, sampler, ts)`.
///
/// Values-only, not the raw msgpack snapshot: names and metadata repeat
/// unchanged every tick, so carrying them per row costs several times the
/// payload for nothing. They are re-anchored once per segment instead — see
/// `WalCell`.
pub(crate) struct WalRow {
    pub sampler: String,
    pub ts: u64,
    pub wall_offset: i64,
    pub row: Vec<u8>,
}

/// What one retention pass removed. Returned rather than logged so a caller
/// can tell "the window moved" from "nothing was old enough yet" — and so a
/// test can assert the WAL rows went with their segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Evicted {
    pub segments: usize,
    pub wal_rows: usize,
}

/// How many rows a table holds and what time span they cover, answered from
/// catalog columns alone — no segment or WAL payload is read. `first_ts` and
/// `last_ts` are `None` when `rows` is 0.
pub(crate) struct Span {
    pub rows: u64,
    pub first_ts: Option<u64>,
    pub last_ts: Option<u64>,
}

/// The recovery rule, as a `WHERE` clause: a WAL row is live iff its `ts` is
/// past the watermark of the sealed segments **for its own sampler in its own
/// recording**. Written once and shared by `live_wal` and `live_wal_span` so a
/// reported WAL depth can never disagree with the rows the reader will replay.
/// See [`RezDb::live_wal`] for why the rule is what it is.
const LIVE_WAL_PREDICATE: &str = "recording_id = ?1 AND sampler = ?2 \
     AND ts > COALESCE( \
           (SELECT MAX(last_ts) FROM segments \
            WHERE recording_id = ?1 AND sampler = ?2), \
           0)";

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
        // Creating a `.rez` means writing one: the recorder's and hindsight's
        // live buffers both start here, and both are the always-on processes
        // whose RSS this is protecting. The only other `create` in the tree is
        // `hindsight::buffer::copy_range`'s dump destination, which is likewise
        // insert-only.
        db.apply_connection_pragmas(WRITER_CACHE_SIZE_KIB)?;

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
        //
        // The reader cache, because every bulk segment read in the tree arrives
        // through `open` — `rez_reader::read_v3_recordings`, and hindsight's
        // dump source. The two `open` call sites that go on to write
        // (hindsight's staged dump) take it too, deliberately: they are
        // short-lived, offline and bounded by the dump, so no long-running
        // process holds it.
        db.apply_connection_pragmas(READER_CACHE_SIZE_KIB)?;
        Ok(db)
    }

    /// The pragmas that live on the connection, not in the file. Applied by
    /// both `create` and `open`.
    ///
    /// `cache_size_kib` is the caller's because it is the one per-connection
    /// knob whose right value depends on what the connection is FOR; see
    /// `READER_CACHE_SIZE_KIB` and `WRITER_CACHE_SIZE_KIB`. Everything else here
    /// is a property of the file's durability contract and is identical on
    /// every connection.
    fn apply_connection_pragmas(&self, cache_size_kib: i32) -> Result<(), String> {
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
        self.set_pragma("cache_size", cache_size_kib)?;
        // NOT set here, and worth knowing about: `busy_timeout` is 5000 ms —
        // rusqlite's default, not SQLite's own (which is 0, i.e. fail at
        // once) and not ours. It never fires for the writer, which owns its
        // file (the journal makes concurrent writers to one file an explicit
        // non-goal), and it never fires for a reader either, because WAL mode
        // lets readers proceed while a write is in flight. The one caller it
        // can bite is a SECOND connection that writes: it will stall up to 5 s
        // before `SQLITE_BUSY`, which against a ~46 ms tick reads as a hang.
        // Left at the default deliberately rather than tuned — no measurement
        // supports any particular number, and every candidate is a guess about
        // a caller that does not exist yet. A future one should set its own,
        // with a value it can justify.
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
        insert_recording_sql(&self.conn, meta)
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

    /// Run `f` inside ONE transaction: it commits when `f` returns `Ok` and
    /// rolls back — leaving the database exactly as it was — when `f` returns
    /// `Err` or the commit itself fails.
    ///
    /// This exists because samplers seal in lockstep — a dozen tables at once
    /// is normal. Without a way to group them, one co-seal is a dozen implicit
    /// commits, i.e. a dozen fsyncs at `synchronous=FULL`, against a tick
    /// budget that a single segment insert already eats into.
    ///
    /// `f` receives a `RezTx`, not the connection: SQL stays inside this
    /// module, and `RezTx` deliberately exposes only the *writes that belong
    /// in a seal batch*. `prune_wal` is NOT among them, which is how "the
    /// prune runs outside the seal transaction" is made unrepresentable rather
    /// than merely documented — inside it, a quiet sampler's accumulated rows
    /// make the delete long enough to threaten the tick.
    pub(crate) fn transaction<T>(
        &mut self,
        f: impl FnOnce(&RezTx<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let tx = RezTx {
            tx: self
                .conn
                .transaction()
                .map_err(|e| format!("failed to begin transaction: {e}"))?,
        };
        // `?` drops `tx` on the error path, and `Transaction`'s drop behavior
        // is rollback — so a failure partway through leaves nothing behind.
        let out = f(&tx)?;
        tx.tx
            .commit()
            .map_err(|e| format!("failed to commit transaction: {e}"))?;
        Ok(out)
    }

    /// Insert one sealed segment's bytes and catalog facts, committing on its
    /// own. Batch writers should use `transaction` instead.
    pub(crate) fn insert_segment(
        &self,
        recording_id: i64,
        sampler: &str,
        seq: u64,
        meta: &SegmentMeta,
        bytes: &[u8],
    ) -> Result<(), String> {
        insert_segment_sql(&self.conn, recording_id, sampler, seq, meta, bytes)
    }

    /// Every segment for `(recording_id, sampler)`, in `seq` order.
    ///
    /// The `ORDER BY seq` is load-bearing, not cosmetic: the reader splices
    /// segment bytes together assuming they arrive in sequence order, and SQL
    /// makes no ordering guarantee without it. Confirmed with
    /// `EXPLAIN QUERY PLAN`: dropping the clause does NOT fall back to
    /// insertion order or to the primary key — the planner instead picks the
    /// `segments_by_time` index for the `(recording_id, sampler)` equality
    /// filter, which is ordered by `last_ts`, not `seq`, and is not even
    /// covering (it still fetches `bytes` per row from the table). `last_ts`
    /// happens to track `seq` in the common case (segments seal in order),
    /// which is exactly the kind of coincidence that makes a missing
    /// `ORDER BY` dangerous rather than obviously wrong.
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
        Self::collect_segments(&mut stmt, rusqlite::params![recording_id, sampler], sampler)
    }

    /// Shared row-materialization for the two segment queries, which differ
    /// only in their `WHERE` clause.
    fn collect_segments(
        stmt: &mut rusqlite::Statement<'_>,
        params: &[&dyn rusqlite::ToSql],
        sampler: &str,
    ) -> Result<Vec<SegmentRow>, String> {
        let rows = stmt
            .query_map(params, |row| {
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

    /// Every segment for `(recording_id, sampler)` that OVERLAPS `[start, end]`
    /// — `last_ts >= start AND first_ts <= end` — in `seq` order.
    ///
    /// This is the ranged dump's selection, and it is a range scan rather than
    /// a table walk: `segments_by_time` is `(recording_id, sampler, last_ts)`,
    /// so the `last_ts >= start` half is served by the index.
    ///
    /// **Whole segments, always.** A segment is an immutable parquet BLOB, so
    /// selecting part of one would mean decoding and re-encoding it — the cost
    /// the container exists to avoid. A caller gets a little more than it asked
    /// for at each edge and should report the span it actually got.
    pub(crate) fn segments_overlapping(
        &self,
        recording_id: i64,
        sampler: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<SegmentRow>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT seq, rows, first_ts, last_ts, bytes FROM segments \
                 WHERE recording_id = ?1 AND sampler = ?2 \
                   AND last_ts >= ?3 AND first_ts <= ?4 ORDER BY seq",
            )
            .map_err(|e| format!("failed to query segments for {sampler}: {e}"))?;
        // Clamped, not cast: `u64::MAX as i64` is -1, which would silently
        // select nothing at all for an unbounded upper edge.
        let params = rusqlite::params![
            recording_id,
            sampler,
            start.min(i64::MAX as u64) as i64,
            end.min(i64::MAX as u64) as i64,
        ];
        Self::collect_segments(&mut stmt, params, sampler)
    }

    /// Run `f` with every read inside ONE transaction, so all of its queries
    /// see the same snapshot of the database.
    ///
    /// The dump needs this: without it, retention can evict a segment between
    /// the query that selected it and the read that copies its bytes, and the
    /// result is a file whose catalog references a BLOB that was never
    /// written. `BEGIN DEFERRED` takes no locks until the first read and never
    /// blocks the writer in WAL mode — it just pins the snapshot.
    ///
    /// `f` gets `&Self`, so it may call any reader here; it must not write
    /// through this handle, which is why this is not exposed as a general
    /// transaction.
    pub(crate) fn read_snapshot<T>(
        &self,
        f: impl FnOnce(&Self) -> Result<T, String>,
    ) -> Result<T, String> {
        self.conn
            .execute_batch("BEGIN DEFERRED")
            .map_err(|e| format!("failed to open a read snapshot: {e}"))?;
        let out = f(self);
        // Read-only either way, so the outcome of ending it cannot change what
        // was read; the snapshot simply has to be released.
        let _ = self
            .conn
            .execute_batch(if out.is_ok() { "COMMIT" } else { "ROLLBACK" });
        out
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
    /// segment yet will NOT appear here — use `all_samplers` for "every
    /// sampler this recording has ever seen".
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

    /// Every distinct sampler this recording has ever seen, alphabetically —
    /// the union of `segments.sampler` and `wal.sampler`. This is what
    /// closes the gap `samplers()` deliberately leaves open: a sampler that
    /// has never sealed a segment (a quiet table, still inside its first
    /// seal period — the 16-of-26 case this whole design exists to fix) is
    /// otherwise unnameable, because `samplers()` only sees `segments` and
    /// this module is the only place that knows the schema well enough to
    /// look at both tables. Recovery/inventory callers should call this, not
    /// `samplers()`, when they need to know which tables exist at all.
    pub(crate) fn all_samplers(&self, recording_id: i64) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT sampler FROM segments WHERE recording_id = ?1 \
                 UNION \
                 SELECT sampler FROM wal WHERE recording_id = ?1 \
                 ORDER BY sampler",
            )
            .map_err(|e| format!("failed to query all_samplers: {e}"))?;
        // `?1` is the SAME parameter both times it appears (SQLite numbers
        // parameters, not occurrences), so this binds once, not twice.
        let rows = stmt
            .query_map([recording_id], |row| row.get::<_, String>(0))
            .map_err(|e| format!("failed to query all_samplers: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("failed to read sampler name: {e}"))?);
        }
        Ok(out)
    }

    /// Insert every WAL row for one tick — one sampler each, typically — in a
    /// single transaction. This is what makes a tick atomic: either every
    /// sampler's row for this tick lands, or none does.
    ///
    /// Takes `&mut self`, unlike every reader in this file: `Connection::
    /// transaction()` requires `&mut Connection`. An earlier version used
    /// `unchecked_transaction()` to keep `&self`, on the reasoning that this
    /// module never nests transactions — but the hazard `&mut` guards
    /// against is on the CALLER's side, not this function's: the v3 writer
    /// thread owns this `RezDb` outright (the journal makes "no concurrent
    /// writers to one file" an explicit non-goal) and does want a transaction
    /// around a whole co-seal batch — `transaction`, which this now goes
    /// through. `&mut self` makes "don't open a nested transaction while one
    /// is outstanding" a compile error for that caller instead of a runtime
    /// one. Reads stay on `&self`.
    pub(crate) fn insert_wal_rows(
        &mut self,
        recording_id: i64,
        rows: &[WalRow],
    ) -> Result<(), String> {
        self.transaction(|tx| tx.insert_wal_rows(recording_id, rows))
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
    /// The prune (`prune_wal`) deliberately runs OUTSIDE the seal transaction,
    /// because a quiet sampler accumulates thousands of rows before it seals
    /// and deleting them in the seal's own commit puts tens of megabytes of
    /// delete on the tick path. That means a crash between "segment committed"
    /// and "prune ran" can leave WAL rows whose `ts` is already covered by a
    /// sealed segment. Rather than prevent that straddle, recovery tolerates
    /// it: a row is live iff its `ts` is past the watermark of the sealed
    /// segments for its own sampler, full stop — one idempotent rule that needs
    /// no ordering guarantee between sealing and pruning.
    ///
    /// `COALESCE(..., 0)` is what makes the rule correct for a sampler with
    /// no segments at all, not just a straddling one: the subquery's `MAX`
    /// over zero rows is SQL `NULL`, which `COALESCE` turns into `0`, so
    /// `ts > 0` — every row with a real timestamp — is live. That is exactly
    /// the quiet-table case: a sampler that has never sealed keeps its WHOLE
    /// history live. That is the property the tar container could not offer,
    /// where kill-safety was per-segment and a table that had not sealed one
    /// yet recovered nothing at all.
    ///
    /// This turns the prune into a pure background optimisation with no
    /// correctness role.
    pub(crate) fn live_wal(&self, recording_id: i64, sampler: &str) -> Result<Vec<WalRow>, String> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT sampler, ts, wall_offset, row FROM wal \
                 WHERE {LIVE_WAL_PREDICATE} ORDER BY ts"
            ))
            .map_err(|e| format!("failed to query live WAL for {sampler}: {e}"))?;
        Self::collect_wal_rows(&mut stmt, recording_id, sampler)
    }

    /// How many rows a sampler's live WAL holds, and the span they cover —
    /// **without materializing them**. Same watermark as [`live_wal`] (they
    /// share `LIVE_WAL_PREDICATE`, so the depth cannot drift from the rows the
    /// reader replays); this is the aggregate form, for callers that want the
    /// number rather than the payload.
    pub(crate) fn live_wal_span(&self, recording_id: i64, sampler: &str) -> Result<Span, String> {
        self.query_span(
            &format!("SELECT COUNT(*), MIN(ts), MAX(ts) FROM wal WHERE {LIVE_WAL_PREDICATE}"),
            recording_id,
            sampler,
        )
        .map_err(|e| format!("failed to measure the live WAL for {sampler}: {e}"))
    }

    /// A sampler's sealed segments as the CATALOG sees them: how many segments,
    /// how many rows across them, and the span they cover. **No BLOB is read** —
    /// `parquet metadata` describes a 197 MB archive from this, and pulling
    /// `bytes` back only to discard it is exactly the cost the catalog exists to
    /// avoid.
    pub(crate) fn segment_span(
        &self,
        recording_id: i64,
        sampler: &str,
    ) -> Result<(u64, Span), String> {
        let segments: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM segments WHERE recording_id = ?1 AND sampler = ?2",
                rusqlite::params![recording_id, sampler],
                |row| row.get(0),
            )
            .map_err(|e| format!("failed to count segments for {sampler}: {e}"))?;
        let span = self
            .query_span(
                "SELECT COALESCE(SUM(rows), 0), MIN(first_ts), MAX(last_ts) FROM segments \
                 WHERE recording_id = ?1 AND sampler = ?2",
                recording_id,
                sampler,
            )
            .map_err(|e| format!("failed to measure the segments of {sampler}: {e}"))?;
        Ok((segments as u64, span))
    }

    /// Shared shape of the two aggregate queries above: `(rows, MIN(ts),
    /// MAX(ts))`, bound to `(recording_id, sampler)`.
    fn query_span(&self, sql: &str, recording_id: i64, sampler: &str) -> rusqlite::Result<Span> {
        self.conn
            .query_row(sql, rusqlite::params![recording_id, sampler], |row| {
                Ok(Span {
                    rows: row.get::<_, i64>(0)? as u64,
                    first_ts: row.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                    last_ts: row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                })
            })
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

    /// **Retention.** Drop every segment that lies wholly before `cutoff_ts`,
    /// and every WAL row stamped before it. This is what makes a bounded
    /// rolling buffer possible — hindsight's whole reason to exist — and it is
    /// the only destructive operation the container has.
    ///
    /// Segment granularity is deliberate and visible to the caller: a segment
    /// goes only when its NEWEST row is out of the window (`last_ts <
    /// cutoff_ts`), so a straddling segment is kept whole and the buffer holds
    /// *at least* the lookback, never less. Trimming inside a sealed segment
    /// would mean rewriting an immutable parquet BLOB, which is exactly what
    /// this container refuses to do.
    ///
    /// `segments_by_time` (`recording_id, sampler, last_ts`) makes the segment
    /// delete an indexed lookup rather than a scan; that index exists for this
    /// statement.
    ///
    /// ONE transaction, and that is load-bearing rather than tidy. Deleting a
    /// segment lowers `live_wal`'s watermark for its sampler, so WAL rows the
    /// segment already covered would become live again — a reader would splice
    /// them back in as a tail. The same-cutoff WAL delete is what stops that,
    /// and it only stops it if the two land together: a straddling row has
    /// `ts <= last_ts < cutoff_ts`, so the WAL delete provably covers every row
    /// the segment delete un-shadows.
    pub(crate) fn evict_before(
        &mut self,
        recording_id: i64,
        cutoff_ts: u64,
    ) -> Result<Evicted, String> {
        self.evict(
            recording_id,
            "DELETE FROM segments WHERE recording_id = ?1 AND last_ts < ?2",
            "DELETE FROM wal WHERE recording_id = ?1 AND ts < ?2",
            cutoff_ts,
        )
    }

    fn evict(
        &mut self,
        recording_id: i64,
        segments_sql: &str,
        wal_sql: &str,
        cutoff_ts: u64,
    ) -> Result<Evicted, String> {
        self.transaction(|tx| {
            let params = rusqlite::params![recording_id, cutoff_ts as i64];
            let segments = tx
                .tx
                .execute(segments_sql, params)
                .map_err(|e| format!("failed to evict segments: {e}"))?;
            let wal_rows = tx
                .tx
                .execute(wal_sql, params)
                .map_err(|e| format!("failed to evict WAL rows: {e}"))?;
            Ok(Evicted { segments, wal_rows })
        })
    }

    /// Return `pages` freed pages to the filesystem, or as many as the free
    /// list holds. Requires `auto_vacuum=INCREMENTAL`, which is set at
    /// creation and cannot be turned on later without a full `VACUUM`.
    ///
    /// Eviction alone keeps the file bounded, since freed pages get reused —
    /// but the bound it keeps is the HIGH-WATER mark, so a transient spike
    /// parks space on the free list permanently. This is the trickle that gives
    /// it back, sized (`pages`) to fit inside a tick.
    ///
    /// **Stepped to exhaustion, and NOT with `execute_batch`.** This pragma
    /// reclaims one page per step and `execute_batch` steps a statement once,
    /// so the obvious spelling silently reclaims exactly ONE page whatever
    /// `pages` says. That is not a slow reclaim, it is no reclaim at all: at
    /// one page per retention pass a hindsight buffer would never work off a
    /// spike.
    pub(crate) fn incremental_vacuum(&self, pages: u32) -> Result<(), String> {
        let fail = |e| format!("failed to reclaim {pages} pages: {e}");
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA incremental_vacuum({pages})"))
            .map_err(fail)?;
        let mut rows = stmt.query([]).map_err(fail)?;
        while rows.next().map_err(fail)?.is_some() {}
        Ok(())
    }

    /// Write a consistent, compacted copy of the whole database to `dest`,
    /// which must not exist.
    ///
    /// **This is the dump.** It runs inside a read transaction, so the copy is
    /// a point-in-time snapshot even while the writer keeps committing — the
    /// property a ring of slots overwritten in place cannot offer. It also
    /// rebuilds the destination from scratch, so a dump is where a hindsight
    /// buffer's free list gets compacted away for free.
    ///
    /// A plain file copy is NOT an equivalent: in WAL mode the main database
    /// file lags every commit since the last checkpoint, so copying it alone
    /// silently loses the most recent ticks.
    pub(crate) fn vacuum_into(&self, dest: &Path) -> Result<(), String> {
        let dest = dest
            .to_str()
            .ok_or_else(|| format!("dump destination {} is not valid UTF-8", dest.display()))?;
        self.conn
            .execute("VACUUM INTO ?1", [dest])
            .map_err(|e| format!("failed to write the dump to {dest}: {e}"))?;
        Ok(())
    }

    /// The whole recording's time span — every sampler, segments and WAL
    /// together — from catalog columns alone. `None` when the recording holds
    /// no rows at all, which for a rolling buffer means "nothing within the
    /// lookback".
    pub(crate) fn recording_time_span(
        &self,
        recording_id: i64,
    ) -> Result<(Option<u64>, Option<u64>), String> {
        self.conn
            .query_row(
                "SELECT MIN(first_ts), MAX(last_ts) FROM ( \
                   SELECT first_ts, last_ts FROM segments WHERE recording_id = ?1 \
                   UNION ALL \
                   SELECT ts, ts FROM wal WHERE recording_id = ?1)",
                [recording_id],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?.map(|v| v as u64),
                        row.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                    ))
                },
            )
            .map_err(|e| format!("failed to measure recording {recording_id}: {e}"))
    }

    /// Mark a recording cleanly finalized, outside any batch. The dump uses
    /// it: a copy taken at time T is a finished artifact even though the
    /// buffer it came from is still running.
    pub(crate) fn mark_complete(&mut self, recording_id: i64) -> Result<(), String> {
        self.transaction(|tx| tx.mark_complete(recording_id))
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

    /// The recording's `(ts, offset_ns)` clock observations, oldest first.
    pub(crate) fn read_clock_offsets(&self, recording_id: i64) -> Result<Vec<(u64, i64)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT ts, offset_ns FROM clock_offsets WHERE recording_id = ?1 ORDER BY ts")
            .map_err(|e| format!("failed to query clock offsets: {e}"))?;
        let rows = stmt
            .query_map([recording_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("failed to query clock offsets: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            let (ts, offset) = row.map_err(|e| format!("failed to read clock offset: {e}"))?;
            out.push((ts as u64, offset));
        }
        Ok(out)
    }

    pub(crate) fn pragma_string(&self, name: &str) -> Result<String, String> {
        self.conn
            .pragma_query_value(None, name, |row| row.get(0))
            .map_err(|e| format!("failed to read pragma {name}: {e}"))
    }
}

/// The writes that may share one transaction, handed to `RezDb::transaction`'s
/// closure. Everything here lands or nothing does.
///
/// What is absent is as deliberate as what is present: there is no `prune_wal`
/// and no read accessor. The prune belongs OUTSIDE the seal transaction, where
/// its cost cannot land on a tick, and `live_wal`'s watermark filter is what
/// makes a crash between the two harmless — see `live_wal`.
pub(crate) struct RezTx<'a> {
    tx: rusqlite::Transaction<'a>,
}

impl RezTx<'_> {
    /// Start a recording, returning its id.
    ///
    /// In a transaction because a `.rez` can be *assembled* as well as
    /// recorded: the ranged dump writes a recording row and every segment it
    /// selected, and either the whole file is that recording or there is no
    /// file at all.
    pub(crate) fn insert_recording(&self, meta: &RecordingMeta) -> Result<i64, String> {
        insert_recording_sql(&self.tx, meta)
    }

    /// Insert one sealed segment's bytes and catalog facts.
    ///
    /// A plain `INSERT` with a `&[u8]` parameter, NOT incremental BLOB I/O
    /// (`blob_open`). At the sizes a segment reaches, `blob_open`'s two-step
    /// (reserve, then stream) is measurably slower than handing SQLite the
    /// whole buffer, so the simpler API is also the faster one here.
    pub(crate) fn insert_segment(
        &self,
        recording_id: i64,
        sampler: &str,
        seq: u64,
        meta: &SegmentMeta,
        bytes: &[u8],
    ) -> Result<(), String> {
        insert_segment_sql(&self.tx, recording_id, sampler, seq, meta, bytes)
    }

    /// Insert every WAL row for one tick — one sampler each, typically.
    pub(crate) fn insert_wal_rows(&self, recording_id: i64, rows: &[WalRow]) -> Result<(), String> {
        let mut stmt = self
            .tx
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
        Ok(())
    }

    /// Append one `(ts, offset_ns)` clock observation for the recording.
    pub(crate) fn insert_clock_offset(
        &self,
        recording_id: i64,
        ts: u64,
        offset_ns: i64,
    ) -> Result<(), String> {
        self.tx
            .execute(
                "INSERT INTO clock_offsets(recording_id, ts, offset_ns) VALUES (?1, ?2, ?3)",
                rusqlite::params![recording_id, ts as i64, offset_ns],
            )
            .map_err(|e| format!("failed to insert clock offset: {e}"))?;
        Ok(())
    }

    /// Mark the recording cleanly finalized. This is what replaced the
    /// `.partial` filename convention: the file is valid from creation, so
    /// "was it finished" is a queryable property instead of a name.
    pub(crate) fn mark_complete(&self, recording_id: i64) -> Result<(), String> {
        self.tx
            .execute(
                "UPDATE recordings SET complete = 1 WHERE id = ?1",
                [recording_id],
            )
            .map_err(|e| format!("failed to mark recording {recording_id} complete: {e}"))?;
        Ok(())
    }
}

/// Shared by `RezDb::insert_recording` (its own commit) and
/// `RezTx::insert_recording` (part of a batch).
fn insert_recording_sql(conn: &Connection, meta: &RecordingMeta) -> Result<i64, String> {
    let labels = serde_json::to_string(&meta.labels)
        .map_err(|e| format!("failed to encode recording labels: {e}"))?;
    let metadata = serde_json::to_string(&meta.metadata)
        .map_err(|e| format!("failed to encode recording metadata: {e}"))?;
    conn.execute(
        "INSERT INTO recordings(labels, metadata, complete, clock_anchor_wall_ns) \
         VALUES (?1, ?2, 0, ?3)",
        rusqlite::params![labels, metadata, meta.clock_anchor_wall_ns as i64],
    )
    .map_err(|e| format!("failed to insert recording: {e}"))?;
    Ok(conn.last_insert_rowid())
}

/// Shared by `RezDb::insert_segment` (its own commit) and
/// `RezTx::insert_segment` (part of a batch): `Transaction` derefs to
/// `Connection`, so both reach the same statement.
fn insert_segment_sql(
    conn: &Connection,
    recording_id: i64,
    sampler: &str,
    seq: u64,
    meta: &SegmentMeta,
    bytes: &[u8],
) -> Result<(), String> {
    conn.execute(
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
-- scans. `live_wal`'s subquery (`SELECT MAX(last_ts) FROM segments WHERE
-- recording_id = ? AND sampler = ?`) already uses it — confirmed by
-- `EXPLAIN QUERY PLAN` during review — so this is not a speculative index
-- sitting unused; keep it maintained.
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
            READER_CACHE_SIZE_KIB as i64,
            "256 MiB reader cache, not SQLite's -2000 default"
        );
    }

    #[test]
    fn a_writing_connection_does_not_get_the_reader_cache() {
        // A `create` connection is the recorder's and hindsight's live writer;
        // giving it the reader's cache spends hundreds of MiB of resident
        // memory on a segment-read optimization it never executes.
        //
        // That the two constants are ORDERED is a compile-time assertion beside
        // them; this is the other half — that `create` reaches for the writer's
        // one. Both connections are asserted, so a change that applied one
        // cache everywhere fails here rather than quietly halving read
        // throughput.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.rez");
        let created = RezDb::create(&path).unwrap();
        assert_eq!(
            created.pragma_i64("cache_size").unwrap(),
            WRITER_CACHE_SIZE_KIB as i64,
            "a created (writing) connection takes the writer cache"
        );
        assert_eq!(
            RezDb::open(&path)
                .unwrap()
                .pragma_i64("cache_size")
                .unwrap(),
            READER_CACHE_SIZE_KIB as i64,
            "an opened connection still takes the reader cache"
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
        // Insert seq 1 BEFORE seq 0, AND give seq 1 the smaller `last_ts`.
        // That makes the three plausible orderings mutually distinct, so only
        // a genuinely seq-ordered result can pass:
        //   - insertion order:        (1, 0)
        //   - `segments_by_time` order (by last_ts, the index the planner
        //     falls back to without an explicit ORDER BY — see the doc
        //     comment on `read_segments`): (1, 0), since 99 < 200
        //   - primary-key / seq order:  (0, 1)  <- the only correct one
        // A same-direction fixture (seq tracking last_ts, as in an earlier
        // version of this test) leaves `segments_by_time` order coinciding
        // with the correct order, so dropping `ORDER BY seq` would silently
        // pass. This fixture doesn't have that escape hatch.
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
                first_ts: 90,
                last_ts: 99,
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
                first_ts: 100,
                last_ts: 200,
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
    fn all_samplers_includes_a_sampler_that_has_never_sealed() {
        // This is the API gap `all_samplers` exists to close: `samplers()`
        // only sees `segments`, so a quiet table still inside its first seal
        // period — the 16-of-26 fleet case — is nameless to it. A caller with
        // only `samplers()` cannot discover, let alone recover, a sampler
        // that has never sealed.
        let dir = tempfile::tempdir().unwrap();
        let mut db = RezDb::create(&dir.path().join("t.rez")).unwrap();
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
                rows: 1,
                first_ts: 0,
                last_ts: 9,
            },
            b"a",
        )
        .unwrap();
        // "drivehealth" never seals in this test — only a WAL row.
        db.insert_wal_rows(rid, &[wal_row("drivehealth", 5)])
            .unwrap();

        assert_eq!(
            db.samplers(rid).unwrap(),
            vec!["cpu_usage"],
            "samplers() legitimately does not see the WAL-only sampler"
        );
        assert_eq!(
            db.all_samplers(rid).unwrap(),
            vec!["cpu_usage", "drivehealth"],
            "all_samplers() must see it — this is the whole point of the accessor"
        );
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
        let (_dir, mut db, rid) = wal_test_db();
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
        let (_dir, mut db, rid) = wal_test_db();
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
    fn live_wal_watermark_is_scoped_to_its_own_sampler_and_recording() {
        // The keystone query has TWO filters inside the watermark subquery
        // (`sampler = ?2` and `recording_id = ?1`), and either one being
        // dropped is invisible to the tests above: both are single-sampler,
        // single-recording, and the multi-sampler / multi-recording tests
        // elsewhere have no segments at all, so the subquery returns NULL
        // everywhere it could otherwise discriminate.
        //
        // Two recordings x two samplers, seal a segment for (r1, cpu_usage)
        // ONLY. If the subquery's `sampler` filter is missing, cpu_usage's
        // watermark leaks into blockio's live_wal within r1. If the
        // `recording_id` filter is missing, it leaks into r2's cpu_usage
        // too. Either leak would silently truncate a quiet sampler's — or a
        // second recording's — live WAL using a watermark that has nothing
        // to do with it: exactly the failure mode this design exists to
        // rule out.
        let dir = tempfile::tempdir().unwrap();
        let mut db = RezDb::create(&dir.path().join("t.rez")).unwrap();
        let meta = |host: &str| RecordingMeta {
            labels: [("host".to_string(), host.to_string())]
                .into_iter()
                .collect(),
            metadata: BTreeMap::new(),
            clock_anchor_wall_ns: 0,
        };
        let r1 = db.insert_recording(&meta("h1")).unwrap();
        let r2 = db.insert_recording(&meta("h2")).unwrap();

        // Seal (r1, cpu_usage) up to ts=30 — a high watermark, so a leaked
        // filter would visibly truncate whichever WAL it leaked into.
        db.insert_segment(
            r1,
            "cpu_usage",
            0,
            &SegmentMeta {
                rows: 3,
                first_ts: 10,
                last_ts: 30,
            },
            b"r1-cpu_usage-sealed",
        )
        .unwrap();

        db.insert_wal_rows(r1, &[wal_row("cpu_usage", 40), wal_row("blockio", 5)])
            .unwrap();
        db.insert_wal_rows(r2, &[wal_row("cpu_usage", 5)]).unwrap();

        // (r1, blockio) has never sealed — its watermark must be its own
        // (nothing), not cpu_usage's 30.
        let r1_blockio = db.live_wal(r1, "blockio").unwrap();
        assert_eq!(
            r1_blockio.len(),
            1,
            "blockio in r1 must not inherit cpu_usage's sealed watermark"
        );
        assert_eq!(r1_blockio[0].ts, 5);

        // (r2, cpu_usage) has never sealed either — its watermark must not
        // be r1's cpu_usage watermark, even though the sampler name matches.
        let r2_cpu_usage = db.live_wal(r2, "cpu_usage").unwrap();
        assert_eq!(
            r2_cpu_usage.len(),
            1,
            "cpu_usage in r2 must not inherit r1's sealed watermark"
        );
        assert_eq!(r2_cpu_usage[0].ts, 5);

        // Sanity: (r1, cpu_usage) itself is correctly filtered by its own
        // watermark.
        let r1_cpu_usage = db.live_wal(r1, "cpu_usage").unwrap();
        assert_eq!(r1_cpu_usage.len(), 1);
        assert_eq!(r1_cpu_usage[0].ts, 40);
    }

    #[test]
    fn segment_span_summarizes_the_catalog_without_reading_a_blob() {
        // `parquet metadata` describes a fleet archive (197 MB, 149 segments)
        // from these numbers, so they must come from the catalog columns and
        // nothing else. The bytes here are deliberately NOT parquet: an
        // implementation that reached into a segment to count its rows — or
        // that pulled `bytes` back merely to discard it — fails or wastes the
        // whole archive's worth of I/O, and this fixture is what makes the
        // first of those visible.
        let (_dir, db, rid) = wal_test_db();
        db.insert_segment(
            rid,
            "cpu_usage",
            0,
            &SegmentMeta {
                rows: 3,
                first_ts: 10,
                last_ts: 29,
            },
            b"not-parquet",
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
            b"not-parquet-either",
        )
        .unwrap();
        // Another sampler's segments must not be counted into this one's.
        db.insert_segment(
            rid,
            "blockio",
            0,
            &SegmentMeta {
                rows: 99,
                first_ts: 0,
                last_ts: 99,
            },
            b"nor-this",
        )
        .unwrap();

        let (segments, span) = db.segment_span(rid, "cpu_usage").unwrap();
        assert_eq!(segments, 2);
        assert_eq!(span.rows, 5, "the SUM of the catalog's row counts");
        assert_eq!((span.first_ts, span.last_ts), (Some(10), Some(49)));

        // A sampler with no segments at all is a span of nothing, not an error:
        // that is the quiet-table case the WAL exists for.
        let (segments, span) = db.segment_span(rid, "drivehealth").unwrap();
        assert_eq!(segments, 0);
        assert_eq!(span.rows, 0);
        assert_eq!((span.first_ts, span.last_ts), (None, None));
    }

    #[test]
    fn live_wal_span_counts_the_same_rows_live_wal_returns() {
        // The depth `parquet metadata` reports is "how many unsealed rows are
        // recoverable", which is exactly what the reader will materialize —
        // so it must apply the SAME watermark `live_wal` does, not count the
        // raw table. Sealed-but-not-yet-pruned rows (the straddle the deferred
        // prune deliberately allows) are the case that tells the two apart.
        let (_dir, mut db, rid) = wal_test_db();
        db.insert_segment(
            rid,
            "cpu_usage",
            0,
            &SegmentMeta {
                rows: 3,
                first_ts: 10,
                last_ts: 30,
            },
            b"not-parquet",
        )
        .unwrap();
        db.insert_wal_rows(
            rid,
            &[
                wal_row("cpu_usage", 10),
                wal_row("cpu_usage", 20),
                wal_row("cpu_usage", 30),
                wal_row("cpu_usage", 40),
                wal_row("cpu_usage", 50),
            ],
        )
        .unwrap();

        let span = db.live_wal_span(rid, "cpu_usage").unwrap();
        assert_eq!(
            span.rows,
            db.live_wal(rid, "cpu_usage").unwrap().len() as u64,
            "the depth must agree with the rows the reader will replay"
        );
        assert_eq!(span.rows, 2, "ts=40 and ts=50 are past the watermark");
        assert_eq!((span.first_ts, span.last_ts), (Some(40), Some(50)));

        // A never-sealed sampler keeps its whole history live.
        db.insert_wal_rows(rid, &[wal_row("drivehealth", 5)])
            .unwrap();
        let span = db.live_wal_span(rid, "drivehealth").unwrap();
        assert_eq!(span.rows, 1);
        assert_eq!((span.first_ts, span.last_ts), (Some(5), Some(5)));
    }

    #[test]
    fn prune_is_idempotent_and_bounded_to_one_sampler() {
        let (_dir, mut db, rid) = wal_test_db();
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
    fn insert_wal_rows_inserts_every_row_in_the_batch() {
        let (_dir, mut db, rid) = wal_test_db();
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
    fn insert_wal_rows_is_one_transaction_for_the_whole_tick() {
        // Asserting "all N rows present" after a call that succeeds (as
        // `insert_wal_rows_inserts_every_row_in_the_batch` does) cannot tell
        // one transaction apart from N independent autocommits — both leave
        // every row present when nothing fails. The only way to observe
        // "one transaction" is to make ONE row in the batch fail and check
        // that the OTHERS, which would have committed fine on their own,
        // are gone too.
        //
        // Two different samplers share ts=10 with a THIRD row that collides
        // with the first on the primary key `(recording_id, sampler, ts)` —
        // that collision is what fails the batch.
        let (_dir, mut db, rid) = wal_test_db();
        let err = db
            .insert_wal_rows(
                rid,
                &[
                    wal_row("cpu_usage", 10),
                    wal_row("blockio", 10),
                    wal_row("cpu_usage", 10), // duplicate PK: (rid, cpu_usage, 10)
                ],
            )
            .expect_err("a PRIMARY KEY collision must fail the whole call");
        assert!(
            err.to_lowercase().contains("unique") || err.to_lowercase().contains("constraint"),
            "{err:?} should name the PK collision, not some other failure"
        );

        // If this were N autocommits instead of one transaction, the first
        // cpu_usage row and the blockio row (both collision-free) would have
        // landed before the third row failed. One transaction means the
        // whole tick is gone.
        assert_eq!(
            db.read_wal(rid, "cpu_usage").unwrap().len(),
            0,
            "a failed tick must leave NO rows, not the one that would have committed alone"
        );
        assert_eq!(
            db.read_wal(rid, "blockio").unwrap().len(),
            0,
            "blockio's collision-free row must also be rolled back"
        );
    }

    #[test]
    fn a_transaction_commits_the_whole_batch_or_none_of_it() {
        // The reason `transaction` exists: the fleet seals 12 tables in
        // lockstep, and 12 implicit commits at `synchronous=FULL` is 12 fsyncs
        // against a ~46 ms tick. One commit is the point, and "one commit" is
        // only observable by making ONE statement in the batch fail and
        // checking that the others — which would have committed fine on their
        // own — are gone too.
        let (_dir, mut db, rid) = wal_test_db();
        let meta = |first_ts, last_ts| SegmentMeta {
            rows: 1,
            first_ts,
            last_ts,
        };

        // A batch that succeeds commits every statement in it.
        db.transaction(|tx| {
            tx.insert_segment(rid, "cpu_usage", 0, &meta(10, 19), b"cpu-0")?;
            tx.insert_segment(rid, "blockio", 0, &meta(10, 19), b"blk-0")?;
            tx.insert_wal_rows(rid, &[wal_row("cpu_usage", 20)])
        })
        .unwrap();
        assert_eq!(db.read_segments(rid, "cpu_usage").unwrap().len(), 1);
        assert_eq!(db.read_segments(rid, "blockio").unwrap().len(), 1);
        assert_eq!(db.read_wal(rid, "cpu_usage").unwrap().len(), 1);

        // A batch that fails partway leaves the database untouched — not even
        // the segment that was inserted before the failing one.
        let err = db
            .transaction(|tx| {
                tx.insert_segment(rid, "cpu_usage", 1, &meta(20, 29), b"cpu-1")?;
                tx.insert_segment(rid, "blockio", 1, &meta(20, 29), b"blk-1")?;
                // Duplicate primary key (recording, sampler, seq): fails.
                tx.insert_segment(rid, "cpu_usage", 1, &meta(20, 29), b"dup")
            })
            .expect_err("a PRIMARY KEY collision must fail the whole batch");
        assert!(
            err.to_lowercase().contains("unique") || err.to_lowercase().contains("constraint"),
            "{err:?} should name the PK collision, not some other failure"
        );
        assert_eq!(
            db.read_segments(rid, "cpu_usage").unwrap().len(),
            1,
            "seq 1 must be rolled back, leaving only the committed seq 0"
        );
        assert_eq!(
            db.read_segments(rid, "blockio").unwrap().len(),
            1,
            "blockio's collision-free insert must be rolled back too"
        );
    }

    #[test]
    fn wal_rows_are_scoped_per_recording() {
        // Same as segments: a .rez can hold several recordings, and reading
        // one must not see another's WAL rows for a same-named sampler.
        let dir = tempfile::tempdir().unwrap();
        let mut db = RezDb::create(&dir.path().join("t.rez")).unwrap();
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
