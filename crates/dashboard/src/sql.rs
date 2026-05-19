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

/// `sum(rate(M[5m]))` over columns matching `re` — per-series
/// reset-aware 5-minute rate then sum. PromQL's `rate(c[5m])` walks
/// samples within the 5-minute lookback, treating each `c < LAG(c)`
/// transition as a counter reset (post-reset value used as the
/// increment), sums the increments, divides by the actual time span.
///
/// The `rate_5m` *macro* is monotonic-only — DuckDB rejects nested
/// window functions (`SUM(... LAG ...) OVER ...`), so reset-aware
/// 5-minute rate must be expressed as the CTE pattern below. Use this
/// helper at any callsite where the underlying counter can wrap or
/// reset within a 5-minute window (UInt64 overflow is real in
/// practice on long-running NUMA / energy counters).
///
/// Equivalent to PromQL `rate(M[5m])` for a single matching column;
/// for multiple columns, sums across them — matching `sum(rate(...))`.
pub fn rate_5m_total(re: &str) -> String {
    format!(
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('{re}') FROM _src)
                  ON COLUMNS('{re}')
                  INTO NAME col VALUE v
           ),
           per_pair AS (
              SELECT timestamp, col,
                     CASE
                         WHEN LAG(v) OVER w IS NULL THEN 0::DOUBLE
                         WHEN v >= LAG(v) OVER w THEN (v - LAG(v) OVER w)::DOUBLE
                         ELSE v::DOUBLE
                     END AS inc
              FROM unp
              WINDOW w AS (PARTITION BY col ORDER BY timestamp)
           ),
           row_sum AS (
              SELECT timestamp, SUM(inc) AS s_inc
              FROM per_pair
              GROUP BY timestamp
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  -- PromQL `rate(c[5m])` walks pairs in samples[lo..hi]
                  -- and sums `windows(2)` increments — that's hi-lo-1
                  -- pairs. The boundary-crossing increment (between the
                  -- sample just before `lo` and `lo` itself) is NOT
                  -- counted. SUM(s_inc) OVER wr includes that crossing
                  -- increment via LAG over the full partition; subtract
                  -- FIRST_VALUE(s_inc) to drop it.
                  --
                  -- The RANGE lower bound is `5m - 1ns` (not exactly
                  -- 5m) so the row at `current - 5m` is *excluded* —
                  -- matching PromQL, whose eval point carries a tiny
                  -- sub-second offset (the parquet's start_ns mod
                  -- sampling-interval) and so its `[t - 5m, t]` window
                  -- starts strictly greater than the snapped sample at
                  -- `t - 5m`. Without the 1-ns shift, SQL's inclusive
                  -- lower bound picks up that boundary sample on every
                  -- real parquet (start_ns is wall-clock-aligned, never
                  -- a clean integer second) and the trailing-edge
                  -- points diverge by ~one boundary increment / 300s.
                  (SUM(s_inc) OVER wr - COALESCE(FIRST_VALUE(s_inc) OVER wr, 0))
                    / NULLIF((timestamp - MIN(timestamp) OVER wr)::DOUBLE/1e9, 0) AS v
           FROM row_sum
           WINDOW wr AS (ORDER BY timestamp RANGE BETWEEN 299999999999 PRECEDING AND CURRENT ROW)"#
    )
}

/// `sum(irate(M[5m]))` over all columns matching `re` — per-series
/// irate then sum. Mirrors PromQL semantics: each series's counter
/// resets are handled locally before aggregation. One output series,
/// scalar v.
///
/// Pre-2026-05 this was `irate_1s(list_sum([*COLUMNS(...)]), ts)` —
/// sum-then-rate, which diverges from PromQL at intra-window counter
/// resets in any individual column. Switched to UNPIVOT + per-series
/// irate + sum-by-timestamp to close those divergences.
pub fn irate_total(re: &str) -> String {
    format!(
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('{re}') FROM _src)
                  ON COLUMNS('{re}')
                  INTO NAME col VALUE v
           ),
           rates AS (
              SELECT timestamp,
                     irate_lag(
                         v,
                         LAG(v) OVER (PARTITION BY col ORDER BY timestamp),
                         timestamp - LAG(timestamp) OVER (PARTITION BY col ORDER BY timestamp)
                     ) AS rate
              FROM unp
           )
           SELECT timestamp::DOUBLE/1e9 AS t, SUM(rate) AS v
           FROM rates
           GROUP BY timestamp"#
    )
}

