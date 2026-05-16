// Pure (DOM-free, Mithril-free) selection-schema validator.
//
// Lives separately from selection.js so Node tests can exercise it
// without pulling in Mithril or localStorage. Re-exported from
// selection.js for callers that pull all selection APIs from one
// module.

export const SELECTION_SCHEMA_VERSION = 3;

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
 * Validate a parsed selection payload against the current schema (v3).
 *
 * Returns a normalized v3 object on success. Throws on unsupported
 * older versions — pre-v3 payloads (v1 unversioned, v2) are not
 * migrated, by design (see spec non-goals).
 *
 * Pass null/undefined to get a fresh default selection.
 */
export const migrateSelection = (sel) => {
    if (sel == null) return defaultSelection();
    const version = Number(sel.version) || 1;
    if (version !== SELECTION_SCHEMA_VERSION) {
        throw new Error(
            `unsupported selection schema version ${version} ` +
            `(expected ${SELECTION_SCHEMA_VERSION}); ` +
            `re-export from the original session with a v${SELECTION_SCHEMA_VERSION} viewer`,
        );
    }
    const out = { ...sel };
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
    return out;
};
