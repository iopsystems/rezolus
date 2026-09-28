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

// Identity ns -> ms conversion. Compare-mode charts draw a relative axis,
// so the caller passes its own `toAxisMs` that subtracts the capture's
// anchor; every other chart uses this default.
const absoluteMs = (ns) => ns / 1_000_000;

// A range event is one with a finite, positive `duration_ns`. A point
// event has none (or zero), and only gets the hairline.
export const isRangeEvent = (e) =>
    e != null && Number.isFinite(e.duration_ns) && e.duration_ns > 0;

// Humanize a duration in ns for the bubble tag: `40s`, `2m30s`, `1h5m`,
// `250ms`. Sub-second durations show ms; anything else drops the
// sub-second part, which the bubble has no room for.
export function formatDuration(ns) {
    if (!Number.isFinite(ns) || ns <= 0) return '';
    const ms = Math.round(ns / 1_000_000);
    if (ms < 1000) return `${ms}ms`;
    const totalSec = Math.round(ms / 1000);
    const h = Math.floor(totalSec / 3600);
    const m = Math.floor((totalSec % 3600) / 60);
    const s = totalSec % 60;
    if (h > 0) return m > 0 ? `${h}h${m}m` : `${h}h`;
    if (m > 0) return s > 0 ? `${m}m${s}s` : `${m}m`;
    return `${s}s`;
}

export function buildMarkLine(events, toAxisMs = absoluteMs) {
    if (!Array.isArray(events) || events.length === 0) return null;
    const data = [];
    for (const e of events) {
        if (e == null || !Number.isFinite(e.timestamp)) continue;
        data.push({
            xAxis: toAxisMs(e.timestamp),
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

// The axis spans of the range events: one `{startMs, endMs, name}` per
// event with a positive `duration_ns`, in the chart's x-axis units. Point
// events are skipped: they are fully drawn by `buildMarkLine`. A range
// event gets BOTH, the hairline at its start (the locator the bubble
// anchors to) and a shaded band over the span.
//
// The band is drawn as an HTML overlay by chart.js::_renderEventBubbles,
// not as an echarts `markArea`: the viewer's heatmaps are `custom` series,
// and echarts does not lay a markArea out on them (it collapsed to the
// axis line in every configuration tried), while the pixel-positioned
// overlay the bubbles already use works on every chart type alike.
//
// Same contract as buildMarkLine: pure, returns null when there is
// nothing to draw.
export function buildRangeSpans(events, toAxisMs = absoluteMs) {
    if (!Array.isArray(events) || events.length === 0) return null;
    const spans = [];
    for (const e of events) {
        if (!isRangeEvent(e) || !Number.isFinite(e.timestamp)) continue;
        spans.push({
            startMs: toAxisMs(e.timestamp),
            endMs: toAxisMs(e.timestamp + e.duration_ns),
            name: e.description || '',
        });
    }
    return spans.length > 0 ? spans : null;
}
