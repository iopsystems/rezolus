//! Native test scaffold for the WASM viewer's `macros.sql`.
//!
//! `macros.sql` is `include_str!`'d at runtime and registered against a
//! browser-side AsyncDuckDB connection. The crate itself targets wasm32
//! and has no in-process DuckDB, so without this test the macros are
//! validated only end-to-end via the JS host.
//!
//! These tests load the same SQL into a native DuckDB connection and
//! assert the per-second rate and 5-minute rate primitives behave the
//! same way as their counterparts in
//! /work/metriken/metriken-query-sql/src/macros.rs (the source of truth
//! for native dashboard SQL). Catches drift like the irate_1s reset case
//! that was missing in this file pre-fix.

#![cfg(not(target_arch = "wasm32"))]

use duckdb::Connection;

const MACROS_SQL: &str = include_str!("../src/macros.sql");

fn fresh() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory");
    // Strip `--` line comments before splitting on `;` — comments may
    // contain semicolons inside parenthetical asides which would
    // otherwise fragment statements.
    let stripped: String = MACROS_SQL
        .lines()
        .map(|line| match line.find("--") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");
    for stmt in stripped.split(';') {
        let stmt = stmt.trim();
        if stmt.is_empty() {
            continue;
        }
        conn.execute(stmt, [])
            .unwrap_or_else(|e| panic!("execute failed:\n{stmt}\nerror: {e}"));
    }
    conn
}

fn col_f64(conn: &Connection, sql: &str) -> Vec<Option<f64>> {
    let mut stmt = conn.prepare(sql).expect("prepare");
    stmt.query_map([], |row| row.get::<_, Option<f64>>(0))
        .expect("query_map")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect")
}

#[test]
fn all_macros_register_without_error() {
    let _conn = fresh();
}

#[test]
fn irate_1s_is_per_second_rate() {
    // Mirrors metriken-query-sql/src/macros.rs::irate_1s_is_per_second_rate.
    let conn = fresh();
    let r = col_f64(
        &conn,
        "WITH t(ts, x) AS (VALUES (1000000000, 100.0), (2000000000, 250.0), (3000000000, 425.0)) \
         SELECT irate_1s(x, ts) FROM t ORDER BY ts",
    );
    assert_eq!(r, vec![None, Some(150.0), Some(175.0)]);
}

#[test]
fn irate_1s_divides_by_actual_dt_when_samples_are_gappy() {
    // Mirrors metriken-query-sql/src/macros.rs equivalent.
    let conn = fresh();
    let r = col_f64(
        &conn,
        "WITH t(ts, x) AS (VALUES (1000000000, 0.0), (2000000000, 100.0), (4000000000, 300.0)) \
         SELECT irate_1s(x, ts) FROM t ORDER BY ts",
    );
    assert_eq!(r, vec![None, Some(100.0), Some(100.0)]);
}

#[test]
fn irate_1s_handles_counter_resets_post_reset_value_as_increment() {
    // PromQL semantics: when c < LAG(c), treat the post-reset value as
    // the increment. Pre-fix the WASM macro produced negatives here
    // (drift from the native crate); this test guards the convergence.
    let conn = fresh();
    let r = col_f64(
        &conn,
        "WITH t(ts, x) AS (VALUES (1000000000, 100.0), (2000000000, 50.0), (3000000000, 70.0)) \
         SELECT irate_1s(x, ts) FROM t ORDER BY ts",
    );
    assert_eq!(r, vec![None, Some(50.0), Some(20.0)]);
}

#[test]
fn rate_5m_lags_300_seconds_and_divides() {
    // Mirrors metriken-query-sql/src/macros.rs::rate_5m_lags_300_seconds_and_divides.
    // Range-based window: at ts=305s, lookback to ts=5s → delta=46350 over 300s = 154.5.
    let conn = fresh();
    let r = col_f64(
        &conn,
        "WITH s AS (SELECT ts*1000000000 AS ts_ns, ts*(ts-1)/2 AS x FROM range(1, 306) t(ts)) \
         SELECT rate_5m(x, ts_ns) FROM s ORDER BY ts_ns DESC LIMIT 1",
    );
    assert_eq!(r, vec![Some(154.5)]);
}

#[test]
fn rate_5m_handles_short_parquets_via_range_window() {
    // Drift guard: pre-B-2 positional LAG(c, 300) returned NULL for
    // every sample on a ≤300-row parquet (the bug remaining_work.md
    // tracked). Range-based form computes a rate over the actual span.
    // 60-row table: at ts=60s, lookback to ts=1s → delta=1770/59 ≈ 30.0.
    let conn = fresh();
    let r = col_f64(
        &conn,
        "WITH s AS (SELECT ts*1000000000 AS ts_ns, ts*(ts-1)/2 AS x FROM range(1, 61) t(ts)) \
         SELECT rate_5m(x, ts_ns) FROM s ORDER BY ts_ns DESC LIMIT 1",
    );
    let v = r[0].expect("non-NULL on short window");
    assert!((v - 30.0).abs() < 1e-9, "expected ~30.0 got {v}");
}
