// Build an ECharts `markLine` config that renders each event as a
// vertical dashed line at its timestamp. Returns null when there's
// nothing to render so callers can branch trivially.
//
// Pure module — no chart instance, no DOM. The caller owns the
// "merge into series[0]" decision because that depends on the chart's
// current configured options.
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
            color: '#1f77b4',  // fallback; CSS var --accent read at runtime in browser
            type: 'dashed',
            width: 1,
            opacity: 0.7,
        },
        label: {
            show: false,
        },
        tooltip: {
            show: true,
            formatter: (p) => p.name,
        },
    };
}
