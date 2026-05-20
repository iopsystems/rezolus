//! Behavioural tests for the cgroup fan-out emitters' handling of
//! columns that go NULL mid-window — the case where a short-lived
//! cgroup exits and its per-op counter stops being written.
//!
//! These run the emitted SQL against an in-memory DuckDB with a
//! fabricated `_src` + `_cgroup_index`, so they exercise the actual
//! query, not just its text form. The snapshot tests in
//! `sql_snapshots.rs` pin the string; these pin the *semantics*.
//!
//! Failure mode the tests guard against (pre-fix): `UNPIVOT` drops
//! NULL rows, so the `(timestamp, name)` series for an exited cgroup
//! ends at the last non-NULL sample instead of continuing for the
//! 5-minute irate lookback the way PromQL does. The plot's tail
//! disappears.

use dashboard::sql::{self, CgroupSide};
use duckdb::{Connection, params};

/// 1-second sampling interval in ns.
const TICK: i64 = 1_000_000_000;

/// Build an in-memory DuckDB connection wired up the way the
/// production native backend wires its slots: shared macros + UDFs
/// registered via `metriken_query_sql::register_all`.
fn open_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open duckdb");
    metriken_query_sql::register_all(&conn).expect("register_all");
    conn
}

/// Fabricate a `_src` with two metric columns and a contrived NULL
/// transition for one cgroup:
///   - 600 ticks (10 minutes at 1 Hz) starting at `t0`.
///   - `cgroup_syscall/foo/0`: linear ramp 100, 200, 300, … through
///     tick 99 (last non-NULL = tick 99 at `t0 + 99 * TICK`); then
///     NULL through tick 599.
///   - `cgroup_syscall/bar/0`: linear ramp present at every tick — a
///     control that should bind across the whole window with or
///     without the fix.
fn build_src(conn: &Connection, t0: i64) {
    conn.execute(
        "CREATE TABLE _src(\
            timestamp BIGINT, \
            \"cgroup_syscall/foo/0\" UBIGINT, \
            \"cgroup_syscall/bar/0\" UBIGINT)",
        [],
    )
    .expect("create _src");
    let mut stmt = conn
        .prepare("INSERT INTO _src VALUES (?, ?, ?)")
        .expect("prepare insert");
    for tick in 0..600i64 {
        let ts = t0 + tick * TICK;
        let foo: Option<u64> = if tick < 100 {
            Some(100 + tick as u64 * 100)
        } else {
            None
        };
        let bar: u64 = 1000 + tick as u64 * 50;
        stmt.execute(params![ts, foo, bar]).expect("insert row");
    }
}

/// One `_cgroup_index` entry per (metric, column) pair we want the
/// dashboard SQL's JOIN to bind against.
fn build_cgroup_index(conn: &Connection) {
    conn.execute(
        "CREATE TABLE _cgroup_index(\
            metric VARCHAR, \
            column_name VARCHAR, \
            name VARCHAR, \
            id VARCHAR, \
            labels MAP(VARCHAR, VARCHAR))",
        [],
    )
    .expect("create _cgroup_index");
    conn.execute(
        "INSERT INTO _cgroup_index VALUES \
            ('cgroup_syscall', 'cgroup_syscall/foo/0', '/foo', '0', MAP()), \
            ('cgroup_syscall', 'cgroup_syscall/bar/0', '/bar', '0', MAP())",
        [],
    )
    .expect("insert _cgroup_index rows");
}

/// Substitute the `__SELECTED_CGROUPS__` placeholder the dashboard
/// emitter leaves in cgroup SQL. The runtime contract (see
/// `src/viewer/assets/lib/section_views.js::setSelectedCgroups`) is
/// a SQL IN-list literal — the frontend builds `(...)` and pushes it
/// through `substituteCgroupPattern`.
fn substitute_cgroups(sql: &str, names: &[&str]) -> String {
    let escaped: Vec<String> = names
        .iter()
        .map(|n| format!("'{}'", n.replace('\'', "''")))
        .collect();
    let list = format!("({})", escaped.join(","));
    sql.replace("__SELECTED_CGROUPS__", &list)
}

