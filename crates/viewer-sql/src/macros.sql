-- Pure-SQL implementations of the H2 histogram operators, registered as
-- DuckDB macros. Drop-in replacements (same names, same signatures) for the
-- duckdb-rs vscalar UDFs in /work/duckdb-prototyping/duck/src/udf.rs.
--
-- Source of truth for the WASM viewer's macro layer. Validated end-to-end
-- by /work/duckdb-prototyping/wasm-poc/ (11 parity cases against the
-- canonical Rust UDFs, including a real-parquet headline query). Kept in
-- sync with that repo's host/macros_pure_sql.mjs.
--
-- Conventions:
--   - p (grouping_power) defaults to 3 (rezolus metrics.parquet uses p=3).
--   - All inputs cast inside the body so macros compose under list_transform
--     lambdas (DuckDB macro params are textual; explicit casts give the
--     binder type info).
--   - Arithmetic uses // for integer division (DuckDB / is float division).
--
-- Performance: at W=496 buckets × 200 rows, h2_quantile is ~50 ms in the
-- browser via list_reduce-with-target-in-accumulator. See
-- /work/duckdb-prototyping/wasm-poc/host/perf_node.mjs and the perf
-- section of the plan doc.

-- ---- Bound math (single macro w/ default p=3) ----
-- DuckDB OR REPLACE replaces the entire macro entry, so we use named-default
-- parameters instead of declaring overloads. Callers can write h2_lower(idx)
-- or h2_lower(idx, 7) or h2_lower(idx, p := 7).
CREATE OR REPLACE MACRO h2_lower(idx, p := 3) AS
    CASE WHEN idx < (1 << (p + 1))
         THEN idx::UBIGINT
         ELSE (1::UBIGINT << (p + ((idx >> p) - 1)))
              + ((idx & ((1 << p) - 1))::UBIGINT << ((idx >> p) - 1))
    END;

CREATE OR REPLACE MACRO h2_upper(idx, p := 3) AS
    CASE
      WHEN idx + 1 = (64 - p + 1) * (1 << p) THEN 18446744073709551615::UBIGINT
      WHEN idx < (1 << (p + 1)) THEN idx::UBIGINT
      ELSE (1::UBIGINT << (p + ((idx >> p) - 1)))
           + (((idx & ((1 << p) - 1)) + 1)::UBIGINT << ((idx >> p) - 1))
           - 1::UBIGINT
    END;

-- Use // (integer division) — DuckDB / is float division.
CREATE OR REPLACE MACRO h2_midpoint(idx, p := 3) AS
    (CASE WHEN idx < (1 << (p + 1))
         THEN idx::UBIGINT
         ELSE (1::UBIGINT << (p + ((idx >> p) - 1)))
              + ((idx & ((1 << p) - 1))::UBIGINT << ((idx >> p) - 1))
    END)
    + ((CASE
          WHEN idx + 1 = (64 - p + 1) * (1 << p) THEN 18446744073709551615::UBIGINT
          WHEN idx < (1 << (p + 1)) THEN idx::UBIGINT
          ELSE (1::UBIGINT << (p + ((idx >> p) - 1)))
               + (((idx & ((1 << p) - 1)) + 1)::UBIGINT << ((idx >> p) - 1))
               - 1::UBIGINT
        END)
        - (CASE WHEN idx < (1 << (p + 1))
            THEN idx::UBIGINT
            ELSE (1::UBIGINT << (p + ((idx >> p) - 1)))
                 + ((idx & ((1 << p) - 1))::UBIGINT << ((idx >> p) - 1))
          END)
      ) // 2::UBIGINT;

-- ---- Aggregators over LIST<UBIGINT> ----

-- h2_total: wrapping sum. DuckDB list_sum errors on overflow; for
-- non-pathological counters this matches Rust's wrapping_add.
CREATE OR REPLACE MACRO h2_total(b) AS list_sum(b::UBIGINT[])::UBIGINT;

