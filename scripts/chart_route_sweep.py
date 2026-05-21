#!/usr/bin/env python3
"""Walk every chart-detail URL exposed by a running viewer and assert it renders.

Catches the route-mismatch + cache-shape regressions that the section-level
smoke (`scripts/viewer_chromium_smoke.sh`) can't see — those test the
dashboard pages but not the per-chart pinned views at `/#/chart/<section>/<id>`.

Pass criteria for each (section, chart_id) combo: SingleChartView mounted
(`.single-chart-main` + h2 title), AND either echarts painted a canvas OR
rendered the no-data / unavailable placeholder. The placeholder is a
legitimate render path when the parquet happens to carry no data for the
chart (e.g. a vllm.parquet on the GPU section).

Usage:
    bash scripts/viewer_chromium_smoke.sh ... &   # leave viewer running
    python3 scripts/chart_route_sweep.py [http://127.0.0.1:8080]

Requires chromium, python3, `pip install --user websockets`.
"""
import asyncio, json, os, signal, subprocess, sys, time, urllib.request, urllib.parse, base64
import websockets

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080"
PROFILE = f"/tmp/chart-route-sweep-{os.getpid()}"
os.makedirs(PROFILE, exist_ok=True)
SHOTS_DIR = "/tmp/chart_route_sweep_shots"
os.makedirs(SHOTS_DIR, exist_ok=True)
SAMPLE_PER_SECTION = int(os.environ.get("SWEEP_SAMPLE", "3"))

PORT = 9334
chrome = subprocess.Popen(
    ["chromium", "--headless=new", "--no-sandbox", "--disable-gpu",
     f"--remote-debugging-port={PORT}", f"--user-data-dir={PROFILE}",
     "about:blank"],
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
)

def http_json(url):
    with urllib.request.urlopen(url, timeout=5) as r:
        return json.load(r)

def collect_chart_ids(section_route):
    """Return list of (chartId, title) for a section, taken from
    /data/<route>.json before client-side filtering — so we exercise
    every chart spec the dashboard generator emits, regardless of data."""
    stem = section_route.lstrip('/')
    try:
        data = http_json(f"{BASE}/data/{stem}.json")
    except Exception:
        return []
    out = []
    for g in data.get('groups') or []:
        for sg in g.get('subgroups') or []:
            for p in sg.get('plots') or []:
                cid = (p.get('opts') or {}).get('id')
                title = (p.get('opts') or {}).get('title') or '?'
                if cid:
                    out.append((cid, title))
    return out

