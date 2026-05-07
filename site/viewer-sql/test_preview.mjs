// Smoke test the static preview page in headless chromium: ensure it
// boots, loads the demo, renders at least one chart with data.
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
page.on('console', (m) => process.stdout.write(`[console.${m.type()}] ${m.text().slice(0, 300)}\n`));
page.on('pageerror', (e) => process.stderr.write(`[pageerror] ${e.message}\n${(e.stack || '').slice(0, 500)}\n`));
let exitCode = 0;
try {
    await page.goto(`http://127.0.0.1:${port}/site/viewer-sql/preview.html`, { waitUntil: 'networkidle0', timeout: 60_000 });
    await page.waitForSelector('#demo:not([disabled])', { timeout: 30_000 });
    await page.click('#demo');
    // Wait until at least one chart has rendered.
    await page.waitForFunction(
        () => document.querySelectorAll('#content .plot canvas').length > 1,
        { timeout: 120_000, polling: 500 },
    );
    // Lazy render: scroll the page to trigger IntersectionObserver for
    // off-screen plots so the test can validate them all.
    await page.evaluate(async () => {
        const step = window.innerHeight * 0.8;
        for (let y = 0; y < document.body.scrollHeight; y += step) {
            window.scrollTo(0, y);
            await new Promise((r) => setTimeout(r, 200));
        }
        window.scrollTo(0, 0);
    });
    await new Promise((r) => setTimeout(r, 2000));
    // Click /cpu to exercise heatmap rendering for per-id and histogram plots.
    await page.evaluate(() => {
        const btn = Array.from(document.querySelectorAll('nav button[data-section]'))
            .find((b) => b.dataset.section === 'cpu');
        btn?.click();
    });
    await new Promise((r) => setTimeout(r, 1000));
    // Scroll through cpu section to trigger IntersectionObserver renders.
    await page.evaluate(async () => {
        const step = window.innerHeight * 0.8;
        for (let y = 0; y < document.body.scrollHeight; y += step) {
            window.scrollTo(0, y);
            await new Promise((r) => setTimeout(r, 200));
        }
        window.scrollTo(0, 0);
    });
    await new Promise((r) => setTimeout(r, 4000));
    await page.screenshot({ path: '/tmp/preview-cpu.png', fullPage: true });
    console.log('screenshot: /tmp/preview-cpu.png');
    // Probe Total / Available data shape directly.
    const dataProbe = await page.evaluate(async () => {
        const v = window.__viewerSqlSession?.viewer;
        if (!v) return { err: 'no session' };
        const info = JSON.parse(v.info());
        const tr = [info.minTime, info.maxTime];
        const totalSql = `SELECT timestamp::DOUBLE/1e9 AS t, "memory_total"::DOUBLE AS v FROM _src`;
        const resp = await v.query_range(totalSql, tr[0], tr[1], 1.0);
        const parsed = JSON.parse(resp);
        return {
            tr,
            seriesCount: parsed?.data?.result?.length,
            firstFew: parsed?.data?.result?.[0]?.values?.slice(0, 3),
            metric0: parsed?.data?.result?.[0]?.metric,
            samples: parsed?.data?.result?.[0]?.values?.length,
            totalCanvas: (() => {
                const plot = Array.from(document.querySelectorAll('#content .plot'))
                    .find((p) => p.querySelector('h4')?.textContent === 'Total');
                if (!plot) return null;
                const c = plot.querySelector('canvas');
                if (!c) return null;
                const ctx = c.getContext('2d');
                const data = ctx.getImageData(c.width / 2, c.height / 2, 1, 1).data;
                return { w: c.width, h: c.height, centerPixel: Array.from(data) };
            })(),
        };
    });
    console.log('--- data probe ---');
    console.log(JSON.stringify(dataProbe, null, 2));

    const summary = await page.evaluate(() => {
        const status = document.getElementById('status').textContent;
        const sections = Array.from(document.querySelectorAll('nav button[data-section]')).map((b) => b.textContent);
        const active = document.querySelector('nav button.active')?.textContent ?? null;
        const plots = document.querySelectorAll('#content .plot');
        const plotDetails = [];
        let withChart = 0, noSql = 0, fail = 0, noData = 0;
        for (const p of plots) {
            const title = p.querySelector('h4')?.textContent;
            const chart = p.querySelector('.chart');
            const canvas = chart?.querySelector('canvas');
            const rect = canvas?.getBoundingClientRect();
            const text = chart?.textContent?.slice(0, 80);
            plotDetails.push({
                title,
                hasCanvas: !!canvas,
                canvasW: rect?.width,
                canvasH: rect?.height,
                cls: p.className,
                inner: text,
            });
            if (p.classList.contains('fail')) fail++;
            else if (p.classList.contains('no-sql')) noSql++;
            else if (canvas) withChart++;
            else noData++;
        }
        return { status, sections, active, total: plots.length, withChart, noSql, fail, noData, plotDetails };
    });
    console.log(JSON.stringify(summary, null, 2));
    await page.screenshot({ path: '/tmp/preview-memory.png', fullPage: true });
    console.log('screenshot: /tmp/preview-memory.png');
    if (summary.withChart < 3 || summary.fail > 0) {
        console.error('preview did not render expected charts');
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
