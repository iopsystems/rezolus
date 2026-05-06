// color_legend.js — shared DOM color legend bar for heatmap chart types.
//
// Renders a vertical gradient bar to the right of the chart canvas with
// tick marks and labels (2 significant digits). Optionally shows a top
// caption (above bar), bottom caption (below bar), and extras (e.g. a
// checkbox toggle) below the bar body.

import { COLORS, FONTS } from './base.js';

// Bar geometry constants.
//
// LEGEND_GRID_RIGHT is the grid.right value heatmap chart configs must
// use so the plot area leaves room for the legend bar + tick labels
// PLUS a horizontal breathing gap between the right edge of the plot
// canvas and the bar itself:
//   right:8 (offset)  +  10 (bar)  +  4 (tick mark)  +  4 (gap)
//   +  ~32 (label width)  +  ~8 (breathing gap)  ≈ 72.
export const BAR_WIDTH = 10;
export const BAR_HEIGHT = 150;
export const LEGEND_RIGHT_OFFSET = 8;
export const LEGEND_GRID_RIGHT = 72;
export const LEGEND_TOP = 50;
export const LABEL_GAP = 4;
const TICK_MARK_LEN = 4;
// Approximate height of the topCaption row (10px font + ~1.2 line +
// 2px margin-bottom). Used to back the container off the grid top so
// the BAR (not the container) aligns with the Y-axis when a top
// caption is present.
const TOP_CAPTION_OFFSET = 14;

/**
 * Build a vertical gradient bar canvas from a color function.
 * Top of the canvas (y=0) maps to t=1 (max); bottom maps to t=0 (min).
 * @param {function} colorFn - maps a value in 0..1 to a CSS color string
 * @returns {HTMLCanvasElement}
 */
export function buildGradientCanvas(colorFn) {
    const canvas = document.createElement('canvas');
    canvas.width = BAR_WIDTH;
    canvas.height = BAR_HEIGHT;
    const ctx = canvas.getContext('2d');
    for (let y = 0; y < BAR_HEIGHT; y++) {
        const t = 1 - y / (BAR_HEIGHT - 1);
        ctx.fillStyle = colorFn(t);
        ctx.fillRect(0, y, BAR_WIDTH, 1);
    }
    return canvas;
}

/**
 * Round a number to 2 significant digits.
 */
export function sigDigits(v) {
    if (!Number.isFinite(v) || v === 0) return v;
    return parseFloat(v.toPrecision(2));
}

/**
 * Generate evenly spaced ticks for a linear scale.
 * Returns [{ pos, value }, ...] where pos=0 is the min end (bottom of bar)
 * and pos=1 is the max end (top of bar).
 */
export function linearTicks(min, max, count = 5) {
    const ticks = [];
    if (!Number.isFinite(min) || !Number.isFinite(max) || count < 2) {
        return [{ pos: 0, value: min }, { pos: 1, value: max }];
    }
    for (let i = 0; i < count; i++) {
        const t = i / (count - 1);
        ticks.push({ pos: t, value: min + t * (max - min) });
    }
    return ticks;
}

/**
 * Generate evenly spaced ticks for a log10 scale.
 * Returns [{ pos, value }, ...] where pos=0 is min, pos=1 is max.
 * Falls back to linearTicks when min/max are non-positive.
 */
export function logTicks(min, max, count = 5) {
    if (!(min > 0 && max > 0) || max <= min) {
        return linearTicks(min, max, count);
    }
    const logMin = Math.log10(min);
    const logMax = Math.log10(max);
    const ticks = [];
    for (let i = 0; i < count; i++) {
        const t = i / (count - 1);
        ticks.push({ pos: t, value: Math.pow(10, logMin + t * (logMax - logMin)) });
    }
    return ticks;
}

/**
 * Create or reuse the legend bar container.
 *
 * Layout (single column inside an absolutely positioned container on the
 * right side of the chart):
 *
 *   ┌──────────────────────────┐
 *   │ [topCaption]    diff only│
 *   ├──────────────────────────┤
 *   │ ┌──┐ ┤── label_top       │
 *   │ │██│ ┤── …               │
 *   │ │██│ ┤── …               │
 *   │ │██│ ┤── label_bottom    │
 *   │ └──┘                     │
 *   ├──────────────────────────┤
 *   │ [bottomCaption] diff only│
 *   ├──────────────────────────┤
 *   │ [extras]   e.g. checkbox │
 *   └──────────────────────────┘
 *
 * On subsequent calls (same wrapper), repaints the gradient from the
 * (possibly new) barCanvas and rebuilds tick rows with the supplied
 * ticks/captions, so callers can swap palettes (e.g. diff-mode diverging
 * palette) or refresh labels without tearing down the container.
 *
 * @param {HTMLElement} wrapper - parent element (Chart component's own
 *     div; owned by Mithril so Mithril removes the legend on unmount)
 * @param {HTMLCanvasElement} barCanvas - pre-rendered gradient canvas
 * @param {object} [options]
 * @param {Array<{pos:number,label:string}>} [options.ticks] - tick rows.
 *     `pos` is 0..1 where 0 is bottom (min) and 1 is top (max). Empty
 *     `label` strings render the tick mark with no number.
 * @param {string} [options.topCaption] - text above the bar (e.g. diff)
 * @param {string} [options.bottomCaption] - text below the bar (e.g. diff)
 * @param {HTMLElement[]} [options.extras] - elements appended at bottom
 * @param {number} [options.barTop] - pixel offset of the BAR's top edge
 *     from wrapper's top. Default LEGEND_TOP. Pass the chart grid's
 *     `y` so the bar aligns with the Y-axis. The container is offset
 *     above the bar to leave room for any topCaption row.
 * @param {number} [options.barHeight] - pixel height of the BAR. Default
 *     BAR_HEIGHT. Pass the chart grid's `height` to match the Y-axis.
 *     Tick positions are recomputed against this value.
 * @returns {HTMLElement} the legend container
 */
