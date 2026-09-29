// The MCP `viewer_link` tool (src/mcp/link.rs) formats a query string the
// viewer's url_state.js parses. The Rust tests assert the exact string for
// one full fixture; this test feeds that same string through the parser, so
// a change on either side that the other cannot read fails here. Keep
// PARITY_FULL identical to `link::tests::PARITY_FULL`.
import test from 'node:test';
import assert from 'node:assert/strict';
import { parseViewState } from '../src/viewer/assets/lib/ui/url_state.js';

const PARITY_FULL = '?from=2026-05-10T00%3A35%3A48.250Z&to=2026-05-10T00%3A38%3A49.000Z&time=raw&node=web-01&gpu=nvidia%3A0&gpu=1&cgroup=%2Fsystem.slice&cgroup=%2Fuser.slice%2Fa%2Cb&instance=3&family=sigma%3A1.5&anchor.baseline=kind%3Arun_start&anchor.experiment=-1500';

test('the viewer reads back every key the MCP link tool writes', () => {
    const st = parseViewState(PARITY_FULL);
    assert.equal(st.from, 1778373348.25);
    assert.equal(st.to, 1778373529);
    assert.equal(st.time, 'raw');
    assert.equal(st.node, 'web-01');
    assert.deepEqual(st.gpu, [{ vendor: 'nvidia', id: '0' }, { vendor: null, id: '1' }]);
    assert.deepEqual(st.cgroup, ['/system.slice', '/user.slice/a,b']);
    assert.equal(st.instance, '3');
    assert.deepEqual(st.family, { kind: 'sigma', k: 1.5 });
    assert.deepEqual(st.anchors, { baseline: { kind: 'run_start' }, experiment: -1500 });
});

test('the Rust fixture in link.rs is the one this test holds', async () => {
    const fs = await import('node:fs/promises');
    const src = await fs.readFile(new URL('../src/mcp/link.rs', import.meta.url), 'utf8');
    const m = /PARITY_FULL: &str = "([^"]+)"/.exec(src);
    assert.ok(m, 'link.rs declares PARITY_FULL');
    assert.equal(m[1], PARITY_FULL);
});
