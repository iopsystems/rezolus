import test from 'node:test';
import assert from 'node:assert/strict';
import {
    buildEffectiveQuery,
    setCombinedAB,
} from '../src/viewer/assets/lib/data.js';

// Module-level state — reset between tests to keep them order-independent.
test.beforeEach(() => setCombinedAB(false));

test('combined-AB off: no container label injected', () => {
    setCombinedAB(false);
    const q = buildEffectiveQuery({ promql_query: 'cpu_usage', opts: {} }, {});
    assert.equal(q, 'cpu_usage');
});

test('combined-AB on: baseline capture gets container="baseline"', () => {
    setCombinedAB(true);
    const q = buildEffectiveQuery({ promql_query: 'cpu_usage', opts: {} }, {});
    assert.equal(q, 'cpu_usage{container="baseline"}');
});

test('combined-AB on: crossCapture flips to container="experiment"', () => {
    setCombinedAB(true);
    const q = buildEffectiveQuery(
        { promql_query: 'cpu_usage', opts: {} },
        { crossCapture: true },
    );
    assert.equal(q, 'cpu_usage{container="experiment"}');
});

test('combined-AB on: injection composes with existing label selector', () => {
    setCombinedAB(true);
    const q = buildEffectiveQuery(
        { promql_query: 'cpu_usage{cpu="0"}', opts: {} },
        {},
    );
    assert.match(q, /container="baseline"/);
    assert.match(q, /cpu="0"/);
});

test('combined-AB toggling: setCombinedAB(false) stops injecting', () => {
    setCombinedAB(true);
    const on = buildEffectiveQuery({ promql_query: 'metric', opts: {} }, {});
    setCombinedAB(false);
    const off = buildEffectiveQuery({ promql_query: 'metric', opts: {} }, {});
    assert.equal(on, 'metric{container="baseline"}');
    assert.equal(off, 'metric');
});
