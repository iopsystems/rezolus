// Jitter chart for a recording's raw sample timestamps. Pure transforms +
// a plot-spec builder, shared by the metric browser (inline) and the
// single-chart route (expand). x = wall-clock timestamp, y = inter-sample
// delta (absolute) or its deviation from the nominal interval.
export const TIMESTAMP_JITTER_CHART_ID = 'source-timestamp-jitter';

const NS_PER_MS = 1e6;
const NS_PER_S = 1e9;

export const deltasMs = (tsNs) => {
    const out = [];
    for (let i = 1; i < tsNs.length; i++) out.push((tsNs[i] - tsNs[i - 1]) / NS_PER_MS);
    return out;
};

export const toDeviation = (dMs, nominalMs) => dMs.map((d) => d - nominalMs);

// `data` must match what charts/line.js (configureLineChart) actually
// consumes: parallel arrays `[timeData, valueData]`, timeData in
// SECONDS (line.js multiplies by 1000 for echarts) — not an array of
// [x,y] pairs. `format.unit_system: 'time'` (crates/dashboard/src/plot.rs
// FormatConfig) has nanoseconds as its base unit, so the ms values from
// deltasMs/toDeviation are scaled back up for display.
export const jitterSpec = (tsNs, { mode = 'absolute', nominalMs = 0 } = {}) => {
    const xs = tsNs.slice(1).map((ns) => ns / NS_PER_S);
    const dMs = deltasMs(tsNs);
    const ysMs = mode === 'deviation' ? toDeviation(dMs, nominalMs) : dMs;
    const ys = ysMs.map((ms) => ms * NS_PER_MS);
    return {
        promql_query: null,
        data: [xs, ys],
        opts: {
            id: TIMESTAMP_JITTER_CHART_ID,
            title: mode === 'deviation'
                ? 'Sampling jitter (deviation from nominal)'
                : 'Sampling interval',
            description: 'Delta between consecutive sample timestamps.',
            type: 'gauge',
            style: 'line',
            format: { unit_system: 'time' },
        },
    };
};
