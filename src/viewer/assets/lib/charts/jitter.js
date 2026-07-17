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

// Average of the deltas (ms) — the data-derived nominal cadence. The deviation
// baseline when the recording doesn't declare a usable interval. (Deviation from
// the mean is centered: the deviations sum to ~0.)
export const averageMs = (dMs) =>
    dMs.length ? dMs.reduce((a, b) => a + b, 0) / dMs.length : 0;

// A declared interval more than this factor above the achieved average is
// treated as bogus. A real target can't be SLOWER than the average you actually
// sample at (you can't beat your own timer), so declared >> average means the
// producer hardcoded a default (e.g. program5 v0.1.0 writes 1000ms while beating
// at ~100ms) rather than declaring its true cadence.
const MAX_DECLARED_RATIO = 2;

// Resolve the deviation baseline. PREFER the recording's DECLARED interval (the
// intended cadence) so a program consistently running behind intent shows a
// steady non-zero offset — but ONLY when it's plausible: at/below the achieved
// average (a real lag) rather than well above it (a bogus default). Otherwise,
// and when no interval is declared, fall back to the average of the actual
// intervals. Deliberately does NOT use reader.interval(): metriken-query
// defaults a missing sampling_interval_ms to 1000ms, which would silently
// mis-nominal a sub-second foreign ("simple capture") recording.
export const nominalMsFor = (dMs, declaredMs) => {
    const avg = averageMs(dMs);
    const plausible = declaredMs != null && declaredMs > 0
        && (avg <= 0 || declaredMs <= avg * MAX_DECLARED_RATIO);
    return plausible ? declaredMs : avg;
};

// `data` must match what charts/line.js (configureLineChart) actually
// consumes: parallel arrays `[timeData, valueData]`, timeData in
// SECONDS (line.js multiplies by 1000 for echarts) — not an array of
// [x,y] pairs. `format.unit_system: 'time'` (crates/dashboard/src/plot.rs
// FormatConfig) has nanoseconds as its base unit, so the ms values from
// deltasMs/toDeviation are scaled back up for display.
//
// `nominalMs` is the DECLARED sampling interval (ms) when the recording carries
// one; pass null/undefined to derive the baseline from the data (average). A
// declared interval implausibly slower than the data is ignored (see nominalMsFor).
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
