// Build an ECharts `markLine` config that renders each event as a
// vertical dashed line at its timestamp. Returns null when there's
// nothing to render so callers can branch trivially.
//
// Pure module — no chart instance, no DOM. The caller owns the
// "merge into series[0]" decision because that depends on the chart's
// current configured options.

const EVENT_MARKER_COLOR = '#cc6600';

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
        // Hidden until hover; emphasis pins it at the top end of the line
        // (above the plot grid) and renders horizontally so the
        // description stays legible regardless of line direction.
        label: {
            show: false,
            position: 'end',
            distance: 4,
            rotate: 0,
            align: 'center',
            verticalAlign: 'bottom',
            formatter: '{b}',
            color: '#fff',
            backgroundColor: EVENT_MARKER_COLOR,
            padding: [2, 6],
            borderRadius: 3,
            fontSize: 11,
        },
        emphasis: {
            label: { show: true },
            lineStyle: { width: 2, opacity: 1 },
        },
    };
}
