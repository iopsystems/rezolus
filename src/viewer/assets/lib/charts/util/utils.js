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
    const thresholdMs = intervalSec * 1500; // 1.5x interval in ms
    const result = [zippedData[0]];
    for (let i = 1; i < zippedData.length; i++) {
        if (zippedData[i][0] - zippedData[i - 1][0] > thresholdMs) {
            result.push([zippedData[i - 1][0] + intervalSec * 1000, null]);
        }
        result.push(zippedData[i]);
    }
    return result;
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