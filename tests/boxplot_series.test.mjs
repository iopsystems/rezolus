import test from 'node:test';
import assert from 'node:assert/strict';
import { buildBoxplotSeries, boxplotChartOption, buildEnvelopeLines, buildDivergenceBand } from '../src/viewer/assets/lib/charts/boxplot.js';

const sample = {
    metric: { __name__: 'memory_free' },
    t: [100, 101, 102],
    min: [1, 1, 1],
    lo: [2, 2, 2],
    median: [3, 3, 3],
    hi: [4, 4, 4],
    max: [5, 100, 5], // spike to 100 in the middle bucket's max
};

test('buildBoxplotSeries: emits 5 series (2 bands + 2 baselines + median)', () => {
    const out = buildBoxplotSeries(sample);
    assert.equal(out.length, 5);
    // last series is the median line
    const median = out[4];
    assert.equal(median.type, 'line');
    assert.equal(median.name, 'memory_free');
    assert.deepEqual(median.data.map((p) => p[1]), [3, 3, 3]);
    // timestamps are converted seconds -> ms
    assert.deepEqual(median.data.map((p) => p[0]), [100000, 101000, 102000]);
});

test('buildBoxplotSeries: outer band fill carries (max - min), preserving the spike', () => {
    const out = buildBoxplotSeries(sample);
    const outerBase = out[0];
    const outerFill = out[1];
    // baseline is min, in a shared stack; fill has areaStyle
    assert.deepEqual(outerBase.data.map((p) => p[1]), [1, 1, 1]);
    assert.equal(outerBase.stack, outerFill.stack, 'band base+fill share a stack');
    assert.ok(outerBase.lineStyle.opacity === 0, 'baseline invisible');
    assert.ok(outerFill.areaStyle && outerFill.areaStyle.color, 'fill has area color');
    // (max - min): [4, 99, 4] — stacked on the min baseline this reaches
    // [5, 100, 5], so the spike to 100 survives as the band top.
    assert.deepEqual(outerFill.data.map((p) => p[1]), [4, 99, 4]);
    // stacked top = base + fill = max
    const stackedTop = outerFill.data.map((p, i) => outerBase.data[i][1] + p[1]);
    assert.deepEqual(stackedTop, [5, 100, 5]);
    assert.equal(Math.max(...stackedTop), 100, 'spike preserved in the rendered band');
});

test('buildBoxplotSeries: inner band fill carries (hi - lo)', () => {
    const out = buildBoxplotSeries(sample);
    const innerBase = out[2];
    const innerFill = out[3];
    assert.deepEqual(innerBase.data.map((p) => p[1]), [2, 2, 2]);
    assert.deepEqual(innerFill.data.map((p) => p[1]), [2, 2, 2]); // hi-lo = 4-2
    assert.equal(innerBase.stack, innerFill.stack);
    assert.notEqual(innerBase.stack, out[0].stack, 'inner and outer are separate stacks');
});

test('buildBoxplotSeries: median line renders above the bands', () => {
    const out = buildBoxplotSeries(sample);
    const median = out[4];
    const bandZ = Math.max(out[0].z, out[1].z, out[2].z, out[3].z);
    assert.ok(median.z > bandZ, 'median z above band z');
});

test('buildBoxplotSeries: outerOnly drops the inner band (3 series)', () => {
    const out = buildBoxplotSeries(sample, { outerOnly: true });
    assert.equal(out.length, 3, 'outer base + outer fill + median');
    const median = out[2];
    assert.equal(median.type, 'line');
    assert.deepEqual(median.data.map((p) => p[1]), [3, 3, 3]);
    // outer fill still carries (max - min), so the spike survives
    assert.deepEqual(out[1].data.map((p) => p[1]), [4, 99, 4]);
});

test('boxplotChartOption: series sharing __name__ get distinct stacks (no collision)', () => {
    // A per-CPU counter: 3 series all named cpu_cycles. Before the fix these
    // shared a stack string and echarts summed their bands into one garble.
    const mk = (id) => ({
        metric: { __name__: 'cpu_cycles', id: String(id) },
        t: [1, 2], min: [1, 1], lo: [1, 1], median: [1, 1], hi: [1, 1], max: [1, 1],
    });
    const opt = boxplotChartOption({ series: [mk(0), mk(1), mk(2)] });
    const outerStacks = opt.series.map((s) => s.stack).filter((s) => s && s.endsWith('outer'));
    assert.equal(new Set(outerStacks).size, 3, 'each of the 3 series has its own outer stack');
    assert.ok(opt.legend, 'multi-series charts get a legend');
    // labels disambiguate by the distinguishing label, not the shared name
    assert.deepEqual(opt.legend.data, ['cpu_cycles{id=0}', 'cpu_cycles{id=1}', 'cpu_cycles{id=2}']);
});

test('buildDivergenceBand: neutral fill spans the gap; collapses to zero on agreement', () => {
    // i=0 medians equal (band 0), i=1 diverge by 60, i=2 by 20
    const band = { t: [10, 11, 12], lower: [100, 100, 90], upper: [100, 160, 110] };
    const out = buildDivergenceBand(band);
    assert.equal(out.length, 2, 'invisible base + filled span');
    const [base, fill] = out;
    assert.deepEqual(base.data.map((p) => p[1]), [100, 100, 90], 'base rides the lower median');
    assert.deepEqual(fill.data.map((p) => p[1]), [0, 60, 20], 'fill = upper - lower');
    assert.equal(fill.data[0][1], 0, 'agreement → zero-height band (reads as a line)');
    assert.equal(base.stack, fill.stack, 'base + fill share a stack');
    assert.ok(fill.areaStyle && fill.areaStyle.color, 'neutral area fill');
    assert.ok(!base.areaStyle, 'base is invisible');
    assert.equal(base.data[0][0], 10000, 'seconds → ms');
    assert.ok(base.silent && fill.silent, 'band is non-interactive');
});

test('buildEnvelopeLines: median line + faint min/max lines, no fill (A/B compare)', () => {
    const s = {
        metric: { __name__: 'cpu' },
        t: [10, 11], min: [1, 1], lo: [2, 2], median: [3, 5], hi: [4, 4], max: [9, 9],
    };
    const out = buildEnvelopeLines(s, { name: 'baseline', color: '#abc' });
    assert.equal(out.length, 3, 'min line + max line + median line');
    // no filled areas — the whole point of the line-envelope
    assert.ok(out.every((series) => !series.areaStyle), 'no areaStyle on any series');
    // all in the given color
    assert.ok(out.every((series) => series.lineStyle.color === '#abc'));
    // the named median carries itemStyle.color too, so the legend/tooltip
    // marker matches the drawn line (echarts reads the marker from itemStyle,
    // not lineStyle — otherwise it falls back to its default palette).
    assert.equal(out[2].itemStyle.color, '#abc', 'median marker color matches line');
    // min/max are faint + silent; median is full-weight + named
    const [minL, maxL, med] = out;
    assert.deepEqual(minL.data.map((p) => p[1]), [1, 1]);
    assert.deepEqual(maxL.data.map((p) => p[1]), [9, 9]);
    assert.ok(minL.silent && maxL.silent, 'bound lines are silent');
    assert.equal(minL.lineStyle.opacity, 0.4);
    assert.equal(med.name, 'baseline');
    assert.deepEqual(med.data.map((p) => p[1]), [3, 5], 'median line carries the medians');
    assert.equal(med.data[0][0], 10 * 1000, 't converted seconds -> ms');
});
