// Boot the SQL-backed viewer: AsyncDuckDB → register parquet → register
// macros → build SqlMetadata via DESCRIBE → construct ViewerSql.
//
// This is a smoke-test bootstrap, NOT the production page. It exercises
// the full pipeline end-to-end so we can verify the wasm-bindgen surface +
// JS host design is sound before wiring viewer-sql into the existing
// Mithril UI in site/viewer/lib/.
//
// Loads duckdb-wasm from jsdelivr to avoid pulling npm into the rezolus
// repo. The eventual production page can switch to a vendored copy.

import * as duckdb from 'https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm@1.33.1-dev45.0/+esm';
import init, { ViewerSql, pure_sql_macros } from '../pkg/wasm_viewer_sql.js';

const $ = (id) => document.getElementById(id);
const setStatus = (text, cls = '') => {
    const el = $('status');
    el.className = cls;
    el.textContent = text;
    console.log('[status]', text);
};
const log = (text) => {
    const el = $('status');
    el.textContent = (el.textContent || '') + '\n' + text;
    console.log('[log]', text);
};

let session = null;       // { conn, viewer }

async function bootDuckDB() {
    setStatus('booting duckdb-wasm…');
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
    log(`  ✓ duckdb instantiated (${bundle.mainModule.split('/').pop()})`);
    return db;
}

async function registerMacros(conn) {
    log('registering pure-SQL macros…');
    const sql = pure_sql_macros();
    // Statements are separated by `;` at column 0; split on `;\n` lines so
    // bodies containing ; don't break us. Simpler: use executeBatch.
    if (typeof conn.send === 'function') {
        // AsyncDuckDB doesn't have executeBatch; run statements one by one.
    }
    const statements = sql
        .split(/;\s*$/m)
        .map((s) => s.trim())
        .filter((s) => s.length > 0);
    for (const stmt of statements) {
        await conn.query(stmt);
    }
    log(`  ✓ ${statements.length} macros registered`);
}

// Build the SqlMetadata object the ViewerSql constructor needs. Reads parquet
// schema + a couple of summary queries from the registered file.
async function buildMetadata(conn, filename, registeredName) {
    log('inspecting parquet schema…');
    // Column types tell us counter / gauge / histogram split. Rezolus
    // parquet metadata uses field-level metric_type, but DESCRIBE only
    // surfaces the data type — UInt64 = counter, Int64 = gauge,
    // List<UInt64> = histogram. (timestamp / duration are skipped.)
    const desc = await conn.query(
        `DESCRIBE SELECT * FROM read_parquet('${registeredName}')`
    );
    const counters = {};
    const gauges = {};
    const histograms = {};
    for (const row of desc.toArray().map((r) => r.toJSON())) {
        const name = row.column_name;
        if (name === 'timestamp' || name === 'duration') continue;
        // Strip label suffix `:buckets` for histogram columns; the
        // `:buckets` columns are the histogram payloads, the rest are
        // counters/gauges.
        const t = String(row.column_type);
        // Coarse classification — production version reads parquet field
        // metadata to get the real metric_type.
        if (t === 'UBIGINT[]' || t === 'BIGINT[]') {
            histograms[name] = (histograms[name] ?? 0) + 1;
        } else if (t === 'BIGINT' || t === 'INTEGER' || t === 'TINYINT') {
            gauges[name] = (gauges[name] ?? 0) + 1;
        } else if (t === 'UBIGINT' || t === 'UINTEGER' || t === 'UTINYINT') {
            counters[name] = (counters[name] ?? 0) + 1;
        }
    }

    // Time range + interval — best-effort from min/max(timestamp).
    const tr = await conn.query(
        `SELECT min(timestamp)::BIGINT AS lo, max(timestamp)::BIGINT AS hi
         FROM read_parquet('${registeredName}')`
    );
    const trRow = tr.toArray()[0]?.toJSON() ?? {};
    const lo = trRow.lo == null ? null : BigInt(trRow.lo);
    const hi = trRow.hi == null ? null : BigInt(trRow.hi);
    // Send timestamps as decimal-strings since JSON numbers in JS are
    // limited to 2^53; nanosecond timestamps exceed that. Rust deserializer
    // uses serde_json which accepts unquoted decimal numerics losslessly,
    // so we emit them as raw numbers via JSON.stringify of a BigInt-aware
    // replacer.
    const time_range_ns = lo != null && hi != null ? [lo, hi] : null;
    // Sampling interval: first two timestamps' delta in seconds. Falls
    // back to 1.0 (typical Rezolus 1Hz capture).
    let interval_seconds = 1.0;
    const ts2 = await conn.query(
        `SELECT timestamp::BIGINT AS t FROM read_parquet('${registeredName}') ORDER BY timestamp LIMIT 2`
    );
    const tsRows = ts2.toArray().map((r) => r.toJSON());
    if (tsRows.length === 2) {
        interval_seconds = Number(BigInt(tsRows[1].t) - BigInt(tsRows[0].t)) / 1e9;
    }

    return {
        interval_seconds,
        time_range_ns,
        source: 'rezolus',
        version: '',
        filename,
        counters,
        gauges,
        histograms,
    };
}

