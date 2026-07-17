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

// Median of the deltas (ms) — the data-derived "typical" cadence. Used as the
// deviation baseline only when the recording doesn't declare a sampling
// interval; robust to outlier late samples (unlike the mean).
export const medianMs = (dMs) => {
    if (!dMs.length) return 0;
    const sorted = [...dMs].sort((a, b) => a - b);
    const mid = sorted.length >> 1;
    return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
};

// Resolve the deviation baseline. PREFER the recording's DECLARED interval (the
// intended cadence) so a program that consistently runs behind intent shows a
// steady non-zero offset; fall back to the data-derived median when no interval
// is declared. Deliberately does NOT use reader.interval(): metriken-query
// defaults a missing sampling_interval_ms to 1000ms, which would silently
// mis-nominal a sub-second foreign ("simple capture") recording.
export const nominalMsFor = (dMs, declaredMs) =>
    (declaredMs != null && declaredMs > 0) ? declaredMs : medianMs(dMs);

// `data` must match what charts/line.js (configureLineChart) actually
// consumes: parallel arrays `[timeData, valueData]`, timeData in
// SECONDS (line.js multiplies by 1000 for echarts) — not an array of
// [x,y] pairs. `format.unit_system: 'time'` (crates/dashboard/src/plot.rs
// FormatConfig) has nanoseconds as its base unit, so the ms values from
// deltasMs/toDeviation are scaled back up for display.
//
// `nominalMs` is the DECLARED sampling interval (ms) when the recording carries
// one; pass null/undefined to derive the baseline from the data (median).
//
// tsNs values exceed Number.MAX_SAFE_INTEGER, so JSON.parse quantizes them to
// ~256ns at current epoch — a sub-256ns fidelity ceiling, negligible for
// ms-scale jitter. If sub-µs fidelity is ever needed, recover it via
// offset-encoding (base + small deltas) on both backends.
export const jitterSpec = (tsNs, { mode = 'absolute', nominalMs = null } = {}) => {
    const xs = tsNs.slice(1).map((ns) => ns / NS_PER_S);
    const dMs = deltasMs(tsNs);
    const ysMs = mode === 'deviation'
        ? toDeviation(dMs, nominalMsFor(dMs, nominalMs))
        : dMs;
    const ys = ysMs.map((ms) => ms * NS_PER_MS);
    return {
        promql_query: null,
        data: [xs, ys],
        opts: {
            id: TIMESTAMP_JITTER_CHART_ID,
            title: mode === 'deviation'
                ? 'Sampling jitter — deviation from nominal (ms)'
                : 'Inter-sample interval (ms)',
            description: 'Delta between consecutive sample timestamps.',
            type: 'gauge',
            style: 'line',
            format: { unit_system: 'time' },
        },
    };
};