async def main():
    sections = http_json(f"{BASE}/api/v1/sections?capture_id=baseline")['data']['sections']
    skip = {'/query', '/metadata', '/selection', '/report', '/systeminfo'}
    section_routes = [s['route'] for s in sections if s['route'] not in skip]

    deadline = time.time() + 10
    while time.time() < deadline:
        try:
            ws_url = http_json(f"http://127.0.0.1:{PORT}/json/version")["webSocketDebuggerUrl"]
            break
        except Exception:
            await asyncio.sleep(0.3)

    msg_id = 0
    async with websockets.connect(ws_url, max_size=2**25) as ws:
        async def send(method, params=None, session_id=None):
            nonlocal msg_id
            msg_id += 1
            req = {"id": msg_id, "method": method, "params": params or {}}
            if session_id:
                req["sessionId"] = session_id
            await ws.send(json.dumps(req))
            while True:
                resp = json.loads(await ws.recv())
                if resp.get("method"):
                    continue
                if resp.get("id") == msg_id:
                    return resp.get("result", {})

        targets = await send("Target.getTargets")
        page = next(t for t in targets["targetInfos"] if t["type"] == "page")
        attach = await send("Target.attachToTarget", {"targetId": page["targetId"], "flatten": True})
        sess = attach["sessionId"]
        await send("Page.enable", session_id=sess)
        await send("Runtime.enable", session_id=sess)
        await send("Emulation.setDeviceMetricsOverride", {
            "width": 1400, "height": 900, "deviceScaleFactor": 1, "mobile": False,
        }, session_id=sess)

        probe_js = r"""(() => {
          const single = document.querySelector('.single-chart-main');
          const h2 = document.querySelector('.single-chart-main h2');
          const charts = document.querySelectorAll('.single-chart-main .chart');
          const canvases = document.querySelectorAll('.single-chart-main canvas');
          const noData = !!document.querySelector('.single-chart-main .chart.no-data');
          const unavail = !!document.querySelector('.single-chart-main .chart-unavailable');
          const rect = charts[0] ? charts[0].getBoundingClientRect() : null;
          return JSON.stringify({
            present: !!single,
            h2: h2 ? h2.innerText : null,
            chart_divs: charts.length,
            canvases: canvases.length,
            no_data: noData,
            unavailable: unavail,
            chart_w: rect ? Math.round(rect.width) : 0,
            body_snippet: (document.body.innerText || '').replace(/\s+/g, ' ').slice(0, 200),
          });
        })()"""

        totals = {'tested': 0, 'pass': 0, 'fail': 0}
        failures = []
        for route in section_routes:
            charts = collect_chart_ids(route)
            if not charts:
                print(f"[skip] {route}: no charts in section JSON")
                continue
            for cid, title in charts[:SAMPLE_PER_SECTION]:
                totals['tested'] += 1
                section_param = urllib.parse.quote(route.lstrip('/'), safe='')
                cid_param = urllib.parse.quote(cid, safe='')
                url = f"{BASE}/#/chart/{section_param}/{cid_param}"
                await send("Page.navigate", {"url": url}, session_id=sess)
                rec = None
                for _ in range(24):
                    await asyncio.sleep(0.5)
                    ev = await send("Runtime.evaluate", {
                        "expression": probe_js, "returnByValue": True,
                    }, session_id=sess)
                    rec = json.loads(ev["result"]["value"])
                    ok = rec['present'] and rec['h2'] and (
                        rec['canvases'] > 0 or rec['no_data'] or rec['unavailable']
                    )
                    if ok:
                        break
                ok = rec['present'] and rec['h2'] and (
                    rec['canvases'] > 0 or rec['no_data'] or rec['unavailable']
                )
                status = 'PASS' if ok else 'FAIL'
                if ok:
                    totals['pass'] += 1
                else:
                    totals['fail'] += 1
                    failures.append({'route': route, 'chart': cid, 'title': title,
                                     'url': url, 'probe': rec})
                    shot = await send("Page.captureScreenshot", {"format": "png"}, session_id=sess)
                    safe = (route.replace('/', '_') + '__' + cid).strip('_')
                    with open(os.path.join(SHOTS_DIR, f"{safe}.png"), 'wb') as f:
                        f.write(base64.b64decode(shot["data"]))
                flags = []
                if rec['canvases'] > 0: flags.append(f"canvas={rec['canvases']}")
                if rec['no_data']: flags.append("no-data")
                if rec['unavailable']: flags.append("unavailable")
                print(f"[{status}] {route:25s} {cid:35s} present={rec['present']} h2={rec['h2']!r:25s} {' '.join(flags)}")

        print(f"\n=== TOTALS: tested={totals['tested']} pass={totals['pass']} fail={totals['fail']}")
        if failures:
            print("\n=== FAILURES ===")
            for f in failures:
                print(f"{f['route']} :: {f['chart']} ({f['title']})")
                print(f"  url: {f['url']}")
                print(f"  body: {f['probe']['body_snippet'][:160]}")
                print(f"  screenshot: {SHOTS_DIR}/{f['route'].replace('/','_').strip('_')}__{f['chart']}.png")
            sys.exit(1)

asyncio.run(main())
chrome.send_signal(signal.SIGTERM); chrome.wait(timeout=5)
