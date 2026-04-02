/**
 * Centralized font definitions for the viewer's charts.
 *
 * Every font family, size, and weight used in chart rendering should be
 * defined here with a meaningful name. No raw font literals in charting code.
 */

const MONO = '"JetBrains Mono", "SF Mono", monospace';
const SANS = '"Inter", -apple-system, sans-serif';

export const FONTS = {
    mono: MONO,
    sans: SANS,

    // ECharts textStyle objects — spread directly into config
    title:            { fontFamily: MONO, fontSize: 13, fontWeight: 600 },
    subtitle:         { fontFamily: SANS, fontSize: 11, fontWeight: 'normal' },
    axisLabel:        { fontFamily: MONO, fontSize: 10 },
    legend:           { fontFamily: MONO, fontSize: 11 },
    tooltipBody:      { fontFamily: SANS, fontSize: 12 },
    tooltipTimestamp:  { fontFamily: MONO, fontSize: 11 },
    tooltipValue:     { fontFamily: MONO, fontSize: 12, fontWeight: 600 },
    tooltipLabel:     { fontSize: 12 },      // inherits family from container
    footnote:         { fontSize: 10 },      // freeze footer, gradient labels
    control:          { fontFamily: SANS, fontSize: 13 },

    // CSS shorthand for inline style= and DOM elements
    cssMono:     `font-family: ${MONO};`,
    cssSans:     `font-family: ${SANS};`,
    cssFootnote: `font: 10px ${SANS};`,
    cssControl:  `font: 13px ${SANS};`,

    // Bare font shorthand strings for ECharts graphic elements (no "font:" prefix or ";")
    footnoteFont: `10px ${SANS}`,
    controlFont:  `13px ${SANS}`,
};
