#!/usr/bin/env bash
# Headless-Chromium smoke for `rezolus view`.
#
# Walks every section in /api/v1/sections, capturing console messages,
# failed network requests, chart-error elements, and a per-section
# screenshot so we can tell whether charts actually render (vs.
# page-loads-but-blank).
#
# Usage:
#   scripts/viewer_chromium_smoke.sh <parquet> [--port 18510] [--out DIR]
#                                              [--wait-ms 4000]
#                                              [--keep-server]
#                                              [--skip <route,route>]
#   scripts/viewer_chromium_smoke.sh --live <agent-url> [--port 18510]
#                                                       [--out DIR]
#                                                       [--wait-ms 4000]
#                                                       [--ingest-wait 5]
#                                                       [--keep-server]
#                                                       [--skip <route,route>]
#
# --live <url> connects the viewer to a running Rezolus agent
# (e.g. http://localhost:4241) instead of opening a parquet file.
# Waits --ingest-wait seconds (default 5) after viewer startup so
# the live source has accumulated rows before chromium loads pages.
#
# Exit codes:
#   0  all sections rendered cleanly
#   1  one or more sections had chart errors / console errors / failed
#      requests (full report in $OUT/report.json)
#   2  usage / setup error
#
# Requires: chromium (or chromium-browser), curl, jq, python3, and the
# python `websockets` package (pip install --user websockets).
#
# Skip routes default to /query (interactive explorer, no charts on
# load), /metadata (text-only), /notebook (text-only), /selection,
# /report. /overview is empty for parquets without a Default section
# but is included so we'd notice if that ever changes.

set -euo pipefail

PARQUET=""
LIVE_URL=""
PORT=18510
OUT=""
KEEP_SERVER=0
WAIT_MS=4000
INGEST_WAIT=5
SKIP="/query,/metadata,/notebook,/selection,/report"

usage() {
    cat >&2 <<EOF
usage: $0 <parquet> [--port N] [--out DIR] [--wait-ms N] [--keep-server]
                    [--skip route1,route2]
EOF
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --live) LIVE_URL="$2"; shift 2;;
        --port) PORT="$2"; shift 2;;
        --out) OUT="$2"; shift 2;;
        --wait-ms) WAIT_MS="$2"; shift 2;;
        --ingest-wait) INGEST_WAIT="$2"; shift 2;;
        --keep-server) KEEP_SERVER=1; shift;;
        --skip) SKIP="$2"; shift 2;;
        -h|--help) usage;;
        *)
            if [[ -z "$PARQUET" ]]; then PARQUET="$1"; shift
            else echo "unexpected: $1" >&2; usage
            fi;;
    esac
done

if [[ -n "$LIVE_URL" ]]; then
    [[ -z "$PARQUET" ]] || { echo "cannot mix --live with a positional parquet" >&2; usage; }
    # Verify the live agent is reachable so we fail fast (rather than
    # spawning a viewer that exits with "could not connect").
    if ! curl -fs --max-time 2 "$LIVE_URL/" >/dev/null 2>&1; then
        echo "live agent not reachable at $LIVE_URL" >&2
        exit 2
    fi
else
    [[ -n "$PARQUET" ]] || usage
    [[ -f "$PARQUET" ]] || { echo "no such file: $PARQUET" >&2; exit 2; }
fi

