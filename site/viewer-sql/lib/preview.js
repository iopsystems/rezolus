// Standalone preview UI for the SQL-backed viewer. Renders every
// migrated section's plots as ECharts line charts / heatmaps, driven
// by the `CaptureRegistry` library at `./duckdb-registry.js`. Not a
// replacement for the real viewer at `site/viewer/` — this exists so a
// human can eyeball that the SQL pipeline works end-to-end against a
// real parquet, and so we have a place to iterate on perf experiments.
//
// The duckdb / viewer-sql plumbing all lives in `duckdb-registry.js`
// (CaptureRegistry mirrors the Mithril viewer's WasmCaptureRegistry
// surface). This file is just UI: DOM, ECharts, and the
// IntersectionObserver-driven render loop.

import {
    CaptureRegistry,
    partitionPlots,
    rowsToPerPlotMatrix,
    wrapWithSrcCte,
} from './duckdb-registry.js';

const $ = (id) => document.getElementById(id);
const setStatus = (text, cls = 'loading') => {
    const el = $('status');
    el.className = 'status ' + cls;
    el.textContent = text;
};

const ALL_SECTIONS = ['memory', 'rezolus', 'scheduler', 'gpu', 'network', 'blockio', 'softirq', 'syscall', 'cpu', 'overview', 'cgroups'];
const CAPTURE = 'baseline';

// One CaptureRegistry per page. Reused across parquet loads.
const wantWorkers = parseInt(new URLSearchParams(location.search).get('workers') ?? '', 10);
const N = Number.isFinite(wantWorkers) && wantWorkers >= 1 ? wantWorkers : 4;
const registry = new CaptureRegistry({ workersPerCapture: N });

// Bumped each time the user navigates to a different section. Pending
// async renders compare against this before writing to the DOM, so
// stale renders from a section the user has navigated away from get
// dropped silently instead of populating detached chart containers.
let renderGen = 0;

let loaded = false;     // true once a parquet is attached to the registry

async function loadParquet(buf, filename) {
    setStatus(`Loading ${filename}…`);
    if (loaded) {
        await registry.detach(CAPTURE);
        loaded = false;
    }
    const byteLength = buf.byteLength;
    await registry.attach(CAPTURE, buf, filename);
    loaded = true;
    // Expose for test harnesses (test_preview.mjs etc.).
    window.__viewerSqlSession = {
        registry,
        capture: CAPTURE,
        viewer: registry.session(CAPTURE).pool.viewer,
        conn: registry.session(CAPTURE).pool.conn,
        get pickedSource() { return registry.pickedSource(CAPTURE); },
        get sources() { return registry.sources(CAPTURE); },
        get rezolusSources() { return registry.rezolusSources(CAPTURE); },
        get columnsBySource() { return registry.columnsBySource(CAPTURE); },
    };

    const meta = registry.session(CAPTURE).metadata;
    const sources = registry.sources(CAPTURE);
    const pickedSource = registry.pickedSource(CAPTURE);
    const cgroupRowCount = Number(
        (await registry.session(CAPTURE).pool.conn.query(`SELECT COUNT(*) AS n FROM _cgroup_index`))
            .toArray()[0]?.toJSON()?.n ?? 0,
    );
    const cgroupRows = (await registry.session(CAPTURE).pool.conn.query(
        `SELECT name FROM _cgroup_index WHERE name IS NOT NULL`,
    )).toArray().map((r) => r.toJSON());

    setStatus(
        `Loaded ${filename} — ${(byteLength/1024/1024).toFixed(1)} MB, `
        + `${Object.keys(meta.counters).length}c/${Object.keys(meta.gauges).length}g/`
        + `${Object.keys(meta.histograms).length}h, cgroup index: ${cgroupRowCount} rows`
        + `${sources.length ? `, sources: ${sources.join(', ')} (showing ${pickedSource})` : ''}`
        + `, workers: ${N}`,
        'loading',
    );

    buildSourcePicker(sources, pickedSource);
    buildCgroupPicker(cgroupRows);
    buildSectionNav();
}

function buildSourcePicker(sources, pickedSource) {
    const sourceBar = $('source-bar');
    if (sources.length === 0) { sourceBar.style.display = 'none'; return; }
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
        await registry.setSource(CAPTURE, select.value);
        const active = document.querySelector('nav button.active');
        if (active) renderSection(active.dataset.section);
    };
    sourceBar.appendChild(select);
}

function buildCgroupPicker(cgroupRows) {
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
            const names = Array.from(sel.selectedOptions).map((o) => o.value);
            registry.setSelectedCgroups(CAPTURE, names);
            const active = document.querySelector('nav button.active');
            if (active && active.dataset.section === 'cgroups') renderSection('cgroups');
        };
    }
}

