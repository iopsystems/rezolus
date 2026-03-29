/**
 * Centralized color definitions for the viewer's charts.
 *
 * Every color literal used in chart rendering should be defined here
 * with a meaningful name. No raw hex/rgba strings in charting code.
 */

// ── Design tokens ────────────────────────────────────────────────────

export const COLORS = {
    // Foreground hierarchy (text, labels)
    fg: '#e6edf3',
    fgSecondary: '#8b949e',
    fgLabel: '#6a7b8f',       // subtitles, gradient bar labels, inactive toggles
    fgMuted: '#484f58',
    fgSubtle: '#30363d',

    // Accent colors (electric blue family)
    accent: '#58a6ff',
    accentEmphasis: '#79c0ff',
    accentMuted: 'rgba(56, 139, 253, 0.4)',
    accentSubtle: 'rgba(56, 139, 253, 0.15)',
    accentGlow: 'rgba(56, 139, 253, 0.25)',

    // Line chart area fill gradient (accent blue at decreasing opacity)
    accentAreaTop: 'rgba(88, 166, 255, 0.2)',
    accentAreaMid: 'rgba(88, 166, 255, 0.08)',
    accentAreaBottom: 'rgba(88, 166, 255, 0.01)',

    // Backgrounds
    bgVoid: '#05080d',
    bgCard: '#0d1117',
    bgTertiary: '#161b22',
    bgElevated: '#1c2128',

    // Borders
    borderSubtle: 'rgba(48, 54, 61, 0.4)',
    borderMuted: 'rgba(48, 54, 61, 0.6)',   // tooltip footer separator
    borderDefault: 'rgba(48, 54, 61, 0.7)',

    // Grid lines
    gridLine: 'rgba(48, 54, 61, 0.5)',

    // Shadows
    shadow: 'rgba(0, 0, 0, 0.4)',           // tooltip box shadow
    shadowStrong: 'rgba(0, 0, 0, 0.5)',     // heatmap hover emphasis

    // Chart series colors — curated palette
    chartBlue: '#58a6ff',
    chartCyan: '#39d5ff',
    chartTeal: '#2dd4bf',
    chartGreen: '#3fb950',
    chartLime: '#a3e635',
    chartYellow: '#fbbf24',
    chartOrange: '#f97316',
    chartRed: '#f85149',
    chartPink: '#f472b6',
    chartPurple: '#a78bfa',
};

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

    if (i >= ramp.length - 1) {
        return `rgb(${ramp[ramp.length - 1].join(',')})`;
    }

    const c0 = ramp[i];
    const c1 = ramp[i + 1];
    const r = Math.round(c0[0] + f * (c1[0] - c0[0]));
    const g = Math.round(c0[1] + f * (c1[1] - c0[1]));
    const b = Math.round(c0[2] + f * (c1[2] - c0[2]));

    return `rgb(${r},${g},${b})`;
}

/** Viridis hex ramp for echarts visualMap (darkest stops removed for visibility on dark bg) */
export const VIRIDIS_COLORS = [
    '#414487', '#2a788e', '#22a884',
    '#7ad151', '#fde725',
];

/** Viridis RGB ramp for custom renderItem (darkest stops removed) */
const VIRIDIS_RGB = [
    [65, 68, 135],
    [42, 120, 142],
    [34, 168, 132],
    [122, 209, 81],
    [253, 231, 37],
];

/**
 * Viridis colormap — interpolates through the RGB ramp.
 * @param {number} t - 0..1
 * @returns {string} `rgb(r,g,b)`
 */
export function viridisColor(t) {
    return interpolateRamp(VIRIDIS_RGB, t);
}

/** Inferno hex ramp for echarts visualMap (darkest stops removed for visibility on dark bg) */
export const INFERNO_COLORS = [
    '#4a0c6b', '#781c6d', '#a52c60',
    '#cf4446', '#ed6925', '#fb9b06', '#f7d13d', '#fcffa4',
];

/** Inferno RGB ramp for custom renderItem (darkest stops removed) */
const INFERNO_RGB = [
    [74, 12, 107],
    [120, 28, 109],
    [165, 44, 96],
    [207, 68, 70],
    [237, 105, 37],
    [251, 155, 6],
    [247, 209, 61],
    [252, 255, 164],
];

/**
 * Inferno colormap — interpolates through the RGB ramp.
 * @param {number} t - 0..1
 * @returns {string} `rgb(r,g,b)`
 */
export function infernoColor(t) {
    return interpolateRamp(INFERNO_RGB, t);
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
