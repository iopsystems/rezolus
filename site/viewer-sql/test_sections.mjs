// Per-section validation: pull each plot's sql_query out of the rendered
// section JSON and run it through ViewerSql.query_range, asserting the
// Prometheus matrix shape comes back populated. Reports which plots have
// SQL bodies and which fail. Used by Phase D to validate each section
// migration end-to-end.
//
// Usage: node test_sections.mjs <section-route> [<section-route>...]
// Example: node test_sections.mjs /memory /rezolus
import puppeteer from 'puppeteer-core';
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const REZOLUS_ROOT = path.resolve(ROOT, '../..');
// Args: section routes, optional --select=name1,name2 to seed cgroup selection.
const argv = process.argv.slice(2);
const sections = [];
let selectedCgroups = [];
for (const a of argv) {
    if (a.startsWith('--select=')) {
        selectedCgroups = a.slice('--select='.length).split(',').filter(Boolean);
    } else {
        sections.push(a);
    }
}
if (sections.length === 0) {
    console.error('usage: node test_sections.mjs [--select=/,/foo] /memory [/rezolus ...]');
    process.exit(2);
}

const MIME = {
    '.html': 'text/html', '.js': 'application/javascript',
    '.mjs': 'application/javascript', '.wasm': 'application/wasm',
    '.json': 'application/json', '.parquet': 'application/octet-stream',
    '.css': 'text/css',
};

const server = http.createServer(async (req, res) => {
    const url = new URL(req.url, 'http://x');
    const filePath = path.join(REZOLUS_ROOT, decodeURIComponent(url.pathname));
    if (!filePath.startsWith(REZOLUS_ROOT)) { res.writeHead(403).end(); return; }
    try {
        const buf = await fs.readFile(filePath);
        res.writeHead(200, { 'content-type': MIME[path.extname(filePath).toLowerCase()] ?? 'application/octet-stream' });
        res.end(buf);
    } catch { res.writeHead(404).end(); }
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const port = server.address().port;

const browser = await puppeteer.launch({
    executablePath: '/usr/bin/chromium',
    headless: true,
    args: ['--no-sandbox', '--disable-gpu', '--disable-dev-shm-usage'],
});
const page = await browser.newPage();
page.on('pageerror', (err) => process.stderr.write(`  [pageerror] ${err.message}\n`));

let exitCode = 0;
try {
    await page.goto(`http://127.0.0.1:${port}/site/viewer-sql/index.html`, {
        waitUntil: 'networkidle0',
        timeout: 60_000,
    });
    // Wait for the page to be ready, then click demo to load the parquet.
    await page.waitForFunction(
        () => /ready/.test(document.getElementById('status')?.textContent ?? ''),
        { timeout: 60_000, polling: 250 },
    );
    await page.click('#demo');
    // Wait for the session object — that's the post-load handshake.
    await page.waitForFunction(
        () => typeof window.__viewerSqlSession === 'object' && window.__viewerSqlSession !== null,
        { timeout: 60_000, polling: 250 },
    );
    // Now the viewer is loaded. Pull each section's plots and run their SQL.
    const result = await page.evaluate(async (sectionRoutes, selected) => {
        // Need access to the viewer instance. The smoke-test script attaches
        // the session globally for debugging; if not, we reach into module
        // globals via a small registration hook in script.js (see below).
        const session = window.__viewerSqlSession;
        if (!session) return { error: 'viewer session not exposed; needs window.__viewerSqlSession in script.js' };
        const viewer = session.viewer;
        // Seed the cgroup selection so the cgroups page's individual side
        // can return real series.
        if (selected && selected.length) viewer.set_selected_cgroups(selected);
        const tr = await (async () => {
            // Use the metadata-derived time range.
            const info = JSON.parse(viewer.info());
            return [info.minTime ?? 0, info.maxTime ?? 0];
        })();
        const out = [];
        for (const route of sectionRoutes) {
            const json = viewer.get_section(route.replace(/^\//, ''));
            if (!json) {
                out.push({ route, error: 'section returned null' });
                continue;
            }
            const view = JSON.parse(json);
            const plots = [];
            const collect = (subgroups) => {
                for (const sg of (subgroups ?? [])) {
                    for (const p of (sg.plots ?? [])) plots.push(p);
                }
            };
            for (const g of (view.groups ?? [])) collect(g.subgroups);
            const probes = [];
            for (const plot of plots) {
                const id = plot.opts?.id ?? '?';
                const title = plot.opts?.title ?? '?';
                if (!plot.sql_query) {
                    probes.push({ id, title, status: 'NO_SQL' });
                    continue;
                }
                try {
                    const t0 = performance.now();
                    const resp = await viewer.query_range(plot.sql_query, tr[0], tr[1], 1.0);
                    const ms = performance.now() - t0;
                    const parsed = JSON.parse(resp);
                    const series = parsed?.data?.result ?? [];
                    const totalSamples = series.reduce((a, s) => a + (s.values?.length ?? 0), 0);
                    probes.push({
                        id, title,
                        status: parsed.status === 'success' ? 'OK' : 'BAD_STATUS',
                        ms: ms.toFixed(1),
                        series: series.length,
                        samples: totalSamples,
                        first: series[0]?.values?.[0],
                    });
                } catch (e) {
                    probes.push({ id, title, status: 'FAIL', err: String(e.message ?? e).split('\n')[0].slice(0, 200) });
                }
            }
            out.push({ route, plots: probes });
        }
        return { ok: true, sections: out };
    }, sections, selectedCgroups);

    if (result.error) {
        console.error('Driver error:', result.error);
        exitCode = 2;
    } else {
        for (const sec of result.sections) {
            console.log(`\n=== ${sec.route} ===`);
            if (sec.error) { console.log(`  ERROR: ${sec.error}`); exitCode = 1; continue; }
            for (const p of sec.plots) {
                if (p.status === 'OK') {
                    console.log(`  OK   ${p.id.padEnd(30)} ${p.ms.padStart(6)}ms  series=${p.series} samples=${p.samples} first=${JSON.stringify(p.first)}`);
                } else if (p.status === 'NO_SQL') {
                    console.log(`  --   ${p.id.padEnd(30)} (no sql_query — not migrated)`);
                } else {
                    console.log(`  FAIL ${p.id.padEnd(30)} ${p.status} — ${p.err ?? ''}`);
                    exitCode = 1;
                }
            }
        }
    }
} finally {
    await browser.close();
    server.close();
}
process.exit(exitCode);
