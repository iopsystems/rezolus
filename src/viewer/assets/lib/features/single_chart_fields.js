// Pure helpers for SingleChartView's inline title / description /
// unit-system editors. Kept separate from explorers.js so the unit
// tests can import them without pulling Chart + echarts through the
// node loader.

/** Unit-system override options exposed in the SingleChartView selector. */
export const UNIT_OPTIONS = [
    { value: '', label: 'Auto (none)' },
    { value: 'count', label: 'Count' },
    { value: 'rate', label: 'Rate (/s)' },
    { value: 'time', label: 'Time (ns)' },
    { value: 'bytes', label: 'Bytes' },
    { value: 'datarate', label: 'Data Rate (B/s)' },
    { value: 'bitrate', label: 'Bit Rate (bps)' },
    { value: 'percentage', label: 'Percentage (0–1 → %)' },
    { value: 'frequency', label: 'Frequency (Hz)' },
];

/**
 * Build a `format` object for the given unit-system override, or
 * `undefined` if no override (caller should fall back to the plot's
 * existing format). Mirrors `buildFormatOverride` from origin/main.
 */
export const buildFormatOverride = (unit) => {
    if (!unit) return undefined;
    const fmt = { unit_system: unit, precision: 2 };
    if (unit === 'percentage') fmt.range = { min: 0, max: 1 };
    return fmt;
};

/** Editor state seeded from a target plot's existing opts. */
export const seedFieldsFromPlot = (plot) => ({
    title: plot?.opts?.title ?? '',
    description: plot?.opts?.description ?? '',
    unitOverride: plot?.opts?.format?.unit_system ?? '',
});

/**
 * Build the rendered spec from a base plot + edited fields. The
 * unit-override falls back to the plot's existing `format` when no
 * override is selected so heatmaps/percentile axes keep their
 * defaults until the user explicitly overrides them.
 */
export const applyFieldsToSpec = (basePlot, fields) => {
    const override = buildFormatOverride(fields.unitOverride);
    return {
        ...basePlot,
        opts: {
            ...(basePlot?.opts || {}),
            title: fields.title,
            description: fields.description,
            format: override ?? basePlot?.opts?.format,
        },
    };
};
