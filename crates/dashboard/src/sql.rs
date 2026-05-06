//! SQL emission helpers for Phase D dashboard generators.
//!
//! Each helper returns a complete SQL string referencing parquet via the
//! `_src` alias (viewer-sql binds it to `read_parquet('<registered>')`).
//! Output rows project at minimum a `t` column (DOUBLE seconds) and a `v`
//! column (numeric value); per-id helpers also project an `id` label that
//! becomes a `metric:{id:value}` entry in the Prometheus matrix.
//!
//! Conventions per `crates/viewer-sql/duckdb.md`:
//!   - Every regex is anchored with `^…$` (DuckDB COLUMNS substring-matches).
//!   - Multiple `[*COLUMNS()]` in one expression are split into separate
//!     CTE projections (DuckDB rejects multiple STAR/COLUMNS in one expr).
//!   - The same names work on both backends: native binds these to vscalar
//!     UDFs (irate_lag, h2_*) and the layer-A/B macros from
//!     `duck/src/macros.rs`; wasm binds them to the macros in
//!     `crates/viewer-sql/src/macros.sql`.

/// `sum(irate(M[5m]))` over all columns matching `re`. One series, scalar v.
pub fn irate_total(re: &str) -> String {
    format!(
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('{re}')]::UBIGINT[]) AS s
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, irate_1s(s, timestamp) AS v FROM agg"#
    )
}

/// `sum by (id) (irate(M[5m]))` — per-id rate via UNPIVOT. Each id becomes
/// a separate Prometheus matrix series.
///
/// `id_extract_re`: capture group 1 in the column name yields the id text.
/// Common case: `'/([0-9]+)$'` (the id is the trailing `/N` segment).
pub fn irate_by_id(re: &str, id_extract_re: &str) -> String {
    format!(
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('{re}') FROM _src)
                  ON COLUMNS('{re}')
                  INTO NAME col VALUE v
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  regexp_extract(col, '{id_extract_re}', 1) AS id,
                  irate_lag(
                      v,
                      LAG(v) OVER (PARTITION BY col ORDER BY timestamp),
                      timestamp - LAG(timestamp) OVER (PARTITION BY col ORDER BY timestamp)
                  ) AS v
           FROM unp"#
    )
}

/// CPU-fraction over all columns matching `re`. Equivalent to PromQL
/// `sum(irate(M[5m])) / cpu_cores / 1e9` — wraps the layer-B `cpu_busy_pct`
/// macro that takes (usage_sum, cores, ts).
pub fn cpu_pct_total(re: &str) -> String {
    format!(
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('{re}')]::UBIGINT[]) AS usage,
                     "cpu_cores" AS cores
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  cpu_busy_pct(usage, cores, timestamp) AS v
           FROM agg"#
    )
}

/// Per-CPU CPU-fraction. Equivalent to PromQL
/// `sum by (id) (irate(M[5m])) / 1e9` (per-CPU values are already nanoseconds
/// of CPU time, so dividing by 1e9 yields the fraction directly).
pub fn cpu_pct_by_id(re: &str, id_extract_re: &str) -> String {
    format!(
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('{re}') FROM _src)
                  ON COLUMNS('{re}')
                  INTO NAME col VALUE v
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  regexp_extract(col, '{id_extract_re}', 1) AS id,
                  irate_lag(
                      v,
                      LAG(v) OVER (PARTITION BY col ORDER BY timestamp),
                      timestamp - LAG(timestamp) OVER (PARTITION BY col ORDER BY timestamp)
                  ) / 1e9 AS v
           FROM unp"#
    )
}

/// Histogram percentile fan-out: one Prometheus matrix series per default
/// percentile, with `metric:{quantile:"0.99"}` etc. The bare metric name
/// `M` becomes the parquet column `M:buckets` (UBIGINT[]). The per-row
/// delta is computed once in a CTE so the LAG window happens once per row
/// regardless of how many quantiles we cross-join with.
pub fn hist_percentile_series(metric: &str) -> String {
    format!(
        r#"WITH d AS (
              SELECT timestamp,
                     h2_delta("{metric}:buckets",
                              LAG("{metric}:buckets") OVER (ORDER BY timestamp)) AS d
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  q::VARCHAR AS quantile,
                  h2_quantile(d, q)::DOUBLE AS v
           FROM d, (VALUES (0.5), (0.9), (0.99), (0.999), (0.9999)) qs(q)
           WHERE d IS NOT NULL"#
    )
}

