// Smoke test: load the multi-source sglang parquet, verify source views
// were created, picker UI appears, and at least some plots render against
// the picked source.
import puppeteer from 'puppeteer-core';
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const REZOLUS_ROOT = path.resolve(ROOT, '../..');
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
    executablePath: '/usr/bin/chromium', headless: true,
    args: ['--no-sandbox', '--disable-gpu', '--disable-dev-shm-usage'],
});
const page = await browser.newPage();
page.on('pageerror', (err) => process.stderr.write(`[pageerror] ${err.message}\n`));
page.on('console', (m) => { if (m.type() === 'error') process.stderr.write(`[console.${m.type()}] ${m.text()}\n`); });
let exitCode = 0;
try {
    await page.goto(`http://127.0.0.1:${port}/site/viewer-sql/preview.html`, { waitUntil: 'networkidle0', timeout: 60_000 });
    await page.waitForSelector('#demo-large:not([disabled])', { timeout: 30_000 });
    await page.click('#demo-large');
    // First wait for the session to come online (parquet loaded, views built).
    await page.waitForFunction(
        () => typeof window.__viewerSqlSession === 'object',
        { timeout: 120_000, polling: 1000 },
    );
    console.log('[harness] session online');
    // Then wait for charts to start appearing.
    await page.waitForFunction(
        () => document.querySelectorAll('#content .plot canvas').length > 2,
        { timeout: 60_000, polling: 1000 },
    ).catch(() => console.log('[harness] no charts in 60s'));
    await new Promise((r) => setTimeout(r, 3000));
    // Navigate to /network and scroll so the lazy renderer fires for every plot.
    await page.evaluate(() => {
        const btn = Array.from(document.querySelectorAll('nav button[data-section]'))
            .find((b) => b.dataset.section === 'network');
        btn?.click();
    });
    await new Promise((r) => setTimeout(r, 1000));
    await page.evaluate(async () => {
        const step = window.innerHeight * 0.8;
        for (let y = 0; y < document.body.scrollHeight; y += step) {
            window.scrollTo(0, y);
            await new Promise((r) => setTimeout(r, 200));
        }
        window.scrollTo(0, 0);
    });
    await new Promise((r) => setTimeout(r, 5000));
    await page.screenshot({ path: '/tmp/preview-sglang-network.png', fullPage: true });
    console.log('screenshot: /tmp/preview-sglang-network.png');
    // Probe the cgroup index now scoped to the picked source.
    const idxProbe = await page.evaluate(async () => {
        const conn = window.__viewerSqlSession.conn;
        const r = await conn.query(`SELECT COUNT(*) AS n, COUNT(DISTINCT metric) AS metrics FROM _cgroup_index`);
        const row = r.toArray()[0]?.toJSON() ?? {};
        const sample = await conn.query(`SELECT metric, column_name, name FROM _cgroup_index LIMIT 3`);
        return {
            n: Number(row.n),
            metrics: Number(row.metrics),
            sample: sample.toArray().map((r) => r.toJSON()),
        };
    });
    console.log('cgroup_index after source pick:', JSON.stringify(idxProbe, null, 2));
    const summary = await page.evaluate(() => {
        const session = window.__viewerSqlSession;
        const sources = session?.sources ?? [];
        const sourceBar = document.getElementById('source-bar');
        const sel = sourceBar?.querySelector('select');
        const plots = document.querySelectorAll('#content .plot');
        let withChart = 0, noSql = 0, fail = 0, noData = 0;
        for (const p of plots) {
            if (p.classList.contains('fail')) fail++;
            else if (p.classList.contains('no-sql')) noSql++;
            else if (p.querySelector('canvas')) withChart++;
            else noData++;
        }
        return {
            status: document.getElementById('status').textContent,
            sources, picked: sel?.value,
            barVisible: sourceBar?.style.display !== 'none',
            total: plots.length, withChart, noSql, fail, noData,
        };
    });
    console.log(JSON.stringify(summary, null, 2));
    await page.screenshot({ path: '/tmp/preview-sglang.png', fullPage: true });
    console.log('screenshot: /tmp/preview-sglang.png');
    if (summary.fail > 0 || summary.sources.length < 2 || summary.withChart < 1) {
        console.error('sglang multi-source did not behave as expected');
        exitCode = 1;
    }
} catch (e) {
    console.error('FAIL', e.message, e.stack);
    exitCode = 1;
} finally {
    await browser.close();
    server.close();
}
process.exit(exitCode);
