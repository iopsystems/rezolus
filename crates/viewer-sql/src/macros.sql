-- Wasm-only H2 replacement macros for the static-site viewer.
--
-- The 19 truly-shared macros (irate_1s, rate_5m, hist_p*, cpu_busy_pct,
-- ipc, frequency_hz, ipns, l3_hit_pct, branch_miss_pct, dtlb_mpki,
-- gpu_mem_used_pct, bps_from_bytes, hist_irate_quantile,
-- hist_rate5m_quantile, delta_1s, hist_p*) now live in
-- /work/metriken/metriken-query/src/shared_macros.sql and are
-- prepended at runtime by `pure_sql_macros()` in lib.rs.
--
-- The macros below replace the Rust vscalar UDFs in
-- /work/metriken/metriken-query/src/udf.rs (h2_lower, h2_upper,
-- h2_midpoint, h2_total, h2_delta, h2_quantile, h2_count_in_range,
-- h2_quantiles, h2_combine, irate_lag). The native crate registers
-- those as C++-backed vscalars; duckdb-wasm doesn't accept Rust
-- vscalar registrations so we re-implement them in pure SQL here.
--
-- Conventions:
--   - p (grouping_power) defaults to 3 (rezolus metrics.parquet uses p=3).
--   - All inputs cast inside the body so macros compose under list_transform
--     lambdas (DuckDB macro params are textual; explicit casts give the
--     binder type info).
--   - Arithmetic uses // for integer division (DuckDB / is float division).
--
-- Parity tests for both this file and the shared macros live at
-- crates/viewer-sql/tests/macros.rs.

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

-- h2_quantile(b, q, gp := 3): nearest-rank quantile.
-- O(W) via list_reduce. Trick: precompute target ONCE in the initial
-- accumulator's `target` field (DuckDB evaluates the initial expression a
-- single time). The lambda then reads acc.target for free. The previous
-- list_transform-based version recomputed list_sum(list_slice(b, 1, i))
-- for every i, giving O(W²) and hitting 46s for 200 rows × W=496.
--
-- Body is a raw CASE expression (no SELECT/CTE/subquery) so callers can
-- invoke h2_quantile from inside a list_transform lambda.
--
-- The grouping power is named `gp` (not `p`) to avoid shadowing the
-- list_reduce lambda's accumulator pair parameter (also named `p` —
-- DuckDB macro params and lambda params share a namespace at expansion
-- time, and a name collision would silently bind h2_upper's `p` to the
-- lambda accumulator). Callers may pass it positionally or by name.
CREATE OR REPLACE MACRO h2_quantile(b, q, gp := 3) AS
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
        ).idx, gp)::UBIGINT
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
    list_transform(qs::DOUBLE[], q -> h2_quantile(b, q, p));

-- h2_combine(lol) used to live here for the wasm side. It moved into
-- shared_macros.sql under the name `h2_combine_lol` so the native and
-- wasm backends use the same name + body for the LIST<LIST<UBIGINT>>
-- shape. The variadic `h2_combine(c1, c2, ...)` UDF on native is
-- intentionally not mirrored here (DuckDB macros can't be variadic);
-- direct-column callers stay native-only.

-- ---- irate_lag: emulates the canonical Rust UDF on the wasm side ----
-- Native registers a vscalar UDF in /work/metriken/metriken-query/src/udf.rs
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
