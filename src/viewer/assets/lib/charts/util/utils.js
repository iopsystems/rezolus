import { getStepOverride } from '../../data.js';

/**
 * Insert a null data point at each gap in the time series so ECharts
 * breaks the line instead of drawing a misleading segment across the gap.
 *
 * @param {Array<[number, number]>} zippedData - [[timestampMs, value], ...]
 * @param {number} intervalSec - canonical sampling interval in seconds
 * @returns {Array<[number, number|null]>} data with null sentinels inserted at gaps
 */
export function insertGapNulls(zippedData, intervalSec) {
    if (!zippedData || zippedData.length < 2 || !intervalSec || intervalSec <= 0) {
        return zippedData;
    }
    // Items may be plain arrays or echarts object-form { value: [...], ... }
    const ts = (item) => Array.isArray(item) ? item[0] : item.value[0];
    // When a coarser step is selected, data points are spaced at the step
    // interval rather than the base sampling interval.  Use the effective
    // interval so legitimate step-spaced gaps don't break the line.
    const effectiveSec = Math.max(intervalSec, getStepOverride() || 0);
    const thresholdMs = effectiveSec * 1500; // 1.5x interval in ms
    const result = [zippedData[0]];
    for (let i = 1; i < zippedData.length; i++) {
        if (ts(zippedData[i]) - ts(zippedData[i - 1]) > thresholdMs) {
            result.push([ts(zippedData[i - 1]) + effectiveSec * 1000, null]);
        }
        result.push(zippedData[i]);
    }
    return result;
}

/**
 * The gap interval and echarts `step` mode for one series, from its own point
 * spacing. A series whose median positive step is at least 1.5 times the
 * effective step (the interval, or a coarser Granularity override) is gapped
 * at that median spacing and drawn unstepped; any other series keeps the
 * interval and `step: 'start'`. The first case covers decimated buckets, such
 * as a compare chart's baseline beside an experiment at the recording's
 * interval, and also a native series that is sparser than the interval: a
 * slow sampler (5 s readings in a 1 s recording) draws as a line between its
 * readings, and a series missing every other row draws across those holes.
 *
 * @param {number[]} timeSec - point timestamps, seconds
 * @param {number} intervalSec - the recording interval, seconds
 * @returns {{ gapInterval: number, step: string|false }}
 */
export function seriesGapGeometry(timeSec, intervalSec) {
    const steps = [];
    for (let i = 1; i < (timeSec?.length || 0); i++) {
        const d = timeSec[i] - timeSec[i - 1];
        if (d > 0) steps.push(d);
    }
    steps.sort((a, b) => a - b);
    const median = steps.length ? steps[Math.floor(steps.length / 2)] : 0;
    const effective = Math.max(intervalSec || 0, getStepOverride() || 0);
    if (effective > 0 && median >= effective * 1.5) {
        return { gapInterval: median, step: false };
    }
    return { gapInterval: intervalSec, step: 'start' };
}

/**
 * Clamp a value to range bounds.
 * @param {number} v - the raw value
 * @param {{ min?: number, max?: number }} [range] - optional range bounds
 * @returns {[number, number|null]} [clamped, rawOrNull] — second element is the
 *   original value when clamping occurred, null otherwise.
 */
export function clampToRange(v, range) {
    if (!range) return [v, null];
    let clamped = v;
    if (range.max != null && clamped > range.max) clamped = range.max;
    if (range.min != null && clamped < range.min) clamped = range.min;
    return [clamped, clamped !== v ? v : null];
}

/**
 * Format a timestamp for display
 * @param {number} timestamp - Unix timestamp in milliseconds
 * @returns {string} Formatted date/time string
 */
export function formatDateTime(timestamp) {
    const date = new Date(timestamp);
    const mainString = `${date.toISOString().slice(0, 10)} ${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}:${String(date.getSeconds()).padStart(2, '0')}`;
    const msString = date.getMilliseconds() === 0 ? '' : `<span style="font-size: .8em;">.${String(date.getMilliseconds()).padStart(3, '0')}</span>`;
    return `${mainString}${msString}`;
}