/// Count rows per cgroup name in the result of running `sql` through
/// `conn`. The dashboard's `cgroup_irate_by_name` emits
/// `(t DOUBLE, name VARCHAR, v DOUBLE)`.
fn count_rows_by_name(conn: &Connection, sql: &str) -> Vec<(String, usize)> {
    let mut stmt = conn.prepare(sql).expect("prepare emitted sql");
    let rows = stmt
        .query_map([], |row| {
            let _t: f64 = row.get(0)?;
            let name: String = row.get(1)?;
            let _v: Option<f64> = row.get(2)?;
            Ok(name)
        })
        .expect("query_map");
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for r in rows {
        let name = r.expect("row");
        *counts.entry(name).or_default() += 1;
    }
    counts.into_iter().collect()
}

/// **Bug guard**: `cgroup_irate_by_name` must emit a row for the
/// exited cgroup `/foo` for at least 300 ticks past the last
/// non-NULL sample at tick 99 — that's PromQL's [5m] lookback. The
/// pre-fix emitter drops at tick 99 and never emits again because
/// UNPIVOT excludes NULL rows.
///
/// Tolerances:
///   - `/bar` (continuously non-NULL) gets all 600 rows minus the
///     first (no irate at the boundary). The exact number is brittle
///     to LAG-boundary handling; we only assert ≥ 599.
///   - `/foo` must reach at least tick 99 + 300 = 399 ticks total,
///     i.e. ≥ 399 rows. The first row is the LAG-boundary, so the
///     lower bound is 99 + 300 - 1 = 398. Generous: ≥ 398.
#[test]
fn cgroup_irate_by_name_emits_tail_after_column_null() {
    let conn = open_conn();
    build_src(&conn, 0);
    build_cgroup_index(&conn);

    let raw = sql::cgroup_irate_by_name("cgroup_syscall", CgroupSide::Individual, None);
    let sql = substitute_cgroups(&raw, &["/foo", "/bar"]);
    let counts = count_rows_by_name(&conn, &sql);

    let foo = counts
        .iter()
        .find(|(n, _)| n == "/foo")
        .map(|(_, c)| *c)
        .expect("/foo series present");
    let bar = counts
        .iter()
        .find(|(n, _)| n == "/bar")
        .map(|(_, c)| *c)
        .expect("/bar series present");

    assert!(
        bar >= 599,
        "/bar continuous series should emit ≥599 rows, got {bar}",
    );
    assert!(
        foo >= 398,
        "/foo exited at tick 99 must keep emitting through the 5-minute \
         lookback (≥398 rows), got {foo} — UNPIVOT NULL-drop regression",
    );
}

/// Same shape for `cgroup_ratio_by_name`. The control cgroup `/bar`
/// has both numerator (cgroup_syscall) and denominator (also
/// cgroup_syscall — we use the same metric for both sides since the
/// emitter doesn't care, only the JOIN does) and so its ratio rows
/// should cover the full window. `/foo`'s numerator goes NULL at
/// tick 100; the ratio series should still emit for ~5 min past it.
#[test]
fn cgroup_ratio_by_name_emits_tail_after_column_null() {
    let conn = open_conn();
    build_src(&conn, 0);
    build_cgroup_index(&conn);

    let raw = sql::cgroup_ratio_by_name("cgroup_syscall", "cgroup_syscall", CgroupSide::Individual);
    let sql = substitute_cgroups(&raw, &["/foo", "/bar"]);
    let counts = count_rows_by_name(&conn, &sql);

    let foo = counts
        .iter()
        .find(|(n, _)| n == "/foo")
        .map(|(_, c)| *c)
        .expect("/foo series present");
    let bar = counts
        .iter()
        .find(|(n, _)| n == "/bar")
        .map(|(_, c)| *c)
        .expect("/bar series present");

    assert!(
        bar >= 599,
        "/bar continuous series should emit ≥599 rows, got {bar}",
    );
    assert!(
        foo >= 398,
        "/foo exited at tick 99 must keep emitting through the 5-minute \
         lookback (≥398 rows), got {foo} — ratio fan-out NULL-drop regression",
    );
}
