import test from 'node:test';
import assert from 'node:assert/strict';
import {
    VIEW_KEYS,
    parseViewState,
    applyViewState,
    parseAnchor,
    formatAnchor,
    ownedKeysIn,
    readViewState,
    writeViewState,
    clearViewState,
} from '../src/viewer/assets/lib/ui/url_state.js';

const T0 = 1778373348.25; // 2026-05-10T00:35:48.250Z
const T1 = 1778373529;

test('empty search parses to the empty state', () => {
    assert.deepEqual(parseViewState(''), {
        from: null, to: null, time: null, node: null,
        cgroup: [], gpu: [], instance: null, anchors: {},
    });
    assert.deepEqual(parseViewState('?'), parseViewState(''));
    assert.deepEqual(parseViewState(undefined), parseViewState(''));
});

test('every key round-trips through apply then parse', () => {
    const search = applyViewState('', {
        from: T0,
        to: T1,
        time: 'raw',
        node: 'web-01',
        gpu: [{ vendor: 'nvidia', id: '0' }, { vendor: null, id: '1' }],
        cgroup: ['/system.slice', '/user.slice/a,b'],
        instance: '3',
        anchors: { baseline: 0, experiment: -1500 },
    });
    assert.ok(search.startsWith('?'));
    // Written as RFC 3339 with ms, not seconds.
    assert.match(search, /from=2026-05-10T00%3A35%3A48\.250Z/);
    const st = parseViewState(search);
    assert.equal(st.from, T0);
    assert.equal(st.to, T1);
    assert.equal(st.time, 'raw');
    assert.equal(st.node, 'web-01');
    assert.deepEqual(st.gpu, [{ vendor: 'nvidia', id: '0' }, { vendor: null, id: '1' }]);
    assert.deepEqual(st.cgroup, ['/system.slice', '/user.slice/a,b']);
    assert.equal(st.instance, '3');
    // A zero anchor is "no shift" and is not written.
    assert.deepEqual(st.anchors, { experiment: -1500 });
    assert.equal(search.includes('anchor.baseline'), false);
});

test('from/to accept Unix seconds and reject ns, a lone bound, or to <= from', () => {
    assert.equal(parseViewState(`?from=${T0}&to=${T1}`).from, T0);
    assert.equal(parseViewState(`?from=${T0}&to=${T1}`).to, T1);
    // Nanoseconds do not fit a JS Number; refused rather than misread.
    assert.equal(parseViewState('?from=1778373348000000000&to=1778373529000000000').from, null);
    assert.equal(parseViewState(`?from=${T0}`).from, null);
    assert.equal(parseViewState(`?from=${T1}&to=${T0}`).from, null);
    assert.equal(parseViewState(`?from=${T0}&to=${T0}`).to, null);
    assert.equal(parseViewState('?from=yesterday&to=today').from, null);
});

test('an RFC 3339 offset survives the + that URLSearchParams turns into a space', () => {
    // As typed into an address bar (unencoded +), and as a browser encodes it.
    const typed = parseViewState('?from=2026-09-28T14:03:11+02:00&to=2026-09-28T14:05:40+02:00');
    const encoded = parseViewState('?from=2026-09-28T14%3A03%3A11%2B02%3A00&to=2026-09-28T14%3A05%3A40%2B02%3A00');
    const utc = parseViewState('?from=2026-09-28T12:03:11Z&to=2026-09-28T12:05:40Z');
    assert.equal(typed.from, utc.from);
    assert.equal(encoded.from, utc.from);
    assert.equal(parseViewState('?from=2026-09-28T14:03:11+00:00&to=2026-09-28T14:05:40+00:00').from,
        parseViewState('?from=2026-09-28T14:03:11Z&to=2026-09-28T14:05:40Z').from);
});

test('duplicate keys: the first from/to wins; a stray empty anchor id is removable', () => {
    const st = parseViewState(`?from=${T0}&from=1&to=${T1}`);
    assert.equal(st.from, T0);
    // `anchor.=1` is never written but a link may carry it; it is ours to clear.
    const s = applyViewState('?anchor.=1&capture=x', { anchors: { '': null } });
    assert.equal(s, '?capture=x');
    // Sub-ms apart instants would be written equal; refused at write time.
    assert.equal(applyViewState('', { from: 1.0001, to: 1.0002 }), '');
});

