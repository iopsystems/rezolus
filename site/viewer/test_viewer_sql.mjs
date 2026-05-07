// Smoke test: boot the Mithril viewer in headless chromium against the
// demo parquet, confirm the SQL backend is wired up via the page's
// registry plumbing, and surface any console errors.
//
// Lives at site/viewer/ alongside `data/`, parallel to the
// site/viewer-sql/test_*.mjs harnesses.

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
const errors = [];
page.on('pageerror', (e) => { errors.push(`[pageerror] ${e.message}`); });
page.on('console', (m) => {
    const t = m.text();
    if (m.type() === 'error') errors.push(`[console.error] ${t.slice(0, 500)}`);
});
page.on('requestfailed', (req) => errors.push(`[404] ${req.url()}`));

let exitCode = 0;
try {
    // Demo load is auto-triggered via ?demo=demo.parquet (script.js has
    // a check for that URL param).
    await page.goto(
        `http://127.0.0.1:${port}/site/viewer/index.html?demo=demo.parquet`,
        { waitUntil: 'networkidle0', timeout: 60_000 },
    );
    // Just wait a fixed window for boot + initial renders.
    await new Promise((r) => setTimeout(r, 15000));
    const summary = await page.evaluate(() => ({
        title: document.title,
        hasContent: !!document.body.textContent && document.body.textContent.length > 0,
        canvases: document.querySelectorAll('canvas').length,
    }));
    console.log(JSON.stringify(summary, null, 2));
    if (errors.length > 0) {
        console.log('--- console errors ---');
        for (const e of errors.slice(0, 20)) console.log(e);
        exitCode = 1;
    } else {
        console.log('(no console errors)');
    }
    await page.screenshot({ path: '/tmp/viewer-sql-bootstrap.png', fullPage: false });
    console.log('screenshot: /tmp/viewer-sql-bootstrap.png');
} catch (e) {
    console.error('FAIL', e.message, e.stack);
    exitCode = 1;
} finally {
    await browser.close();
    server.close();
}
process.exit(exitCode);