/// `sum by (id) (irate(M[5m]))` over columns whose names share an id
/// segment — per-series irate then sum-by-(timestamp, id). Mirrors
/// PromQL's per-series-rate-then-sum semantics; correct in the
/// presence of per-series counter resets (sum-then-rate would mask
/// individual resets).
///
/// Use this when more than one column maps to the same id (e.g.
/// `softirq/<kind>/<cpu>` where `<kind>` varies per cpu). When each
/// id has exactly one matching column, `irate_by_id` produces the
/// same rows without the GROUP BY pass.
///
/// `id_extract_re`: capture group 1 in the column name yields the id
/// text. Common case: `'/([0-9]+)$'` (id is the trailing `/N` segment).
pub fn irate_sum_by_id(re: &str, id_extract_re: &str) -> String {
    format!(
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('{re}') FROM _src)
                  ON COLUMNS('{re}')
                  INTO NAME col VALUE v
           ),
           rates AS (
              SELECT timestamp,
                     regexp_extract(col, '{id_extract_re}', 1) AS id,
                     irate_lag(
                         v,
                         LAG(v) OVER (PARTITION BY col ORDER BY timestamp),
                         timestamp - LAG(timestamp) OVER (PARTITION BY col ORDER BY timestamp)
                     ) AS rate
              FROM unp
           )
           SELECT timestamp::DOUBLE/1e9 AS t, id, SUM(rate) AS v
           FROM rates
           GROUP BY timestamp, id"#
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
/// `sum(irate(M[5m])) / cpu_cores / 1e9` — per-series irate, sum, then
/// scale. Like `irate_total` this avoids the sum-then-rate semantic
/// gap at intra-window counter resets.
pub fn cpu_pct_total(re: &str) -> String {
    format!(
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('{re}') FROM _src)
                  ON COLUMNS('{re}')
                  INTO NAME col VALUE v
           ),
           rates AS (
              SELECT timestamp,
                     irate_lag(
                         v,
                         LAG(v) OVER (PARTITION BY col ORDER BY timestamp),
                         timestamp - LAG(timestamp) OVER (PARTITION BY col ORDER BY timestamp)
                     ) AS rate
              FROM unp
           ),
           summed AS (
              SELECT timestamp, SUM(rate) AS s FROM rates GROUP BY timestamp
           )
           SELECT s.timestamp::DOUBLE/1e9 AS t,
                  s.s / NULLIF(c."cpu_cores", 0) / 1e9 AS v
           FROM summed s
              JOIN _src c ON c.timestamp = s.timestamp"#
    )
}

