// color_legend.js — shared DOM color legend bar for heatmap chart types.
//
// Renders [minLabel] [gradient bar] [maxLabel] in a flex row, optionally
// followed by extra elements (e.g. a checkbox toggle).

import { COLORS, FONTS } from './base.js';

// Bar geometry constants
export const BAR_WIDTH = 120;
export const BAR_HEIGHT = 10;
export const BAR_TOP = 47;
export const LABEL_GAP = 4;

/**
 * Build a gradient bar canvas from a color function.
 * @param {function} colorFn - maps a value in 0..1 to a CSS color string
 * @returns {HTMLCanvasElement}
 */
export function buildGradientCanvas(colorFn) {
    const canvas = document.createElement('canvas');
    canvas.width = BAR_WIDTH;
    canvas.height = BAR_HEIGHT;
    const ctx = canvas.getContext('2d');
    for (let x = 0; x < BAR_WIDTH; x++) {
        ctx.fillStyle = colorFn(x / (BAR_WIDTH - 1));
        ctx.fillRect(x, 0, 1, BAR_HEIGHT);
    }
    return canvas;
}

/**
 * Create or reuse the legend bar container.
 *
 * On first call, builds:
 *
 *   ┌──────────── optional caption row ────────────┐
 *   │ [leftCaption]                  [rightCaption] │
 *   ├──────────────── legend row ───────────────────┤
 *   │ [minLabel]  [colorBar]  [maxLabel] [extras]   │
 *   └───────────────────────────────────────────────┘
 *
 * The caption row spans the same width as the legend row below, so the
 * left caption sits directly above minLabel's left edge and the right
 * caption sits directly above maxLabel's right edge.
 *
 * On subsequent calls (same wrapper), repaints the gradient from the
 * (possibly new) barCanvas and returns existing element references. The
 * repaint lets callers swap palettes (e.g. diff-mode diverging palette)
 * without tearing down the container.
 *
 * @param {HTMLElement} wrapper - parent element (Chart component's own
 *     div; owned by Mithril so Mithril removes the legend on unmount)
 * @param {HTMLCanvasElement} barCanvas - pre-rendered gradient canvas
 * @param {HTMLElement[]} [extraElements] - additional elements appended after maxLabel
 * @returns {{ minLabel, maxLabel, leftCaption, rightCaption }} -
 *     leftCaption/rightCaption are always present (empty by default);
 *     callers that want a caption set their textContent.
 */
export function ensureLegendBar(wrapper, barCanvas, extraElements) {
    let container = wrapper.querySelector('.heatmap-legend-bar');
    if (container) {
        const bar = container.querySelector('canvas');
        if (bar && barCanvas) bar.getContext('2d').drawImage(barCanvas, 0, 0);
        return {
            minLabel: container.querySelector('.heatmap-label-min'),
            maxLabel: container.querySelector('.heatmap-label-max'),
            leftCaption: container.querySelector('.heatmap-caption-left'),
            rightCaption: container.querySelector('.heatmap-caption-right'),
            captionRow: container.querySelector('.heatmap-caption-row'),
        };
    }

    container = document.createElement('div');
    container.className = 'heatmap-legend-bar';
    // BAR_TOP lines up the GRADIENT ROW's top edge with the previous
    // single-row layout's position; the caption row (when visible) sits
    // above via negative margin so the gradient stays put whether or
    // not captions are rendered.
    container.style.cssText = `
        position: absolute;
        top: ${BAR_TOP}px;
        right: 16px;
        display: flex;
        flex-direction: column;
        align-items: stretch;
        gap: 2px;
        z-index: 10;
    `;

    // Caption row: same width as the legend row below, so left/right
    // captions anchor to minLabel's left edge and maxLabel's right edge.
    // Defaults to display: none — callers that want a caption set text
    // on leftCaption/rightCaption and toggle captionRow.style.display.
    const captionRow = document.createElement('div');
    captionRow.className = 'heatmap-caption-row';
    captionRow.style.cssText = `
        display: none;
        justify-content: space-between;
        align-items: center;
        ${FONTS.cssFootnote}
        font-size: 10px;
        color: ${COLORS.fgSecondary};
        pointer-events: none;
        margin-top: -14px;
    `;
    const leftCaption = document.createElement('span');
    leftCaption.className = 'heatmap-caption-left';
    const rightCaption = document.createElement('span');
    rightCaption.className = 'heatmap-caption-right';
    captionRow.appendChild(leftCaption);
    captionRow.appendChild(rightCaption);

    // Legend row: [min] [gradient] [max] [extras]
    const legendRow = document.createElement('div');
    legendRow.style.cssText = `
        display: flex;
        align-items: center;
        gap: ${LABEL_GAP}px;
    `;

    const minLabel = document.createElement('span');
    minLabel.className = 'heatmap-label-min';
    minLabel.style.cssText = `${FONTS.cssFootnote} color: ${COLORS.fgSecondary}; pointer-events: none;`;

    const bar = document.createElement('canvas');
    bar.width = barCanvas.width;
    bar.height = barCanvas.height;
    bar.style.cssText = `width: ${BAR_WIDTH}px; height: ${BAR_HEIGHT}px; display: block;`;
    bar.getContext('2d').drawImage(barCanvas, 0, 0);

    const maxLabel = document.createElement('span');
    maxLabel.className = 'heatmap-label-max';
    maxLabel.style.cssText = `${FONTS.cssFootnote} color: ${COLORS.fgSecondary}; pointer-events: none;`;

    legendRow.appendChild(minLabel);
    legendRow.appendChild(bar);
    legendRow.appendChild(maxLabel);
    if (extraElements) {
        for (const el of extraElements) legendRow.appendChild(el);
    }

    container.appendChild(captionRow);
    container.appendChild(legendRow);
    wrapper.appendChild(container);

    return { minLabel, maxLabel, leftCaption, rightCaption, captionRow };
}
