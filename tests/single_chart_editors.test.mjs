// Unit tests for the SingleChartView inline editor helpers
// (`single_chart_fields.js`). Follows the pure-helper pattern from
// `query_explorer.test.mjs` — no jsdom, no mithril.

import test from 'node:test';
import assert from 'node:assert/strict';
import {
    UNIT_OPTIONS,
    buildFormatOverride,
    seedFieldsFromPlot,
    applyFieldsToSpec,
} from '../src/viewer/assets/lib/features/single_chart_fields.js';

test('UNIT_OPTIONS covers every formatter dispatch slot', () => {
    // Every option value must be a key the chart formatters understand
    // (see UNIT_SYSTEMS in charts/util/units.js). Empty string is the
    // "inherit" sentinel.
    const values = UNIT_OPTIONS.map(o => o.value);
    assert.deepEqual(values, [
        '', 'count', 'rate', 'time', 'bytes',
        'datarate', 'bitrate', 'percentage', 'frequency',
    ]);
});

test('buildFormatOverride returns undefined for the empty (inherit) sentinel', () => {
    assert.equal(buildFormatOverride(''), undefined);
    assert.equal(buildFormatOverride(null), undefined);
    assert.equal(buildFormatOverride(undefined), undefined);
});

test('buildFormatOverride builds {unit_system, precision} for a plain unit', () => {
    assert.deepEqual(buildFormatOverride('time'), { unit_system: 'time', precision: 2 });
    assert.deepEqual(buildFormatOverride('bytes'), { unit_system: 'bytes', precision: 2 });
});

test('buildFormatOverride clamps percentage to range 0..1', () => {
    assert.deepEqual(buildFormatOverride('percentage'), {
        unit_system: 'percentage',
        precision: 2,
        range: { min: 0, max: 1 },
    });
});

test('seedFieldsFromPlot pulls title/description/unit_system from opts', () => {
    const plot = {
        opts: {
            title: 'CPU busy %',
            description: 'fraction of time scheduled',
            format: { unit_system: 'percentage', precision: 2 },
        },
    };
    assert.deepEqual(seedFieldsFromPlot(plot), {
        title: 'CPU busy %',
        description: 'fraction of time scheduled',
        unitOverride: 'percentage',
    });
});

test('seedFieldsFromPlot defaults to empty strings when opts are missing', () => {
    assert.deepEqual(seedFieldsFromPlot({}), { title: '', description: '', unitOverride: '' });
    assert.deepEqual(seedFieldsFromPlot({ opts: {} }), { title: '', description: '', unitOverride: '' });
    assert.deepEqual(seedFieldsFromPlot(null), { title: '', description: '', unitOverride: '' });
});

test('applyFieldsToSpec mutates title/description without touching the base plot', () => {
    const base = {
        opts: { id: 'p1', title: 'orig', description: 'orig-desc', format: { unit_system: 'rate', precision: 2 } },
        data: [[1, 2, 3]],
    };
    const spec = applyFieldsToSpec(base, {
        title: 'edited',
        description: 'edited-desc',
        unitOverride: '',
    });
    assert.equal(spec.opts.title, 'edited');
    assert.equal(spec.opts.description, 'edited-desc');
    // Inherit-sentinel keeps the existing format.
    assert.deepEqual(spec.opts.format, { unit_system: 'rate', precision: 2 });
    assert.equal(spec.opts.id, 'p1');
    // Original is untouched.
    assert.equal(base.opts.title, 'orig');
    assert.equal(base.opts.description, 'orig-desc');
});

test('applyFieldsToSpec overrides the format when a unit is selected', () => {
    const base = {
        opts: { id: 'p1', title: 'x', format: { unit_system: 'rate', precision: 2 } },
    };
    const spec = applyFieldsToSpec(base, {
        title: 'x',
        description: '',
        unitOverride: 'bytes',
    });
    assert.deepEqual(spec.opts.format, { unit_system: 'bytes', precision: 2 });
});

test('applyFieldsToSpec clamps percentage override to range 0..1', () => {
    const base = { opts: { id: 'p1', title: 'x', format: { unit_system: 'rate', precision: 2 } } };
    const spec = applyFieldsToSpec(base, {
        title: 'x', description: '', unitOverride: 'percentage',
    });
    assert.deepEqual(spec.opts.format, {
        unit_system: 'percentage',
        precision: 2,
        range: { min: 0, max: 1 },
    });
});

test('applyFieldsToSpec preserves non-opts plot fields (data, etc.)', () => {
    const base = {
        opts: { id: 'p1', title: 't', format: undefined },
        data: [[1, 2], [3, 4]],
        custom: { foo: 'bar' },
    };
    const spec = applyFieldsToSpec(base, { title: 't', description: '', unitOverride: '' });
    assert.equal(spec.data, base.data);
    assert.equal(spec.custom, base.custom);
});
