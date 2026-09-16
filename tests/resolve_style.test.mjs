// `resolveStyle` decides the chart type for every gauge and delta_counter plot
// in the viewer — CPU, network, disk, cgroup, GPU alike — from the shape of the
// query result. It had no tests, and a GPU-motivated change to it silently
// flipped single-entity charts across every subsystem from a line to a one-row
// heatmap (a one-row heatmap encodes value as colour and drops the y-axis, so
// it is strictly less readable than the line it replaced).
//
// These pin the inference so a change aimed at one subsystem cannot quietly
// reshape the others. A plot that wants a specific style declares it via
// PlotOpts::style, which data.js honours ahead of this function.
import test from 'node:test';
import assert from 'node:assert/strict';
import { resolveStyle } from '../src/viewer/assets/lib/charts/metric_types.js';

const result = (...metrics) => ({ data: { result: metrics.map((m) => ({ metric: m })) } });

test('a single series is a line, even when it carries an id', () => {
    // The regression case: a 1-vCPU VM, a single NIC, a single disk, one
    // cgroup, or a one-GPU host must keep its line chart.
    assert.equal(resolveStyle('gauge', undefined, result({ id: '0' })), 'line');
    assert.equal(resolveStyle('gauge', undefined, result({})), 'line');
});

test('several id-bearing series are a per-entity heatmap', () => {
    assert.equal(resolveStyle('gauge', undefined, result({ id: '0' }, { id: '1' })), 'heatmap');
});

test('several series without an id are a multi-line chart', () => {
    assert.equal(
        resolveStyle('gauge', undefined, result({ state: 'used' }, { state: 'free' })),
        'multi',
    );
});

test('an empty or absent result falls back to a line', () => {
    assert.equal(resolveStyle('gauge', undefined, { data: { result: [] } }), 'line');
    assert.equal(resolveStyle('gauge', undefined, undefined), 'line');
});

test('histogram subtypes are resolved by subtype, not result shape', () => {
    assert.equal(resolveStyle('histogram', 'buckets'), 'histogram_heatmap');
    assert.equal(resolveStyle('histogram', 'quantile_heatmap'), 'quantile_heatmap');
    assert.equal(resolveStyle('histogram', 'percentiles'), 'scatter');
});
