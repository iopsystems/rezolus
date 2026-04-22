// Compare-mode chart adapter.
//
// Public entry: renderCompareChart({container, spec, captures, anchors,
// toggles, onToggle}).
//
//   captures = [
//     { id: 'baseline',   result, duration },
//     { id: 'experiment', result, duration },
//   ]
//
// Dispatches on `spec.opts.style` and delegates to existing per-type
// renderers, composing multiple sibling renders into a subgroup container
// where side-by-side layouts are needed. Owns timestamp translation to
// relative time, null propagation for diff math, and the intersection
// rule for multi/scatter labels.

import { toRelative, nullDiff, intersectLabels, longerDuration } from './util/compare_math.js';
import { DIVERGING_BLUE_GREEN, nullCellColor } from './util/colormap.js';

export const BASELINE_COLOR = '#2E5BFF';
export const EXPERIMENT_COLOR = '#00C46A';

/**
 * Format a relative offset in milliseconds as `+Xs`, `+XmYs`, or `+XhYm`.
 */
export const relativeTimeFormatter = (ms) => {
    const totalSec = Math.round(ms / 1000);
    const sign = totalSec < 0 ? '-' : '+';
    const s = Math.abs(totalSec);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = s % 60;
    if (h > 0) return `${sign}${h}h${m}m`;
    if (m > 0) return `${sign}${m}m${sec}s`;
    return `${sign}${sec}s`;
};

/**
 * Dispatch on chart style and delegate to the matching strategy.
 * The caller is responsible for issuing the two per-capture queries and
 * supplying the resolved results. Strategies mutate `container` only.
 */
export const renderCompareChart = (opts) => {
    const style = opts.spec?.opts?.style;
    switch (style) {
        case 'line':              return overlayLine(opts);
        case 'heatmap':           return sideBySideHeatmap(opts);
        case 'multi':             return splitMultiToSubgroup(opts);
        case 'scatter':           return splitScatterToSubgroup(opts);
        case 'histogram_heatmap': return sideBySideHistogramHeatmap(opts);
        default:
            // Unknown style: fall back to baseline-only render (future tasks
            // may extend this). Returning false signals "not handled".
            return false;
    }
};

// ── Strategy stubs — filled in by subsequent tasks ───────────────────

const overlayLine = (_opts) => {
    throw new Error('compare.js: overlayLine not yet implemented (Task 15)');
};

const sideBySideHeatmap = (_opts) => {
    throw new Error('compare.js: sideBySideHeatmap not yet implemented (Task 17)');
};

const splitMultiToSubgroup = (_opts) => {
    throw new Error('compare.js: splitMultiToSubgroup not yet implemented (Task 20)');
};

const splitScatterToSubgroup = (_opts) => {
    throw new Error('compare.js: splitScatterToSubgroup not yet implemented (Task 20)');
};

const sideBySideHistogramHeatmap = (_opts) => {
    throw new Error('compare.js: sideBySideHistogramHeatmap not yet implemented (Task 21)');
};

// Re-export utilities consumed by strategies.
export { toRelative, nullDiff, intersectLabels, longerDuration, DIVERGING_BLUE_GREEN, nullCellColor };
