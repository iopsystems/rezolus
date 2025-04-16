// cgroup-utils.js - Utilities for better cgroup visualization
// This file should be imported in script.js

/**
 * Provides utilities for improved cgroup data handling and visualization
 */
export class CGroupUtils {
  constructor() {
    // Store for cgroup data stats 
    this.cgroupStats = new Map();
  }
  
  /**
   * Process cgroup data to identify and mark sparse regions
   * 
   * @param {Array} data - Chart data in the format [times, series1Values, series2Values, ...]
   * @param {Array} seriesNames - Array of series names
   * @returns {Object} Enhanced data with stats and markup for sparse regions
   */
  processCGroupData(data, seriesNames) {
    if (!data || data.length < 2 || !seriesNames || seriesNames.length === 0) {
      return { data, stats: {} };
    }
    
    const timeArray = data[0];
    const stats = {};
    const enhancedData = [timeArray];
    
    // Process each series
    for (let i = 1; i < data.length && i <= seriesNames.length; i++) {
      const seriesName = seriesNames[i-1];
      const values = data[i];
      const seriesStats = this.analyzeTimeSeries(timeArray, values);
      
      // Store stats for this cgroup
      stats[seriesName] = seriesStats;
      
      // Enhance the data by marking sparse regions for better visualization
      const enhancedValues = this.markSparseRegions(timeArray, values, seriesStats);
      enhancedData.push(enhancedValues);
    }
    
    // Update the global stats store
    for (const [cgroupName, cgroupStats] of Object.entries(stats)) {
      this.cgroupStats.set(cgroupName, cgroupStats);
    }
    
    return {
      data: enhancedData,
      stats: stats
    };
  }
  
  /**
   * Analyze a time series to identify data characteristics
   * 
   * @param {Array} timeArray - Array of timestamps
   * @param {Array} values - Array of values
   * @returns {Object} Statistics about the time series
   */
  analyzeTimeSeries(timeArray, values) {
    if (!timeArray || !values || timeArray.length !== values.length) {
      return { 
        dataPoints: 0,
        validPoints: 0,
        densityRatio: 0,
        sparseRegions: []
      };
    }
    
    const dataPoints = values.length;
    let validPoints = 0;
    const sparseRegions = [];
    let inSparseRegion = false;
    let sparseStart = -1;
    
    // Identify valid points and sparse regions
    for (let i = 0; i < values.length; i++) {
      const value = values[i];
      
      if (value !== null && value !== undefined && value !== '-' && isFinite(value)) {
        validPoints++;
        
        // Check if we're exiting a sparse region
        if (inSparseRegion) {
          sparseRegions.push({
            start: sparseStart,
            end: i - 1
          });
          inSparseRegion = false;
        }
      } else {
        // Check if we're entering a sparse region
        if (!inSparseRegion) {
          inSparseRegion = true;
          sparseStart = i;
        }
      }
    }
    
    // Check if we ended in a sparse region
    if (inSparseRegion) {
      sparseRegions.push({
        start: sparseStart,
        end: values.length - 1
      });
    }
    
    return {
      dataPoints,
      validPoints,
      densityRatio: validPoints / dataPoints,
      sparseRegions
    };
  }
  
  /**
   * Mark sparse regions in a time series to improve visualization
   * 
   * @param {Array} timeArray - Array of timestamps
   * @param {Array} values - Array of values
   * @param {Object} stats - Statistics about the time series
   * @returns {Array} Enhanced values array with marked sparse regions
   */
  markSparseRegions(timeArray, values, stats) {
    if (!stats || !stats.sparseRegions || stats.sparseRegions.length === 0) {
      return values;
    }
    
    // Clone the values array to avoid modifying the original
    const enhancedValues = [...values];
    
    // Process each sparse region
    for (const region of stats.sparseRegions) {
      // Skip very short sparse regions (1-2 points)
      if (region.end - region.start < 2) continue;
      
      // For longer sparse regions, use interpolation
      const startIdx = Math.max(0, region.start - 1);
      const endIdx = Math.min(values.length - 1, region.end + 1);
      
      // Only interpolate if we have valid boundary values
      if (startIdx >= 0 && endIdx < values.length) {
        const startValue = values[startIdx];
        const endValue = values[endIdx];
        
        // Only interpolate if both boundary values are valid
        if (startValue !== null && startValue !== undefined && startValue !== '-' &&
            endValue !== null && endValue !== undefined && endValue !== '-') {
            
          // Linear interpolation
          const regionLength = endIdx - startIdx;
          
          for (let i = startIdx + 1; i < endIdx; i++) {
            const ratio = (i - startIdx) / regionLength;
            enhancedValues[i] = startValue + ratio * (endValue - startValue);
          }
        }
      }
    }
    
    return enhancedValues;
  }
  
  /**
   * Get statistics for a specific cgroup
   * 
   * @param {string} cgroupName - Name of the cgroup
   * @returns {Object|null} Statistics or null if not found
   */
  getCGroupStats(cgroupName) {
    return this.cgroupStats.get(cgroupName) || null;
  }
  
  /**
   * Create enhanced chart options for cgroup visualization
   * 
   * @param {Object} chartOptions - Base chart options
   * @param {Object} stats - Statistics about cgroup data
   * @returns {Object} Enhanced chart options
   */
  enhanceChartOptions(chartOptions, stats) {
    if (!chartOptions || !stats) {
      return chartOptions;
    }
    
    // Create a deep copy of the chart options
    const enhancedOptions = JSON.parse(JSON.stringify(chartOptions));
    
    // Enhance series options based on their stats
    if (enhancedOptions.series && enhancedOptions.series.length > 0) {
      for (let i = 0; i < enhancedOptions.series.length; i++) {
        const series = enhancedOptions.series[i];
        const seriesName = series.name;
        
        if (seriesName && stats[seriesName]) {
          const seriesStats = stats[seriesName];
          
          // For sparse series (less than 80% valid points), enhance visualization
          if (seriesStats.densityRatio < 0.8) {
            // Use step line for sparse data to make gaps more visible
            series.step = 'middle';
            
            // Add markers for actual data points
            series.showSymbol = true;
            series.symbolSize = 6;
            
            // Use dashed line for series with significant gaps
            if (seriesStats.densityRatio < 0.5) {
              series.lineStyle = {
                ...series.lineStyle,
                type: 'dashed',
                dashOffset: 2
              };
            }
          }
        }
      }
    }
    
    return enhancedOptions;
  }
}

// Create and export a singleton instance
const cgroupUtils = new CGroupUtils();
export default cgroupUtils;