-- h2_delta(b1, b0): element-wise saturating subtract.
CREATE OR REPLACE MACRO h2_delta(b1, b0) AS
    list_transform(
        generate_series(1, least(length(b1::UBIGINT[]), length(b0::UBIGINT[]))),
        i -> CASE WHEN (b1::UBIGINT[])[i] >= (b0::UBIGINT[])[i]
                  THEN (b1::UBIGINT[])[i] - (b0::UBIGINT[])[i]
                  ELSE 0::UBIGINT END
    );

-- h2_quantile(b, q): nearest-rank quantile for default p=3.
-- O(W) via list_reduce. Trick: precompute target ONCE in the initial
-- accumulator's `target` field (DuckDB evaluates the initial expression a
-- single time). The lambda then reads acc.target for free. The previous
-- list_transform-based version recomputed list_sum(list_slice(b, 1, i))
-- for every i, giving O(W²) and hitting 46s for 200 rows × W=496.
--
-- Body is a raw CASE expression (no SELECT/CTE/subquery) so callers can
-- invoke h2_quantile from inside a list_transform lambda.
CREATE OR REPLACE MACRO h2_quantile(b, q) AS
    CASE
        WHEN list_sum(b::UBIGINT[]) IS NULL OR list_sum(b::UBIGINT[])::UBIGINT = 0::UBIGINT
            THEN NULL::UBIGINT
        ELSE h2_upper(list_reduce(
            list_transform(b::UBIGINT[],
                v -> {val: v, target: 0::UBIGINT, sum: 0::UBIGINT, idx: 0, found: false}),
            (acc, p) -> CASE
                WHEN acc.found THEN acc
                WHEN acc.sum + p.val >= acc.target
                    THEN {val: 0::UBIGINT, target: acc.target,
                          sum: acc.sum + p.val, idx: acc.idx + 1, found: true}
                ELSE {val: 0::UBIGINT, target: acc.target,
                      sum: acc.sum + p.val, idx: acc.idx + 1, found: false}
            END,
            {val: 0::UBIGINT,
             target: greatest(ceil(q::DOUBLE * list_sum(b::UBIGINT[])::DOUBLE)::UBIGINT, 1::UBIGINT),
             sum: 0::UBIGINT, idx: -1, found: false}
        ).idx)::UBIGINT
    END;

-- ---- Convenience layer ----

CREATE OR REPLACE MACRO h2_count_in_range(b, lo, hi, p := 3) AS
    list_sum(
        list_transform(
            generate_series(1, length(b::UBIGINT[])),
            i -> CASE
                WHEN h2_lower(i - 1, p) >= lo::UBIGINT
                 AND h2_upper(i - 1, p) <= hi::UBIGINT
                THEN (b::UBIGINT[])[i]
                ELSE 0::UBIGINT
            END
        )
    )::UBIGINT;

CREATE OR REPLACE MACRO h2_quantiles(b, qs, p := 3) AS
    list_transform(qs::DOUBLE[], q -> h2_quantile(b, q));

CREATE OR REPLACE MACRO h2_combine(lol) AS
    list_transform(
        generate_series(1, list_max(list_transform(lol, h -> length(h::UBIGINT[])))),
        j -> list_sum(list_transform(lol, h -> coalesce((h::UBIGINT[])[j], 0::UBIGINT)))::UBIGINT
    );

CREATE OR REPLACE MACRO hist_p(buckets, q)         AS h2_quantile(buckets, q);
CREATE OR REPLACE MACRO hist_p50(buckets)          AS h2_quantile(buckets, 0.50);
CREATE OR REPLACE MACRO hist_p90(buckets)          AS h2_quantile(buckets, 0.90);
CREATE OR REPLACE MACRO hist_p99(buckets)          AS h2_quantile(buckets, 0.99);
CREATE OR REPLACE MACRO hist_p999(buckets)         AS h2_quantile(buckets, 0.999);

CREATE OR REPLACE MACRO hist_irate_quantile(buckets, q, ts) AS
    h2_quantile(h2_delta(buckets, LAG(buckets) OVER (ORDER BY ts)), q);

CREATE OR REPLACE MACRO hist_rate5m_quantile(buckets, q, ts) AS
    h2_quantile(h2_delta(buckets, LAG(buckets, 300) OVER (ORDER BY ts)), q);

