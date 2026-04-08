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
 * On first call, builds: [minLabel] [colorBar] [maxLabel] + any extraElements,
 * appends the container to `wrapper`, and returns element references.
 *
 * On subsequent calls (same wrapper), returns existing element references.
 *
 * @param {HTMLElement} wrapper - parent element (chart-wrapper)
 * @param {HTMLCanvasElement} barCanvas - pre-rendered gradient canvas
 * @param {HTMLElement[]} [extraElements] - additional elements appended after maxLabel
 * @returns {{ minLabel: HTMLElement, maxLabel: HTMLElement }}
 */
export function ensureLegendBar(wrapper, barCanvas, extraElements) {
    let container = wrapper.querySelector('.heatmap-legend-bar');
    if (container) {
        return {
            minLabel: container.querySelector('.heatmap-label-min'),
            maxLabel: container.querySelector('.heatmap-label-max'),
        };
    }

    container = document.createElement('div');
    container.className = 'heatmap-legend-bar';
    container.style.cssText = `
        position: absolute;
        top: ${BAR_TOP}px;
        right: 16px;
        display: flex;
        align-items: center;
        gap: ${LABEL_GAP}px;
        z-index: 10;
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

    container.appendChild(minLabel);
    container.appendChild(bar);
    container.appendChild(maxLabel);
    if (extraElements) {
        for (const el of extraElements) container.appendChild(el);
    }
    wrapper.appendChild(container);

    return { minLabel, maxLabel };
}
