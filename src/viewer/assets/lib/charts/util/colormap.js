/**
 * Centralized color definitions for the viewer's charts.
 *
 * Every color literal used in chart rendering should be defined here
 * with a meaningful name. No raw hex/rgba strings in charting code.
 *
 * Theme-sensitive tokens (fg, bg, borders) are read from CSS custom properties
 * at access time so they automatically reflect the active light/dark theme.
 */

// ── CSS variable reader ──────────────────────────────────────────────

function cssVar(name) {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

// ── Design tokens ────────────────────────────────────────────────────

// Chart series colors are theme-invariant (same vivid colors on both themes).
const SERIES_COLORS = {
    chartBlue: '#3b82f6',
    chartCyan: '#06b6d4',
    chartTeal: '#14b8a6',
    chartGreen: '#22c55e',
    chartLime: '#84cc16',
    chartYellow: '#eab308',
    chartOrange: '#f97316',
    chartRed: '#ef4444',
    clamped: '#ef4444',
    chartPink: '#ec4899',
    chartPurple: '#8b5cf6',
};

export const COLORS = new Proxy(SERIES_COLORS, {
    get(target, prop) {
        // Return series colors directly (theme-invariant)
        if (prop in target) return target[prop];

        // Map property names to CSS custom properties
        const cssMap = {
            fg:               '--fg',
            fgSecondary:      '--fg-secondary',
            fgLabel:          '--fg-muted',
            fgMuted:          '--fg-muted',
            fgSubtle:         '--fg-subtle',
            accent:           '--accent',
            accentEmphasis:   '--accent-emphasis',
            accentMuted:      '--accent-muted',
            accentSubtle:     '--accent-subtle',
            accentGlow:       '--accent-glow',
            accentAreaTop:    '--chart-area-top',
            accentAreaMid:    '--chart-area-mid',
            accentAreaBottom: '--chart-area-bottom',
            bgVoid:           '--bg-void',
            bgCard:           '--bg-card',
            bgTertiary:       '--bg-tertiary',
            bgElevated:       '--bg-elevated',
            borderSubtle:     '--border-subtle',
            borderMuted:      '--border-default',
            borderDefault:    '--border-default',
            gridLine:         '--chart-grid-line',
            shadow:           '--chart-shadow',
            shadowStrong:     '--chart-shadow-strong',
        };

        if (cssMap[prop]) return cssVar(cssMap[prop]);
        return undefined;
    },
});

// ── Palettes ─────────────────────────────────────────────────────────

/** Default 10-color palette for echarts `color` option */
export const CHART_PALETTE = [
    COLORS.chartBlue,
    COLORS.chartCyan,
    COLORS.chartTeal,
    COLORS.chartGreen,
    COLORS.chartLime,
    COLORS.chartYellow,
    COLORS.chartOrange,
    COLORS.chartRed,
    COLORS.chartPink,
    COLORS.chartPurple,
];

/** 5-color subset for percentile scatter charts */
export const SCATTER_PALETTE = [
    COLORS.accent,
    COLORS.chartCyan,
    COLORS.chartTeal,
    COLORS.chartGreen,
    COLORS.chartPurple,
];

// ── Heatmap colormaps ────────────────────────────────────────────────

/**
 * Interpolate through an RGB ramp.
 * @param {Array<Array<number>>} ramp - array of [r,g,b] stops
 * @param {number} t - 0..1
 * @returns {string} `rgb(r,g,b)`
 */
function interpolateRamp(ramp, t) {
    const idx = t * (ramp.length - 1);
    const i = Math.floor(idx);
    const f = idx - i;
    const clampIdx = Math.min(i, ramp.length - 1);
    const c0 = ramp[clampIdx];
    const c1 = ramp[Math.min(clampIdx + 1, ramp.length - 1)];
    const hasAlpha = c0.length === 4;
    const r = Math.round(c0[0] + f * (c1[0] - c0[0]));
    const g = Math.round(c0[1] + f * (c1[1] - c0[1]));
    const b = Math.round(c0[2] + f * (c1[2] - c0[2]));
    if (!hasAlpha) return `rgb(${r},${g},${b})`;
    const a = c0[3] + f * (c1[3] - c0[3]);
    return `rgba(${r},${g},${b},${a.toFixed(3)})`;
}

/**
 * Parse a hex color array into an RGB(A) ramp for interpolateRamp.
 * When `alphas` is supplied, its length must match `colors` — each hex
 * stop is paired with the matching alpha, yielding rgba interpolation.
 */
function hexToRgbRamp(colors, alphas) {
    return colors.map((hex, i) => {
        const stop = [
            parseInt(hex.slice(1, 3), 16),
            parseInt(hex.slice(3, 5), 16),
            parseInt(hex.slice(5, 7), 16),
        ];
        if (alphas) stop.push(alphas[i]);
        return stop;
    });
}

/** Viridis hex ramp (darkest stops removed for visibility on dark bg) */
export const VIRIDIS_COLORS = [
    '#472d7b', '#3b528b', '#2c728e',
    '#23898e', '#2ab07f', '#4ec36b',
    '#a2da37', '#fde725',
];

const VIRIDIS_RGB = hexToRgbRamp(VIRIDIS_COLORS);

/**
 * Viridis colormap — interpolates through the RGB ramp.
 * @param {number} t - 0..1
 * @returns {string} `rgb(r,g,b)`
 */
export function viridisColor(t) {
    return interpolateRamp(VIRIDIS_RGB, t);
}

/** Inferno hex ramp (darkest stops removed for visibility on dark bg) */
const INFERNO_COLORS = [
    '#4a0c6b', '#781c6d', '#a52c60',
    '#cf4446', '#ed6925', '#fb9b06', '#f7d13d', '#fcffa4',
];

const INFERNO_RGB = hexToRgbRamp(INFERNO_COLORS);

/**
 * Inferno colormap — interpolates through the RGB ramp.
 * @param {number} t - 0..1
 * @returns {string} `rgb(r,g,b)`
 */
export function infernoColor(t) {
    return interpolateRamp(INFERNO_RGB, t);
}

// ── Compare-mode palette ─────────────────────────────────────────────

/**
 * Diverging blue→neutral→green scale for compare-mode diff heatmaps.
 * Caller maps values symmetrically around 0 (−absMax..0..+absMax).
 * Baseline-heavy cells read blue, experiment-heavy cells read green.
 */
export const DIVERGING_BLUE_GREEN = [
    '#2E5BFF', '#6A8BFF', '#A5BBFF', '#D5DFFF',
    '#F2F2F2',
    '#CFEBD7', '#9ED6B2', '#5FBD83', '#00C46A',
];

/**
 * V-shaped alpha curve matched to DIVERGING_BLUE_GREEN's 9 stops.
 * Neutral sits at 0.1 so cells near zero-diff fade into whatever
 * background they're painted against (white on light theme, near-black
 * on dark theme). Extremes ramp to 0.9 so the interesting values stay
 * saturated and demand attention.
 */
export const DIVERGING_BLUE_GREEN_ALPHA_V = [
    0.9, 0.8, 0.7, 0.6,
    0.5,
    0.6, 0.7, 0.8, 0.9,
];

/** Theme-aware null cell color for diff heatmaps — distinct from zero. */
export const NULL_CELL_COLOR_DARK = 'rgba(255,255,255,0.04)';
export const NULL_CELL_COLOR_LIGHT = 'rgba(0,0,0,0.04)';

/** Returns the null cell color for the active theme. */
export function nullCellColor(isDark) {
    return isDark ? NULL_CELL_COLOR_DARK : NULL_CELL_COLOR_LIGHT;
}

/**
 * Resample a diverging palette (blue…neutral…green, with neutral at the
 * palette's own midpoint) so that value=0 lands on the neutral color
 * regardless of whether the data range is symmetric around zero.
 *
 * echarts' `visualMap.inRange.color` samples the returned array linearly
 * across [min, max]. Without resampling, an asymmetric range like [0, 10]
 * would map 0 → fully-blue and 5 → neutral, which is wrong for a signed
 * diff. With the remapping below:
 *   - value < 0  → lower half of the palette (blue side)
 *   - value = 0  → neutral
 *   - value > 0  → upper half of the palette (green side)
 * For one-sided ranges ([0, max] or [min, 0]), the result is just the
 * relevant half of the palette.
 *
 * @param {string[]} palette - diverging hex palette (odd length; middle = neutral).
 * @param {number} min
 * @param {number} max
 * @param {number} [sampleCount] - number of output stops (defaults to 21).
 * @returns {string[]} resampled hex/rgb color array for echarts `inRange.color`.
 */
export function resampleDivergingForRange(palette, min, max, sampleCount = 21, alphas) {
    if (!Array.isArray(palette) || palette.length === 0 || !(max > min)) {
        return palette;
    }
    const ramp = hexToRgbRamp(palette, alphas);
    const clamp = (t) => Math.max(0, Math.min(1, t));
    // Map value to the palette's native t in [0,1]:
    //   min >= 0  → only the upper half of the palette (neutral..green)
    //   max <= 0  → only the lower half (blue..neutral)
    //   straddling → zero always lands at t=0.5 (neutral)
    const valueToT = (value) => {
        if (min >= 0) return 0.5 + 0.5 * (value / max);
        if (max <= 0) return 0.5 * (1 - value / min);
        if (value < 0) return 0.5 * (1 - value / min);
        if (value > 0) return 0.5 + 0.5 * (value / max);
        return 0.5;
    };
    const out = [];
    for (let i = 0; i < sampleCount; i++) {
        const frac = i / (sampleCount - 1);
        const value = min + frac * (max - min);
        out.push(interpolateRamp(ramp, clamp(valueToT(value))));
    }
    return out;
}

// ── Cgroup color mapper ──────────────────────────────────────────────

/**
 * ColorMapper provides consistent color assignment for cgroups across charts.
 * Uses a deterministic hash so the same cgroup name always gets the same color.
 * The "Other" category always gets fgMuted gray.
 */
export class ColorMapper {
    constructor() {
        this.colorMap = new Map();

        // Wider palette optimized for dark backgrounds.
        // Green first to differentiate from the blue aggregate charts on the left.
        this.colorPalette = [
            COLORS.chartGreen,
            COLORS.chartOrange,
            COLORS.chartPurple,
            COLORS.chartCyan,
            COLORS.chartRed,
            COLORS.chartLime,
            COLORS.chartPink,
            COLORS.chartYellow,
            COLORS.chartTeal,
            '#818cf8', // Indigo
            COLORS.chartBlue,
            '#38bdf8', // Sky blue
            '#34d399', // Emerald
            '#facc15', // Yellow
            '#fb923c', // Light orange
            '#e879f9', // Fuchsia
            '#c084fc', // Violet
            '#22d3ee', // Cyan bright
            '#4ade80', // Light green
            '#fca5a1', // Light coral
        ];
    }

    stringToHash(str) {
        let hash = 0;
        for (let i = 0; i < str.length; i++) {
            const char = str.charCodeAt(i);
            hash = ((hash << 5) - hash) + char;
            hash = hash & hash;
        }
        return Math.abs(hash);
    }

    getColorByName(cgroupName) {
        if (cgroupName === 'Other') {
            return COLORS.fgMuted;
        }

        if (this.colorMap.has(cgroupName)) {
            return this.colorMap.get(cgroupName);
        }

        const hash = this.stringToHash(cgroupName);
        const color = this.colorPalette[hash % this.colorPalette.length];
        this.colorMap.set(cgroupName, color);
        return color;
    }
}

const globalColorMapper = new ColorMapper();
export default globalColorMapper;