/// Per-CPU CPU-fraction. Equivalent to PromQL
/// `sum by (id) (irate(M[5m])) / 1e9` (per-CPU values are already nanoseconds
/// of CPU time, so dividing by 1e9 yields the fraction directly).
///
/// Two stages:
///   1. UNPIVOT + windowed irate per source column (`PARTITION BY col`).
///   2. Aggregate by (timestamp, id) to collapse multiple matching
///      columns that share an `id`. This matters when `re` matches
///      across states — e.g. `^cpu_usage/[a-z]+/[0-9]+$` matches both
///      `cpu_usage/user/<n>` and `cpu_usage/system/<n>`, which both
///      extract the same `id`. Without the SUM step the result has
///      two rows per (timestamp, id), and the heatmap renderer ends
///      up with ambiguous values per cell. Single-state regexes
///      degenerate to one row per group, so the SUM is a no-op there.
pub fn cpu_pct_by_id(re: &str, id_extract_re: &str) -> String {
    format!(
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('{re}') FROM _src)
                  ON COLUMNS('{re}')
                  INTO NAME col VALUE v
           ),
           rates AS (
              SELECT timestamp,
                     regexp_extract(col, '{id_extract_re}', 1) AS id,
                     irate_lag(
                         v,
                         LAG(v) OVER (PARTITION BY col ORDER BY timestamp),
                         timestamp - LAG(timestamp) OVER (PARTITION BY col ORDER BY timestamp)
                     ) AS rate_ns
              FROM unp
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  id,
                  SUM(rate_ns) / 1e9 AS v
           FROM rates
           GROUP BY timestamp, id"#
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

/// Aggregate vs individual sides of the cgroups dashboard. Aggregate
/// sums non-selected cgroups; individual fans out per selected cgroup.
#[derive(Clone, Copy)]
pub enum CgroupSide {
    Aggregate,
    Individual,
}

fn cgroup_name_filter(side: CgroupSide) -> &'static str {
    // Aggregate: include cgroups whose name is NOT in the selection,
    // including columns with no `name` label at all (root host metrics).
    // `NULL NOT IN (...)` returns NULL → COALESCE makes those columns
    // pass through. Individual: `IN` filters out NULLs naturally.
    match side {
        CgroupSide::Aggregate => "COALESCE(idx.name, '') NOT IN __SELECTED_CGROUPS__",
        CgroupSide::Individual => "idx.name IN __SELECTED_CGROUPS__",
    }
}

fn cgroup_label_filter(filter: Option<(&str, &str)>) -> String {
    match filter {
        Some((k, v)) => format!(" AND idx.labels[{}] = {}", sql_string(k), sql_string(v),),
        None => String::new(),
    }
}

fn sql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

/// Aggregate cgroup rate: `sum(irate(<metric>{[label_filter,]name<side>"X"}[5m]))`.
/// Joins the unpivoted `<metric>/...` columns against `_cgroup_index` to
/// filter by the `name` label (and optionally one other label like `state`
/// or `op`). Returns scalar `v` per timestamp.
///
/// Per-series irate first, then SUM — mirrors PromQL's
/// `sum(irate(...))`. The earlier sum-then-rate form masked per-column
/// counter resets (e.g. cgroup id recycling at boot: the same id slot
/// gets reassigned to a different cgroup, every op-column's counter
/// drops on the same sample, and the summed series climbs *up* across
/// the reset — producing a bogus positive rate). With per-series irate
/// each column detects its own reset and contributes the post-reset
/// value, matching `irate_total` / `cpu_pct_total` which migrated to
/// this shape in 2026-05.
pub fn cgroup_irate_total(
    metric: &str,
    side: CgroupSide,
    label_filter: Option<(&str, &str)>,
) -> String {
    let name_clause = cgroup_name_filter(side);
    let extra = cgroup_label_filter(label_filter);
    format!(
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('^{metric}(/[^:]+)?$') FROM _src)
                  ON COLUMNS('^{metric}(/[^:]+)?$') INTO NAME col VALUE v
           ),
           joined AS (
              SELECT u.timestamp, u.col, u.v
              FROM unp u JOIN _cgroup_index idx
                  ON idx.column_name = u.col AND idx.metric = '{metric}'{extra}
              WHERE {name_clause}
           ),
           rates AS (
              SELECT timestamp,
                     irate_lag(
                         v,
                         LAG(v) OVER (PARTITION BY col ORDER BY timestamp),
                         timestamp - LAG(timestamp) OVER (PARTITION BY col ORDER BY timestamp)
                     ) AS rate
              FROM joined
           )
           SELECT timestamp::DOUBLE/1e9 AS t, SUM(rate) AS v
           FROM rates
           GROUP BY timestamp"#,
    )
}

