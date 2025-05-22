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