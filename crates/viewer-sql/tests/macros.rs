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

// Load the same SQL the wasm host registers: H2 replacement macros from
// this crate's macros.sql, then the cross-crate shared macros via
// viewer_sql::SHARED_MACROS (re-exported from
// /work/metriken/metriken-query-sql/src/shared_macros.sql).
const H2_MACROS_SQL: &str = include_str!("../src/macros.sql");
const SHARED_MACROS_SQL: &str = viewer_sql::SHARED_MACROS;

fn fresh() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory");
    // Strip `--` line comments before splitting on `;` — comments may
    // contain semicolons inside parenthetical asides which would
    // otherwise fragment statements. H2 macros first so the shared
    // Layer A.h macros (hist_p, hist_irate_quantile, …) bind cleanly.
    let combined = format!("{H2_MACROS_SQL}\n{SHARED_MACROS_SQL}");
    let stripped: String = combined
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

// ---------- Extended parity coverage (Layer A, Layer B, H2 macros) ----------
//
// The first batch of tests above covered irate_1s + rate_5m. The
// macros below were drift-tested only via the end-to-end harness
// previously; these tests pin per-macro behaviour so a wasm-side edit
// that breaks parity surfaces here (not only via the long harness run).

fn one_f64(conn: &Connection, sql: &str) -> Option<f64> {
    conn.query_row(sql, [], |row| row.get::<_, Option<f64>>(0))
        .expect("query")
}

fn one_u64(conn: &Connection, sql: &str) -> Option<u64> {
    conn.query_row(sql, [], |row| row.get::<_, Option<u64>>(0))
        .expect("query")
}

fn one_list_u64(conn: &Connection, sql: &str) -> Option<Vec<u64>> {
    use duckdb::types::Value;
    conn.query_row(sql, [], |row| {
        let v: Value = row.get(0)?;
        Ok(match v {
            Value::Null => None,
            Value::List(items) | Value::Array(items) => Some(
                items
                    .into_iter()
                    .map(|x| match x {
                        Value::UBigInt(n) => n,
                        Value::BigInt(n) => n as u64,
                        Value::Int(n) => n as u64,
                        _ => 0,
                    })
                    .collect(),
            ),
            _ => None,
        })
    })
    .expect("query")
}

#[test]
fn delta_1s_equals_per_second_increment() {
    // delta_1s mirrors irate_1s but returns the raw increment (no /dt).
    // For 1-second spacing they coincide; test on uneven spacing to
    // separate the two.
    let conn = fresh();
    let r = col_f64(
        &conn,
        "WITH t(ts, x) AS (VALUES (1000000000, 0.0), (3000000000, 200.0)) \
         SELECT delta_1s(x, ts) FROM t ORDER BY ts",
    );
    assert_eq!(r, vec![None, Some(200.0)]);
}

#[test]
fn h2_total_sums_bucket_list() {
    let conn = fresh();
    // Mirrors metriken-query-sql/src/udf.rs::h2_total_is_sum.
    assert_eq!(
        one_u64(&conn, "SELECT h2_total([10,20,30,40]::UBIGINT[])"),
        Some(100)
    );
}

#[test]
fn h2_delta_is_elementwise_saturating_sub() {
    let conn = fresh();
    assert_eq!(
        one_list_u64(
            &conn,
            "SELECT h2_delta([100,200,300]::UBIGINT[], [10,20,30]::UBIGINT[])",
        ),
        Some(vec![90, 180, 270])
    );
    // Saturating: 5 - 100 == 0, not negative — matches native UDF.
    assert_eq!(
        one_list_u64(
            &conn,
            "SELECT h2_delta([5, 10]::UBIGINT[], [100, 5]::UBIGINT[])",
        ),
        Some(vec![0, 5])
    );
}

#[test]
fn h2_combine_sums_elementwise_widest_wins() {
    let conn = fresh();
    // **Parity hazard discovered by this test.** The wasm macro takes
    // a single LIST<LIST<UBIGINT>> (param `lol`), while the native UDF
    // is variadic UBIGINT[]. They aren't drop-in interchangeable from a
    // bare-SQL caller. The dashboard SQL emits
    // `h2_combine([*COLUMNS(...)])` which constructs a single
    // list-of-lists (wasm shape) — so this matches the dashboard's
    // calling convention. If you ever call h2_combine directly with
    // variadic args, the native UDF accepts it but the wasm macro
    // rejects it. See macros.sql for the wasm signature, udf.rs for
    // the native one.
    //
    // Behaviour: per-element sum across N inner lists, output length
    // = max inner length, missing entries treated as 0.
    assert_eq!(
        one_list_u64(
            &conn,
            "SELECT h2_combine([[1,2,3]::UBIGINT[], [10,20,30,40]::UBIGINT[]])",
        ),
        Some(vec![11, 22, 33, 40])
    );
}

