// Minimal preview UI for the SQL-backed viewer. Renders every migrated
// section's plots as ECharts line charts, driven directly by viewer-sql's
// `query_range`. Not a replacement for the real viewer — that lives in
// `site/viewer/lib/` and is much more capable (selection, compare, heatmaps,
// histograms, etc.). This page exists so a human can eyeball that the SQL
// pipeline works end-to-end against a real parquet.

import * as duckdb from 'https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm@1.33.1-dev45.0/+esm';
import * as arrow from 'https://cdn.jsdelivr.net/npm/apache-arrow@17.0.0/+esm';
import init, { ViewerSql, pure_sql_macros } from '../pkg/wasm_viewer_sql.js';

const $ = (id) => document.getElementById(id);
const setStatus = (text, cls = 'loading') => {
    const el = $('status');
    el.className = 'status ' + cls;
    el.textContent = text;
};

let session = null;
let selectedCgroups = [];
// Per-session result cache: key = `${source}|${cgroupsKeyOrEmpty}|${sql}`,
// value = the parsed Prometheus matrix `data.result` array. Time window
// isn't keyed because we always use the full file range; revisit when
// zoom/compare arrives. Cgroup selection only enters the key when the
// SQL references the `__SELECTED_CGROUPS__` placeholder, so non-cgroup
// plots correctly hit across selection changes.
let resultCache = new Map();
// Bumped each time the user navigates to a different section. Pending
// async renders compare against this before writing to the DOM, so
// stale renders from a section the user has navigated away from get
// dropped silently instead of populating detached chart containers.
let renderGen = 0;

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
    return {
        interval_seconds, time_range_ns,
        source: 'rezolus', version: '', filename,
        parquet_name: registeredName, counters, gauges, histograms,
    };
}

// For multi-source combined parquets (one file produced by `parquet combine`
// from N sources), all metric columns are prefixed `<source>::`. Build a
// VIEW per source that aliases its columns back to their unprefixed names —
// dashboard SQL can then run verbatim against any chosen source's view.
//
// Returns the list of detected source prefixes (without the `::`). Empty
// when the file is single-source (no prefixes).
async function buildSourceViews(conn, registeredName) {
    const desc = await conn.query(`DESCRIBE SELECT * FROM read_parquet('${registeredName}')`);
    const cols = desc.toArray().map((r) => r.toJSON().column_name);
    const bySource = new Map();
    for (const c of cols) {
        const m = c.match(/^([^:]+)::(.+)$/);
        if (!m) continue;
        const [, prefix, rest] = m;
        if (!bySource.has(prefix)) bySource.set(prefix, []);
        bySource.get(prefix).push({ orig: c, alias: rest });
    }
    // columnsBySource: source name → Set of unprefixed column names
    // available in that source's view. Used by `sqlReferencesMissingColumn`
    // to short-circuit dashboard queries whose `COLUMNS('regex')` would
    // resolve empty — DuckDB throws a Binder Error in that case which the
    // worker logs to console regardless of how the caller handles it.
    const columnsBySource = new Map();
    if (bySource.size === 0) {
        // Single-source file: every parquet column is available unprefixed.
        columnsBySource.set('', new Set(cols));
        return { sources: [], rezolusSources: [], columnsBySource };
    }
    const sources = [...bySource.keys()].sort();
    const rezolusSources = sources.filter(
        (s) => bySource.get(s).some((a) => a.alias === 'memory_total'),
    );
    for (const src of sources) {
        const aliases = bySource.get(src);
        const q = (s) => '"' + s.replace(/"/g, '""') + '"';
        const projections = ['timestamp', 'duration']
            .filter((c) => cols.includes(c))
            .map((c) => q(c))
            .concat(aliases.map((a) => `${q(a.orig)} AS ${q(a.alias)}`))
            .join(', ');
        const viewName = `_src_${src.replace(/[^a-zA-Z0-9_]/g, '_')}`;
        await conn.query(`CREATE VIEW ${viewName} AS SELECT ${projections} FROM read_parquet('${registeredName}')`);
        columnsBySource.set(src, new Set([
            ...['timestamp', 'duration'].filter((c) => cols.includes(c)),
            ...aliases.map((a) => a.alias),
        ]));
    }
    return { sources, rezolusSources, columnsBySource };
}

// Pre-flight check: does any `COLUMNS('regex')` in the SQL match no
// column in the current source's view? If so, submitting it would throw
// a Binder Error in the worker and pollute the console even though our
// Rust-side catch turns it into an empty matrix. We replicate the
// regex check here so the query never goes to the worker.
//
// Limited to `COLUMNS('regex')` patterns since those are the dashboard's
// runtime schema-resolution mechanism. Bare column refs like
// `"memory_total"` are stable per parquet and either work for the whole
// session or fail fast on first use.
function sqlReferencesMissingColumn(sql, columnSet) {
    const re = /COLUMNS\('([^']+)'\)/g;
    let m;
    while ((m = re.exec(sql)) !== null) {
        let pattern;
        try { pattern = new RegExp(m[1]); }
        catch { continue; } // unparsable — let the worker handle it
        let any = false;
        for (const c of columnSet) {
            if (pattern.test(c)) { any = true; break; }
        }
        if (!any) return true;
    }
    return false;
}

