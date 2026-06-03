// License is MIT: https://opensource.org/license/mit

import {
  brush,
  brushX,
  brushY,
  select,
  zoom,
  ZoomTransform,
} from "../libraries/d3.js";

export class ZoomInteraction {
  // Attach d3-zoom to the host element and initialize it from the current public view state.
  constructor(el, { view, worldExtent, scaleExtent, zoomAxis, mode }) {
    this.el = el;
    this.el.style.touchAction = "none";

    this.worldExtent = worldExtent;
    this.scaleExtent = scaleExtent;
    this.zoomAxis = zoomAxis;
    this.mode = mode;

    this.brushSurface = document.createElementNS(SVG_NS, "svg");
    this.brushSurface.setAttribute("part", "brush-surface");
    this.brushSurface.style.position = "absolute";
    this.brushSurface.style.inset = "0";
    this.brushSurface.style.display = "block";
    this.brushSurface.style.overflow = "visible";

    this.brushLayer = document.createElementNS(SVG_NS, "g");
    this.brushLayer.setAttribute("part", "brush-layer");
    this.brushSurface.appendChild(this.brushLayer);
    (el.shadowRoot ?? el).appendChild(this.brushSurface);

    // Use cursor feedback so drag state is visible without reading any code.
    const defaultZoomFilter = zoom().filter();
    const defaultZoomConstrain = zoom().constrain();
    this.zoomBehavior = zoom()
      .filter(
        (event) =>
          this.mode === "zoom" && defaultZoomFilter.call(this.el, event),
      )
      // Run d3's built-in translateExtent clamp first, then canonicalize into the public view model.
      .constrain((transform, extent, translateExtent) =>
        this.#constrainTransform(
          defaultZoomConstrain(transform, extent, translateExtent),
        ),
      )
      // Show the active grab state while d3 is handling a gesture.
      .on("start", () => {
        if (this.mode === "zoom") {
          this.el.style.cursor = "grabbing";
        }
      })
      // Restore the idle cursor once d3 ends the current gesture.
      .on("end", () => {
        this.el.style.cursor = this.mode === "brush" ? "crosshair" : "grab";
      })
      .scaleExtent(scaleExtent)
      // Convert each d3 zoom event into the component's world-space view model.
      .on("zoom", (event) => this.#handleZoom(event));
    select(this.el).call(this.zoomBehavior);

    this.#installBrushBehavior();
    this.setView(view);
    this.setMode(mode);
  }

  // Adopt an externally supplied view, normalize it, and sync all derived interaction state to match.
  setView(view) {
    const nextView = this.#normalizeView(view);
    const sizeChanged =
      !this.view ||
      this.view.size.width !== nextView.size.width ||
      this.view.size.height !== nextView.size.height;
    this.view = nextView;
    this.el.style.width = this.view.size.width + "px";
    this.el.style.height = this.view.size.height + "px";
    this.#updateBounds();
    // d3-zoom documents `__zoom` as the stored current transform; write it directly here so
    // controlled prop sync stays silent instead of dispatching synthetic zoom lifecycle events.
    this.el.__zoom = this.#transformFromView(this.view);
    if (sizeChanged) {
      this.brushSurface.setAttribute("width", this.view.size.width);
      this.brushSurface.setAttribute("height", this.view.size.height);
      this.brushSurface.setAttribute(
        "viewBox",
        `0 0 ${this.view.size.width} ${this.view.size.height}`,
      );
      this.brushSurface.style.width = this.view.size.width + "px";
      this.brushSurface.style.height = this.view.size.height + "px";
      this.brushBehavior.extent([
        [0, 0],
        [this.view.size.width, this.view.size.height],
      ]);
      select(this.brushLayer).call(this.brushBehavior);
    }
    return this;
  }

  // Update the identity world frame and reapply the current view under the new bounds.
  setWorldExtent(worldExtent) {
    this.worldExtent = worldExtent;
    if (this.view) {
      this.setView(this.view);
    }
    return this;
  }

  // Switch which axes the interaction is allowed to move and reapply the current view under that mode.
  setZoomAxis(zoomAxis) {
    this.zoomAxis = zoomAxis;
    this.#installBrushBehavior();
    if (this.view) {
      this.setView(this.view);
    }
    return this;
  }

  // Switch between the zoom and brush interaction layers without changing the public view model.
  setMode(mode) {
    this.mode = mode;
    this.brushSurface.style.display = mode === "brush" ? "block" : "none";
    this.brushSurface.style.pointerEvents = mode === "brush" ? "auto" : "none";
    this.el.style.cursor = mode === "brush" ? "crosshair" : "grab";
    if (mode !== "brush") {
      this.#clearBrush();
    }
    return this;
  }

  // Update d3's raw scale limits and reapply the current view under the new limits.
  setScaleExtent(scaleExtent) {
    this.scaleExtent = scaleExtent;
    this.zoomBehavior.scaleExtent(scaleExtent);
    if (this.view) {
      this.setView(this.view);
    }
    return this;
  }