export function ensureLegendBar(wrapper, barCanvas, options = {}) {
    const {
        ticks = [],
        topCaption = '',
        bottomCaption = '',
        extras = [],
        barTop,
        barHeight,
    } = options;
    const effectiveBarHeight = (Number.isFinite(barHeight) && barHeight > 0)
        ? Math.round(barHeight)
        : BAR_HEIGHT;
    const requestedTop = Number.isFinite(barTop) ? Math.round(barTop) : LEGEND_TOP;
    // Container.top must offset upward by the topCaption's height when
    // present so the BAR's top — not the container's top — lines up with
    // the requested barTop (which callers set to the grid's y).
    const containerTop = topCaption
        ? requestedTop - TOP_CAPTION_OFFSET
        : requestedTop;

    let container = wrapper.querySelector('.heatmap-legend-bar');
    if (!container) {
        container = document.createElement('div');
        container.className = 'heatmap-legend-bar';
        wrapper.appendChild(container);
    }
    // Always (re-)apply container styles so dynamic top updates from
    // the chart's `finished`-event handler take effect on every call.
    container.style.cssText = `
        position: absolute;
        top: ${containerTop}px;
        right: ${LEGEND_RIGHT_OFFSET}px;
        display: flex;
        flex-direction: column;
        align-items: stretch;
        gap: 2px;
        z-index: 10;
        pointer-events: none;
    `;
    container.innerHTML = '';

    // Top caption (diff mode only)
    if (topCaption) {
        const top = document.createElement('div');
        top.className = 'heatmap-caption-top';
        top.style.cssText = `
            ${FONTS.cssFootnote}
            font-size: 10px;
            color: ${COLORS.fgSecondary};
            text-align: right;
            margin-bottom: 2px;
        `;
        top.textContent = topCaption;
        container.appendChild(top);
    }

    // Bar + ticks row. Right-aligned within the container so a wider
    // extras row (e.g. histogram_heatmap's "Raw count" checkbox) can
    // expand the container's width without pushing the bar leftward —
    // the bar always hugs the container's right edge.
    const body = document.createElement('div');
    body.className = 'heatmap-legend-body';
    body.style.cssText = `
        display: flex;
        align-items: stretch;
        justify-content: flex-end;
    `;

    // CSS-scale the source canvas to the effective bar height. The
    // gradient is smooth so up/down-scaling produces no visible
    // artifacts; rebuilding the canvas at the exact display height
    // would buy nothing.
    const bar = document.createElement('canvas');
    bar.width = barCanvas.width;
    bar.height = barCanvas.height;
    bar.style.cssText = `width: ${BAR_WIDTH}px; height: ${effectiveBarHeight}px; display: block; flex: none;`;
    bar.getContext('2d').drawImage(barCanvas, 0, 0);
    body.appendChild(bar);

    // Tick column to the right of the bar.
    const tickCol = document.createElement('div');
    tickCol.className = 'heatmap-legend-ticks';
    tickCol.style.cssText = `
        position: relative;
        width: 40px;
        height: ${effectiveBarHeight}px;
        margin-left: ${LABEL_GAP}px;
    `;
    for (const t of ticks) {
        const pos = Math.max(0, Math.min(1, t.pos));
        // pos=0 means bottom (min); pos=1 means top (max). Convert to
        // CSS top offset where 0 = top of column.
        const topPx = (1 - pos) * (effectiveBarHeight - 1);

        const row = document.createElement('div');
        row.className = 'heatmap-legend-tick';
        row.style.cssText = `
            position: absolute;
            left: 0;
            right: 0;
            top: ${topPx}px;
            display: flex;
            align-items: center;
            transform: translateY(-50%);
        `;

        const mark = document.createElement('span');
        mark.style.cssText = `
            display: inline-block;
            width: ${TICK_MARK_LEN}px;
            height: 1px;
            background: ${COLORS.fgSecondary};
            flex: none;
        `;
        row.appendChild(mark);

        const label = document.createElement('span');
        label.className = 'heatmap-legend-tick-label';
        label.style.cssText = `
            ${FONTS.cssFootnote}
            margin-left: ${LABEL_GAP}px;
            color: ${COLORS.fgSecondary};
            white-space: nowrap;
            line-height: 1;
        `;
        label.textContent = t.label || '';
        row.appendChild(label);

        tickCol.appendChild(row);
    }
    body.appendChild(tickCol);
    container.appendChild(body);

    // Bottom caption (diff mode only)
    if (bottomCaption) {
        const bot = document.createElement('div');
        bot.className = 'heatmap-caption-bottom';
        bot.style.cssText = `
            ${FONTS.cssFootnote}
            font-size: 10px;
            color: ${COLORS.fgSecondary};
            text-align: right;
            margin-top: 2px;
        `;
        bot.textContent = bottomCaption;
        container.appendChild(bot);
    }

    if (extras.length) {
        const extrasRow = document.createElement('div');
        extrasRow.className = 'heatmap-legend-extras';
        extrasRow.style.cssText = `
            display: flex;
            flex-direction: column;
            align-items: flex-start;
            margin-top: 6px;
            pointer-events: auto;
        `;
        for (const el of extras) extrasRow.appendChild(el);
        container.appendChild(extrasRow);
    }

    return container;
}
