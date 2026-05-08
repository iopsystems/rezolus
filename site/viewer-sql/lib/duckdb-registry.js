// JS-side library for driving `crates/viewer-sql/` (the duckdb-wasm WASM
// backend) from a browser. Shaped to mirror `WasmCaptureRegistry` in
// `crates/viewer/src/lib.rs` so the Mithril viewer at `site/viewer/` can
// swap PromQL → SQL with minimum data-layer churn: same method names,
// same captureId-keyed surface, same Prometheus matrix-shaped results.
//
// What's here:
//   - `class CaptureRegistry`  — top-level entry point. One per page.
//   - `class CaptureSession`   — one per attached capture. Internal.
//   - `class WorkerPool`       — N AsyncDuckDB workers per capture for
//                                  query parallelism. Internal.
//   - Pure helpers (`partitionPlots`, `rowsToPerPlotMatrix`,
//                                  `wrapWithSrcCte`, `sqlReferencesMissingColumn`)
//
// Consumers: `site/viewer-sql/lib/preview.js` (the standalone preview)
// and `site/viewer/lib/...` (the production Mithril viewer, after the
// Stage 2 swap).

import * as duckdb from 'https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm@1.33.1-dev45.0/+esm';
import * as arrow from 'https://cdn.jsdelivr.net/npm/apache-arrow@17.0.0/+esm';
import init, { ViewerSql, pure_sql_macros } from '../pkg/wasm_viewer_sql.js';

// ─── duckdb-wasm bootstrap ────────────────────────────────────────────

async function bootDuckDB() {
    const bundles = duckdb.getJsDelivrBundles();
    const bundle = await duckdb.selectBundle(bundles);
    const worker_url = URL.createObjectURL(
        new Blob([`importScripts("${bundle.mainWorker}");`], { type: 'text/javascript' })
    );
    const worker = new Worker(worker_url);
    URL.revokeObjectURL(worker_url);
    const logger = { log: () => {}, info: () => {}, warn: () => {}, error: () => {} };
    const db = new duckdb.AsyncDuckDB(logger, worker);
    await db.instantiate(bundle.mainModule, bundle.pthreadWorker);
    return db;
}

async function registerMacros(conn) {
    const sql = pure_sql_macros();
    const statements = sql.split(/;\s*$/m).map((s) => s.trim()).filter(Boolean);
    for (const stmt of statements) await conn.query(stmt);
}

// ─── parquet schema introspection ────────────────────────────────────

async function buildMetadata(conn, filename, registeredName) {
    const desc = await conn.query(`DESCRIBE SELECT * FROM read_parquet('${registeredName}')`);
    const counters = {}, gauges = {}, histograms = {};
    for (const row of desc.toArray().map((r) => r.toJSON())) {
        const name = row.column_name;
        if (name === 'timestamp' || name === 'duration') continue;
        const t = String(row.column_type);
        if (t === 'UBIGINT[]' || t === 'BIGINT[]') histograms[name] = (histograms[name] ?? 0) + 1;
        else if (t === 'BIGINT' || t === 'INTEGER' || t === 'TINYINT') gauges[name] = (gauges[name] ?? 0) + 1;
        else if (t === 'UBIGINT' || t === 'UINTEGER' || t === 'UTINYINT') counters[name] = (counters[name] ?? 0) + 1;
    }
    const tr = await conn.query(`SELECT min(timestamp)::BIGINT AS lo, max(timestamp)::BIGINT AS hi FROM read_parquet('${registeredName}')`);
    const trRow = tr.toArray()[0]?.toJSON() ?? {};
    const lo = trRow.lo == null ? null : BigInt(trRow.lo);
    const hi = trRow.hi == null ? null : BigInt(trRow.hi);
    const time_range_ns = lo != null && hi != null ? [lo, hi] : null;
    let interval_seconds = 1.0;
    const ts2 = await conn.query(`SELECT timestamp::BIGINT AS t FROM read_parquet('${registeredName}') ORDER BY timestamp LIMIT 2`);
    const tsRows = ts2.toArray().map((r) => r.toJSON());
    if (tsRows.length === 2) {
        interval_seconds = Number(BigInt(tsRows[1].t) - BigInt(tsRows[0].t)) / 1e9;
    }
    // Pull the parquet's file-level KV metadata (per_source_metadata,
    // systeminfo, source, version, etc.). Drives `init_templates`'s
    // service-extension detection and `systeminfo()`. Heavy entries like
    // `ARROW:schema` (the embedded IPC schema) and `descriptions` are
    // dropped — they're large and we don't need them on the Rust side.
    const file_metadata = {};
    const kvRows = (await conn.query(
        `SELECT key::VARCHAR AS k, value::VARCHAR AS v FROM parquet_kv_metadata('${registeredName}')`,
    )).toArray().map((r) => r.toJSON());
    for (const { k, v } of kvRows) {
        if (k === 'ARROW:schema') continue;
        file_metadata[k] = v;
    }
    // Prefer the parquet's recorded `source` over our generic fallback.
    const recordedSource = file_metadata.source ?? 'rezolus';
    const recordedVersion = file_metadata.version ?? '';
    return {
        interval_seconds, time_range_ns,
        source: recordedSource, version: recordedVersion, filename,
        parquet_name: registeredName, counters, gauges, histograms,
        file_metadata,
    };
}

