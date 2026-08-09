// Shade-meaning legend ("swatches") for charts that draw translucent band
// fills. Explains what each SHADE means — not which series is which color
// (that's the tooltip/legend's job). Three families of fill can appear, and
// two of them mean fundamentally different things:
//
//   inner/outer  — value SPREAD within a decimated display bucket
//                  (typical p25–p75 range / hard min–max extremes)
//   bounds       — measurement PRECISION: worst-case rate bounds from
//                  sample-acquisition timing (#1017), a hard interval,
//                  not a statistical confidence band
//
// `chartSwatches` is pure (node-tested); `renderSwatchRow` injects the DOM.

const finite = (v) => Number.isFinite(v);

// Any index where a strictly-wider pair exists — a fully collapsed band
// (native resolution, or a flat signal) draws nothing, so it gets no swatch.
const anyWidth = (loArr, hiArr) => {
    if (!loArr || !hiArr) return false;
    const n = Math.min(loArr.length, hiArr.length);
    for (let i = 0; i < n; i++) {
        if (finite(loArr[i]) && finite(hiArr[i]) && hiArr[i] > loArr[i]) return true;
    }
    return false;
};

const pLabel = (q) => {
    const pct = q * 100;
    return 'p' + (Number.isInteger(pct) ? pct : pct.toFixed(1).replace(/\.0$/, ''));
};

const TITLES = {
    median: 'Each display pixel spans many samples; the line is their median, so one outlier cannot drag it.',
    inner: 'Middle spread of the samples within each display bucket — how much the value typically varied.',
    outer: 'Per-bucket min–max range: keeps brief spikes visible that downsampling would otherwise hide.',
    bounds: 'Worst-case bounds on the value from sample-acquisition timing. A hard interval, not a statistical confidence interval.',
};

/**
 * Decide which shade-meaning entries a chart's rendered data implies.
 *
 * `boxplot`: decoded display series (data.js decodeDisplayBinary) or null.
 * `intervals`: per-series `[[lo,hi]|null, …]` arrays for the native-resolution
 * uncertainty band (line.js buildBandSeries input).
 *
 * Returns `[{kind, label, title}]` ordered inside-out from the line
 * (median, inner, outer, bounds); empty when no shade is actually visible.
 */
export function chartSwatches({ boxplot = null, intervals = [] } = {}) {
    const out = [];
    const series = Array.isArray(boxplot) ? boxplot : [];

    const inner = series.some((s) => anyWidth(s.lo, s.hi));
    const outer = series.some((s) => anyWidth(s.min, s.max));
    let bounds = series.some((s) => anyWidth(s.uncLo, s.uncHi));

    if (inner || outer) {
        out.push({ kind: 'median', label: 'median', title: TITLES.median });
    }
    if (inner) {
        // Label from the wire's band quantiles so a custom --band renders honestly.
        const band = series.find((s) => Array.isArray(s.band))?.band || [0.25, 0.75];
        out.push({
            kind: 'inner',
            label: `typical (${pLabel(band[0])}–${pLabel(band[1])})`,
            title: TITLES.inner,
        });
    }
    if (outer) {
        out.push({ kind: 'outer', label: 'extremes (min–max)', title: TITLES.outer });
    }

    // Native-resolution path: uncertainty band drawn from per-point intervals.
    if (!bounds) {
        bounds = (intervals || []).some((iv) =>
            Array.isArray(iv) && iv.some((p) => Array.isArray(p) && finite(p[0]) && finite(p[1]) && p[1] > p[0]));
    }
    if (bounds) {
        out.push({ kind: 'bounds', label: 'timing bounds', title: TITLES.bounds });
    }
    return out;
}

// Extra grid height (px) reserved under the x-axis labels when a swatch row is
// shown; charts add this to grid.bottom so the row doesn't overlap tick labels.
export const SWATCH_ROW_HEIGHT = 16;

const ROW_CLASS = 'band-swatches';

/**
 * Create/update/remove the swatch row inside a chart's DOM node. Idempotent —
 * safe to call on every (re)configure. Pass an empty list to remove.
 */
export function renderSwatchRow(domNode, swatches) {
    if (!domNode) return;
    let el = domNode.querySelector(':scope > .' + ROW_CLASS);
    if (!swatches || swatches.length === 0) {
        el?.remove();
        return;
    }
    if (!el) {
        el = document.createElement('div');
        el.className = ROW_CLASS;
        domNode.appendChild(el);
    }
    el.innerHTML = swatches.map((s) =>
        `<span class="swatch" title="${s.title}">`
        + `<span class="swatch-chip swatch-${s.kind}"></span>${s.label}</span>`,
    ).join('');
}
