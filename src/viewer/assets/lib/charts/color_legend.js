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
// use so the plot area leaves room for the legend bar + tick labels:
//   right:8 (offset)  +  10 (bar)  +  4 (tick mark)  +  4 (gap)
//   +  ~32 (label width)  =  ~58, rounded up to 64.
export const BAR_WIDTH = 10;
export const BAR_HEIGHT = 150;
export const LEGEND_RIGHT_OFFSET = 8;
export const LEGEND_GRID_RIGHT = 64;
export const LEGEND_TOP = 50;
export const LABEL_GAP = 4;
const TICK_MARK_LEN = 4;

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
 * @returns {HTMLElement} the legend container
 */
export function ensureLegendBar(wrapper, barCanvas, options = {}) {
    const { ticks = [], topCaption = '', bottomCaption = '', extras = [] } = options;

    let container = wrapper.querySelector('.heatmap-legend-bar');
    if (!container) {
        container = document.createElement('div');
        container.className = 'heatmap-legend-bar';
        container.style.cssText = `
            position: absolute;
            top: ${LEGEND_TOP}px;
            right: ${LEGEND_RIGHT_OFFSET}px;
            display: flex;
            flex-direction: column;
            align-items: stretch;
            gap: 2px;
            z-index: 10;
            pointer-events: none;
        `;
        wrapper.appendChild(container);
    }
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

    // Bar + ticks row
    const body = document.createElement('div');
    body.className = 'heatmap-legend-body';
    body.style.cssText = `
        display: flex;
        align-items: stretch;
    `;

    const bar = document.createElement('canvas');
    bar.width = barCanvas.width;
    bar.height = barCanvas.height;
    bar.style.cssText = `width: ${BAR_WIDTH}px; height: ${BAR_HEIGHT}px; display: block; flex: none;`;
    bar.getContext('2d').drawImage(barCanvas, 0, 0);
    body.appendChild(bar);

    // Tick column to the right of the bar.
    const tickCol = document.createElement('div');
    tickCol.className = 'heatmap-legend-ticks';
    tickCol.style.cssText = `
        position: relative;
        width: 40px;
        height: ${BAR_HEIGHT}px;
        margin-left: ${LABEL_GAP}px;
    `;
    for (const t of ticks) {
        const pos = Math.max(0, Math.min(1, t.pos));
        // pos=0 means bottom (min); pos=1 means top (max). Convert to
        // CSS top offset where 0 = top of column.
        const topPx = (1 - pos) * (BAR_HEIGHT - 1);

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
