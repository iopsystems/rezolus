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
