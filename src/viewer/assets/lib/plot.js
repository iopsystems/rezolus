/**
 * Plot options class for configuring different chart types
 */
export class PlotOpts {
  /**
   * Create a new plot options object
   * 
   * @param {string} title - The title of the plot
   * @param {string} id - Unique identifier for the plot
   * @param {string} style - Plot style ('line', 'scatter', 'heatmap', 'multi')
   * @param {object} yAxis - Y-axis configuration
   */
  constructor(title, id, style, yAxis = {}) {
    this.title = title;
    this.id = id;
    this.style = style;

    // Y-axis configuration
    this.yAxis = {
      // Label to display on the Y-axis
      label: yAxis.label || null,

      // Unit system for automatic scaling ('time', 'bytes', 'bitrate', etc.)
      unitSystem: yAxis.unitSystem || null,

      // For log scale axes
      logScale: yAxis.logScale || false,

      // Custom min/max values if needed
      min: yAxis.min,
      max: yAxis.max
    };
  }

  // Static factory methods for different plot types
  static heatmap(title, id, yAxis = {}) {
    return new PlotOpts(title, id, "heatmap", yAxis);
  }

  static line(title, id, yAxis = {}) {
    return new PlotOpts(title, id, "line", yAxis);
  }

  static scatter(title, id, yAxis = {}) {
    return new PlotOpts(title, id, "scatter", yAxis);
  }
  
  static multi(title, id, yAxis = {}) {
    return new PlotOpts(title, id, "multi", yAxis);
  }
  
  // Builder methods for additional configuration
  with_axis_label(label) {
    this.yAxis.label = label;
    return this;
  }
  
  with_unit_system(unitSystem) {
    this.yAxis.unitSystem = unitSystem;
    return this;
  }
  
  with_log_scale(enabled = true) {
    this.yAxis.logScale = enabled;
    return this;
  }
}