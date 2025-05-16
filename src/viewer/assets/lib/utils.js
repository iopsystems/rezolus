/**
 * Format a timestamp for display
 * @param {number} timestamp - Unix timestamp in milliseconds
 * @returns {string} Formatted date/time string
 */
export function formatDateTime(timestamp) {
    const date = new Date(timestamp);
    return `${date.toISOString().slice(0, 10)} ${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}:${String(date.getSeconds()).padStart(2, '0')}`;
}

/**
 * Helper function to check if a chart element is visible in the viewport
 * @param {HTMLElement} chartDom - Chart DOM element
 * @returns {boolean} True if chart is visible in viewport
 */
export function isChartVisible(chartDom) {
    if (!chartDom) return false;

    const rect = chartDom.getBoundingClientRect();
    const windowHeight = window.innerHeight || document.documentElement.clientHeight;
    const windowWidth = window.innerWidth || document.documentElement.clientWidth;

    // Consider charts partially in view to be visible
    return (
        rect.top <= windowHeight &&
        rect.bottom >= 0 &&
        rect.left <= windowWidth &&
        rect.right >= 0
    );
}
