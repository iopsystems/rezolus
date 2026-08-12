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
        let dir = tempfile::tempdir().unwrap();
        let db = RezDb::create(&dir.path().join("t.rez")).unwrap();
        // page_size and auto_vacuum are baked in at creation; a regression here
        // silently writes the wrong value into every fleet file, and fixing it
        // later means leaving WAL mode and VACUUMing every one of them.
        assert_eq!(db.pragma_u32("page_size").unwrap(), PAGE_SIZE);
        assert_eq!(db.pragma_u32("auto_vacuum").unwrap(), 2, "INCREMENTAL");
        assert_eq!(db.pragma_string("journal_mode").unwrap(), "wal");
        assert_eq!(
            db.pragma_u32("synchronous").unwrap(),
            2,
            "FULL (3 is EXTRA)"
        );
    }

    #[test]
    fn create_honors_the_page_size_it_is_given() {
        // The assertion above is coincidental: SQLite's compiled default is
        // already 4096, so it stays green even if the pragma is never issued —
        // or is issued AFTER journal_mode=WAL or a CREATE TABLE, at which point
        // SQLite silently ignores it. Creating at a non-default size is what
        // makes that reordering fail loudly here instead of invisibly in the
        // fleet.
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
        // synchronous is per-connection and NOT persistent: an open() that
        // forgets it silently downgrades durability on every subsequent write.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.rez");
        drop(RezDb::create(&path).unwrap());
        let db = RezDb::open(&path).unwrap();
        assert_eq!(db.pragma_u32("synchronous").unwrap(), 2);
        // These two carry the test. `synchronous` above documents intent but
        // cannot detect a missing apply: SQLite's own default is already
        // FULL(2). These defaults differ from ours — 1000 pages and -2000 KiB —
        // so they read back wrong the moment the per-connection pragmas are
        // skipped.
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
        assert_eq!(db.pragma_u32("page_size").unwrap(), PAGE_SIZE, "persisted");
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
}
