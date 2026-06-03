// License is MIT: https://opensource.org/license/mit

import {
  axisBottom,
  axisLeft,
  format,
  scaleLinear,
  select,
} from "../libraries/d3.js";

export class PlotAxis {
  constructor(container, { width, height, margin, axes }) {
    this.container = container;
    this.width = width;
    this.height = height;
    this.margin = margin;
    this.axes = axes;

    this.root =
      container.querySelector('[data-role="plot-axis-root"]') ??
      container.appendChild(document.createElement("div"));
    this.root.dataset.role = "plot-axis-root";
    this.root.style.position = "relative";
    this.root.style.display = "block";

    this.styleEl =
      container.querySelector("style") ??
      container.appendChild(document.createElement("style"));
    this.styleEl.textContent = `
      :host {
        display: inline-block;
        position: relative;
      }

      [part="frame-svg"] {
        position: absolute;
        inset: 0;
        display: block;
        overflow: visible;
        pointer-events: none;
      }

      [part="plot-frame"] {
        position: absolute;
        overflow: visible;
      }

      ::slotted(line-plot),
      ::slotted(raster-plot),
      ::slotted(zoom-interaction) {
        position: absolute;
        inset: 0;
        display: block;
      }

      ::slotted(zoom-interaction) {
        z-index: 1;
      }
    `;

    this.svg =
      this.root.querySelector("svg") ??
      this.root.appendChild(document.createElementNS(SVG_NS, "svg"));
    this.svg.setAttribute("part", "frame-svg");

    this.xAxisGroup =
      this.svg.querySelector('g[data-role="x-axis"]') ??
      this.svg.appendChild(document.createElementNS(SVG_NS, "g"));
    this.xAxisGroup.dataset.role = "x-axis";
    this.xAxisGroup.setAttribute("part", "x-axis");
    this.xAxisGroup.style.color = "currentColor";

    this.yAxisGroup =
      this.svg.querySelector('g[data-role="y-axis"]') ??
      this.svg.appendChild(document.createElementNS(SVG_NS, "g"));
    this.yAxisGroup.dataset.role = "y-axis";
    this.yAxisGroup.setAttribute("part", "y-axis");
    this.yAxisGroup.style.color = "currentColor";

    this.plotFrame =
      this.root.querySelector('div[data-role="plot-frame"]') ??
      this.root.appendChild(document.createElement("div"));
    this.plotFrame.dataset.role = "plot-frame";
    this.plotFrame.setAttribute("part", "plot-frame");

    this.slot =
      this.plotFrame.querySelector("slot") ??
      this.plotFrame.appendChild(document.createElement("slot"));

    this.#updateLayout();
    this.#draw();
  }

  setWidth(width) {
    this.width = width;
    this.#updateLayout();
    this.#draw();
    return this;
  }

  setHeight(height) {
    this.height = height;
    this.#updateLayout();
    this.#draw();
    return this;
  }

  setMargin(margin) {
    this.margin = margin;
    this.#updateLayout();
    this.#draw();
    return this;
  }

  setAxes(axes) {
    this.axes = axes;
    this.#draw();
    return this;
  }

  destroy() {
    this.root.remove();
    this.styleEl.remove();
  }

  #updateLayout() {
    const outerWidth = this.width + this.margin.left + this.margin.right;
    const outerHeight = this.height + this.margin.top + this.margin.bottom;

    this.root.style.width = outerWidth + "px";
    this.root.style.height = outerHeight + "px";
    this.svg.setAttribute("width", outerWidth);
    this.svg.setAttribute("height", outerHeight);
    this.svg.setAttribute("viewBox", `0 0 ${outerWidth} ${outerHeight}`);
    this.svg.style.width = outerWidth + "px";
    this.svg.style.height = outerHeight + "px";

    this.plotFrame.style.left = this.margin.left + "px";
    this.plotFrame.style.top = this.margin.top + "px";
    this.plotFrame.style.width = this.width + "px";
    this.plotFrame.style.height = this.height + "px";
  }

  #draw() {
    if (this.axes.x) {
      const xScale = scaleLinear()
        .domain(this.axes.x.domain)
        .range([this.margin.left, this.margin.left + this.width]);
      const xAxis = axisBottom(xScale)
        .ticks(Math.max(2, Math.floor(this.width / 80)))
        .tickSizeOuter(0);
      xAxis.tickFormat(normalizeTickFormat(this.axes.x.tickFormat));
      select(this.xAxisGroup)
        .attr("transform", `translate(0,${this.margin.top + this.height})`)
        .call(xAxis)
        .call(styleAxisGroup);
    } else {
      select(this.xAxisGroup).selectAll("*").remove();
    }

    if (this.axes.y) {
      const yScale = scaleLinear()
        .domain(this.axes.y.domain)
        .range([this.margin.top + this.height, this.margin.top]);
      const yAxis = axisLeft(yScale)
        .ticks(Math.max(2, Math.floor(this.height / 36)))
        .tickSizeOuter(0);
      yAxis.tickFormat(normalizeTickFormat(this.axes.y.tickFormat));
      select(this.yAxisGroup)
        .attr("transform", `translate(${this.margin.left},0)`)
        .call(yAxis)
        .call(styleAxisGroup);
    } else {
      select(this.yAxisGroup).selectAll("*").remove();
    }
  }
}

function styleAxisGroup(group) {
  group.selectAll("path,line").attr("stroke", "currentColor");
  group.selectAll("text").attr("fill", "currentColor");
}

function normalizeTickFormat(tickFormat) {
  return format(tickFormat ?? DEFAULT_TICK_FORMAT);
}

const SVG_NS = "http://www.w3.org/2000/svg";
const DEFAULT_TICK_FORMAT = ".2~s";
