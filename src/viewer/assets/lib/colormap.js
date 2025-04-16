// colormap.js - Global color mapping for consistent cgroup colors across charts

/**
 * ColorMapper provides consistent color assignment for cgroups across all charts and page refreshes
 * Uses a deterministic hash function to assign colors based on cgroup names
 */
export class ColorMapper {
  constructor() {
    // Store color assignments for cgroups
    this.colorMap = new Map();
    
    // Color palette for assignment - using ECharts default colors plus additional colors
    this.colorPalette = [
      '#5470c6', '#91cc75', '#fac858', '#ee6666', '#73c0de',
      '#3ba272', '#fc8452', '#9a60b4', '#ea7ccc', '#8d98b3',
      '#e5cf0d', '#97b552', '#95706d', '#dc69aa', '#07a2a4',
      '#9467bd', '#a05195', '#d45087', '#f95d6a', '#ff7c43',
      '#ffa600'
    ];
  }
  
  /**
   * Get a simple hash value from a string
   * This creates a deterministic numeric value from any string
   * @param {string} str - The string to hash
   * @returns {number} A numeric hash value
   */
  stringToHash(str) {
    let hash = 0;
    for (let i = 0; i < str.length; i++) {
      const char = str.charCodeAt(i);
      hash = ((hash << 5) - hash) + char;
      hash = hash & hash; // Convert to 32bit integer
    }
    // Make sure it's positive
    return Math.abs(hash);
  }
  
  /**
   * Get the color for a specific cgroup, using a deterministic mapping
   * @param {string} cgroupName - The name of the cgroup
   * @returns {string} The color code for this cgroup
   */
  getColor(cgroupName) {
    // If we already have a color for this cgroup, return it
    if (this.colorMap.has(cgroupName)) {
      return this.colorMap.get(cgroupName);
    }
    
    // Generate a deterministic index based on the cgroup name
    const hash = this.stringToHash(cgroupName);
    const colorIndex = hash % this.colorPalette.length;
    const color = this.colorPalette[colorIndex];
    
    // Store the mapping for future reference
    this.colorMap.set(cgroupName, color);
    
    return color;
  }
  
  /**
   * Get colors for an array of cgroup names
   * @param {string[]} cgroupNames - Array of cgroup names
   * @returns {string[]} Array of color codes in the same order
   */
  getColors(cgroupNames) {
    return cgroupNames.map(name => this.getColor(name));
  }
  
  /**
   * Clear all color mappings - generally only needed for testing
   */
  clear() {
    this.colorMap.clear();
  }
}

// Create a singleton instance to share across the application
const globalColorMapper = new ColorMapper();
export default globalColorMapper;