// utils.js - Common utility functions for chart rendering with fixed time axis handling

/**
 * Format a date for display with different formats based on context
 * @param {number} timestamp - Unix timestamp in seconds
 * @param {string} format - Format type: 'time', 'short', 'full', or 'axis'
 * @returns {string} Formatted date/time string
 */
export function formatDateTime(timestamp, format = 'time') {
    const date = new Date(timestamp * 1000);

    if (format === 'time') {
        // Simple time format HH:MM:SS
        return `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}:${String(date.getSeconds()).padStart(2, '0')}`;
    } else if (format === 'short') {
        // Return HH:MM format for compact display
        return `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`;
    } else if (format === 'axis') {
        // For axis labels, forward to formatTimeAxisLabel
        return formatTimeAxisLabel('', -1, [timestamp]);
    } else {
        // Full format with date
        return `${date.toISOString().slice(0, 10)} ${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}:${String(date.getSeconds()).padStart(2, '0')}`;
    }
}

/**
 * Enhanced formatter function for time axis labels that doesn't rely on index
 * @param {string} value - Formatted time value (unused)
 * @param {number} index - Index in the data array (unused in fixed version)
 * @param {Array} timeData - Original timestamp array
 * @returns {string} Formatted time label
 */
export function formatTimeAxisLabel(value, index, timeData) {
    // In the fixed version, we ignore index and use the actual timestamp directly
    if (!timeData || timeData.length === 0) return value;

    // For the new approach, we expect timeData to contain the specific timestamp
    // for this label, not the entire array of timestamps
    const timestamp = index >= 0 && index < timeData.length ? timeData[index] : timeData[0];
    const date = new Date(timestamp * 1000);

    const seconds = date.getSeconds();
    const minutes = date.getMinutes();
    const hours = date.getHours();

    // On the hour boundary
    if (seconds === 0 && minutes === 0) {
        return `${String(hours).padStart(2, '0')}:00`;
    }
    // On the minute boundary
    else if (seconds === 0) {
        return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}`;
    }
    // On a nice 30-second boundary
    else if (seconds % 30 === 0) {
        return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
    }
    // On a nice 15-second boundary
    else if (seconds % 15 === 0) {
        return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
    }
    // On a nice 5-second boundary
    else if (seconds % 5 === 0) {
        return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
    }
    // Default case - the original timestamp, properly formatted
    return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
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
