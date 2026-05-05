// Headless smoke test for the viewer-sql page. Boots a static server, runs
// chromium, clicks the demo button, and verifies the dashboard section
// renders without error. Exit code 0 on green.
import puppeteer from 'puppeteer-core';
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const REZOLUS_ROOT = path.resolve(ROOT, '../..');

// Tiny static server: maps URL paths to files under REZOLUS_ROOT.
const MIME = {
    '.html': 'text/html', '.js': 'application/javascript',
    '.mjs': 'application/javascript', '.wasm': 'application/wasm',
    '.json': 'application/json', '.parquet': 'application/octet-stream',
    '.css': 'text/css', '.svg': 'image/svg+xml',
};

const server = http.createServer(async (req, res) => {
    const url = new URL(req.url, 'http://x');
    const filePath = path.join(REZOLUS_ROOT, decodeURIComponent(url.pathname));
    if (!filePath.startsWith(REZOLUS_ROOT)) { res.writeHead(403).end(); return; }
    try {
        const buf = await fs.readFile(filePath);
        const ext = path.extname(filePath).toLowerCase();
        // No COOP/COEP — duckdb-wasm AsyncDuckDB worker uses postMessage,
        // doesn't require SharedArrayBuffer. Cross-origin isolation would
        // also block our jsdelivr CDN import unless the CDN sets CORP.
        res.writeHead(200, {
            'content-type': MIME[ext] ?? 'application/octet-stream',
        });
        res.end(buf);
    } catch {
        res.writeHead(404).end('not found: ' + url.pathname);
    }
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const port = server.address().port;
const URL_BASE = `http://127.0.0.1:${port}`;

const browser = await puppeteer.launch({
    executablePath: '/usr/bin/chromium',
    headless: true,
    args: ['--no-sandbox', '--disable-gpu', '--disable-dev-shm-usage'],
});
const page = await browser.newPage();
page.on('console', (msg) => process.stderr.write(`  [console.${msg.type()}] ${msg.text()}\n`));
page.on('pageerror', (err) => process.stderr.write(`  [pageerror] ${err.message}\n${err.stack ?? ''}\n`));
page.on('requestfailed', (req) => process.stderr.write(`  [requestfailed] ${req.url()} — ${req.failure()?.errorText}\n`));
page.on('response', (res) => {
    if (res.status() >= 400) process.stderr.write(`  [http ${res.status()}] ${res.url()}\n`);
});

let exitCode = 0;
try {
    await page.goto(`${URL_BASE}/site/viewer-sql/index.html`, {
        waitUntil: 'networkidle0',
        timeout: 60_000,
    });
    // Wait for "ready — drop a parquet file" status, then click demo.
    await page.waitForFunction(
        () => /ready/.test(document.getElementById('status')?.textContent ?? ''),
        { timeout: 60_000, polling: 250 },
    );
    await page.click('#demo');
    // Wait for the section JSON to appear.
    await page.waitForFunction(
        () => {
            const txt = document.getElementById('section')?.textContent ?? '';
            return txt.length > 50 && txt !== '—';
        },
        { timeout: 120_000, polling: 500 },
    );
    const status = await page.$eval('#status', (el) => el.textContent);
    const info = await page.$eval('#info', (el) => el.textContent);
    const section = await page.$eval('#section', (el) => el.textContent);
    console.log('=== STATUS ===');
    console.log(status);
    console.log('\n=== INFO ===');
    console.log(info.slice(0, 500));
    console.log('\n=== SECTION (first 400) ===');
    console.log(section.slice(0, 400));
    if (status.includes('FAIL') || status.includes('error')) exitCode = 1;
} catch (e) {
    console.error('Driver error:', e.message);
    const status = await page.$eval('#status', (el) => el.textContent).catch(() => '?');
    console.error('Last status:', status);
    exitCode = 2;
} finally {
    await browser.close();
    server.close();
}
process.exit(exitCode);
