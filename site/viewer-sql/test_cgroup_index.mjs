// Probe the _cgroup_index table built at parquet-load time.
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
    executablePath: '/usr/bin/chromium',
    headless: true,
    args: ['--no-sandbox', '--disable-gpu', '--disable-dev-shm-usage'],
});
const page = await browser.newPage();
page.on('pageerror', (err) => process.stderr.write(`[pageerror] ${err.message}\n`));
let exitCode = 0;
try {
    await page.goto(`http://127.0.0.1:${port}/site/viewer-sql/index.html`, {
        waitUntil: 'networkidle0', timeout: 60_000,
    });
    await page.waitForFunction(
        () => /ready/.test(document.getElementById('status')?.textContent ?? ''),
        { timeout: 60_000, polling: 250 },
    );
    await page.click('#demo');
    await page.waitForFunction(
        () => typeof window.__viewerSqlSession === 'object' && window.__viewerSqlSession !== null,
        { timeout: 60_000, polling: 250 },
    );
    const result = await page.evaluate(async () => {
        const conn = window.__viewerSqlSession.conn;
        const r = await conn.query(`
            SELECT metric, column_name, name, id, labels
            FROM _cgroup_index
            ORDER BY metric, column_name
        `);
        const out = [];
        for (const row of r.toArray().map((r) => r.toJSON())) {
            // labels is a Map-like; stringify entries
            const entries = [];
            if (row.labels && row.labels.entries) {
                for (const [k, v] of row.labels.entries()) entries.push(`${k}=${v}`);
            } else if (row.labels && Array.isArray(row.labels)) {
                // Sometimes serializes as array of [k,v] tuples
                for (const [k, v] of row.labels) entries.push(`${k}=${v}`);
            } else if (row.labels && typeof row.labels === 'object') {
                for (const [k, v] of Object.entries(row.labels)) entries.push(`${k}=${v}`);
            }
            out.push({ metric: row.metric, column_name: row.column_name, name: row.name, id: row.id, labels: entries.join(',') });
        }
        return out;
    });
    console.log(`_cgroup_index rows: ${result.length}`);
    for (const r of result) {
        console.log(`  metric=${r.metric.padEnd(36)} col=${r.column_name.padEnd(46)} name=${String(r.name).padEnd(8)} id=${String(r.id).padEnd(4)} labels={${r.labels}}`);
    }
} catch (e) {
    console.error('FAIL', e.message, e.stack);
    exitCode = 1;
} finally {
    await browser.close();
    server.close();
}
process.exit(exitCode);