// For multi-source combined parquets (one file produced by `parquet
// combine` from N sources) all metric columns are prefixed `<source>::`.
// Build a VIEW per source that aliases its columns back to their
// unprefixed names — dashboard SQL can then run verbatim against any
// chosen source's view.
// True when `t` is a non-list DuckDB type (e.g. UBIGINT, BIGINT,
// DOUBLE) — we only synthesize `:src<i>` per-source aliases for
// scalar metrics. List columns (histogram bucket arrays) go through
// `h2_combine` for cross-source aggregation, not `:src<i>`.
function isScalarType(t) {
    return typeof t === 'string' && !t.endsWith('[]');
}

// "Infrastructure" labels — Arrow field metadata keys that don't
// participate in the canonical column name (they describe where the
// value came from rather than which series it belongs to).
const NON_VALUE_METADATA_KEYS = new Set([
    'metric', 'metric_type', 'unit',
    'endpoint', 'instance', 'node', 'source',
    'grouping_power', 'max_value_power',
]);

// Resolve a parquet field to the column name the dashboard SQL would
// reference. For named-column parquets the column name is already
// canonical (`network_bytes/receive`, `cpu_usage/user/0`, …); pass it
// through. For numeric-encoded parquets the column name is just an
// ordinal (`117`) and the canonical identity lives in the field's
// Arrow metadata — rebuild the name from `metric` + value-label
// values. The order of value labels is metric-specific (e.g.
// `cpu_usage/<state>/<id>`); we sort alphabetically, which agrees
// with the named-column convention for every metric in our demo set
// that has 0 or 1 value labels. Multi-label metrics in numeric-encoded
// parquets remain a known limitation.
function canonicalAlias(colName, restAfterPrefix, metricName, metricType, fieldMetadata) {
    if (!metricName) return restAfterPrefix;
    // Column-name canonical: starts with `metric/`, or equals `metric`,
    // or equals `metric:buckets`. Trust it.
    if (
        restAfterPrefix === metricName
        || restAfterPrefix === `${metricName}:buckets`
        || restAfterPrefix.startsWith(`${metricName}/`)
    ) {
        return restAfterPrefix;
    }
    const valueLabels = [];
    for (const [k, v] of fieldMetadata.entries()) {
        if (NON_VALUE_METADATA_KEYS.has(k)) continue;
        valueLabels.push([k, v]);
    }
    valueLabels.sort((a, b) => a[0].localeCompare(b[0]));
    let name = metricName;
    for (const [, v] of valueLabels) name += `/${v}`;
    if (metricType === 'histogram') name += ':buckets';
    return name;
}

// Read the parquet's Arrow schema and return Map<column_name,
// {metric, metric_type, metadata}> for every field that carries a
// `metric` metadata key. Used by `buildSourceViews` to canonicalise
// numeric-encoded column aliases.
async function readFieldMetadata(conn, registeredName) {
    const sch = await conn.query(
        `SELECT value::VARCHAR AS v FROM parquet_kv_metadata('${registeredName}') WHERE key::VARCHAR = 'ARROW:schema'`,
    );
    const rows = sch.toArray();
    const out = new Map();
    if (rows.length === 0) return out;
    const b64 = rows[0].toJSON().v;
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    const reader = await arrow.RecordBatchReader.from(bytes);
    await reader.open();
    for (const f of reader.schema.fields) {
        const md = f.metadata;
        if (!md || !md.get) continue;
        const metric = md.get('metric');
        if (!metric) continue;
        out.set(f.name, {
            metric,
            metric_type: md.get('metric_type') ?? '',
            metadata: md,
        });
    }
    return out;
}

