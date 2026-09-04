// N-way overlay: a compare-line chart draws one series per capture, each with a
// distinct color, not just baseline+experiment. Scope of the N-way arc is
// overlay charts; diff/side-by-side stay 2-only (covered elsewhere).
import test from 'node:test';
import assert from 'node:assert/strict';

// compare.js reads CSS custom properties at module load; stub the DOM so the
// palette falls back to its literals (same shim the other compare tests use).
globalThis.document = globalThis.document || { documentElement: {} };
globalThis.getComputedStyle = globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));
const { renderCompareChart, captureColorFor, BASELINE_COLOR, EXPERIMENT_COLOR } =
    await import('../src/viewer/assets/lib/charts/compare.js');
const { CAPTURE_BASELINE, CAPTURE_EXPERIMENT } = await import('../src/viewer/assets/lib/data.js');

const line = (id, alias, base) => ({
    id, alias,
    timeData: [100, 101, 102],
    valueData: [base, base + 1, base + 2],
});

test('a compare-line overlay draws one series per capture, N > 2', () => {
    const captures = [
        line(CAPTURE_BASELINE, 'redis', 10),
        line(CAPTURE_EXPERIMENT, 'valkey', 20),
        line('envoy', 'envoy', 30),
        line('nginx', 'nginx', 40),
    ];
    const out = renderCompareChart({
        spec: { opts: { style: 'line' } },
        captures,
        anchors: {},
        captureLabels: {},
    });
    assert.equal(out.kind, 'spec');
    const ms = out.spec.multiSeries;
    assert.equal(ms.length, 4, 'all four captures overlaid');
    assert.deepEqual(ms.map((s) => s.name), ['redis', 'valkey', 'envoy', 'nginx']);

    // baseline/experiment keep their signature colors; extras come from the
    // palette; every color is distinct so the arms are tellable apart.
    assert.equal(ms[0].color, BASELINE_COLOR);
    assert.equal(ms[1].color, EXPERIMENT_COLOR);
    assert.equal(ms[2].color, captureColorFor('envoy', 0));
    assert.equal(ms[3].color, captureColorFor('nginx', 1));
    assert.equal(new Set(ms.map((s) => s.color)).size, 4, 'distinct colors');

    // No divergence band for N > 2 (it is a two-capture concept).
    assert.equal(out.spec.divergenceBand, null);
});

test('the two-capture overlay is unchanged: signature colors, divergence band', () => {
    const captures = [line(CAPTURE_BASELINE, 'redis', 10), line(CAPTURE_EXPERIMENT, 'valkey', 20)];
    const out = renderCompareChart({
        spec: { opts: { style: 'line' } },
        captures,
        anchors: {},
        captureLabels: {},
    });
    const ms = out.spec.multiSeries;
    assert.equal(ms.length, 2);
    assert.equal(ms[0].color, BASELINE_COLOR);
    assert.equal(ms[1].color, EXPERIMENT_COLOR);
    // The two medians sit on a coincident grid → a divergence band is present.
    assert.ok(out.spec.divergenceBand, 'two captures get a divergence band');
});

test('a lone anchor does not overlay (renders baseline-only upstream)', () => {
    const out = renderCompareChart({
        spec: { opts: { style: 'line' } },
        captures: [line(CAPTURE_BASELINE, 'redis', 10)],
        anchors: {},
        captureLabels: {},
    });
    assert.equal(out, false);
});
