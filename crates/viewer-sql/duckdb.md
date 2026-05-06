# duckdb-wasm notes for `viewer-sql`

Constraints and quirks of duckdb-wasm that shaped this crate's design.
Pinned to the version we ship: `@duckdb/duckdb-wasm 1.33.1-dev45.0`.

## Why we use AsyncDuckDB (worker-backed), not sync DuckDB

The sync `DuckDB`/`DuckDBConnection` runs queries on the main thread; the
async one runs them in a Web Worker. We use **async** for two reasons:

1. **Sync `read_parquet` is broken in the browser.** Calling `SELECT … FROM
   read_parquet('<registered_name>')` against a `registerFileBuffer`-registered
   parquet throws `RuntimeError: function signature mismatch` deep inside the
   WASM. `SELECT 1+1` and `SELECT … FROM (VALUES …)` work fine, so the
   WASM/JS bridge is intact — only the FS hook for `read_parquet` is broken
   under the browser-blocking variant. Reproduced on both 1.29 and 1.33.
2. Even if it worked, sync would block the UI thread on every dashboard
   render. The async worker model is the right shape for an interactive viewer.

## JS UDFs are not viable for our use case

`conn.createScalarFunction(name, returnType, fn)` exists, but with two
restrictions that together rule it out:

1. **Sync only.** `AsyncDuckDBConnection` does not expose
   `createScalarFunction`. Even if we could solve the sync `read_parquet` bug,
   the perf hit of running queries on the main thread would defeat the point.
2. **Limited input types.** Even on sync, `createScalarFunction` accepts only:

   | Type        | Status |
   |-------------|--------|
   | `INTEGER`   | OK     |
   | `DOUBLE`    | OK     |
   | `VARCHAR`   | OK     |
   | `STRUCT`    | OK     |
   | `BIGINT`    | rejected — `Unsupported UDF argument type BIGINT` |
   | `UBIGINT`   | rejected |
   | `UINTEGER`  | rejected |
   | `SMALLINT`  | rejected |
   | `REAL`      | rejected (`FLOAT`) |
   | `BOOLEAN`   | rejected |
   | `INTEGER[]` | rejected |
   | `UBIGINT[]` | rejected |
   | other lists | rejected |

   The histogram operators we need (`h2_quantile` etc.) take `LIST<UBIGINT>`.
   The rate operators want `UBIGINT` / `BIGINT` for counters and nanosecond
   timestamps. None of them fit the supported set.

Re-probed under both 1.29 and 1.33 with the same result. If duckdb-wasm ever
(a) exposes `createScalarFunction` on AsyncDuckDB AND (b) accepts BIGINT /
UBIGINT / list types, we could bring back JS UDFs for perf-critical paths.
Until then we go through pure-SQL macros.

The trade-off is real but bounded: native (duckdb-rs with vscalar UDFs) is
~8× faster than the wasm macro path on the H2 quantile bench (W=496 × 200
rows: 4.7ms vs 37ms). Acceptable for a browser viewer.

## SQL quirks the dashboard SQL emitter must respect

### `[*COLUMNS('regex')]` splats into a list literal

This is the foundation of the schema-independent SQL strategy. The regex
resolves at parse time against whichever parquet is loaded:

```sql
list_sum([*COLUMNS('cpu_cycles/[0-9]+')]::UBIGINT[])::UBIGINT
```

Works. Required cast to `UBIGINT[]` because the splat produces an untyped
list literal.

### Multiple `STAR/COLUMNS` in the same expression are rejected

```sql
-- FAILS: "Binder Error: Multiple different STAR/COLUMNS in the same expression are not supported"
SELECT list_sum([*COLUMNS('A/[0-9]+')]) / list_sum([*COLUMNS('B/[0-9]+')]) FROM _src
```

**Workaround**: split each `[*COLUMNS]` into its own SELECT projection in a
CTE, then combine in the outer SELECT. This is the canonical pattern
dashboard SQL uses for any ratio / multi-aggregation:

```sql
WITH agg AS (
    SELECT timestamp,
           list_sum([*COLUMNS('cpu_instructions/[0-9]+')]::UBIGINT[]) AS instr,
           list_sum([*COLUMNS('cpu_cycles/[0-9]+')]::UBIGINT[]) AS cyc
    FROM _src
)
SELECT ipc(instr, cyc, timestamp) AS v FROM agg
```

### `UNPIVOT … ON COLUMNS(...)` preserves `timestamp`

Per-id fan-out works via UNPIVOT — the non-pivoted columns survive:

```sql
UNPIVOT (SELECT timestamp, COLUMNS('cpu_cycles/[0-9]+') FROM _src)
    ON COLUMNS('cpu_cycles/[0-9]+')
    INTO NAME col VALUE v
```

