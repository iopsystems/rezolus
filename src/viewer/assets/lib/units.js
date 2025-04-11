/**
 * Unit formatting system for chart axes
 * Supports automatic scaling of values based on magnitude
 */

// Define unit scaling systems
const UNIT_SYSTEMS = {
  // Time-based units (nanoseconds to seconds)
  time: {
    base: 'ns',
    scales: [
      { threshold: 0, suffix: 'ns', divisor: 1 },
      { threshold: 1000, suffix: 'μs', divisor: 1000 },
      { threshold: 1000000, suffix: 'ms', divisor: 1000000 },
      { threshold: 1000000000, suffix: 's', divisor: 1000000000 }
    ]
  },
  
  // Data size (bytes)
  bytes: {
    base: 'B',
    scales: [
      { threshold: 0, suffix: 'B', divisor: 1 },
      { threshold: 1024, suffix: 'KB', divisor: 1024 },
      { threshold: 1048576, suffix: 'MB', divisor: 1048576 },
      { threshold: 1073741824, suffix: 'GB', divisor: 1073741824 },
      { threshold: 1099511627776, suffix: 'TB', divisor: 1099511627776 }
    ]
  },
  
  // Network data rate (bits per second)
  bitrate: {
    base: 'bps',
    scales: [
      { threshold: 0, suffix: 'bps', divisor: 1 },
      { threshold: 1000, suffix: 'Kbps', divisor: 1000 },
      { threshold: 1000000, suffix: 'Mbps', divisor: 1000000 },
      { threshold: 1000000000, suffix: 'Gbps', divisor: 1000000000 },
      { threshold: 1000000000000, suffix: 'Tbps', divisor: 1000000000000 }
    ]
  },
  
  // Percentage (already formatted, just add %)
  percentage: {
    base: '%',
    scales: [
      { threshold: 0, suffix: '%', divisor: 1, multiplier: 100 } // Added multiplier for percentage
    ]
  },

  // Frequency (Hz to GHz)
  frequency: {
    base: 'Hz',
    scales: [
      { threshold: 0, suffix: 'Hz', divisor: 1 },
      { threshold: 1000, suffix: 'KHz', divisor: 1000 },
      { threshold: 1000000, suffix: 'MHz', divisor: 1000000 },
      { threshold: 1000000000, suffix: 'GHz', divisor: 1000000000 }
    ]
  },
  
  // Count (no units, just numbers with K, M, B suffixes)
  count: {
    base: '',
    scales: [
      { threshold: 0, suffix: '', divisor: 1 },
      { threshold: 1000, suffix: 'K', divisor: 1000 },
      { threshold: 1000000, suffix: 'M', divisor: 1000000 },
      { threshold: 1000000000, suffix: 'B', divisor: 1000000000 }
    ]
  }
};

/**
 * Format a value with the appropriate unit scaling
 * 
 * @param {number} value - The value to format
 * @param {string} unitSystem - The unit system to use ('time', 'bytes', 'bitrate', etc.)
 * @param {number} precision - Number of decimal places to include (default: 2)
 * @return {object} Object with formatted value and unit string
 */
function formatWithUnit(value, unitSystem, precision = 2) {
  // Handle invalid or zero values
  if (value === null || value === undefined || isNaN(value)) {
    return { value: '0', unit: UNIT_SYSTEMS[unitSystem]?.base || '' };
  }
  
  // Get absolute value for scaling (we'll preserve sign later)
  const absValue = Math.abs(value);
  
  // Normalize unit system name - handle both 'time' and 'time_ns'
  const normalizedUnitSystem = unitSystem === 'time_ns' ? 'time' : unitSystem;
  
  // Get the unit system configuration
  const system = UNIT_SYSTEMS[normalizedUnitSystem];
  if (!system) {
    // Fallback for unknown unit systems
    return { 
      value: value.toFixed(precision), 
      unit: unitSystem || '' 
    };
  }
  
  // Find the appropriate scale for this value
  let scale = system.scales[0]; // Default to the smallest scale
  
  // Start from the largest scale and work backwards
  for (let i = system.scales.length - 1; i >= 0; i--) {
    if (absValue >= system.scales[i].threshold) {
      scale = system.scales[i];
      break;
    }
  }
  
  // Apply multiplier if needed (e.g., for percentages)
  const multiplier = scale.multiplier || 1;
  
  // Format the value with the selected scale and multiplier
  const scaledValue = ((value * multiplier) / scale.divisor).toFixed(precision);
  
  // Remove trailing zeros after decimal point
  const cleanValue = scaledValue.replace(/\.0+$/, '').replace(/(\.\d*[1-9])0+$/, '$1');
  
  return {
    value: cleanValue,
    unit: scale.suffix
  };
}

/**
 * Creates a formatter function for ECharts that applies unit scaling
 * 
 * @param {string} unitSystem - The unit system to use
 * @param {number} precision - Number of decimal places (default: 2)
 * @return {Function} Formatter function for ECharts
 */
function createAxisLabelFormatter(unitSystem, precision = 2) {
  return function(value) {
    // Skip formatting for empty values
    if (value === '' || value === null || value === undefined) return '';
    
    const formatted = formatWithUnit(value, unitSystem, precision);
    return formatted.value + (formatted.unit ? ' ' + formatted.unit : '');
  };
}


/**
 * Creates a tooltip formatter with appropriate unit scaling
 * 
 * @param {string} unitSystem - The unit system to use
 * @param {number} precision - Number of decimal places (default: 2)
 * @return {Function} Formatter function for tooltip values
 */
function createTooltipFormatter(unitSystem, precision = 2) {
  return function(params) {
    // The params object includes the value to format
    if (Array.isArray(params.value)) {
      // Handle scatter plots with [x, y] values
      // params.value[0] is the timestamp (already formatted)
      // params.value[1] is the actual value that needs unit formatting
      const formatted = formatWithUnit(params.value[1], unitSystem, precision);
      return `${formatted.value} ${formatted.unit}`;
    } else {
      // Handle normal line plots where params.value is the numeric value
      const formatted = formatWithUnit(params.value, unitSystem, precision);
      return `${formatted.value} ${formatted.unit}`;
    }
  };
}

// Export the utility functions
export {
  UNIT_SYSTEMS,
  formatWithUnit,
  createAxisLabelFormatter,
  createTooltipFormatter
};