-- ---- Layer A: rate / delta primitives ----
-- Layer A primitives copy verbatim from /work/duckdb-prototyping/duck/src/macros.rs:20-26
-- so dashboard SQL using these names is identical between native and wasm.

CREATE OR REPLACE MACRO irate_1s(c, ts) AS c - LAG(c) OVER (ORDER BY ts);

CREATE OR REPLACE MACRO delta_1s(c, ts) AS c - LAG(c) OVER (ORDER BY ts);

CREATE OR REPLACE MACRO rate_5m(c, ts) AS (c - LAG(c, 300) OVER (ORDER BY ts)) / 300.0;

-- ---- irate_lag: emulates the canonical Rust UDF on the wasm side ----
-- Native registers a vscalar UDF in /work/metriken/metriken-query-sql/src/udf.rs
-- which runs in C++ for perf (~8x faster than this macro form per the bench).
-- Wasm has no UDF support, so we provide a same-name same-semantics macro:
--   prev IS NULL                  → NULL (first sample)
--   curr >= prev                  → (curr - prev) / dt_seconds
--   curr <  prev (counter reset)  → curr / dt_seconds
CREATE OR REPLACE MACRO irate_lag(curr, prev, dt_ns) AS
    CASE
        WHEN prev IS NULL THEN NULL
        WHEN curr >= prev THEN ((curr - prev)::DOUBLE / NULLIF(dt_ns::DOUBLE / 1e9, 0))
        ELSE (curr::DOUBLE / NULLIF(dt_ns::DOUBLE / 1e9, 0))
    END;

-- ---- Layer B: dashboard-concept helpers ----
-- Each composes Layer A primitives. Expanding by hand recovers the same SQL
-- the original PromQL `irate(...)` formulas spell out. Names + signatures
-- match /work/duckdb-prototyping/duck/src/macros.rs:54-89 verbatim.

-- CPU fraction (0..1) — works for total CPU busy and for per-state usage.
CREATE OR REPLACE MACRO cpu_busy_pct(usage, cores, ts) AS
    irate_1s(usage, ts) / cores / 1e9;

-- Instructions per cycle.
CREATE OR REPLACE MACRO ipc(instructions, cycles, ts) AS
    irate_1s(instructions, ts) / NULLIF(irate_1s(cycles, ts), 0);

-- Effective CPU frequency in Hz.
CREATE OR REPLACE MACRO frequency_hz(tsc, aperf, mperf, cores, ts) AS
    irate_1s(tsc, ts) * irate_1s(aperf, ts) / NULLIF(irate_1s(mperf, ts), 0) / cores;

-- Instructions per nanosecond (wall-clock-normalised throughput).
CREATE OR REPLACE MACRO ipns(instructions, cycles, tsc, aperf, mperf, cores, ts) AS
    ipc(instructions, cycles, ts)
    * irate_1s(tsc, ts) * irate_1s(aperf, ts)
    / NULLIF(irate_1s(mperf, ts) * cores * 1e9, 0);

-- L3 cache hit fraction.
CREATE OR REPLACE MACRO l3_hit_pct(miss, access, ts) AS
    1 - irate_1s(miss, ts) / NULLIF(irate_1s(access, ts), 0);

-- Branch misprediction fraction.
CREATE OR REPLACE MACRO branch_miss_pct(misses, branches, ts) AS
    irate_1s(misses, ts) / NULLIF(irate_1s(branches, ts), 0);

-- DTLB misses per thousand instructions.
CREATE OR REPLACE MACRO dtlb_mpki(misses, instructions, ts) AS
    irate_1s(misses, ts) / NULLIF(irate_1s(instructions, ts), 0) * 1000;

-- GPU memory used as fraction of total (used + free). No window needed.
CREATE OR REPLACE MACRO gpu_mem_used_pct(used, free) AS
    used / NULLIF(used + free, 0);

-- Bandwidth in bits per second from a byte counter.
CREATE OR REPLACE MACRO bps_from_bytes(bytes, ts) AS
    irate_1s(bytes, ts) * 8;