#[test]
fn h2_quantile_basic_boundaries() {
    let conn = fresh();
    // [0,0,5,0,10]: q=0 → first non-empty bucket (idx 2 inclusive
    // upper at p=3 is 2); q=1 → last non-empty bucket (idx 4 inclusive
    // upper at p=3 is 4). Matches native UDF behaviour.
    assert_eq!(
        one_u64(&conn, "SELECT h2_quantile([0,0,5,0,10]::UBIGINT[], 0.0)"),
        Some(2)
    );
    assert_eq!(
        one_u64(&conn, "SELECT h2_quantile([0,0,5,0,10]::UBIGINT[], 1.0)"),
        Some(4)
    );
}

#[test]
fn h2_quantile_empty_histogram_is_null() {
    let conn = fresh();
    assert_eq!(
        one_u64(&conn, "SELECT h2_quantile([0,0,0]::UBIGINT[], 0.5)"),
        None
    );
    assert_eq!(
        one_u64(&conn, "SELECT h2_quantile([]::UBIGINT[], 0.5)"),
        None
    );
}

#[test]
fn h2_lower_upper_inclusive_bounds_at_p3() {
    let conn = fresh();
    // First bucket: [0, 0] — holds only the value 0. Last bucket
    // saturates to u64::MAX. Same invariants the native UDF tests
    // assert (first_bucket_holds_only_zero,
    // last_bucket_saturates_to_u64_max).
    assert_eq!(one_u64(&conn, "SELECT h2_lower(0, 3)"), Some(0));
    assert_eq!(one_u64(&conn, "SELECT h2_upper(0, 3)"), Some(0));
}

#[test]
fn cpu_busy_pct_decomposes_to_irate_over_cores_over_1e9() {
    // cpu_busy_pct(usage, cores, ts) = (irate_1s(usage,ts) / cores) / 1e9
    // For a steady 1-core machine where the counter ticks at 1e9/s:
    // irate_1s = 1e9, divide by 1 core, divide by 1e9 → 1.0 (= 100% busy).
    let conn = fresh();
    let r = col_f64(
        &conn,
        "WITH t(ts, u, c) AS (VALUES \
              (1000000000, 1000000000::DOUBLE, 1::DOUBLE), \
              (2000000000, 2000000000::DOUBLE, 1::DOUBLE)) \
         SELECT cpu_busy_pct(u, c, ts) FROM t ORDER BY ts",
    );
    assert_eq!(r, vec![None, Some(1.0)]);
}

#[test]
fn ipc_is_ratio_of_two_irate_1s() {
    // ipc(instructions, cycles, ts) = irate_1s(i,ts) / irate_1s(c,ts)
    // For instructions ticking at 200/s and cycles at 100/s → ipc=2.0.
    let conn = fresh();
    let r = col_f64(
        &conn,
        "WITH t(ts, i, c) AS (VALUES \
              (1000000000, 100::DOUBLE, 50::DOUBLE), \
              (2000000000, 300::DOUBLE, 150::DOUBLE)) \
         SELECT ipc(i, c, ts) FROM t ORDER BY ts",
    );
    assert_eq!(r, vec![None, Some(2.0)]);
}

#[test]
fn bps_from_bytes_is_irate_times_8() {
    // bps_from_bytes(bytes, ts) = irate_1s(bytes, ts) * 8
    let conn = fresh();
    let r = col_f64(
        &conn,
        "WITH t(ts, b) AS (VALUES \
              (1000000000, 0::DOUBLE), \
              (2000000000, 1000::DOUBLE)) \
         SELECT bps_from_bytes(b, ts) FROM t ORDER BY ts",
    );
    assert_eq!(r, vec![None, Some(8000.0)]);
}

#[test]
fn gpu_mem_used_pct_uses_used_plus_free_in_denominator() {
    // The shape that matters: denominator is `used + free`, NOT `free`
    // alone. Distinguishes the correct formula from a plausible
    // wrong one — `used/free` would yield 0.4286 here, not 0.3.
    let conn = fresh();
    let r = col_f64(
        &conn,
        "SELECT gpu_mem_used_pct(30::DOUBLE, 70::DOUBLE)",
    );
    let v = r[0].expect("non-NULL");
    assert!(
        (v - 0.3).abs() < 1e-9,
        "expected 30/(30+70)=0.3, got {v} (used/free would be ~0.4286)",
    );
}