/// Specifies a layer-B concept-macro argument: either a `list_sum` over
/// matching columns, or a literal column reference (for gauge scalars
/// like `cpu_cores`).
#[derive(Clone, Copy)]
pub enum Arg<'a> {
    Sum(&'a str),
    Col(&'a str),
}

/// Emit a concept-macro plot (cpu_busy_pct, ipc, ipns, frequency_hz,
/// l3_hit_pct, branch_miss_pct, dtlb_mpki, gpu_mem_used_pct, etc.).
/// Each argument is wired into a CTE projection so the final macro call
/// gets named columns rather than nested `[*COLUMNS()]` (which DuckDB
/// rejects when more than one appears in a single expression).
///
/// The macro is invoked as `MACRO(arg1, arg2, ..., timestamp)` — Layer-B
/// concept macros take the timestamp as their last positional arg by
/// convention. Macros that don't take a timestamp (`gpu_mem_used_pct`)
/// can use `concept_total_no_ts`.
pub fn concept_total(macro_name: &str, args: &[(&str, Arg)]) -> String {
    let projections: Vec<String> = args
        .iter()
        .map(|(name, a)| match a {
            Arg::Sum(re) => format!("list_sum([*COLUMNS('{re}')]::UBIGINT[]) AS {name}"),
            Arg::Col(c) => format!(r#""{c}" AS {name}"#),
        })
        .collect();
    let macro_args: Vec<&str> = args.iter().map(|(name, _)| *name).collect();
    format!(
        "WITH agg AS (SELECT timestamp, {projs} FROM _src) \
         SELECT timestamp::DOUBLE/1e9 AS t, {mname}({margs}, timestamp) AS v FROM agg",
        projs = projections.join(", "),
        mname = macro_name,
        margs = macro_args.join(", "),
    )
}

/// Per-CPU rate ratio: pairs two metrics by id via UNPIVOT+JOIN, computes
/// per-id rates with PARTITION BY id (so LAG doesn't compare across CPUs),
/// then plugs them into a `formula` template with `{n}` (numerator rate)
/// and `{d}` (denominator rate) placeholders.
///
/// Examples:
///   "{n} / NULLIF({d}, 0)"               — IPC, branch_miss_pct shape
///   "1 - {n} / NULLIF({d}, 0)"           — l3_hit_pct shape
///   "{n} / NULLIF({d}, 0) * 1000"        — dtlb_mpki shape
///
/// We can't use the layer-B macros directly here because their internal
/// `irate_1s` uses an unpartitioned LAG — across UNPIVOT-interleaved rows
/// that would compare values from different CPUs and underflow.
pub fn ratio_by_id(num_re: &str, den_re: &str, id_extract_re: &str, formula: &str) -> String {
    let body = formula.replace("{n}", "n_rate").replace("{d}", "d_rate");
    format!(
        r#"WITH n_unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('{num_re}') FROM _src)
                  ON COLUMNS('{num_re}') INTO NAME col VALUE v
           ),
           d_unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('{den_re}') FROM _src)
                  ON COLUMNS('{den_re}') INTO NAME col VALUE v
           ),
           joined AS (
              SELECT n.timestamp,
                     regexp_extract(n.col, '{id_extract_re}', 1) AS id,
                     n.v AS num_v,
                     d.v AS den_v
              FROM n_unp n JOIN d_unp d
                  ON n.timestamp = d.timestamp
                 AND regexp_extract(n.col, '{id_extract_re}', 1)
                     = regexp_extract(d.col, '{id_extract_re}', 1)
           ),
           rates AS (
              SELECT timestamp, id,
                     irate_lag(num_v,
                               LAG(num_v) OVER (PARTITION BY id ORDER BY timestamp),
                               timestamp - LAG(timestamp) OVER (PARTITION BY id ORDER BY timestamp)) AS n_rate,
                     irate_lag(den_v,
                               LAG(den_v) OVER (PARTITION BY id ORDER BY timestamp),
                               timestamp - LAG(timestamp) OVER (PARTITION BY id ORDER BY timestamp)) AS d_rate
              FROM joined
           )
           SELECT timestamp::DOUBLE/1e9 AS t, id, ({body}) AS v FROM rates"#
    )
}

/// Like `hist_percentile_series` but for the "Overall" pattern: combine all
/// `:buckets` columns matching a regex into a single histogram per row via
/// `h2_combine`, then compute the per-quantile fan-out. PromQL's bare-metric
/// histogram (`syscall_latency`) implicitly aggregates across labels; the SQL
/// emits `h2_combine` over the matching label-suffixed columns.
pub fn hist_percentile_series_combined(buckets_re: &str) -> String {
    format!(
        r#"WITH combined AS (
              SELECT timestamp,
                     h2_combine([*COLUMNS('{buckets_re}')]) AS b
              FROM _src
           ),
           d AS (
              SELECT timestamp,
                     h2_delta(b, LAG(b) OVER (ORDER BY timestamp)) AS d
              FROM combined
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  q::VARCHAR AS quantile,
                  h2_quantile(d, q)::DOUBLE AS v
           FROM d, (VALUES (0.5), (0.9), (0.99), (0.999), (0.9999)) qs(q)
           WHERE d IS NOT NULL"#
    )
}
