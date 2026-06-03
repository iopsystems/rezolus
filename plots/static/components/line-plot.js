// License is MIT: https://opensource.org/license/mit

import { select } from "../libraries/d3.js";

const SVG_NS = "http://www.w3.org/2000/svg";
const CLIP_PADDING_PX = 1; // So line edges stay visible

function getPathKey(path, index) {
  return typeof path.id === "string" && path.id.length > 0
    ? path.id
    : `${index}`;
}

export class LinePlot {
  constructor(container, { view, paths = [] }) {
    // Initialize the SVG, which is both the viewport shell and the renderer.
    this.el =
      container.querySelector("svg") ??
      container.appendChild(document.createElementNS(SVG_NS, "svg"));
    this.el.style.display = "block";
    // Allow a tiny stroke overscan so lines touching the boundary do not look cut off.
    this.el.style.overflow = "visible";

    this.defs =
      this.el.querySelector("defs") ??
      this.el.appendChild(document.createElementNS(SVG_NS, "defs"));

    this.clipPath =
      this.defs.querySelector("clipPath") ??
      this.defs.appendChild(document.createElementNS(SVG_NS, "clipPath"));
    this.clipPath.setAttribute("id", "line-plot-clip");

    this.clipRect =
      this.clipPath.querySelector("rect") ??
      this.clipPath.appendChild(document.createElementNS(SVG_NS, "rect"));

    this.viewport =
      this.el.querySelector('g[data-role="viewport"]') ??
      this.el.appendChild(document.createElementNS(SVG_NS, "g"));
    this.viewport.setAttribute("data-role", "viewport");
    this.viewport.setAttribute("clip-path", "url(#line-plot-clip)");

    this.world =
      this.viewport.querySelector("g") ??
      this.viewport.appendChild(document.createElementNS(SVG_NS, "g"));
    this.world.setAttribute("fill", "none");
    this.world.setAttribute("stroke", "currentColor");
    this.world.setAttribute("stroke-width", 1.5);
    this.world.setAttribute("opacity", 1);
    this.world.setAttribute("stroke-linecap", "round");
    this.world.setAttribute("stroke-linejoin", "round");

    this.view = view;
    this.paths = [];

    this.setView(view);
    this.setPaths(paths);
  }

  // Resize the viewport shell and recompute where the world layer belongs on screen.
  setView(view) {
    this.view = view;
    this.el.style.width = this.view.size.width + "px";
    this.el.style.height = this.view.size.height + "px";
    this.#update();
  }

  // Swap in a new set of world-space path payloads and redraw them.
  setPaths(paths) {
    this.paths = paths;
    this.#update();
    this.#draw();
  }

  // Tear down the renderer's DOM when the host component is leaving the page.
  destroy() {
    this.el.remove();
  }

  // Project the world-space view into the current viewport and update the group transform.
  #update() {
    const view = this.view;
    const width = view.size.width;
    const height = view.size.height;

    this.el.setAttribute("width", width);
    this.el.setAttribute("height", height);
    this.el.style.width = width + "px";
    this.el.style.height = height + "px";
    this.clipRect.setAttribute("x", -CLIP_PADDING_PX);
    this.clipRect.setAttribute("y", -CLIP_PADDING_PX);
    this.clipRect.setAttribute("width", width + CLIP_PADDING_PX * 2);
    this.clipRect.setAttribute("height", height + CLIP_PADDING_PX * 2);

    const placement = {
      originX:
        ((0 - view.extent.x0) / (view.extent.x1 - view.extent.x0)) * width,
      originY:
        ((view.extent.y1 - 0) / (view.extent.y1 - view.extent.y0)) * height,
      scaleX: (1 / (view.extent.x1 - view.extent.x0)) * width,
      scaleY: -((1 / (view.extent.y1 - view.extent.y0)) * height),
    };

    this.world.setAttribute(
      "transform",
      `translate(${placement.originX},${placement.originY}) ` +
        `scale(${placement.scaleX}, ${placement.scaleY})`,
    );
  }

  // Join the current path payloads onto the world layer and apply per-series styles.
  #draw() {
    select(this.world)
      .selectAll("path")
      .data(this.paths, getPathKey)
      .join((enter) =>
        enter.append("path").attr("vector-effect", "non-scaling-stroke"),
      )
      .attr("d", (path) => path.d)
      .attr("fill", (path) => path.fill)
      .attr("stroke", (path) => path.stroke)
      .attr("stroke-width", (path) => path.strokeWidth ?? null)
      .attr("opacity", (path) => path.opacity ?? null);
  }
}