async function buildSourceViews(conn, registeredName) {
    const desc = await conn.query(`DESCRIBE SELECT * FROM read_parquet('${registeredName}')`);
    const descRows = desc.toArray().map((r) => r.toJSON());
    const cols = descRows.map((r) => r.column_name);
    const colType = new Map();
    for (const r of descRows) colType.set(r.column_name, String(r.column_type));
    const fieldMeta = await readFieldMetadata(conn, registeredName);
    const bySource = new Map();
    const aliasSeen = new Map(); // src → Set<alias>, for dedup
    for (const c of cols) {
        const m = c.match(/^([^:]+)::(.+)$/);
        if (!m) continue;
        const [, prefix, rest] = m;
        const meta = fieldMeta.get(c);
        const alias = meta
            ? canonicalAlias(c, rest, meta.metric, meta.metric_type, meta.metadata)
            : rest;
        if (!bySource.has(prefix)) {
            bySource.set(prefix, []);
            aliasSeen.set(prefix, new Set());
        }
        // Skip duplicate alias within a source — if two parquet columns
        // resolve to the same canonical name (would be a parquet
        // authoring bug), keep the first occurrence so CREATE VIEW
        // doesn't error on duplicate output column names.
        if (aliasSeen.get(prefix).has(alias)) continue;
        aliasSeen.get(prefix).add(alias);
        bySource.get(prefix).push({ orig: c, alias });
    }
    const columnsBySource = new Map();
    const q = (s0) => '"' + s0.replace(/"/g, '""') + '"';
    if (bySource.size === 0) {
        // Single-source parquet — no `::` prefixes. Build a synthetic
        // `_src_default` view that exposes every column plus `:src0`
        // aliases for scalar columns, so the multi-source-aware
        // avg/max/min emitters (which match `:src[0-9]+$`) work the
        // same way as in per-source views.
        const projections = cols.map((c) => q(c));
        for (const c of cols) {
            if (c === 'timestamp' || c === 'duration') continue;
            if (!isScalarType(colType.get(c))) continue;
            projections.push(`${q(c)} AS ${q(`${c}:src0`)}`);
        }
        await conn.query(
            `CREATE VIEW _src_default AS SELECT ${projections.join(', ')} FROM read_parquet('${registeredName}')`,
        );
        const colSet = new Set(cols);
        for (const c of cols) {
            if (c === 'timestamp' || c === 'duration') continue;
            if (isScalarType(colType.get(c))) colSet.add(`${c}:src0`);
        }
        columnsBySource.set('_src_default', colSet);
        return {
            sources: [],
            rezolusSources: [],
            columnsBySource,
            combinedView: null,
            defaultView: '_src_default',
        };
    }
    const sources = [...bySource.keys()].sort();
    const rezolusSources = sources.filter(
        (s) => bySource.get(s).some((a) => a.alias === 'memory_total'),
    );
    // Per-source view: each prefixed column is exposed under its bare
    // metric name AND a `:src0` synthetic alias. The `:src0` form lets
    // multi-source-aware emitters (avg/max/min over per-(source,id))
    // use a single regex that works in both single-source mode (one
    // entry per id) and combined mode (N entries per id, see below).
    for (const src of sources) {
        const aliases = bySource.get(src);
        const baseProjections = aliases.map((a) => `${q(a.orig)} AS ${q(a.alias)}`);
        const srcProjections = aliases
            .filter((a) => isScalarType(colType.get(a.orig)))
            .map((a) => `${q(a.orig)} AS ${q(`${a.alias}:src0`)}`);
        const projections = ['timestamp', 'duration']
            .filter((c) => cols.includes(c))
            .map((c) => q(c))
            .concat(baseProjections, srcProjections)
            .join(', ');
        const viewName = viewNameForSource(src);
        await conn.query(`CREATE VIEW ${viewName} AS SELECT ${projections} FROM read_parquet('${registeredName}')`);
        const colSet = new Set([
            ...['timestamp', 'duration'].filter((c) => cols.includes(c)),
            ...aliases.map((a) => a.alias),
        ]);
        for (const a of aliases) {
            if (isScalarType(colType.get(a.orig))) colSet.add(`${a.alias}:src0`);
        }
        columnsBySource.set(src, colSet);
    }
    // Multi-rezolus aggregation. The legacy PromQL evaluator's Tsdb
    // stores every source's series under a flat metric → series map,
    // so `sum(metric)` / `sum by (id) (...)` / `histogram_quantiles(...)`
    // implicitly aggregate across all rezolus sources. To match that
    // behaviour from SQL, build a combined view whose columns are the
    // per-source contributions summed at each timestamp:
    //   - scalar metrics: COALESCE(<src1>::col, 0) + COALESCE(<src2>::col, 0) + ...
    //   - histogram (LIST<UBIGINT>) metrics: h2_combine([COALESCE(...), ...])
    // Sources missing a given metric column contribute zero / empty list.
    let combinedView = null;
    if (rezolusSources.length >= 2) {
        const allMetrics = new Set();
        for (const src of rezolusSources) {
            for (const a of bySource.get(src)) allMetrics.add(a.alias);
        }
        const projections = ['timestamp', 'duration']
            .filter((c) => cols.includes(c))
            .map((c) => q(c));
        for (const metric of [...allMetrics].sort()) {
            // Collect (source-index, prefixed-column) for sources that
            // have this metric. The source index is its position in
            // `rezolusSources`, used as the `:src<i>` synthetic suffix.
            const contribs = [];
            let isList = false;
            for (let i = 0; i < rezolusSources.length; i++) {
                const src = rezolusSources[i];
                const a = bySource.get(src).find((x) => x.alias === metric);
                if (!a) continue;
                contribs.push({ srcIdx: i, prefixed: a.orig });
                const t = colType.get(a.orig) ?? '';
                if (t.endsWith('[]')) isList = true;
            }
            // Sum form: drives `sum(...)` / `sum by (id) (...)` /
            // `histogram_quantiles(...)` emitters.
            if (contribs.length === 1) {
                projections.push(`${q(contribs[0].prefixed)} AS ${q(metric)}`);
            } else if (isList) {
                const parts = contribs
                    .map((c) => `COALESCE(${q(c.prefixed)}, []::UBIGINT[])`)
                    .join(', ');
                projections.push(`h2_combine([${parts}]) AS ${q(metric)}`);
            } else {
                const parts = contribs.map((c) => `COALESCE(${q(c.prefixed)}, 0)`).join(' + ');
                projections.push(`(${parts}) AS ${q(metric)}`);
            }
            // Per-source `:src<i>` aliases for scalar columns: drive
            // multi-source-aware avg/max/min emitters (PromQL's
            // `avg(...)` / `max(...)` / `min(...)` aggregate over every
            // series, so we need each per-(source,id) value as its own
            // entry in the COLUMNS('regex') match).
            if (!isList) {
                for (const c of contribs) {
                    projections.push(
                        `${q(c.prefixed)} AS ${q(`${metric}:src${c.srcIdx}`)}`,
                    );
                }
            }
        }
        combinedView = '_src_rezolus_combined';
        await conn.query(
            `CREATE VIEW ${combinedView} AS SELECT ${projections.join(', ')} FROM read_parquet('${registeredName}')`,
        );
        columnsBySource.set(combinedView, new Set([
            ...['timestamp', 'duration'].filter((c) => cols.includes(c)),
            ...allMetrics,
        ]));
    }
    return { sources, rezolusSources, columnsBySource, combinedView, defaultView: null };
}

export function viewNameForSource(src) {
    return `_src_${src.replace(/[^a-zA-Z0-9_]/g, '_')}`;
}

// Build (or rebuild) `_cgroup_index` for the currently active source.
// Stores unprefixed column names so cgroup-page SQL JOINs resolve
// against the source's aliased `_src_<src>` view (or the
// `_src_rezolus_combined` view when `prefixSet` lists multiple
// rezolus sources, in which case we union the cgroup metadata over
// every source — same shape the legacy PromQL viewer's Tsdb produces).
async function buildCgroupIndex(conn, registeredName, sourcePrefix = null) {
    // Allow `sourcePrefix` to be a string (single source), an array
    // (combined-rezolus mode — union cgroups across sources), or null
    // (single-source parquet — no prefix to strip).
    const prefixes = sourcePrefix == null
        ? null
        : Array.isArray(sourcePrefix) ? sourcePrefix : [sourcePrefix];
    const sch = await conn.query(`SELECT value::VARCHAR AS v FROM parquet_kv_metadata('${registeredName}') WHERE key::VARCHAR = 'ARROW:schema'`);
    const rows = sch.toArray();
    await conn.query(`DROP TABLE IF EXISTS _cgroup_index`);
    await conn.query(`CREATE TABLE _cgroup_index(metric VARCHAR, column_name VARCHAR, name VARCHAR, id VARCHAR, labels MAP(VARCHAR, VARCHAR))`);
    if (rows.length === 0) return [];
    const b64 = rows[0].toJSON().v;
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    const reader = await arrow.RecordBatchReader.from(bytes);
    await reader.open();
    const cgroupRows = [];
    // De-dupe entries that appear identically (same metric + unprefixed
    // column_name) under multiple source prefixes — when a cgroup
    // exists on more than one rezolus node, the combined view sums
    // its bucket arrays into one column, so the index needs one row.
    const seen = new Set();
    for (const f of reader.schema.fields) {
        const md = f.metadata;
        if (!md || !md.get) continue;
        const metric = md.get('metric');
        if (!metric || !metric.startsWith('cgroup_')) continue;
        let columnName = f.name;
        if (prefixes) {
            const matched = prefixes.find((p) => columnName.startsWith(`${p}::`));
            if (!matched) continue;
            columnName = columnName.slice(matched.length + 2);
        }
        const dedupKey = `${metric}\u{1f}${columnName}`;
        if (seen.has(dedupKey)) continue;
        seen.add(dedupKey);
        const name = md.has('name') ? md.get('name') : null;
        const id = md.has('id') ? md.get('id') : null;
        const labels = {};
        for (const [k, v] of md.entries()) {
            if (['metric', 'metric_type', 'unit', 'name', 'id'].includes(k)) continue;
            labels[k] = v;
        }
        cgroupRows.push({ metric, column_name: columnName, name, id, labels });
    }
    if (cgroupRows.length === 0) return [];
    const esc = (s) => "'" + String(s).replace(/'/g, "''") + "'";
    const mapLit = (m) => {
        const keys = Object.keys(m);
        if (keys.length === 0) return 'MAP{}';
        return 'MAP{' + keys.map((k) => `${esc(k)}:${esc(m[k])}`).join(',') + '}';
    };
    const valueRows = cgroupRows
        .map((r) => `(${esc(r.metric)},${esc(r.column_name)},${r.name == null ? 'NULL' : esc(r.name)},${r.id == null ? 'NULL' : esc(r.id)},${mapLit(r.labels)})`)
        .join(',');
    await conn.query(`INSERT INTO _cgroup_index VALUES ${valueRows}`);
    return cgroupRows;
}

// JSON.stringify with BigInt support: emits BigInts as unquoted decimal
// numerics (lossless) since `metadata.time_range_ns` exceeds Number.MAX_SAFE_INTEGER
// and serde_json's u64 parser accepts decimal-number tokens.
function stringifyWithBigInt(value) {
    if (value === null) return 'null';
    if (typeof value === 'bigint') return value.toString();
    if (typeof value === 'number' || typeof value === 'string' || typeof value === 'boolean') return JSON.stringify(value);
    if (Array.isArray(value)) return '[' + value.map(stringifyWithBigInt).join(',') + ']';
    if (typeof value === 'object') {
        const parts = [];
        for (const k of Object.keys(value)) parts.push(JSON.stringify(k) + ':' + stringifyWithBigInt(value[k]));
        return '{' + parts.join(',') + '}';
    }
    throw new Error('cannot stringify ' + typeof value);
}

// ─── Worker pool: N AsyncDuckDB instances + connections + ViewerSql ──

// Boot one pool slot end-to-end: instantiate AsyncDuckDB, register the
// parquet bytes (zero-copy transfer — caller passes a fresh slice), run
// macro install + source views + cgroup index. Returns the slot fields
// the WorkerPool needs.
async function bootSlot(parquetBytes, registered, pickedSource) {
    const db = await bootDuckDB();
    await db.registerFileBuffer(registered, new Uint8Array(parquetBytes));
    const conn = await db.connect();
    await registerMacros(conn);
    const { sources, rezolusSources, columnsBySource, combinedView, defaultView } =
        await buildSourceViews(conn, registered);
    // The cgroup index needs to know which source prefix(es) to strip
    // when materialising column_name. For the combined-rezolus view
    // we union cgroup metadata from every rezolus source so all
    // selectable cgroups land in the index. For `_src_default`
    // (single-source parquets) the parquet's columns aren't prefixed,
    // so no stripping is needed.
    let cgroupPrefix;
    if (pickedSource === combinedView) cgroupPrefix = rezolusSources;
    else if (pickedSource === defaultView) cgroupPrefix = null;
    else cgroupPrefix = pickedSource;
    await buildCgroupIndex(conn, registered, cgroupPrefix);
    return { db, conn, sources, rezolusSources, columnsBySource, combinedView, defaultView };
}

// Round-robin query pool. AsyncDuckDB serialises through one Worker; for
// real parallelism we spawn N AsyncDuckDB instances with N independent
// Workers and route queries to whichever is idle.
class WorkerPool {
    constructor(slots, registeredName) {
        this.slots = slots;          // [{ db, conn, viewer, idle, columnsBySource, ... }]
        this.registered = registeredName;
        this.queue = [];             // pending [{ resolve, run }] entries
        for (const s of slots) s.idle = true;
    }
    get viewer() { return this.slots[0].viewer; }
    get conn() { return this.slots[0].conn; }
    _dispatch(slot, run, resolve, reject) {
        slot.idle = false;
        Promise.resolve()
            .then(() => run(slot))
            .then((v) => { resolve(v); this._release(slot); },
                  (e) => { reject(e); this._release(slot); });
    }
    _release(slot) {
        const next = this.queue.shift();
        if (next) this._dispatch(slot, next.run, next.resolve, next.reject);
        else slot.idle = true;
    }
    _enqueue(run) {
        return new Promise((resolve, reject) => {
            const free = this.slots.find((s) => s.idle);
            if (free) this._dispatch(free, run, resolve, reject);
            else this.queue.push({ run, resolve, reject });
        });
    }
    runQuery(sql, start, end, step) {
        return this._enqueue((slot) => slot.viewer.query_range(sql, start, end, step));
    }
    runRawQuery(sql) {
        return this._enqueue((slot) => slot.viewer.query_sql(sql));
    }
    setSourceRelation(viewName) {
        for (const s of this.slots) s.viewer.set_source_relation(viewName);
    }
    setSelectedCgroups(names) {
        for (const s of this.slots) s.viewer.set_selected_cgroups(names);
    }
    async rebuildCgroupIndex(sourcePrefix) {
        await Promise.all(this.slots.map((s) =>
            buildCgroupIndex(s.conn, this.registered, sourcePrefix)));
    }
    async close() {
        for (const s of this.slots) {
            try { await s.conn.close(); } catch {}
        }
    }
}

// ─── Pre-flight + combined-query helpers (pure, exported) ───────────

// Pre-flight check: does any `COLUMNS('regex')` in the SQL match no
// column in the active source's view? If so, submitting it would throw
// a Binder Error in the worker even though our Rust-side catch turns
// it into an empty matrix — the worker still logs to console. Replicate
// the regex check here so the query never goes to the worker.
export function sqlReferencesMissingColumn(sql, columnSet) {
    const re = /COLUMNS\('([^']+)'\)/g;
    let m;
    while ((m = re.exec(sql)) !== null) {
        let pattern;
        try { pattern = new RegExp(m[1]); }
        catch { continue; }
        let any = false;
        for (const c of columnSet) {
            if (pattern.test(c)) { any = true; break; }
        }
        if (!any) return true;
    }
    return false;
}

// Match `SELECT timestamp::DOUBLE/1e9 AS t, <expr> AS v FROM _src`.
// Group 1 is the per-plot projection expression.
const SIMPLE_GAUGE_RE = /^\s*SELECT\s+timestamp::DOUBLE\s*\/\s*1e9\s+AS\s+t\s*,\s*([\s\S]+?)\s+AS\s+v\s+FROM\s+_src\s*$/;

// Match `irate_total(re)`-shaped SQL. Group 1 is the inner regex.
const IRATE_TOTAL_RE = /^\s*WITH\s+agg\s+AS\s+\(\s*SELECT\s+timestamp\s*,\s*list_sum\(\[\*COLUMNS\('([^']+)'\)\]::UBIGINT\[\]\)\s+AS\s+s\s+FROM\s+_src\s*\)\s+SELECT\s+timestamp::DOUBLE\s*\/\s*1e9\s+AS\s+t\s*,\s*irate_1s\(s\s*,\s*timestamp\)\s+AS\s+v\s+FROM\s+agg\s*$/;

// Inspect each plot's SQL and partition into batches + loners. Each
// batch fires one combined SQL that produces N value columns; per-plot
// results are demuxed by column index.
export function partitionPlots(plots) {
    const gauges = [];
    const irates = [];
    const loners = [];
    for (const p of plots) {
        if (!p.sql_query) { loners.push(p); continue; }
        const sql = p.sql_query;
        // Cgroup-selection-bearing SQL would force per-selection cache
        // keys; keep those as loners.
        if (sql.includes('__SELECTED_CGROUPS__')) { loners.push(p); continue; }
        let m;
        if ((m = sql.match(SIMPLE_GAUGE_RE))) {
            gauges.push({ plot: p, expr: m[1] });
        } else if ((m = sql.match(IRATE_TOTAL_RE))) {
            irates.push({ plot: p, regex: m[1] });
        } else {
            loners.push(p);
        }
    }
    const batches = [];
    if (gauges.length >= 2) {
        const projs = gauges.map((g, i) => `(${g.expr}) AS v_${i}`).join(', ');
        batches.push({
            kind: 'gauge',
            plots: gauges.map((g) => g.plot),
            sqlBody: `SELECT timestamp::DOUBLE/1e9 AS t, ${projs} FROM _src`,
            demuxFn: rowsToPerPlotMatrix,
        });
    } else {
        for (const g of gauges) loners.push(g.plot);
    }
    if (irates.length >= 2) {
        const sums = irates.map((r, i) =>
            `list_sum([*COLUMNS('${r.regex}')]::UBIGINT[]) AS s_${i}`).join(', ');
        // Cast each irate_1s output to DOUBLE so int128 deltas don't
        // reach JS as arrow Int128 objects — `query_sql`'s BigInt-only
        // JSON replacer doesn't catch those, leading to doubly-quoted
        // strings in the row JSON.
        const rates = irates.map((_, i) => `irate_1s(s_${i}, timestamp)::DOUBLE AS v_${i}`).join(', ');
        batches.push({
            kind: 'irate_total',
            plots: irates.map((r) => r.plot),
            sqlBody:
                `WITH agg AS (SELECT timestamp, ${sums} FROM _src) ` +
                `SELECT timestamp::DOUBLE/1e9 AS t, ${rates} FROM agg`,
            demuxFn: rowsToPerPlotMatrix,
        });
    } else {
        for (const r of irates) loners.push(r.plot);
    }
    return { batches, loners };
}

// Convert rows-as-objects into the Prometheus-matrix shape the existing
// chart code expects.
export function rowsToPerPlotMatrix(rows, idx) {
    const values = [];
    for (const r of rows) {
        const v = r[`v_${idx}`];
        if (v == null) continue;
        values.push([Number(r.t), String(v)]);
    }
    if (values.length === 0) return [];
    return [{ metric: {}, values }];
}

// Wrap a body that references `_src` with the same time-windowed CTE
// `viewer-sql`'s `query_range` would apply.
export function wrapWithSrcCte(body, startSec, endSec, sourcePrefix) {
    const fromClause = sourcePrefix
        ? viewNameForSource(sourcePrefix)
        : "read_parquet('capture.parquet')";
    const startNs = BigInt(Math.floor(startSec * 1e9));
    const endNs = BigInt(Math.floor(endSec * 1e9));
    return `WITH _src AS (SELECT * FROM ${fromClause} `
        + `WHERE timestamp BETWEEN ${startNs} AND ${endNs}) `
        + `SELECT * FROM (${body}) ORDER BY t`;
}

// ─── CaptureSession: per-capture state (internal) ───────────────────

const REGISTERED_NAME = 'capture.parquet';

class CaptureSession {
    constructor({ pool, metadata, sources, rezolusSources, columnsBySource, pickedSource, combinedView, defaultView }) {
        this.pool = pool;
        this.metadata = metadata;
        this.sources = sources;
        this.rezolusSources = rezolusSources;
        this.columnsBySource = columnsBySource;
        this.pickedSource = pickedSource;
        this.combinedView = combinedView;
        this.defaultView = defaultView;
        // Per-session result cache: key = `${source}|${cgroupsKey}|${sql}`,
        // value = parsed Prometheus matrix `data.result` array. Time
        // window isn't keyed because it's constant per session today;
        // revisit when zoom/compare-time-shift arrives. Cgroup
        // selection only enters the key when the SQL references the
        // `__SELECTED_CGROUPS__` placeholder, so non-cgroup plots stay
        // hot across selection changes.
        this.resultCache = new Map();
        this.selectedCgroups = [];
    }
    cacheKey(sql) {
        const sourceKey = this.pickedSource ?? '';
        const cg = sql.includes('__SELECTED_CGROUPS__') ? this.selectedCgroups.join(',') : '';
        return `${sourceKey}|${cg}|${sql}`;
    }
    cachedResult(key) { return this.resultCache.get(key); }
    setCached(key, val) { this.resultCache.set(key, val); }
    activeColumns() {
        return this.columnsBySource?.get(this.pickedSource ?? '');
    }
    async setSource(sourceName) {
        this.pickedSource = sourceName;
        const rel = (sourceName === this.combinedView || sourceName === this.defaultView)
            ? sourceName
            : viewNameForSource(sourceName);
        this.pool.setSourceRelation(rel);
        let cgPrefix;
        if (sourceName === this.combinedView) cgPrefix = this.rezolusSources;
        else if (sourceName === this.defaultView) cgPrefix = null;
        else cgPrefix = sourceName;
        await this.pool.rebuildCgroupIndex(cgPrefix);
        // Cache entries from the previous source are still valid (key
        // includes source), no flush needed.
    }
    setSelectedCgroups(names) {
        this.selectedCgroups = Array.from(names);
        this.pool.setSelectedCgroups(this.selectedCgroups);
    }
    async close() { await this.pool.close(); }
}

// ─── CaptureRegistry: top-level entry point ─────────────────────────

let wasmInitPromise = null;
function ensureWasmInit() {
    if (!wasmInitPromise) wasmInitPromise = init();
    return wasmInitPromise;
}

export class CaptureRegistry {
    constructor({ workersPerCapture = 4 } = {}) {
        this.workersPerCapture = workersPerCapture;
        this.captures = new Map(); // captureId → CaptureSession
    }

    async attach(captureId, bytes, filename) {
        await ensureWasmInit();
        if (this.captures.has(captureId)) {
            await this.captures.get(captureId).close();
            this.captures.delete(captureId);
        }
        const N = this.workersPerCapture;
        // Boot slot 0 first so we can compute pickedSource from its
        // schema before the rest are spun up — passing pickedSource
        // into the remaining workers lets each one build a correctly-
        // scoped cgroup index in its own bootSlot call.
        const slot0 = await bootSlot(bytes.slice(), REGISTERED_NAME, null);
        const { sources, rezolusSources, columnsBySource, combinedView, defaultView } = slot0;
        // Source picker:
        //   - 2+ rezolus sources → use the combined view so SQL queries
        //     match the legacy PromQL evaluator's cross-source aggregation.
        //   - exactly 1 rezolus source → use that source's view (filename
        //     hint is moot when there's only one).
        //   - non-rezolus multi-source → filename hint, fallback to first.
        //   - single-source (no `::` prefixes) → `_src_default` view
        //     so multi-source-aware avg/max/min emitters still find
        //     `:src0` aliases.
        let pickedSource = null;
        if (combinedView) {
            pickedSource = combinedView;
        } else if (sources.length > 0) {
            const fnLower = filename.toLowerCase();
            pickedSource = rezolusSources.find((s) => fnLower.includes(s.toLowerCase()))
                ?? rezolusSources[0]
                ?? sources[0];
        } else if (defaultView) {
            pickedSource = defaultView;
        }
        if (pickedSource) {
            let cgPrefix;
            if (pickedSource === combinedView) cgPrefix = rezolusSources;
            else if (pickedSource === defaultView) cgPrefix = null;
            else cgPrefix = pickedSource;
            await buildCgroupIndex(slot0.conn, REGISTERED_NAME, cgPrefix);
        }

        const restSlots = await Promise.all(
            Array.from({ length: N - 1 }, () => bootSlot(bytes.slice(), REGISTERED_NAME, pickedSource))
        );
        const slots = [slot0, ...restSlots];
        const metadata = await buildMetadata(slot0.conn, filename, REGISTERED_NAME);
        const metaJson = stringifyWithBigInt(metadata);
        for (const s of slots) {
            s.viewer = new ViewerSql(s.conn, metaJson);
            if (pickedSource) {
                // The combined and default views' names are real SQL
                // identifiers — don't run them through `viewNameForSource`
                // (which sanitises a source label).
                const rel = (pickedSource === combinedView || pickedSource === defaultView)
                    ? pickedSource
                    : viewNameForSource(pickedSource);
                s.viewer.set_source_relation(rel);
            }
        }
        const pool = new WorkerPool(slots, REGISTERED_NAME);
        const session = new CaptureSession({
            pool, metadata, sources, rezolusSources, columnsBySource, pickedSource,
            combinedView, defaultView,
        });
        this.captures.set(captureId, session);
        return session;
    }

    async detach(captureId) {
        const s = this.captures.get(captureId);
        if (!s) return;
        await s.close();
        this.captures.delete(captureId);
    }

    has(captureId) { return this.captures.has(captureId); }
    session(captureId) {
        const s = this.captures.get(captureId);
        if (!s) throw new Error(`no capture attached at id ${JSON.stringify(captureId)}`);
        return s;
    }

    // Mirror of WasmCaptureRegistry.query_range. The `query` arg is now
    // an SQL string (not PromQL); the return shape is identical
    // Prometheus matrix JSON.
    query_range(captureId, sql, start, end, step) {
        return this.session(captureId).pool.runQuery(sql, start, end, step);
    }

    // Combined-query path: callers wrap their multi-projection SQL with
    // `wrapWithSrcCte` and submit here. Returns rows-as-objects JSON.
    query_sql_rows(captureId, wrappedSql) {
        return this.session(captureId).pool.runRawQuery(wrappedSql);
    }

    metadata(captureId) {
        return this.session(captureId).pool.viewer.metadata();
    }
    info(captureId) {
        return this.session(captureId).pool.viewer.info();
    }
    get_section(captureId, key) {
        return this.session(captureId).pool.viewer.get_section(key);
    }
    get_sections(captureId) {
        return this.session(captureId).pool.viewer.get_sections();
    }
    // init_templates / systeminfo route through every slot's
    // ViewerSql so each one's dashboard context stays in lockstep —
    // `pool.runQuery` round-robins across slots and they must all
    // agree on which sections exist.
    init_templates(captureId, templatesJson) {
        const slots = this.session(captureId).pool.slots;
        for (const s of slots) s.viewer.init_templates(templatesJson);
    }
    systeminfo(captureId) {
        return this.session(captureId).pool.viewer.systeminfo();
    }
    // Selection blob from parquet file metadata (drives the URL state
    // that compare-mode bridges from baseline → experiment). The legacy
    // viewer's WASM exposes this from the Rust side reading the parquet
    // KV; we already pull KV metadata into `metadata.file_metadata` at
    // load time, so this is a JS-side lookup.
    selection(captureId) {
        return this.session(captureId).metadata.file_metadata?.selection ?? null;
    }
    // File-level KV metadata for `/file_metadata` endpoint parity.
    // Returns a JSON-encoded object (each value embedded as JSON when
    // it parses, raw string otherwise). The legacy viewer's
    // `enrich_with_multi_node_info` is a Rust-side massage that adds
    // pre-computed `nodes`/`node_versions`/`service_instances` fields
    // for the frontend's convenience — keep the surface compatible by
    // including those when readily derivable.
    file_metadata_json(captureId) {
        const fm = this.session(captureId).metadata.file_metadata ?? {};
        const out = {};
        for (const [k, v] of Object.entries(fm)) {
            try { out[k] = JSON.parse(v); } catch { out[k] = v; }
        }
        // enrich_with_multi_node_info equivalent: derive node names from
        // per_source_metadata.rezolus when present.
        const psm = out.per_source_metadata;
        if (psm && typeof psm === 'object' && psm.rezolus && typeof psm.rezolus === 'object') {
            const nodes = [];
            const node_versions = {};
            for (const [subKey, entry] of Object.entries(psm.rezolus)) {
                if (entry && typeof entry === 'object') {
                    const node = entry.node ?? subKey;
                    nodes.push(node);
                    if (entry.version) node_versions[node] = entry.version;
                }
            }
            if (nodes.length > 0) out.nodes = nodes;
            if (Object.keys(node_versions).length > 0) out.node_versions = node_versions;
        }
        return JSON.stringify(out);
    }
    // Compare-mode combined section.
    //
    // Legacy `WasmCaptureRegistry::regenerate_combined` did two things:
    //
    //   (a) initialised both captures' dashboard contexts with the
    //       UNION of their detected service extensions (so a service
    //       only present on one capture still shows in the other's
    //       nav, with KPIs marked unavailable on the missing side);
    //   (b) when both captures matched a registered `category`, it
    //       built one combined "category" section that paired the two
    //       captures side-by-side under a single section in the nav.
    //
    // Stage 2d ships (a) only — call init_templates on each capture
    // independently so per-capture service sections appear in compare
    // mode. The category-combined section (b) requires a multi-
    // capture init API on viewer-sql (the dashboard crate's
    // `build_dashboard_context` already accepts a category arg, but
    // wiring it through wasm-bindgen + JS-side template-registry
    // round-tripping is its own piece of work). Documented as a
    // follow-up; users in compare mode see two per-capture service
    // sections instead of one combined section, which is the same
    // fallback shape the legacy registry produces when category
    // matching fails.
    regenerate_combined(templatesJson, _categoryName) {
        for (const captureId of this.captures.keys()) {
            this.init_templates(captureId, templatesJson);
        }
    }

    // ─── Additive surface: source picker + cgroup selection ────────

    sources(captureId) { return this.session(captureId).sources; }
    rezolusSources(captureId) { return this.session(captureId).rezolusSources; }
    pickedSource(captureId) { return this.session(captureId).pickedSource; }
    columnsBySource(captureId) { return this.session(captureId).columnsBySource; }

    async setSource(captureId, sourceName) {
        await this.session(captureId).setSource(sourceName);
    }
    setSelectedCgroups(captureId, names) {
        this.session(captureId).setSelectedCgroups(names);
    }
    selectedCgroups(captureId) {
        return [...this.session(captureId).selectedCgroups];
    }

    // ─── Cache + pre-flight (per-capture passthroughs) ─────────────

    cacheKey(captureId, sql) {
        return this.session(captureId).cacheKey(sql);
    }
    cachedResult(captureId, key) {
        return this.session(captureId).cachedResult(key);
    }
    setCached(captureId, key, val) {
        this.session(captureId).setCached(key, val);
    }
    preflightSkip(captureId, sql) {
        const cols = this.session(captureId).activeColumns();
        return cols ? sqlReferencesMissingColumn(sql, cols) : false;
    }
}
