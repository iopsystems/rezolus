// Probe __SELECTED_CGROUPS__ substitution and the JOIN-against-_cgroup_index
// pattern that cgroup dashboard SQL will use.
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
let exitCode = 0;
try {
    await page.goto(`http://127.0.0.1:${port}/site/viewer-sql/index.html`, { waitUntil: 'networkidle0', timeout: 60_000 });
    await page.waitForFunction(() => /ready/.test(document.getElementById('status')?.textContent ?? ''), { timeout: 60_000, polling: 250 });
    await page.click('#demo');
    await page.waitForFunction(() => typeof window.__viewerSqlSession === 'object' && window.__viewerSqlSession !== null, { timeout: 60_000, polling: 250 });
    const result = await page.evaluate(async () => {
        const viewer = window.__viewerSqlSession.viewer;
        const info = JSON.parse(viewer.info());
        const tr = [info.minTime ?? 0, info.maxTime ?? 0];
        const out = {};

        // Aggregate-side cpu_usage rate (NO selection): all cgroups except none.
        const aggSql = `
            WITH unp AS (
                UNPIVOT (SELECT timestamp, COLUMNS('^cgroup_cpu_usage(/.+)?$') FROM _src)
                    ON COLUMNS('^cgroup_cpu_usage(/.+)?$') INTO NAME col VALUE v
            ),
            joined AS (
                SELECT u.timestamp, u.v
                FROM unp u JOIN _cgroup_index idx
                    ON idx.column_name = u.col AND idx.metric = 'cgroup_cpu_usage'
                WHERE COALESCE(idx.name, '') NOT IN __SELECTED_CGROUPS__
            ),
            agg AS (SELECT timestamp, SUM(v) AS s FROM joined GROUP BY timestamp)
            SELECT timestamp::DOUBLE/1e9 AS t, irate_1s(s, timestamp) AS v FROM agg
        `;
        // 1) Empty selection — should aggregate ALL cgroups (since selection sentinel doesn't match anything).
        viewer.set_selected_cgroups([]);
        let resp = await viewer.query_range(aggSql, tr[0], tr[1], 1.0);
        out.aggregateNoSelection = (() => {
            const p = JSON.parse(resp);
            const s = p?.data?.result?.[0]?.values ?? [];
            return { series: p?.data?.result?.length ?? 0, samples: s.length, first: s[0] };
        })();

        // 2) Select '/' — aggregate side excludes name='/', should keep just the no-name (root host) columns.
        viewer.set_selected_cgroups(['/']);
        resp = await viewer.query_range(aggSql, tr[0], tr[1], 1.0);
        out.aggregateExcludeRoot = (() => {
            const p = JSON.parse(resp);
            const s = p?.data?.result?.[0]?.values ?? [];
            return { series: p?.data?.result?.length ?? 0, samples: s.length, first: s[0] };
        })();

        // 3) Individual side: WHERE name IN selection, fan out by name.
        const indSql = `
            WITH unp AS (
                UNPIVOT (SELECT timestamp, COLUMNS('^cgroup_cpu_usage(/.+)?$') FROM _src)
                    ON COLUMNS('^cgroup_cpu_usage(/.+)?$') INTO NAME col VALUE v
            ),
            joined AS (
                SELECT u.timestamp, idx.name, u.v
                FROM unp u JOIN _cgroup_index idx
                    ON idx.column_name = u.col AND idx.metric = 'cgroup_cpu_usage'
                WHERE idx.name IN __SELECTED_CGROUPS__
            ),
            by_name AS (
                SELECT timestamp, name, SUM(v) AS s FROM joined GROUP BY timestamp, name
            )
            SELECT timestamp::DOUBLE/1e9 AS t, name,
                   irate_lag(s,
                       LAG(s) OVER (PARTITION BY name ORDER BY timestamp),
                       timestamp - LAG(timestamp) OVER (PARTITION BY name ORDER BY timestamp)
                   ) AS v
            FROM by_name
        `;
        viewer.set_selected_cgroups(['/']);
        resp = await viewer.query_range(indSql, tr[0], tr[1], 1.0);
        out.individualRoot = (() => {
            const p = JSON.parse(resp);
            const series = p?.data?.result ?? [];
            return {
                seriesCount: series.length,
                metrics: series.map((s) => s.metric),
                firstSamples: series[0]?.values?.slice(0, 2) ?? [],
            };
        })();

        return out;
    });
    console.log(JSON.stringify(result, null, 2));
} catch (e) {
    console.error('FAIL', e.message, e.stack);
    exitCode = 1;
} finally {
    await browser.close();
    server.close();
}
process.exit(exitCode);