CHROMIUM=$(command -v chromium || command -v chromium-browser || command -v google-chrome || true)
[[ -n "$CHROMIUM" ]] || { echo "chromium not found in PATH" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq required" >&2; exit 2; }
command -v python3 >/dev/null || { echo "python3 required" >&2; exit 2; }
python3 -c "import websockets" 2>/dev/null || {
    echo "python websockets package required: pip install --user websockets" >&2
    exit 2
}

OUT="${OUT:-$(mktemp -d -t rezolus-chrome-XXXXXX)}"
mkdir -p "$OUT"
echo "out: $OUT"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# Prefer the more recently built binary so iterative edits land in the
# server we're driving. Falls back to whichever exists.
DEBUG_BIN="$REPO_ROOT/target/debug/rezolus"
RELEASE_BIN="$REPO_ROOT/target/release/rezolus"
if [[ -x "$DEBUG_BIN" && -x "$RELEASE_BIN" ]]; then
    if [[ "$DEBUG_BIN" -nt "$RELEASE_BIN" ]]; then BIN="$DEBUG_BIN"
    else BIN="$RELEASE_BIN"; fi
elif [[ -x "$RELEASE_BIN" ]]; then BIN="$RELEASE_BIN"
elif [[ -x "$DEBUG_BIN" ]]; then BIN="$DEBUG_BIN"
else echo "no rezolus binary built — run cargo build first" >&2; exit 2; fi
echo "binary: $BIN"

# --- launch rezolus ----------------------------------------------------
SERVER_LOG="$OUT/rezolus.log"
if [[ -n "$LIVE_URL" ]]; then
    INPUT_ARG="$LIVE_URL"
    echo "input: live agent at $LIVE_URL"
else
    INPUT_ARG="$PARQUET"
    echo "input: parquet $PARQUET"
fi
REZOLUS_NO_OPEN=1 "$BIN" view "$INPUT_ARG" --listen "127.0.0.1:$PORT" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!

cleanup() {
    if [[ "$KEEP_SERVER" -eq 0 ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    if [[ -n "${CHROME_PID:-}" ]] && kill -0 "$CHROME_PID" 2>/dev/null; then
        kill "$CHROME_PID" 2>/dev/null || true
        wait "$CHROME_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# Wait for server to bind.
for _ in $(seq 1 40); do
    if curl -fs "http://127.0.0.1:$PORT/api/v1/mode" >/dev/null 2>&1; then break; fi
    sleep 0.25
done
if ! curl -fs "http://127.0.0.1:$PORT/api/v1/mode" >/dev/null; then
    echo "rezolus didn't come up — log:" >&2
    tail -40 "$SERVER_LOG" >&2
    exit 1
fi

# In live mode the viewer starts with an empty _src; let the ingest
# loop accumulate at least a few rows so charts have data when
# chromium loads them. Skip in file mode (data is already present).
if [[ -n "$LIVE_URL" ]]; then
    echo "live mode: waiting ${INGEST_WAIT}s for ingest to accumulate rows"
    sleep "$INGEST_WAIT"
    # Re-check metadata so we know how many seconds of data we have.
    META=$(curl -fs "http://127.0.0.1:$PORT/api/v1/metadata?capture=baseline" 2>/dev/null || true)
    SPAN_MS=$(echo "$META" | python3 -c "import json,sys; d=json.load(sys.stdin)['data']; print(d.get('maxTime',0)-d.get('minTime',0))" 2>/dev/null || echo "?")
    echo "  accumulated span: ${SPAN_MS}ms"
fi

# --- discover sections -------------------------------------------------
SECTIONS_JSON=$(curl -fs "http://127.0.0.1:$PORT/api/v1/sections")
echo "$SECTIONS_JSON" > "$OUT/sections.json"

# --- launch chromium with remote debugging ----------------------------
CDP_PORT=$((PORT + 1000))
PROFILE="$OUT/chrome-profile"
"$CHROMIUM" \
    --headless=new \
    --disable-gpu \
    --no-sandbox \
    --hide-scrollbars \
    --remote-debugging-port="$CDP_PORT" \
    --remote-allow-origins="http://127.0.0.1:$CDP_PORT" \
    --user-data-dir="$PROFILE" \
    --window-size=1600,1000 \
    "about:blank" \
    >"$OUT/chrome.log" 2>&1 &
CHROME_PID=$!

# Wait for CDP.
for _ in $(seq 1 40); do
    if curl -fs "http://127.0.0.1:$CDP_PORT/json/version" >/dev/null 2>&1; then break; fi
    sleep 0.25
done
WS_URL=$(curl -fs "http://127.0.0.1:$CDP_PORT/json/version" | jq -r '.webSocketDebuggerUrl')
[[ -n "$WS_URL" && "$WS_URL" != null ]] || {
    echo "couldn't get CDP websocket" >&2
    tail -40 "$OUT/chrome.log" >&2
    exit 1
}

# --- drive the page via CDP ------------------------------------------
PAGE_URL="http://127.0.0.1:$PORT/"
REPORT="$OUT/report.json"
SHOTS_DIR="$OUT/shots"
mkdir -p "$SHOTS_DIR"

WS_URL="$WS_URL" PAGE_URL="$PAGE_URL" REPORT="$REPORT" SHOTS_DIR="$SHOTS_DIR" \
WAIT_MS="$WAIT_MS" SKIP="$SKIP" SECTIONS_JSON="$SECTIONS_JSON" \
python3 - <<'PY'
import asyncio, base64, json, os, re, sys

import websockets

WS_URL = os.environ['WS_URL']
PAGE_URL = os.environ['PAGE_URL']
REPORT = os.environ['REPORT']
SHOTS_DIR = os.environ['SHOTS_DIR']
WAIT_MS = int(os.environ['WAIT_MS'])
SKIP = set(s.strip() for s in os.environ['SKIP'].split(',') if s.strip())
SECTIONS = json.loads(os.environ['SECTIONS_JSON'])['data']['sections']

class CDP:
    def __init__(self, ws):
        self.ws = ws
        self.next_id = 0
        self.pending = {}
        self.events = []
        self.reader_task = None

    async def _reader(self):
        try:
            async for msg in self.ws:
                data = json.loads(msg)
                if 'id' in data:
                    fut = self.pending.pop(data['id'], None)
                    if fut and not fut.done():
                        fut.set_result(data)
                else:
                    self.events.append(data)
        except asyncio.CancelledError:
            pass

    def start_reader(self):
        self.reader_task = asyncio.create_task(self._reader())

    def drain_events(self):
        evs = self.events
        self.events = []
        return evs

    async def send(self, method, params=None, session_id=None):
        self.next_id += 1
        payload = {'id': self.next_id, 'method': method, 'params': params or {}}
        if session_id:
            payload['sessionId'] = session_id
        fut = asyncio.get_event_loop().create_future()
        self.pending[self.next_id] = fut
        await self.ws.send(json.dumps(payload))
        resp = await fut
        if 'error' in resp:
            raise RuntimeError(f"CDP {method} failed: {resp['error']}")
        return resp.get('result', {})


def slug(route):
    return re.sub(r'[^a-z0-9]+', '_', route.lower()).strip('_') or 'root'


def classify_events(events):
    console = []
    net_failures = []
    net_responses = {}
    for ev in events:
        m = ev.get('method')
        if m == 'Runtime.consoleAPICalled':
            p = ev['params']
            args_text = ' '.join(
                str(a.get('value', a.get('description', '')))
                for a in p.get('args', [])
            )
            console.append({'level': p.get('type'), 'text': args_text})
        elif m == 'Runtime.exceptionThrown':
            p = ev['params']['exceptionDetails']
            console.append({
                'level': 'exception',
                'text': p.get('text', '') + ' '
                        + (p.get('exception', {}).get('description') or ''),
            })
        elif m == 'Network.responseReceived':
            r = ev['params']['response']
            net_responses[ev['params']['requestId']] = {
                'url': r['url'], 'status': r['status'],
            }
        elif m == 'Network.loadingFailed':
            p = ev['params']
            net_failures.append({
                'requestId': p['requestId'],
                'error': p.get('errorText'),
                'url': net_responses.get(p['requestId'], {}).get('url'),
            })
    bad_responses = [
        r for r in net_responses.values()
        if r['status'] >= 400 and not (
            # /selection and /systeminfo legitimately 404 when absent.
            r['url'].endswith('/api/v1/selection')
            or r['url'].endswith('/api/v1/systeminfo')
        )
    ]
    return console, net_failures, bad_responses


CHART_PROBE_JS = r'''
JSON.stringify((() => {
  const sectionTitle = (() => {
    const h1 = document.querySelector('h1.section-title, #section-content h1');
    return h1 ? h1.textContent.trim() : '';
  })();
  const wrappers = document.querySelectorAll('.chart-wrapper');
  let svgs_in_wrappers = 0;
  let canvases_in_wrappers = 0;
  let non_empty_wrappers = 0;
  wrappers.forEach((w) => {
    const s = w.querySelectorAll('svg').length;
    const c = w.querySelectorAll('canvas').length;
    svgs_in_wrappers += s;
    canvases_in_wrappers += c;
    if (s + c > 0) non_empty_wrappers++;
  });
  const errEls = Array.from(document.querySelectorAll('.chart-error'))
    .map(e => e.textContent.trim()).slice(0, 20);
  const loadingEls = document.querySelectorAll('.chart-loading').length;
  const splash = document.querySelector('#splash');
  // `.chart-unavailable` placeholders are intentional — KPI plots
  // whose SQL hasn't been transcribed yet, rendered to reserve grid
  // footprint. `.section-notes` is the "Charts with no data" /
  // "Unavailable KPIs" callout. Either is a legitimate "section
  // rendered, just no time-series data" state.
  const unavailable_placeholders = document.querySelectorAll('.chart-unavailable').length;
  const notes_sections = document.querySelectorAll('.section-notes').length;
  const notes_items = document.querySelectorAll('.section-notes li').length;
  const groups_count = document.querySelectorAll('#groups > *').length;
  return {
    section_title: sectionTitle,
    chart_wrappers: wrappers.length,
    non_empty_chart_wrappers: non_empty_wrappers,
    svgs_in_wrappers,
    canvases_in_wrappers,
    chart_errors: errEls,
    chart_loading_count: loadingEls,
    unavailable_placeholders,
    notes_sections,
    notes_items,
    groups_count,
    splash_visible: !!splash,
    body_text_excerpt: (document.body.innerText || '').replace(/\s+/g, ' ').slice(0, 2000),
  };
})())
'''


async def probe_section(cdp, sess, route, name):
    cdp.drain_events()  # baseline noise from prior section
    target_url = f"{PAGE_URL}#{route}"
    await cdp.send('Page.navigate', {'url': target_url}, session_id=sess)
    await asyncio.sleep(WAIT_MS / 1000)
    # Poll until splash disappears, capped at 2x WAIT_MS.
    deadline = asyncio.get_event_loop().time() + (WAIT_MS / 1000) * 2
    while asyncio.get_event_loop().time() < deadline:
        probe = await cdp.send('Runtime.evaluate', {
            'expression': CHART_PROBE_JS, 'returnByValue': True,
        }, session_id=sess)
        info = json.loads(probe['result']['value'])
        if not info['splash_visible'] and info['chart_loading_count'] == 0:
            break
        await asyncio.sleep(0.5)
    else:
        probe = await cdp.send('Runtime.evaluate', {
            'expression': CHART_PROBE_JS, 'returnByValue': True,
        }, session_id=sess)
        info = json.loads(probe['result']['value'])

    events = cdp.drain_events()
    console, failed, bad_resp = classify_events(events)

    shot = await cdp.send('Page.captureScreenshot', {'format': 'png'}, session_id=sess)
    shot_path = os.path.join(SHOTS_DIR, f"{slug(route)}.png")
    with open(shot_path, 'wb') as f:
        f.write(base64.b64decode(shot['data']))

    return {
        'route': route, 'name': name, 'url': target_url,
        'probe': info,
        'console': console,
        'failed_requests': failed,
        'http_error_responses': bad_resp,
        'screenshot': shot_path,
    }


def score_section(s):
    """Return (passed, reasons, note) for a section probe.

    A section passes if EITHER:
    - it rendered at least one chart with data, OR
    - it explicitly explains the no-data state (placeholders or notes).

    Console exceptions, chart-error elements, and HTTP failures always
    fail the section."""
    reasons = []
    notes = []
    info = s['probe']
    rendered_data = info['non_empty_chart_wrappers'] > 0
    explained = (info['unavailable_placeholders'] > 0
                 or info['notes_items'] > 0)

    if s['route'] == '/overview' and info['chart_wrappers'] == 0:
        # Overview is intentionally empty when no Default section is
        # configured — the sidebar is the content.
        pass
    elif rendered_data:
        pass  # happy path
    elif explained:
        notes.append(
            f"no data — {info['unavailable_placeholders']} placeholder(s), "
            f"{info['notes_items']} notes item(s)"
        )
    else:
        reasons.append(
            f"section appears silently empty "
            f"(wrappers={info['chart_wrappers']}, "
            f"placeholders={info['unavailable_placeholders']}, "
            f"notes={info['notes_items']})"
        )

    if info['chart_errors']:
        reasons.append(f"chart errors: {info['chart_errors'][:3]}")
    if info['chart_loading_count']:
        reasons.append(f"{info['chart_loading_count']} charts stuck in loading state")
    err_console = [c for c in s['console']
                   if c['level'] in ('error', 'exception')]
    if err_console:
        reasons.append(f"{len(err_console)} console error(s): {err_console[0]['text'][:200]}")
    if s['http_error_responses']:
        urls = [r['url'] for r in s['http_error_responses']]
        reasons.append(f"{len(urls)} HTTP error(s): {urls[:3]}")
    if s['failed_requests']:
        reasons.append(f"{len(s['failed_requests'])} request(s) failed to complete")
    return (len(reasons) == 0, reasons, notes)


async def main():
    async with websockets.connect(WS_URL, max_size=64 * 1024 * 1024) as ws:
        cdp = CDP(ws)
        cdp.start_reader()

        targets = (await cdp.send('Target.getTargets'))['targetInfos']
        page = next(t for t in targets if t['type'] == 'page')
        sess = (await cdp.send('Target.attachToTarget',
                {'targetId': page['targetId'], 'flatten': True}))['sessionId']

        await cdp.send('Page.enable', session_id=sess)
        await cdp.send('Runtime.enable', session_id=sess)
        await cdp.send('Network.enable', session_id=sess)
        await cdp.send('Log.enable', session_id=sess)

        # First load — establishes baseline.
        await cdp.send('Page.navigate', {'url': PAGE_URL}, session_id=sess)
        await asyncio.sleep(WAIT_MS / 1000)
        cdp.drain_events()

        section_results = []
        for s in SECTIONS:
            if s['route'] in SKIP:
                section_results.append({
                    'route': s['route'], 'name': s['name'], 'skipped': True,
                })
                continue
            print(f"  -> {s['name']} ({s['route']})", file=sys.stderr)
            res = await probe_section(cdp, sess, s['route'], s['name'])
            passed, reasons, notes = score_section(res)
            res['passed'] = passed
            res['fail_reasons'] = reasons
            res['notes'] = notes
            section_results.append(res)

        # Roll-up.
        evaluated = [s for s in section_results if not s.get('skipped')]
        failures = [s for s in evaluated if not s.get('passed', True)]
        report = {
            'page_url': PAGE_URL,
            'sections': section_results,
            'summary': {
                'total': len(SECTIONS),
                'evaluated': len(evaluated),
                'skipped': len(section_results) - len(evaluated),
                'passed': len(evaluated) - len(failures),
                'failed': len(failures),
            },
        }
        with open(REPORT, 'w') as f:
            json.dump(report, f, indent=2)

        cdp.reader_task.cancel()
        try:
            await cdp.reader_task
        except asyncio.CancelledError:
            pass

        # Console summary.
        print()
        print("=== per-section ===")
        for s in section_results:
            if s.get('skipped'):
                print(f"  SKIP  {s['route']}")
            elif s['passed']:
                p = s['probe']
                tail = ''
                if s.get('notes'):
                    tail = f"  ({'; '.join(s['notes'])})"
                print(f"  PASS  {s['route']:30s}  wrappers={p['chart_wrappers']:3d}  rendered={p['non_empty_chart_wrappers']:3d}{tail}")
            else:
                print(f"  FAIL  {s['route']:30s}  -> {'; '.join(s['fail_reasons'])}")
        print()
        print(f"=== summary: {report['summary']} ===")

        sys.exit(0 if not failures else 1)

asyncio.run(main())
PY

PY_EXIT=$?
echo
echo "screenshots: $SHOTS_DIR"
echo "report:      $REPORT"
echo "server log:  $SERVER_LOG"
exit $PY_EXIT