/// Per-name cgroup rate fan-out. Each selected cgroup becomes one
/// Prometheus matrix series with `metric:{name:value}`.
///
/// Per-column irate (PARTITION BY col) then SUM-by-(timestamp, name) —
/// mirrors PromQL's `sum by (name) (irate(<metric>{...}[5m]))`. Each
/// underlying physical column is its own PromQL series; counter resets
/// (cgroup id recycling, NULL gaps in one op while siblings keep
/// climbing) are detected per-series before aggregation.
///
/// **NULL-tail handling.** `UNPIVOT` uses `INCLUDE NULLS` so the
/// (timestamp, col) grid survives across cgroup-exit boundaries.
/// `LAST_VALUE(rate IGNORE NULLS) OVER (RANGE 5min PRECEDING)` then
/// carries the last computed per-col rate forward for 5 min — matching
/// PromQL's `irate(c[5m])` lookback. Past 5 min the carry expires and
/// the row drops. This was the previous regression-vs-PromQL: without
/// it the per-name series ended at the cgroup's last non-NULL sample.
pub fn cgroup_irate_by_name(
    metric: &str,
    side: CgroupSide,
    label_filter: Option<(&str, &str)>,
) -> String {
    let name_clause = cgroup_name_filter(side);
    let extra = cgroup_label_filter(label_filter);
    format!(
        r#"WITH unp AS (
              -- The SQL-standard `UNPIVOT … (v FOR col IN …)` form is
              -- the one that accepts the `INCLUDE NULLS` modifier in
              -- DuckDB's grammar; the friendlier `UNPIVOT t ON … INTO
              -- NAME col VALUE v` form does not, as of duckdb 1.5.
              SELECT * FROM (SELECT timestamp, COLUMNS('^{metric}(/[^:]+)?$') FROM _src)
                  UNPIVOT INCLUDE NULLS
                  (v FOR col IN (COLUMNS('^{metric}(/[^:]+)?$')))
           ),
           joined AS (
              SELECT u.timestamp, idx.name, u.col, u.v
              FROM unp u JOIN _cgroup_index idx
                  ON idx.column_name = u.col AND idx.metric = '{metric}'{extra}
              WHERE {name_clause}
           ),
           rates AS (
              SELECT timestamp, name, col,
                     irate_lag(
                         v,
                         LAG(v) OVER (PARTITION BY col ORDER BY timestamp),
                         timestamp - LAG(timestamp) OVER (PARTITION BY col ORDER BY timestamp)
                     ) AS rate
              FROM joined
           ),
           carried AS (
              SELECT timestamp, name,
                     COALESCE(
                         rate,
                         LAST_VALUE(rate IGNORE NULLS) OVER (
                             PARTITION BY col
                             ORDER BY timestamp
                             RANGE BETWEEN 300000000000 PRECEDING AND CURRENT ROW
                         )
                     ) AS rate
              FROM rates
           )
           SELECT timestamp::DOUBLE/1e9 AS t, name, SUM(rate) AS v
           FROM carried
           WHERE rate IS NOT NULL
           GROUP BY timestamp, name"#,
    )
}

/// Aggregate cgroup ratio: `sum(irate(<num>)) / sum(irate(<den>))` where
/// both are filtered by the same cgroup-name selection. Used for IPC
/// (instructions/cycles), branch-miss-pct, etc.
pub fn cgroup_ratio_total(num_metric: &str, den_metric: &str, side: CgroupSide) -> String {
    let name_clause = cgroup_name_filter(side);
    format!(
        r#"WITH n_unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('^{num_metric}(/[^:]+)?$') FROM _src)
                  ON COLUMNS('^{num_metric}(/[^:]+)?$') INTO NAME col VALUE v
           ),
           d_unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('^{den_metric}(/[^:]+)?$') FROM _src)
                  ON COLUMNS('^{den_metric}(/[^:]+)?$') INTO NAME col VALUE v
           ),
           n_join AS (
              SELECT u.timestamp, u.v FROM n_unp u JOIN _cgroup_index idx
                  ON idx.column_name = u.col AND idx.metric = '{num_metric}'
              WHERE {name_clause}
           ),
           d_join AS (
              SELECT u.timestamp, u.v FROM d_unp u JOIN _cgroup_index idx
                  ON idx.column_name = u.col AND idx.metric = '{den_metric}'
              WHERE {name_clause}
           ),
           n_agg AS (SELECT timestamp, SUM(v) AS s FROM n_join GROUP BY timestamp),
           d_agg AS (SELECT timestamp, SUM(v) AS s FROM d_join GROUP BY timestamp),
           n_rate AS (SELECT timestamp, irate_1s(s, timestamp) AS r FROM n_agg),
           d_rate AS (SELECT timestamp, irate_1s(s, timestamp) AS r FROM d_agg)
           SELECT n.timestamp::DOUBLE/1e9 AS t,
                  n.r / NULLIF(d.r, 0) AS v
           FROM n_rate n JOIN d_rate d USING(timestamp)"#,
    )
}

/// Per-name cgroup ratio fan-out: `sum by (name) (irate(<num>)) / sum by
/// (name) (irate(<den>))`. Like `ratio_by_id` but joins on the `name`
/// label via `_cgroup_index`.
///
/// Same NULL-tail handling as `cgroup_irate_by_name` — `INCLUDE NULLS`
/// on UNPIVOT preserves the per-(col, timestamp) grid across cgroup
/// exits, and a 5 min `LAST_VALUE(... IGNORE NULLS)` carry forward on
/// each side's per-name rate matches PromQL's `irate(c[5m])` lookback.
pub fn cgroup_ratio_by_name(num_metric: &str, den_metric: &str, side: CgroupSide) -> String {
    let name_clause = cgroup_name_filter(side);
    format!(
        r#"WITH n_unp AS (
              -- SQL-standard UNPIVOT for INCLUDE NULLS — see comment
              -- on `cgroup_irate_by_name` for the grammar wart.
              SELECT * FROM (SELECT timestamp, COLUMNS('^{num_metric}(/[^:]+)?$') FROM _src)
                  UNPIVOT INCLUDE NULLS
                  (v FOR col IN (COLUMNS('^{num_metric}(/[^:]+)?$')))
           ),
           d_unp AS (
              SELECT * FROM (SELECT timestamp, COLUMNS('^{den_metric}(/[^:]+)?$') FROM _src)
                  UNPIVOT INCLUDE NULLS
                  (v FOR col IN (COLUMNS('^{den_metric}(/[^:]+)?$')))
           ),
           n_join AS (
              SELECT u.timestamp, idx.name, u.v
              FROM n_unp u JOIN _cgroup_index idx
                  ON idx.column_name = u.col AND idx.metric = '{num_metric}'
              WHERE {name_clause}
           ),
           d_join AS (
              SELECT u.timestamp, idx.name, u.v
              FROM d_unp u JOIN _cgroup_index idx
                  ON idx.column_name = u.col AND idx.metric = '{den_metric}'
              WHERE {name_clause}
           ),
           -- SUM(UBIGINT) promotes to HUGEINT in DuckDB; cast back so
           -- `irate_lag` (which is UBIGINT-typed) binds. See
           -- `cgroup_irate_by_name` for the rationale and overflow note.
           n_by AS (SELECT timestamp, name, CAST(SUM(v) AS UBIGINT) AS s
                    FROM n_join GROUP BY timestamp, name),
           d_by AS (SELECT timestamp, name, CAST(SUM(v) AS UBIGINT) AS s
                    FROM d_join GROUP BY timestamp, name),
           n_rate_raw AS (
              SELECT timestamp, name,
                     irate_lag(s,
                         LAG(s) OVER (PARTITION BY name ORDER BY timestamp),
                         timestamp - LAG(timestamp) OVER (PARTITION BY name ORDER BY timestamp)
                     ) AS r
              FROM n_by
           ),
           d_rate_raw AS (
              SELECT timestamp, name,
                     irate_lag(s,
                         LAG(s) OVER (PARTITION BY name ORDER BY timestamp),
                         timestamp - LAG(timestamp) OVER (PARTITION BY name ORDER BY timestamp)
                     ) AS r
              FROM d_by
           ),
           n_rate AS (
              SELECT timestamp, name,
                     COALESCE(r, LAST_VALUE(r IGNORE NULLS) OVER (
                         PARTITION BY name ORDER BY timestamp
                         RANGE BETWEEN 300000000000 PRECEDING AND CURRENT ROW
                     )) AS r
              FROM n_rate_raw
           ),
           d_rate AS (
              SELECT timestamp, name,
                     COALESCE(r, LAST_VALUE(r IGNORE NULLS) OVER (
                         PARTITION BY name ORDER BY timestamp
                         RANGE BETWEEN 300000000000 PRECEDING AND CURRENT ROW
                     )) AS r
              FROM d_rate_raw
           )
           SELECT n.timestamp::DOUBLE/1e9 AS t, n.name,
                  n.r / NULLIF(d.r, 0) AS v
           FROM n_rate n JOIN d_rate d USING(timestamp, name)
           WHERE n.r IS NOT NULL OR d.r IS NOT NULL"#,
    )
}

/// N-way per-id ratio fan-out. Joins the unpivoted streams of N
/// metrics on `(timestamp, id)`, computes per-id rates with
/// `PARTITION BY id` LAG (so LAG doesn't compare across CPUs), and
/// applies the `formula` template — placeholders are
/// `<arg_name>_rate` (e.g. `t_rate`, `a_rate`, `m_rate`).
///
/// Used for plots that need 3+ per-id rates combined arithmetically
/// — `frequency-per-cpu` (tsc × aperf / mperf), `ipns-per-cpu`
/// ((i/c) × (t·a/m) / 1e9). `ratio_by_id` covers the 2-arg case
/// already.
pub fn nway_ratio_by_id(args: &[(&str, &str)], id_extract_re: &str, formula: &str) -> String {
    assert!(args.len() >= 2, "nway_ratio_by_id needs ≥ 2 inputs");
    // CTE per arg: UNPIVOT the matching columns into (timestamp, col, v).
    let mut sql = String::from("WITH ");
    for (i, (name, re)) in args.iter().enumerate() {
        if i > 0 {
            sql.push_str(",\n     ");
        }
        sql.push_str(&format!(
            "{name}_unp AS (\n        UNPIVOT (SELECT timestamp, COLUMNS('{re}') FROM _src) \
                ON COLUMNS('{re}') INTO NAME col VALUE v\n     )"
        ));
    }
    // Joined CTE: anchor on the first arg, JOIN the rest on
    // (timestamp, id) — id extracted via the same regex on each side.
    let (anchor_name, _) = args[0];
    sql.push_str(&format!(
        ",\n     joined AS (\n        SELECT {a}_unp.timestamp, \
            regexp_extract({a}_unp.col, '{id}', 1) AS id",
        a = anchor_name,
        id = id_extract_re,
    ));
    for (name, _) in args {
        sql.push_str(&format!(", {name}_unp.v AS {name}_v"));
    }
    sql.push_str(&format!("\n        FROM {a}_unp", a = anchor_name));
    for (name, _) in &args[1..] {
        sql.push_str(&format!(
            "\n            JOIN {n}_unp ON {a}_unp.timestamp = {n}_unp.timestamp \
                AND regexp_extract({a}_unp.col, '{id}', 1) = regexp_extract({n}_unp.col, '{id}', 1)",
            n = name,
            a = anchor_name,
            id = id_extract_re,
        ));
    }
    sql.push_str("\n     ),\n     rates AS (\n        SELECT timestamp, id");
    for (name, _) in args {
        sql.push_str(&format!(
            ",\n               irate_lag({n}_v, \
                LAG({n}_v) OVER (PARTITION BY id ORDER BY timestamp), \
                timestamp - LAG(timestamp) OVER (PARTITION BY id ORDER BY timestamp)) AS {n}_rate",
            n = name,
        ));
    }
    sql.push_str("\n        FROM joined\n     )\n     SELECT timestamp::DOUBLE/1e9 AS t, id, (");
    sql.push_str(formula);
    sql.push_str(") AS v FROM rates");
    sql
}

/// Wrap a helper-emitted SQL string with a `v / divisor` projection.
/// Used for the cores-from-nanoseconds conversion (`/ 1e9`).
pub fn scale_v(inner: String, divisor: f64) -> String {
    format!("WITH _w AS ({inner}) SELECT _w.* REPLACE (_w.v / {divisor} AS v) FROM _w")
}

/// Bucket-heatmap source SQL: emits per-row histogram deltas as a
/// `LIST<UBIGINT>` named `buckets`. The viewer route expands the LIST
/// into sparse `(t_idx, b_idx, count)` triples server-side.
///
/// `source` is the data source key — `None` resolves to the default
/// `_src` view; `Some("cachecannon")` resolves to `_src_cachecannon`.
/// (`per_source` views are materialized by metriken-query-sql.)
pub fn bucket_heatmap_sql(metric: &str, source: Option<&str>) -> String {
    let view = match source {
        Some(s) => format!("_src_{s}"),
        None => "_src".to_string(),
    };
    format!(
        r#"SELECT timestamp,
                  h2_delta("{metric}:buckets",
                           LAG("{metric}:buckets") OVER (ORDER BY timestamp)) AS buckets
           FROM {view}
           ORDER BY timestamp"#
    )
}

/// Quantile-spectrum source SQL: emits, per-row, a `LIST<UBIGINT>` of
/// computed quantile values (one entry per `q` in `quantiles`, in
/// order). The viewer route unpacks the LIST into per-quantile parallel
/// arrays for the frontend.
///
/// `p` is the histogram's `grouping_power` (lookup-based via
/// `MetricCatalog::histogram_p_by_metric`). Without it `h2_quantiles`
/// defaults to `p=3`, which under-resolves higher-`p` recorders.
pub fn quantile_spectrum_sql(
    metric: &str,
    quantiles: &[f64],
    p: u8,
    source: Option<&str>,
) -> String {
    let view = match source {
        Some(s) => format!("_src_{s}"),
        None => "_src".to_string(),
    };
    let qs = quantiles
        .iter()
        .map(|q| format!("{q}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"WITH d AS (
              SELECT timestamp,
                     h2_delta("{metric}:buckets",
                              LAG("{metric}:buckets") OVER (ORDER BY timestamp)) AS d
              FROM {view}
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  h2_quantiles(d, [{qs}], {p}) AS qs
           FROM d
           WHERE d IS NOT NULL
           ORDER BY t"#
    )
}

/// Like `hist_percentile_series` but for the "Overall" pattern: combine all
/// `:buckets` columns matching a regex into a single histogram per row via
/// `h2_combine`, then compute the per-quantile fan-out. PromQL's bare-metric
/// histogram (`syscall_latency`) implicitly aggregates across labels; the SQL
/// emits `h2_combine` over the matching label-suffixed columns.
pub fn hist_percentile_series_combined(buckets_re: &str) -> String {
    // Emits `h2_combine_lol(...)`, a shared pure-SQL macro
    // (defined in `metriken-query-sql/src/shared_macros.sql`) that
    // accepts a single `LIST<LIST<UBIGINT>>` and folds it element-
    // wise. The macro is loaded on both the native and wasm DuckDB
    // builds via `SHARED_MACROS`, so the same dashboard SQL binds on
    // both backends. The native variadic UDF `h2_combine(c1, …, cN)`
    // is the fast path for direct column-by-column callers and is
    // unchanged; this macro exists specifically for the
    // `[*COLUMNS('regex')]` column-spread shape, which wraps the
    // spread in a LIST literal that the variadic UDF can't bind.
    format!(
        r#"WITH combined AS (
              SELECT timestamp,
                     h2_combine_lol([*COLUMNS('{buckets_re}')]) AS b
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
