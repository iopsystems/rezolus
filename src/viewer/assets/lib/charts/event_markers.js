// Build an ECharts `markLine` config that renders each event as a
// vertical dashed line at its timestamp. Returns null when there's
// nothing to render so callers can branch trivially.
//
// Pure module — no chart instance, no DOM. The caller owns the
// "merge into series[0]" decision because that depends on the chart's
// current configured options.

const EVENT_MARKER_COLOR = '#0d8b8b';

export function buildMarkLine(events) {
    if (!Array.isArray(events) || events.length === 0) return null;
    const data = [];
    for (const e of events) {
        if (e == null || !Number.isFinite(e.timestamp)) continue;
        data.push({
            xAxis: e.timestamp / 1_000_000,  // ns -> ms
            name: e.description || '',
        });
    }
    if (data.length === 0) return null;
    return {
        silent: false,
        symbol: 'none',
        data,
        lineStyle: {
            color: EVENT_MARKER_COLOR,
            type: 'dashed',
            width: 1,
            opacity: 0.85,
        },
        // Inline label is hidden until hover; emphasis flips it on so
        // the description appears next to the line under the cursor.
        label: {
            show: false,
            position: 'insideEndTop',
            formatter: '{b}',
            color: EVENT_MARKER_COLOR,
            fontSize: 11,
        },
        emphasis: {
            label: { show: true },
            lineStyle: { width: 2, opacity: 1 },
        },
    };
}