function buildSectionNav() {
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
    list.firstChild?.click();
}

// ─── Chart renderers (UI-side) ─────────────────────────────────────

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
            // No `scale: true` for non-percentage axes — when values are
            // constant min == max and the axis collapses to nothing.
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

// `logScale=true` switches the colour mapping to log10(value+1).
// Latency histograms span orders of magnitude (median ~20µs vs p9999
// ~100ms), and a linear scale collapses 99% of cells into the floor
// colour. We apply the log transform to the data itself rather than
// ECharts' visualMap log option since that misbehaves at v=0.
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
        series: [{ type: 'heatmap', data: xformed, progressive: 1000 }],
    });
    return chart;
}

// Decide which renderer to use for a Prometheus matrix result.
function renderResult(container, result, isPercent) {
    const allHaveQuantile = result.length > 0 && result.every((s) => s.metric?.quantile != null);
    const allHaveId = result.length > 0 && result.every((s) => s.metric?.id != null);
    if (allHaveQuantile && result.length >= 3) {
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

// ─── Render a section: build DOM, fire batched + per-plot queries ───

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

async function renderSection(sectionKey) {
    if (!loaded) return;
    // Bump generation so any in-flight renders from the previous
    // section see a stale `myGen` and bail out before mutating DOM.
    renderGen += 1;
    const myGen = renderGen;
    const session = registry.session(CAPTURE);
    const pool = session.pool;
    const json = pool.viewer.get_section(sectionKey);
    const content = $('content');
    if (!json) { content.innerHTML = `<p>section ${sectionKey} not found</p>`; return; }
    const view = JSON.parse(json);
    const tr = session.metadata.time_range_ns;
    const startSec = tr ? Number(BigInt(tr[0])) / 1e9 : 0;
    const endSec = tr ? Number(BigInt(tr[1])) / 1e9 : 0;
    content.innerHTML = `<h2>${sectionKey}</h2>`;
    const groups = view.groups ?? [];
    if (groups.length === 0) {
        content.innerHTML += '<p style="color:#888">(empty section)</p>';
        return;
    }

    // Pass 1: build all the DOM up front so each .chart container has
    // been laid out by the browser before we init ECharts against it.
    const renderQueue = [];
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

    // Pass 2: lazy render via IntersectionObserver. Charts render in
    // viewport-distance order; the cache keeps revisits O(1).
    await new Promise((r) => requestAnimationFrame(r));
    await new Promise((r) => requestAnimationFrame(r));

    const sectionPlots = renderQueue.map((q) => q.p);
    const { batches } = partitionPlots(sectionPlots);
    const plotToBatch = new Map();
    for (const b of batches) {
        b.promise = null;
        for (let i = 0; i < b.plots.length; i++) {
            plotToBatch.set(b.plots[i], { batch: b, idx: i });
        }
    }

    const fireBatch = (b) => {
        if (b.promise) return b.promise;
        if (registry.preflightSkip(CAPTURE, b.sqlBody)) {
            b.promise = Promise.resolve([]);
        } else {
            const wrapped = wrapWithSrcCte(b.sqlBody, startSec, endSec, registry.pickedSource(CAPTURE));
            b.promise = registry.query_sql_rows(CAPTURE, wrapped).then((s) => JSON.parse(s));
        }
        return b.promise;
    };

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
            const key = registry.cacheKey(CAPTURE, p.sql_query);
            let result = registry.cachedResult(CAPTURE, key);
            if (result === undefined) {
                const batchEntry = plotToBatch.get(p);
                if (batchEntry) {
                    const rows = await fireBatch(batchEntry.batch);
                    result = batchEntry.batch.demuxFn(rows, batchEntry.idx);
                } else {
                    if (registry.preflightSkip(CAPTURE, p.sql_query)) {
                        result = [];
                    } else {
                        const resp = await registry.query_range(CAPTURE, p.sql_query, startSec, endSec, 1.0);
                        const parsed = JSON.parse(resp);
                        result = parsed?.data?.result ?? [];
                    }
                }
                registry.setCached(CAPTURE, key, result);
            }
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
        if (myGen !== renderGen) { observer.disconnect(); return; }
        // Each plot's query takes ~50–1100 ms and they serialise through
        // one worker (we have N parallel, but a single user-paced scroll
        // typically saturates only one). Sort by distance to the
        // viewport's vertical centre so the most-visible plots fire
        // first — the user sees on-screen content populate before
        // below-the-fold.
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