function viewNameForSource(src) {
    return `_src_${src.replace(/[^a-zA-Z0-9_]/g, '_')}`;
}

// Build (or rebuild) `_cgroup_index` for the currently active source.
//
// `sourcePrefix` is the source name (without the `::` separator) for
// multi-source combined parquets, or `null` for single-source files.
// When set, the index only contains cgroup columns from that source,
// and `column_name` stores the *unprefixed* name so it matches the
// columns in the source's aliased `_src_<src>` view (which dashboard
// SQL JOINs against in the cgroup helpers).
async function buildCgroupIndex(conn, registeredName, sourcePrefix = null) {
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
    const prefixWithSep = sourcePrefix ? `${sourcePrefix}::` : null;
    const cgroupRows = [];
    for (const f of reader.schema.fields) {
        const md = f.metadata;
        if (!md || !md.get) continue;
        const metric = md.get('metric');
        if (!metric || !metric.startsWith('cgroup_')) continue;
        // Strip the source prefix to match the alias used in `_src_<src>`.
        // Skip columns from other sources entirely.
        let columnName = f.name;
        if (prefixWithSep) {
            if (!columnName.startsWith(prefixWithSep)) continue;
            columnName = columnName.slice(prefixWithSep.length);
        }
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

const ALL_SECTIONS = ['memory', 'rezolus', 'scheduler', 'gpu', 'network', 'blockio', 'softirq', 'syscall', 'cpu', 'overview', 'cgroups'];

async function loadParquet(buf, filename) {
    setStatus(`Loading ${filename}…`);
    if (session) {
        try { await session.conn.close(); } catch {}
        session = null;
    }
    // New parquet → fresh cache. The Map's lifetime is tied to a session;
    // there's no in-session invalidation because the source data never
    // mutates while the file is loaded.
    resultCache = new Map();
    const db = await bootDuckDB();
    const REGISTERED = 'capture.parquet';
    const byteLength = buf.byteLength;
    await db.registerFileBuffer(REGISTERED, new Uint8Array(buf));
    const conn = await db.connect();
    await registerMacros(conn);
    const metadata = await buildMetadata(conn, filename, REGISTERED);
    const { sources, rezolusSources, columnsBySource } = await buildSourceViews(conn, REGISTERED);

    // Default source pick order:
    //   1. A rezolus-shaped source whose name appears in the filename.
    //   2. The first rezolus-shaped source.
    //   3. The first source.
    let pickedSource = null;
    if (sources.length > 0) {
        const fnLower = filename.toLowerCase();
        pickedSource = rezolusSources.find((s) => fnLower.includes(s.toLowerCase()))
            ?? rezolusSources[0]
            ?? sources[0];
    }
    // Build the cgroup index *after* we know the source — its rows store
    // unprefixed column names that match the source's aliased view, so it
    // has to be source-scoped from the start.
    const cgroupRows = await buildCgroupIndex(conn, REGISTERED, pickedSource);
    const viewer = new ViewerSql(conn, stringifyWithBigInt(metadata));
    session = { conn, viewer, metadata, sources, rezolusSources, pickedSource, columnsBySource };
    window.__viewerSqlSession = session;
    if (pickedSource) viewer.set_source_relation(viewNameForSource(pickedSource));

    setStatus(`Loaded ${filename} — ${(byteLength/1024/1024).toFixed(1)} MB, ${Object.keys(metadata.counters).length}c/${Object.keys(metadata.gauges).length}g/${Object.keys(metadata.histograms).length}h, cgroup index: ${cgroupRows.length} rows${sources.length ? `, sources: ${sources.join(', ')} (showing ${pickedSource})` : ''}`, 'loading');

    // Build source picker UI (only when multiple sources are present).
    const sourceBar = $('source-bar');
    if (sources.length > 0) {
        sourceBar.style.display = '';
        sourceBar.innerHTML = '';
        const label = document.createElement('label');
        label.textContent = 'Source:';
        sourceBar.appendChild(label);
        const select = document.createElement('select');
        for (const s of sources) {
            const opt = document.createElement('option');
            opt.value = s; opt.textContent = s;
            if (s === pickedSource) opt.selected = true;
            select.appendChild(opt);
        }
        select.onchange = async () => {
            session.pickedSource = select.value;
            viewer.set_source_relation(viewNameForSource(select.value));
            // Cgroup index is source-scoped — rebuild when source changes
            // so cgroup-page SQL JOINs resolve against the new source's view.
            await buildCgroupIndex(conn, REGISTERED, select.value);
            const active = document.querySelector('nav button.active');
            if (active) renderSection(active.dataset.section);
        };
        sourceBar.appendChild(select);
    } else {
        sourceBar.style.display = 'none';
    }

    // Populate cgroup selection UI from the index.
    const cgroupNames = [...new Set(cgroupRows.map((r) => r.name).filter(Boolean))].sort();
    const sel = $('cgroup-select');
    sel.innerHTML = '';
    for (const n of cgroupNames) {
        const opt = document.createElement('option');
        opt.value = n; opt.textContent = n;
        sel.appendChild(opt);
    }
    if (cgroupNames.length > 0) {
        $('cgroup-bar').style.display = '';
        sel.onchange = () => {
            selectedCgroups = Array.from(sel.selectedOptions).map((o) => o.value);
            viewer.set_selected_cgroups(selectedCgroups);
            // Refresh current section if it's cgroups.
            const active = document.querySelector('nav button.active');
            if (active && active.dataset.section === 'cgroups') renderSection('cgroups');
        };
    }

    // Build the section nav.
    const list = $('section-list');
    list.innerHTML = '';
    for (const s of ALL_SECTIONS) {
        const b = document.createElement('button');
        b.textContent = s;
        b.dataset.section = s;
        b.onclick = () => {
            for (const x of list.querySelectorAll('button')) x.classList.remove('active');
            b.classList.add('active');
            renderSection(s);
        };
        list.appendChild(b);
    }
    // Auto-render the first section.
    list.firstChild?.click();
}

function makeLineChart(container, series, isPercent) {
    const chart = echarts.init(container, 'dark', { renderer: 'canvas' });
    if (window.ResizeObserver) new ResizeObserver(() => chart.resize()).observe(container);
    chart.setOption({
        backgroundColor: 'transparent',
        animation: false,
        grid: { left: 40, right: 8, top: 8, bottom: 20 },
        xAxis: {
            type: 'time',
            axisLabel: { color: '#888', fontSize: 9 },
            axisLine: { lineStyle: { color: '#333' } },
            splitLine: { show: false },
        },
        yAxis: {
            type: 'value',
            // Don't use `scale: true` for non-percentage axes — when the
            // values are constant (e.g. memory total), min == max and the
            // axis collapses to nothing, so no line is drawn.
            min: isPercent ? 0 : null,
            max: isPercent ? 1 : null,
            axisLabel: { color: '#888', fontSize: 9 },
            axisLine: { lineStyle: { color: '#333' } },
            splitLine: { lineStyle: { color: '#1a1f2c' } },
        },
        tooltip: { trigger: 'axis' },
        series: series.map((s) => ({
            name: s.name,
            type: 'line',
            showSymbol: false,
            lineStyle: { width: 1 },
            data: s.data,
        })),
    });
    return chart;
}

// Heatmap: per-(time, lane) intensity. `lanes` is the categorical Y axis
// (sorted CPU ids, sorted percentile labels, etc.). `data` is a flat array
// of [timeIdx, laneIdx, value] triples.
//
// `logScale=true` switches the colour mapping to log10(value+1). Latency
// histograms span orders of magnitude (median ~20µs vs p9999 ~100ms), and
// a linear scale collapses 99% of cells into the floor colour. We apply
// the log transform to the data itself rather than ECharts' visualMap log
// option since ECharts' built-in log mode misbehaves at the v=0 boundary.
function makeHeatmap(container, timestamps, lanes, data, logScale = false) {
    const chart = echarts.init(container, 'dark', { renderer: 'canvas' });
    if (window.ResizeObserver) new ResizeObserver(() => chart.resize()).observe(container);
    const xform = logScale ? ((v) => Math.log10(Math.max(0, v) + 1)) : ((v) => v);
    const xformed = data.map(([t, l, v]) => [t, l, xform(v)]);
    let vMin = Infinity, vMax = -Infinity;
    for (const [, , v] of xformed) {
        if (v == null || Number.isNaN(v)) continue;
        if (v < vMin) vMin = v;
        if (v > vMax) vMax = v;
    }
    if (!isFinite(vMin)) { vMin = 0; vMax = 1; }
    if (vMin === vMax) vMax = vMin + 1;
    data = xformed;
    chart.setOption({
        backgroundColor: 'transparent',
        animation: false,
        grid: { left: 50, right: 50, top: 8, bottom: 30 },
        xAxis: {
            type: 'category',
            data: timestamps.map((ts) => new Date(ts * 1000).toLocaleTimeString('en-US', { hour12: false })),
            axisLabel: { color: '#888', fontSize: 9, interval: Math.max(1, Math.floor(timestamps.length / 6)) },
            axisLine: { lineStyle: { color: '#333' } },
            splitLine: { show: false },
        },
        yAxis: {
            type: 'category',
            data: lanes,
            axisLabel: { color: '#888', fontSize: 9 },
            axisLine: { lineStyle: { color: '#333' } },
            splitLine: { show: false },
        },
        visualMap: {
            min: vMin, max: vMax,
            calculable: false, show: true, orient: 'vertical',
            right: 4, top: 'middle',
            itemWidth: 8, itemHeight: 60,
            textStyle: { color: '#888', fontSize: 9 },
            inRange: { color: ['#0a0e14', '#1f3a52', '#3066be', '#5fb6ff', '#bce0ff', '#ffffff'] },
        },
        tooltip: { trigger: 'item', formatter: (p) => `${lanes[p.value[1]]} @ ${timestamps[p.value[0]] ? new Date(timestamps[p.value[0]] * 1000).toLocaleTimeString('en-US') : ''}<br>value: ${p.value[2]}` },
        series: [{ type: 'heatmap', data, progressive: 1000 }],
    });
    return chart;
}

// Decide which renderer to use for a result.
//
// - Histograms emit one series per quantile with `metric.quantile` set
//   → render as 5-row heatmap (quantile on Y, intensity = value).
// - Per-id plots (id label, multiple series) → heatmap with id on Y.
// - Otherwise → line chart with one line per series.
function renderResult(container, result, isPercent) {
    const allHaveQuantile = result.length > 0 && result.every((s) => s.metric?.quantile != null);
    const allHaveId = result.length > 0 && result.every((s) => s.metric?.id != null);
    if (allHaveQuantile && result.length >= 3) {
        // Quantile heatmaps almost always span orders of magnitude
        // (latency / IO size distributions); log scale prevents the
        // colour gradient from collapsing into the floor.
        return renderHeatmap(container, result, 'quantile', (a, b) => parseFloat(b) - parseFloat(a), true);
    }
    if (allHaveId && result.length >= 4) {
        return renderHeatmap(container, result, 'id', (a, b) => parseInt(a, 10) - parseInt(b, 10), false);
    }
    const series = result.map((s, i) => {
        const labelStr = Object.entries(s.metric ?? {}).map(([k, v]) => `${k}=${v}`).join(' ');
        return {
            name: labelStr || `series ${i}`,
            data: (s.values ?? []).map(([t, v]) => [t * 1000, parseFloat(v)]).filter(([_, v]) => !Number.isNaN(v)),
        };
    });
    return makeLineChart(container, series, isPercent);
}

// Convert a Prometheus matrix result into heatmap triples + axes.
// `laneKey`: which metric label to use as the Y axis (e.g. 'id', 'quantile').
// `laneSort`: sort comparator for unique lane values.
function renderHeatmap(container, result, laneKey, laneSort, logScale = false) {
    const tSet = new Set();
    for (const s of result) for (const [t] of (s.values ?? [])) tSet.add(t);
    const timestamps = Array.from(tSet).sort((a, b) => a - b);
    const tIdx = new Map(timestamps.map((t, i) => [t, i]));
    const lanes = Array.from(new Set(result.map((s) => String(s.metric?.[laneKey] ?? '')))).sort(laneSort);
    const lIdx = new Map(lanes.map((l, i) => [l, i]));
    const data = [];
    for (const s of result) {
        const lane = String(s.metric?.[laneKey] ?? '');
        const li = lIdx.get(lane);
        if (li == null) continue;
        for (const [t, v] of (s.values ?? [])) {
            const ti = tIdx.get(t);
            const num = parseFloat(v);
            if (ti == null || Number.isNaN(num)) continue;
            data.push([ti, li, num]);
        }
    }
    return makeHeatmap(container, timestamps, lanes, data, logScale);
}

// Build the cache key for a given plot's SQL. Time window isn't included
// (assumed constant per session). Cgroup selection only enters the key for
// SQL strings that reference `__SELECTED_CGROUPS__` — most plots don't,
// and over-keying would needlessly miss when the user changes selection.
function cacheKeyFor(sql) {
    const sourceKey = session?.pickedSource ?? '';
    const cg = sql.includes('__SELECTED_CGROUPS__') ? selectedCgroups.join(',') : '';
    return `${sourceKey}|${cg}|${sql}`;
}

async function renderSection(sectionKey) {
    if (!session) return;
    const { viewer, metadata } = session;
    // Bump generation so any in-flight renders from the previous section
    // see a stale `myGen` and bail out before mutating DOM.
    renderGen += 1;
    const myGen = renderGen;
    const json = viewer.get_section(sectionKey);
    const content = $('content');
    if (!json) { content.innerHTML = `<p>section ${sectionKey} not found</p>`; return; }
    const view = JSON.parse(json);
    const tr = metadata.time_range_ns;
    const startSec = tr ? Number(BigInt(tr[0])) / 1e9 : 0;
    const endSec = tr ? Number(BigInt(tr[1])) / 1e9 : 0;
    content.innerHTML = `<h2>${sectionKey}</h2>`;
    const groups = view.groups ?? [];
    if (groups.length === 0) {
        content.innerHTML += '<p style="color:#888">(empty section)</p>';
        return;
    }
    // Pass 1: build all the DOM up front. This guarantees every .chart
    // container has been laid out by the browser before we init ECharts
    // against it (otherwise the first few charts measure 0×0 because Promise
    // microtasks resolve before paint).
    const renderQueue = [];
    const el = (tag, props = {}, children = []) => {
        const e = document.createElement(tag);
        for (const [k, v] of Object.entries(props)) {
            if (k === 'class') e.className = v;
            else if (k === 'text') e.textContent = v;
            else if (k === 'style') e.setAttribute('style', v);
            else e.setAttribute(k, v);
        }
        for (const c of children) if (c) e.appendChild(c);
        return e;
    };
    for (const g of groups) {
        const gEl = el('div', { class: 'group' }, [
            el('h3', { text: g.name ?? '' }),
        ]);
        for (const sg of (g.subgroups ?? [])) {
            if (sg.name) gEl.appendChild(el('h3', { style: 'font-size:1em;color:#aaa', text: sg.name }));
            if (sg.description) gEl.appendChild(el('p', { class: 'desc', text: sg.description }));
            const plotsEl = el('div', { class: 'plots' });
            gEl.appendChild(plotsEl);
            for (const p of (sg.plots ?? [])) {
                const id = p.opts?.id ?? '?';
                const title = p.opts?.title ?? id;
                const unit = p.opts?.unit ?? '';
                const isPercent = String(unit).toLowerCase().includes('percent') || p.opts?.percentage_range === true;
                const div = el('div', { class: 'plot' }, [
                    el('h4', { text: title }),
                    el('div', { class: 'meta', text: `${id} · ${unit}` }),
                    el('div', { class: 'chart' }),
                ]);
                plotsEl.appendChild(div);
                renderQueue.push({ p, div, title, isPercent });
            }
        }
        content.appendChild(gEl);
    }
    // Pass 2: lazy render via IntersectionObserver. Each plot's query
    // fires only when the plot is within ~one screen of the viewport
    // (rootMargin 300px). The duckdb worker serialises queries; the
    // result cache (keyed by source + cgroup-selection-when-relevant +
    // sql) makes revisiting a section after a navigation away O(1) per
    // chart instead of re-running every query.
    //
    // Renders compare against `myGen` before mutating the DOM — if the
    // user has navigated to a different section since this render was
    // queued, the chartEl is detached and writing to it is wasted work.
    await new Promise((r) => requestAnimationFrame(r));
    await new Promise((r) => requestAnimationFrame(r));

    const renderOne = async (entry) => {
        const { p, div, isPercent } = entry;
        if (div.dataset.rendered) return;
        div.dataset.rendered = '1';
        const tStart = performance.now();
        const chartEl = div.querySelector('.chart');
        if (!p.sql_query) {
            div.classList.add('no-sql');
            chartEl.textContent = '(no sql_query — plot uses PromQL only)';
            return;
        }
        try {
            const key = cacheKeyFor(p.sql_query);
            let result = resultCache.get(key);
            if (result === undefined) {
                // Short-circuit when the SQL's COLUMNS() regexes don't
                // resolve to anything in the active source — DuckDB throws
                // a Binder Error in that case which the worker logs to
                // the console regardless of how Rust handles the result.
                const cols = session.columnsBySource?.get(session.pickedSource ?? '');
                if (cols && sqlReferencesMissingColumn(p.sql_query, cols)) {
                    result = [];
                } else {
                    const resp = await viewer.query_range(p.sql_query, startSec, endSec, 1.0);
                    const parsed = JSON.parse(resp);
                    result = parsed?.data?.result ?? [];
                }
                resultCache.set(key, result);
            }
            // Drop the render if the user has moved to another section
            // while we were awaiting the worker — DOM is detached.
            if (myGen !== renderGen) return;
            if (result.length === 0) {
                chartEl.textContent = '(no data)';
                chartEl.style.color = '#666';
                chartEl.style.fontSize = '0.8em';
                chartEl.style.display = 'flex';
                chartEl.style.alignItems = 'center';
                chartEl.style.justifyContent = 'center';
                console.log(`[plot] ${p.opts?.id} no-data ${(performance.now()-tStart).toFixed(1)}ms`);
                return;
            }
            renderResult(chartEl, result, isPercent);
            console.log(`[plot] ${p.opts?.id} ${(performance.now()-tStart).toFixed(1)}ms`);
        } catch (e) {
            if (myGen !== renderGen) return;
            div.classList.add('fail');
            chartEl.textContent = String(e.message ?? e).split('\n').slice(0, 3).join('\n');
        }
    };

    const queueByDiv = new Map(renderQueue.map((q) => [q.div, q]));
    const observer = new IntersectionObserver((entries) => {
        // If the section has changed, stop observing and don't fire renders.
        if (myGen !== renderGen) { observer.disconnect(); return; }
        // Each plot's query takes ~230 ms (gauge) to ~1.1 s (histogram)
        // and they serialise through one worker. When multiple plots
        // intersect at once we sort by distance from the viewport's
        // vertical centre so the most-visible plots fire first — the
        // user sees on-screen content populate before below-the-fold.
        const visible = entries.filter((e) => e.isIntersecting);
        const cy = window.innerHeight / 2;
        visible.sort((a, b) => {
            const ay = a.boundingClientRect.top + a.boundingClientRect.height / 2;
            const by = b.boundingClientRect.top + b.boundingClientRect.height / 2;
            return Math.abs(ay - cy) - Math.abs(by - cy);
        });
        for (const entry of visible) {
            const q = queueByDiv.get(entry.target);
            if (q) renderOne(q);
        }
    }, { rootMargin: '50px' });
    for (const { div } of renderQueue) observer.observe(div);
}

async function main() {
    setStatus('Initializing wasm…');
    await init();
    setStatus('Ready — drop a parquet file or click "demo".', 'loading');

    $('file').addEventListener('change', async (ev) => {
        const f = ev.target.files[0];
        if (!f) return;
        const buf = await f.arrayBuffer();
        try { await loadParquet(buf, f.name); }
        catch (e) { setStatus(`FAIL: ${e.message}\n${e.stack ?? ''}`, 'error'); }
    });
    const fetchAndLoad = async (path, name) => {
        try {
            const resp = await fetch(path);
            const buf = await resp.arrayBuffer();
            await loadParquet(buf, name);
        } catch (e) { setStatus(`FAIL: ${e.message}\n${e.stack ?? ''}`, 'error'); }
    };
    $('demo').addEventListener('click', () => fetchAndLoad('../viewer/data/demo.parquet', 'demo.parquet'));
    $('demo-large').addEventListener('click', () => fetchAndLoad('../viewer/data/disagg/sglang-nixl-16c.parquet', 'sglang-nixl-16c.parquet'));
}

main();
