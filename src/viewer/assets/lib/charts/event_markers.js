// Build an ECharts `markLine` config that renders each event as a
// subtle vertical dashed hairline at its timestamp. Returns null when
// there's nothing to render so callers can branch trivially.
//
// The description is NOT rendered here — it lives in an HTML bubble
// above the plot grid (see chart.js::_renderEventBubbles) so it can't
// overlap the on-canvas data tooltip and stays clickable.
//
// Pure module — no chart instance, no DOM. The caller owns the
// "merge into series[0]" decision because that depends on the chart's
// current configured options.

export const EVENT_MARKER_COLOR = '#a85d23';

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
        // Not interactive — the HTML bubble owns hover/click; the line
        // is just a visual locator.
        silent: true,
        symbol: 'none',
        data,
        lineStyle: {
            color: EVENT_MARKER_COLOR,
            type: 'dashed',
            width: 1,
            opacity: 0.7,
        },
        label: { show: false },
    };
}
