// Pure (DOM-free, Mithril-free) selection-migration helpers.
//
// Kept separate from `selection.js` so Node-based tests can import the
// migration logic without dragging in Mithril, localStorage, or other
// browser-only globals. Re-exported from `selection.js` for callers
// that already pull their selection API from one module.

export const SELECTION_SCHEMA_VERSION = 2;

/**
 * Return a fresh v2-shaped selection default. Used when the input is
 * null/undefined and as a template for migration fill-ins.
 */
export const defaultSelection = () => ({
    version: SELECTION_SCHEMA_VERSION,
    tagline: '',
    entries: [],
    zoom: null,
    stepOverride: null,
    anchors: { baseline: 0, experiment: 0 },
    chartToggles: {},
});

/**
 * Migrate a parsed selection object (as loaded from localStorage or
 * from an imported JSON payload) to the current schema version.
 *
 * Guarantees on the returned object:
 *   - version === SELECTION_SCHEMA_VERSION
 *   - anchors has numeric `baseline` and `experiment` keys
 *   - chartToggles is a plain object (possibly empty)
 *
 * Callers may pass `null`/`undefined` to get a default selection
 * instead of an empty migration result.
 */
export const migrateSelection = (sel) => {
    if (sel == null) return defaultSelection();
    const out = { ...sel };
    if (!out.version || out.version < 2) {
        if (!out.anchors || typeof out.anchors !== 'object') {
            out.anchors = { baseline: 0, experiment: 0 };
        } else {
            out.anchors = {
                baseline: Number(out.anchors.baseline) || 0,
                experiment: Number(out.anchors.experiment) || 0,
            };
        }
        if (!out.chartToggles || typeof out.chartToggles !== 'object') {
            out.chartToggles = {};
        }
        out.version = SELECTION_SCHEMA_VERSION;
    }
    return out;
};