Output is `(timestamp, col, v)`. Extract the id with `regexp_extract(col,
'/([0-9]+)$', 1)`.

### Macro bodies cannot wrap a SELECT/CTE/subquery if called from inside a lambda

If a macro body looks like `(SELECT … FROM …)`, it opens a new scope. When
that macro is then invoked from inside a `list_transform` lambda (e.g.
`h2_quantiles(b, qs)` calls `list_transform(qs, q -> h2_quantile(b, q))`),
the lambda's parameter `q` becomes invisible to the inner scope and the
binder errors with "Referenced column not found".

Fix: macro bodies stay as raw expressions (CASE / list_transform). Anything
that needs intermediate state goes via `list_reduce` with the state in the
accumulator (see `h2_quantile` for the canonical pattern).

### `/` is float division, `//` is integer division

DuckDB's `/` operator always returns DOUBLE. UBIGINT-on-UBIGINT division
also returns DOUBLE. Use `//` (or `div(a, b)`) for integer division when the
expected result type is a UBIGINT / BIGINT. Bit us in `h2_midpoint` early.

### Lambda parameter names are SQL-keyword-sensitive

`inner` is a SQL keyword (`INNER JOIN`). Using it as a lambda parameter
breaks the parser:

```sql
-- FAILS: Parser Error: syntax error at or near "->"
list_transform(lol, inner -> length(inner))
```

Pick parameter names like `h`, `x`, `v` instead.

### `MACRO foo(a, p := 3)` accepts both positional and named-arg overrides

`CREATE OR REPLACE MACRO foo(a, p := 3)` accepts:
- `foo(5)` — uses default
- `foo(5, 7)` — positional override
- `foo(5, p := 7)` — named-arg override

Both 1.29 and 1.33 accept all three forms. Earlier 1.29 testing showed a
case where the positional form failed inside macro bodies — couldn't
reproduce on 1.33. Use either form; named-arg is clearer for callers with
multiple defaults.

### `CREATE OR REPLACE MACRO` replaces, doesn't add overloads

DuckDB does not support arity-based MACRO overloading — the second
`CREATE OR REPLACE MACRO foo(...)` replaces the first entirely, regardless
of parameter count. Use named-default parameters (`MACRO foo(a, p := 3)`)
for "overloads" instead.

### List type matching for `list_reduce` accumulators

`list_reduce(list, lambda, initial)` requires the initial accumulator's
type to match the list's element type. If the accumulator wants to be a
struct but the input is `UBIGINT[]`, **pre-transform the input list to
the same struct type via `list_transform`**:

```sql
list_reduce(
    list_transform(b, v -> {val: v, sum: 0::UBIGINT, idx: 0, found: false}),
    (acc, p) -> /* … */,
    {val: 0::UBIGINT, sum: 0::UBIGINT, idx: -1, found: false}
)
```

This is how `h2_quantile` achieves O(W) behavior — the target value is
precomputed in the initial accumulator and the lambda reads `acc.target`
for free.

## Bundle/loader notes

- **Browser**: import via jsdelivr CDN (`https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm@<ver>/+esm`).
  Avoids pulling npm into the rezolus repo. The page exists at
  `site/viewer-sql/lib/script.js`.
- **Node** (for headless tests): npm-installed `@duckdb/duckdb-wasm` plus
  `web-worker` polyfill. The polyfill 1.3.0 has a broken ESM entry — use
  `createRequire` to import its CJS variant.
- **AsyncDuckDB worker construction in browser** needs to wrap the worker
  URL in a Blob to dodge cross-origin worker restrictions:
  ```js
  const worker_url = URL.createObjectURL(
      new Blob([`importScripts("${bundle.mainWorker}");`], { type: 'text/javascript' })
  );
  ```

## Performance reference (from the bench)

200 rows × W bucket-width, headline `h2_quantile(h2_delta(b, LAG(b)), 0.99)`:

| W   | Native + canonical UDFs | Native + macros | WASM + macros |
|-----|-------------------------|-----------------|---------------|
| 8   | 1.2 ms                  | 6.7 ms (5.6×)   | 8.6 ms (7.2×) |
| 64  | 1.7 ms                  | 9.8 ms (5.8×)   | 16 ms (9.4×)  |
| 256 | 2.7 ms                  | 23 ms (8.5×)    | 29 ms (10.7×) |
| 496 | 4.7 ms                  | 37 ms (7.9×)    | 48 ms (10.2×) |

The macro path is O(W) (was O(W²) before the `list_reduce`-with-target-in-acc
fix). Native UDFs run in compiled C++ and are predictably ~8× faster than
the macro form. WASM adds another ~30% on top of native macros.

For Rezolus dashboards (3600 rows × ~10 plots × 1 quantile each), expect
~5-10s cold render in the browser, ~1s server-side via UDFs.
