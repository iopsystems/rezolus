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
 * One anchor value, in the v3 shape: a finite number (a signed ms offset
 * from the capture's recording start), or `{ kind }` (align on the first
 * event of that kind in the capture). Anything else is `0`, "no shift".
 *
 * Still v3: an older viewer reading `{ kind }` does `Number(v) || 0` and
 * lands on the start, which is this form's own fallback; a schema bump
 * would make it refuse the whole payload instead.
 */
export const normalizeAnchor = (v) => {
    if (typeof v === 'number') return Number.isFinite(v) ? v : 0;
    if (typeof v === 'string' && v.trim() !== '' && Number.isFinite(Number(v))) return Number(v);
    if (v && typeof v === 'object' && typeof v.kind === 'string' && v.kind.trim()) {
        return { kind: v.kind.trim() };
    }
    return 0;
};

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
        // Every key is kept (a multi-recording archive names its captures),
        // each value normalized; the two A/B slots always exist.
        const anchors = {};
        for (const [id, v] of Object.entries(out.anchors)) {
            if (id) anchors[id] = normalizeAnchor(v);
        }
        if (!('baseline' in anchors)) anchors.baseline = 0;
        if (!('experiment' in anchors)) anchors.experiment = 0;
        out.anchors = anchors;
    }
    if (!out.chartToggles || typeof out.chartToggles !== 'object') {
        out.chartToggles = {};
    }
    return out;
};
