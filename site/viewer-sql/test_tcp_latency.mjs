// Trace what happens when we ask the sglang/router source for tcp packet
// latency: dashboard SQL → query_range → arrow result.
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
page.on('console', (m) => process.stdout.write(`[${m.type()}] ${m.text()}\n`));
page.on('pageerror', (err) => process.stderr.write(`[pageerror] ${err.message}\n`));
try {
    await page.goto(`http://127.0.0.1:${port}/site/viewer-sql/preview.html`, { waitUntil: 'networkidle0', timeout: 60_000 });
    await page.waitForFunction(() => /Ready/i.test(document.getElementById('status')?.textContent ?? ''), { timeout: 60_000, polling: 250 });
    await page.click('#demo-large');
    await page.waitForFunction(() => typeof window.__viewerSqlSession === 'object', { timeout: 120_000, polling: 1000 });
    await new Promise((r) => setTimeout(r, 2000));
    const out = await page.evaluate(async () => {
        const v = window.__viewerSqlSession.viewer;
        const conn = window.__viewerSqlSession.conn;
        // 1) Confirm the source view exposes the column.
        const probe1 = await conn.query(`SELECT typeof("tcp_packet_latency:buckets") AS t FROM _src_router LIMIT 1`).catch((e) => ({ err: String(e.message ?? e) }));
        const probeText = probe1.toArray ? probe1.toArray()[0]?.toJSON() : probe1;
        // 2) Pull the dashboard SQL the network section emits for tcp packet latency.
        const json = JSON.parse(v.get_section('network'));
        const plots = [];
        for (const g of (json.groups ?? [])) for (const sg of (g.subgroups ?? [])) for (const p of (sg.plots ?? [])) plots.push(p);
        const tcp = plots.find((p) => /tcp.*latency/i.test(p.opts?.id ?? ''));
        const sql = tcp?.sql_query;
        // 3) Run it through query_range like the preview would.
        const info = JSON.parse(v.info());
        const tr = [info.minTime, info.maxTime];
        const t0 = performance.now();
        const resp = await v.query_range(sql, tr[0], tr[1], 1.0).catch((e) => ({ err: String(e.message ?? e) }));
        const dt = performance.now() - t0;
        let parsed = null, err = null;
        if (typeof resp === 'string') { parsed = JSON.parse(resp); }
        else { err = resp?.err; }
        return {
            probeColumn: probeText,
            sql: sql?.slice(0, 200),
            wallMs: dt.toFixed(1),
            err,
            seriesCount: parsed?.data?.result?.length,
            firstSeries: parsed?.data?.result?.[0],
        };
    });
    console.log(JSON.stringify(out, null, 2));
} catch (e) {
    console.error('FAIL', e.message, e.stack);
} finally {
    await browser.close();
    server.close();
}