// JSON.stringify with BigInt support: emits BigInts as unquoted decimal
// numerics. Lossless on the wire; serde_json on the Rust side parses them
// into u64 without going through f64.
function stringifyWithBigInt(value) {
    if (value === null) return 'null';
    if (typeof value === 'bigint') return value.toString();
    if (typeof value === 'number') return JSON.stringify(value);
    if (typeof value === 'string') return JSON.stringify(value);
    if (typeof value === 'boolean') return JSON.stringify(value);
    if (Array.isArray(value)) {
        return '[' + value.map(stringifyWithBigInt).join(',') + ']';
    }
    if (typeof value === 'object') {
        const parts = [];
        for (const k of Object.keys(value)) {
            parts.push(JSON.stringify(k) + ':' + stringifyWithBigInt(value[k]));
        }
        return '{' + parts.join(',') + '}';
    }
    throw new Error('cannot stringify ' + typeof value);
}

async function loadParquet(buf, filename) {
    if (session) {
        await session.conn.close();
        session = null;
    }
    setStatus('loading parquet…');
    const db = await bootDuckDB();
    const REGISTERED = 'capture.parquet';
    // registerFileBuffer transfers the underlying ArrayBuffer ownership
    // to the duckdb worker (zero-copy), leaving the source detached.
    // Capture the size first so the log reflects the real byte count.
    const byteLength = buf.byteLength;
    await db.registerFileBuffer(REGISTERED, new Uint8Array(buf));
    log(`  ✓ registered ${filename} (${byteLength} bytes) as '${REGISTERED}'`);
    const conn = await db.connect();
    await registerMacros(conn);
    const metadata = await buildMetadata(conn, filename, REGISTERED);
    log(`  ✓ metadata: ${Object.keys(metadata.counters).length} counters, ${Object.keys(metadata.gauges).length} gauges, ${Object.keys(metadata.histograms).length} histograms`);

    // BigInts can't be JSON.stringify'd. Manual stringify keeps BigInt
    // values as unquoted decimal numerics in the JSON text — serde_json's
    // Value::Number parses arbitrary-precision integers losslessly, so the
    // u64 nanosecond timestamps survive end-to-end.
    const metadataJson = stringifyWithBigInt(metadata);
    const viewer = new ViewerSql(conn, metadataJson);
    session = { conn, viewer };

    setStatus('ready', 'ok');
    $('info').textContent = JSON.stringify(JSON.parse(viewer.info()), null, 2);
    const sectionJson = viewer.get_section('cpu');
    $('section').textContent = sectionJson
        ? sectionJson.slice(0, 1000) + (sectionJson.length > 1000 ? '\n…' : '')
        : '(no section)';
}

async function main() {
    setStatus('initializing wasm…');
    await init();
    setStatus('ready — drop a parquet file', 'ok');

    $('file').addEventListener('change', async (ev) => {
        const f = ev.target.files[0];
        if (!f) return;
        const buf = await f.arrayBuffer();
        try {
            await loadParquet(buf, f.name);
        } catch (e) {
            setStatus(`FAIL: ${e.message}\n${e.stack ?? ''}`, 'err');
        }
    });

    $('demo').addEventListener('click', async () => {
        try {
            const resp = await fetch('../viewer/data/demo.parquet');
            const buf = await resp.arrayBuffer();
            await loadParquet(buf, 'demo.parquet');
        } catch (e) {
            setStatus(`FAIL: ${e.message}\n${e.stack ?? ''}`, 'err');
        }
    });
}

main();