  // Unbind d3 and remove the interaction surface when the host component tears down.
  destroy() {
    select(this.el).on(".zoom", null);
    select(this.brushLayer).on(".brush", null);
    this.brushSurface.remove();
  }

  // Translate a trusted d3 zoom event into the public view model and emit it back out.
  // Interactive zoom transforms are already normalized by `constrain`.
  #handleZoom(event) {
    const { transform, sourceEvent } = event;
    if (!sourceEvent?.isTrusted) return;

    const view = this.#viewFromTransform(transform);

    // Emit the new world-space view so parent code can update linked renderers.
    this.el.dispatchEvent(
      new CustomEvent("viewchange", {
        bubbles: true,
        composed: true,
        detail: { view, worldExtent: this.worldExtent, sourceEvent },
      }),
    );
  }

  // Reinstall the axis-specific d3 brush primitive while leaving the zoom implementation alone.
  #installBrushBehavior() {
    const behavior =
      this.zoomAxis === "x"
        ? brushX()
        : this.zoomAxis === "y"
          ? brushY()
          : brush();
    const defaultBrushFilter = behavior.filter();

    this.brushBehavior = behavior
      .filter(
        (event) =>
          this.mode === "brush" &&
          defaultBrushFilter.call(this.brushLayer, event),
      )
      .on("end", (event) => this.#handleBrush(event));

    if (this.view) {
      this.brushBehavior.extent([
        [0, 0],
        [this.view.size.width, this.view.size.height],
      ]);
    }

    select(this.brushLayer).on(".brush", null).call(this.brushBehavior);
  }

  // Translate a trusted brush gesture into a world-space extent and emit it when the gesture ends.
  #handleBrush(event) {
    const { selection, sourceEvent } = event;
    if (!sourceEvent?.isTrusted || !selection) return;

    const extent = this.#extentFromBrushSelection(selection);
    const brushView = {
      extent,
      size: this.view.size,
    };
    const view = this.#normalizeView(brushView);
    this.#clearBrush(event);

    this.el.dispatchEvent(
      new CustomEvent("brushselect", {
        bubbles: true,
        composed: true,
        detail: {
          extent,
          view,
          selection,
          sourceEvent,
          worldExtent: this.worldExtent,
        },
      }),
    );
  }

  // d3 calls `constrain` before committing interactive zoom transforms. That is the right place to
  // normalize freeform input into the view rectangle this component actually supports.
  #constrainTransform(transform) {
    if (!this.view) return transform;
    const view = this.#viewFromTransform(transform);
    return this.#transformFromView(this.#normalizeView(view));
  }

  #normalizeView(view) {
    if (!view) return view;

    // d3-zoom only has a single scale factor in "xy", so any free view rectangle has to
    // be expanded to the viewport aspect ratio before it becomes a canonical zoom target.
    if (this.zoomAxis === "xy") {
      const targetAspect = view.size.width / view.size.height;
      const centerX = (view.extent.x0 + view.extent.x1) / 2;
      const centerY = (view.extent.y0 + view.extent.y1) / 2;
      const width = view.extent.x1 - view.extent.x0;
      const height = view.extent.y1 - view.extent.y0;
      const aspect = width / height;
      let nextWidth = width;
      let nextHeight = height;

      if (aspect > targetAspect) {
        nextHeight = nextWidth / targetAspect;
      } else {
        nextWidth = nextHeight * targetAspect;
      }

      view = {
        extent: {
          x0: centerX - nextWidth / 2,
          x1: centerX + nextWidth / 2,
          y0: centerY - nextHeight / 2,
          y1: centerY + nextHeight / 2,
        },
        size: view.size,
      };
    }

    const [minK = 1, maxK = Infinity] = this.scaleExtent ?? [1, Infinity];
    const worldWidth = this.worldExtent.x1 - this.worldExtent.x0;
    const worldHeight = this.worldExtent.y1 - this.worldExtent.y0;

    let x0 = view.extent.x0;
    let x1 = view.extent.x1;
    let y0 = view.extent.y0;
    let y1 = view.extent.y1;

    let width = x1 - x0;
    let height = y1 - y0;
    const centerX = (x0 + x1) / 2;
    const centerY = (y0 + y1) / 2;

    if (
      this.zoomAxis !== "y" &&
      Number.isFinite(worldWidth) &&
      worldWidth > 0
    ) {
      const minWidth = worldWidth / maxK;
      const maxWidth = worldWidth / minK;
      width = Math.max(minWidth, Math.min(maxWidth, width));
      x0 = centerX - width / 2;
      x1 = centerX + width / 2;
    }

    if (
      this.zoomAxis !== "x" &&
      Number.isFinite(worldHeight) &&
      worldHeight > 0
    ) {
      const minHeight = worldHeight / maxK;
      const maxHeight = worldHeight / minK;
      height = Math.max(minHeight, Math.min(maxHeight, height));
      y0 = centerY - height / 2;
      y1 = centerY + height / 2;
    }

    ({ start: x0, end: x1 } = clampRangeToBounds(
      x0,
      x1,
      this.worldExtent.x0,
      this.worldExtent.x1,
    ));
    ({ start: y0, end: y1 } = clampRangeToBounds(
      y0,
      y1,
      this.worldExtent.y0,
      this.worldExtent.y1,
    ));

    return {
      extent: { x0, x1, y0, y1 },
      size: view.size,
    };
  }

  // Keep d3's pixel bounds aligned with the current size of the interaction surface.
  #updateBounds() {
    const bounds = [
      [0, 0],
      [this.view.size.width, this.view.size.height],
    ];
    this.zoomBehavior.extent(bounds).translateExtent(bounds);
  }

  // Convert a pixel-space brush selection into the equivalent world-space extent inside the current view.
  #extentFromBrushSelection(selection) {
    const width = this.view.size.width;
    const height = this.view.size.height;
    let x0Px = 0;
    let x1Px = width;
    let y0Px = 0;
    let y1Px = height;

    if (this.zoomAxis === "x") {
      [x0Px, x1Px] = selection;
    } else if (this.zoomAxis === "y") {
      [y0Px, y1Px] = selection;
    } else {
      [[x0Px, y0Px], [x1Px, y1Px]] = selection;
    }

    const xSpan = this.view.extent.x1 - this.view.extent.x0;
    const ySpan = this.view.extent.y1 - this.view.extent.y0;

    return {
      x0:
        this.zoomAxis === "y"
          ? this.view.extent.x0
          : this.view.extent.x0 + (x0Px / width) * xSpan,
      x1:
        this.zoomAxis === "y"
          ? this.view.extent.x1
          : this.view.extent.x0 + (x1Px / width) * xSpan,
      y0:
        this.zoomAxis === "x"
          ? this.view.extent.y0
          : this.view.extent.y1 - (y1Px / height) * ySpan,
      y1:
        this.zoomAxis === "x"
          ? this.view.extent.y1
          : this.view.extent.y1 - (y0Px / height) * ySpan,
    };
  }

  // Clear d3's live brush state without treating the synthetic update as a real selection.
  #clearBrush(event) {
    select(this.brushLayer).call(this.brushBehavior.move, null, event);
  }

  // Interpret d3's single transform as per-axis zoom and pan according to the current lock mode.
  #decompose(transform) {
    return {
      kx: this.zoomAxis === "y" ? 1 : transform.k,
      ky: this.zoomAxis === "x" ? 1 : transform.k,
      tx: this.zoomAxis === "y" ? 0 : transform.x,
      ty: this.zoomAxis === "x" ? 0 : transform.y,
    };
  }

  // Convert d3's current transform into the public world-space view rectangle.
  #viewFromTransform(transform) {
    const width = this.view.size.width;
    const height = this.view.size.height;
    const worldExtent = this.worldExtent;
    const worldWidth = worldExtent.x1 - worldExtent.x0;
    const worldHeight = worldExtent.y1 - worldExtent.y0;
    const ppuX = width / worldWidth;
    const ppuY = height / worldHeight;
    const d = this.#decompose(transform);
    return {
      extent: {
        x0: worldExtent.x0 + (0 - d.tx) / (ppuX * d.kx),
        x1: worldExtent.x0 + (width - d.tx) / (ppuX * d.kx),
        y0: worldExtent.y1 - (height - d.ty) / (ppuY * d.ky),
        y1: worldExtent.y1 - (0 - d.ty) / (ppuY * d.ky),
      },
      size: this.view.size,
    };
  }

  // Convert a public world-space view rectangle back into d3's canonical transform.
  #transformFromView(view) {
    const worldExtent = this.worldExtent;
    const worldWidth = worldExtent.x1 - worldExtent.x0;
    const worldHeight = worldExtent.y1 - worldExtent.y0;
    const viewWidth = view.extent.x1 - view.extent.x0;
    const viewHeight = view.extent.y1 - view.extent.y0;
    const ppuX = view.size.width / worldWidth;
    const ppuY = view.size.height / worldHeight;
    const kx = worldWidth / viewWidth;
    const ky = worldHeight / viewHeight;
    const k = this.zoomAxis === "y" ? ky : kx;
    const tx =
      this.zoomAxis === "y" ? 0 : (worldExtent.x0 - view.extent.x0) * ppuX * kx;
    const ty =
      this.zoomAxis === "x" ? 0 : (view.extent.y1 - worldExtent.y1) * ppuY * ky;

    return new ZoomTransform(k, tx, ty);
  }
}

const SVG_NS = "http://www.w3.org/2000/svg";

function clampRangeToBounds(start, end, min, max) {
  const span = end - start;
  const boundsSpan = max - min;

  if (
    !Number.isFinite(span) ||
    !Number.isFinite(boundsSpan) ||
    boundsSpan <= 0 ||
    span >= boundsSpan
  ) {
    return { start: min, end: max };
  }

  let nextStart = start;
  let nextEnd = end;
  if (nextStart < min) {
    nextEnd += min - nextStart;
    nextStart = min;
  }
  if (nextEnd > max) {
    nextStart -= nextEnd - max;
    nextEnd = max;
  }
  return { start: nextStart, end: nextEnd };
}