test('time accepts raw, grid and the Aligned label; writes raw only', () => {
    assert.equal(parseViewState('?time=raw').time, 'raw');
    assert.equal(parseViewState('?time=grid').time, 'grid');
    assert.equal(parseViewState('?time=aligned').time, 'grid');
    assert.equal(parseViewState('?time=fast').time, null);
    assert.equal(applyViewState('', { time: 'grid' }), '');
    assert.equal(applyViewState('?time=raw', { time: 'grid' }), '');
    assert.equal(applyViewState('', { time: 'raw' }), '?time=raw');
});

test('gpu parses vendor:id at the first colon and drops an empty id', () => {
    assert.deepEqual(parseViewState('?gpu=nvidia:0').gpu, [{ vendor: 'nvidia', id: '0' }]);
    assert.deepEqual(parseViewState('?gpu=0').gpu, [{ vendor: null, id: '0' }]);
    assert.deepEqual(parseViewState('?gpu=a:b:c').gpu, [{ vendor: 'a', id: 'b:c' }]);
    assert.deepEqual(parseViewState('?gpu=nvidia:').gpu, []);
    assert.deepEqual(parseViewState('?gpu=').gpu, []);
});

test('anchors: integers and kind: forms; malformed dropped; any capture id', () => {
    assert.equal(parseAnchor('-1500'), -1500);
    assert.deepEqual(parseAnchor('kind:run_start'), { kind: 'run_start' });
    assert.equal(parseAnchor('abc'), null);
    assert.equal(parseAnchor('1.5'), null);
    assert.equal(parseAnchor('kind:'), null);
    assert.equal(formatAnchor(0), null);
    assert.equal(formatAnchor(-1500), '-1500');
    assert.equal(formatAnchor({ kind: 'run_start' }), 'kind:run_start');
    assert.equal(formatAnchor({ kind: '' }), null);
    const st = parseViewState('?anchor.baseline=kind:run_start&anchor.redis=2000&anchor.x=nope');
    assert.deepEqual(st.anchors, { baseline: { kind: 'run_start' }, redis: 2000 });
    const s = applyViewState('', { anchors: { experiment: { kind: 'deploy' } } });
    assert.equal(s, '?anchor.experiment=kind%3Adeploy');
    assert.deepEqual(parseViewState(s).anchors, { experiment: { kind: 'deploy' } });
});

test('unknown keys are preserved in place; owned keys are removed by null', () => {
    const start = '?capture=demo.parquet&from=2026-05-10T00:35:48.250Z&to=2026-05-10T00:38:49.000Z&compare=1';
    const s = applyViewState(start, { from: null, to: null });
    assert.equal(s, '?capture=demo.parquet&compare=1');
    const s2 = applyViewState(s, { node: 'n1' });
    assert.equal(s2, '?capture=demo.parquet&compare=1&node=n1');
    assert.deepEqual(parseViewState(s2).node, 'n1');
});

test('apply returns the input unchanged when nothing differs', () => {
    const start = '?capture=x&node=n1';
    assert.equal(applyViewState(start, { node: 'n1' }), start);
    assert.equal(applyViewState(start, {}), start);
    assert.equal(applyViewState('', { from: null }), '');
});

test('an invalid range patch clears both bounds', () => {
    const s = applyViewState(`?from=${T0}&to=${T1}`, { from: T1, to: T0 });
    assert.equal(s, '');
    assert.equal(applyViewState('', { from: T0, to: NaN }), '');
});

test('ownedKeysIn lists the module keys present, including anchor.*', () => {
    assert.deepEqual(
        ownedKeysIn('?capture=x&from=1&anchor.redis=2&gpu=0&gpu=1'),
        ['from', 'anchor.redis', 'gpu'],
    );
    assert.ok(VIEW_KEYS.includes('anchor.experiment'));
});

test('browser wrappers are inert without a window', () => {
    assert.deepEqual(readViewState(), parseViewState(''));
    assert.doesNotThrow(() => writeViewState({ node: 'x' }));
    assert.doesNotThrow(() => clearViewState());
});
