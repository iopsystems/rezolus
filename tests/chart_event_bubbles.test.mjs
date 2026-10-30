import test from 'node:test';
import assert from 'node:assert/strict';

// A chart draws one description tag per event the host shows on it, in a
// layer above the plot, and removes the layer when the host shows none. A
// tag opens the host's info popover when clicked; without one a click does
// nothing and the tag has a default cursor. A shim stands in for the DOM and
// the echart.
globalThis.document = {
    documentElement: { getAttribute: () => 'dark' },
    createElement: () => fakeElement(),
};
globalThis.getComputedStyle = () => ({ getPropertyValue: () => '' });

const { Chart, ChartsState } = await import('../src/viewer/assets/lib/charts/chart.js');
const { setChartHost, resetChartHost } = await import('../src/viewer/assets/lib/charts/host.js');

function fakeElement() {
    const el = {
        className: '',
        style: {},
        children: [],
        listeners: {},
        textContent: '',
        appendChild: (c) => { el.children.push(c); c.parent = el; return c; },
        remove: () => {
            if (el.parent) el.parent.children = el.parent.children.filter((c) => c !== el);
            el.removed = true;
        },
        addEventListener: (type, fn) => { el.listeners[type] = fn; },
        getBoundingClientRect: () => ({ left: 0, top: 0, width: 10, height: 10 }),
    };
    return el;
}

function chartWithFakeEchart() {
    const chart = new Chart({ spec: { opts: { id: 'cpu' } }, chartsState: new ChartsState(), interval: 1 });
    chart.domNode = fakeElement();
    chart.echart = {
        getModel: () => ({
            getComponent: () => ({ coordinateSystem: { getRect: () => ({ x: 0, y: 20, width: 500, height: 200 }) } }),
        }),
        convertToPixel: () => 100,
    };
    return chart;
}

const EVENT = { timestamp: 1_000_000_000, description: 'deploy' };
const layers = (chart) => chart.domNode.children.filter((c) => c.className === 'event-bubble-layer');

test('a chart draws one tag per event the host shows, and a click opens its popover', () => {
    const opened = [];
    setChartHost({ eventsForChart: () => [EVENT], openEventInfo: (args) => opened.push(args) });
    try {
        const chart = chartWithFakeEchart();
        chart._renderEventBubbles();
        const [layer] = layers(chart);
        assert.ok(layer, 'the layer was added');
        const tags = layer.children.filter((c) => c.className === 'event-bubble-tag');
        assert.equal(tags.length, 1);
        assert.equal(tags[0].textContent, 'deploy');
        assert.notEqual(tags[0].style.cursor, 'default');
        let stopped = false;
        tags[0].listeners.click({ stopPropagation() { stopped = true; } });
        assert.equal(stopped, true);
        assert.equal(opened.length, 1);
        assert.equal(opened[0].event, EVENT);
        // The fake tag's rect is 10x10 at the origin: anchored at its top centre.
        assert.deepEqual(opened[0].anchorPoint, { x: 5, y: 0 });
    } finally {
        resetChartHost();
    }
});

test('the layer is removed when the host shows no events', () => {
    try {
        setChartHost({ eventsForChart: () => [EVENT] });
        const chart = chartWithFakeEchart();
        chart._renderEventBubbles();
        assert.equal(layers(chart).length, 1);
        resetChartHost();
        chart._renderEventBubbles();
        assert.equal(layers(chart).length, 0);
        assert.equal(chart._eventBubbleLayer, null);
    } finally {
        resetChartHost();
    }
});

test('without openEventInfo a tag has no click handler and a default cursor', () => {
    setChartHost({ eventsForChart: () => [EVENT] });
    try {
        const chart = chartWithFakeEchart();
        chart._renderEventBubbles();
        const tag = layers(chart)[0].children.find((c) => c.className === 'event-bubble-tag');
        assert.equal(tag.listeners.click, undefined);
        assert.equal(tag.style.cursor, 'default');
    } finally {
        resetChartHost();
    }
});

// "+ Add Event" on a frozen tooltip needs both eventEditingAllowed() and an
// openEventForm from the host.
const { buildFreezeFooterContent } = await import('../src/viewer/assets/lib/charts/base.js');
const frozen = { _tooltipFrozen: true, chartId: 'x' };

test('the frozen tooltip offers "+ Add Event" only when the host allows it and has a form', () => {
    try {
        resetChartHost();
        assert.ok(!buildFreezeFooterContent(frozen).includes('tooltip-add-event'));
        setChartHost({ eventEditingAllowed: () => true });
        assert.ok(!buildFreezeFooterContent(frozen).includes('tooltip-add-event'));
        setChartHost({ openEventForm: () => {} });
        assert.ok(buildFreezeFooterContent(frozen).includes('tooltip-add-event'));
        setChartHost({ eventEditingAllowed: () => false });
        assert.ok(!buildFreezeFooterContent(frozen).includes('tooltip-add-event'));
    } finally {
        resetChartHost();
    }
});

test('the add-event form opens only when the host allows editing', () => {
    const opened = [];
    try {
        const chart = chartWithFakeEchart();
        chart._frozenAxisNs = 42;
        setChartHost({ openEventForm: (args) => opened.push(args) });
        chart._openAddEventForm({});
        assert.equal(opened.length, 0);
        setChartHost({ eventEditingAllowed: () => true });
        chart._openAddEventForm({});
        assert.equal(opened.length, 1);
        assert.equal(opened[0].prefill.chart_id, 'cpu');
        assert.equal(opened[0].prefill.timestamp_ns, 42);
    } finally {
        resetChartHost();
    }
});
