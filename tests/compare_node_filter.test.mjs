// Regression tests for compare-mode topology label injection.
//
// Pre-fix: buildEffectiveQuery(crossCapture=true) skipped BOTH node and
// instance label injection on the experiment side, so a multi-node
// rezolus chart fanned out across all nodes in the experiment capture
// while the baseline side was correctly pinned to the selected node.
// Asymmetric and confusing.
//
// Post-fix: node injection always applies (so the user's pinned-node
// selection in the top nav targets both captures consistently).
// Instance injection still only applies on the baseline path —
// service KPIs are composable by design (e.g. sum across instances)
// so an unfiltered aggregate is the correct A/B baseline.

import test from 'node:test';
import assert from 'node:assert/strict';
import {
    buildEffectiveQuery,
    setSelectedNode,
    setSelectedInstance,
} from '../src/viewer/assets/lib/data.js';

const counterPlot = (query) => ({
    promql_query: query,
    opts: { type: 'delta_counter' },
});

test('node label is injected on the cross-capture (experiment) path for non-service routes', () => {
    setSelectedNode('web01');
    try {
        const q = buildEffectiveQuery(
            counterPlot('sum(irate(cpu_usage[5m]))'),
            { sectionRoute: '/cpu', crossCapture: true },
        );
        assert.match(q, /node="web01"/);
    } finally {
        setSelectedNode(null);
    }
});

test('node label is injected on the baseline (non-cross-capture) path too', () => {
    setSelectedNode('web01');
    try {
        const q = buildEffectiveQuery(
            counterPlot('sum(irate(cpu_usage[5m]))'),
            { sectionRoute: '/cpu', crossCapture: false },
        );
        assert.match(q, /node="web01"/);
    } finally {
        setSelectedNode(null);
    }
});

test('node label is NOT injected for /service/* routes (service queries are intentionally node-agnostic)', () => {
    setSelectedNode('web01');
    try {
        const q = buildEffectiveQuery(
            counterPlot('sum(irate(sglang_generation_tokens_total[5s]))'),
            { sectionRoute: '/service/sglang', crossCapture: true },
        );
        assert.doesNotMatch(q, /node="/);
    } finally {
        setSelectedNode(null);
    }
});

test('instance label is injected on baseline path but skipped on cross-capture path', () => {
    setSelectedInstance('sglang', 'primary');
    try {
        const baseline = buildEffectiveQuery(
            counterPlot('sum(irate(sglang_generation_tokens_total[5s]))'),
            { sectionRoute: '/service/sglang', serviceName: 'sglang', crossCapture: false },
        );
        assert.match(baseline, /instance="primary"/, 'baseline side should pin instance');

        const experiment = buildEffectiveQuery(
            counterPlot('sum(irate(sglang_generation_tokens_total[5s]))'),
            { sectionRoute: '/service/sglang', serviceName: 'sglang', crossCapture: true },
        );
        assert.doesNotMatch(experiment, /instance="/, 'experiment side aggregates across instances');
    } finally {
        setSelectedInstance('sglang', null);
    }
});

test('with no selected node, queries pass through unchanged on either path', () => {
    setSelectedNode(null);
    const original = 'sum(irate(cpu_usage[5m]))';
    const baseline = buildEffectiveQuery(
        counterPlot(original),
        { sectionRoute: '/cpu', crossCapture: false },
    );
    const experiment = buildEffectiveQuery(
        counterPlot(original),
        { sectionRoute: '/cpu', crossCapture: true },
    );
    assert.equal(baseline, original);
    assert.equal(experiment, original);
});
