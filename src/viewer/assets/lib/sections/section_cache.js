const createSectionCacheState = () => ({
    responses: {},
    sections: [],
    // Per-section summary `{ total, withData }` kept alive across
    // response-cache evictions. The response cache itself is capped
    // (defaults to 3 entries) so a long session evicts older section
    // bodies; this map only stores two integers per section, so it
    // can persist for the life of the viewer and feed the sidebar's
    // "gray out empty sections" affordance.
    sectionStatus: {},
});

const getSections = (state) => state.sections || [];

const storeSharedSections = (state, sections) => {
    state.sections = Array.isArray(sections) ? sections : [];
};

const setSectionCacheLimit = (state, limit) => {
    state.limit = Math.max(1, limit | 0);
};

const pinSectionKey = (state, key) => {
    state.pinned = state.pinned || new Set();
    state.pinned.add(key);
};

const storeSectionResponse = (state, key, data) => {
    if (Array.isArray(data?.sections) && data.sections.length > 0) {
        state.sections = data.sections;
    }

    if (!data || typeof data !== 'object') {
        state.responses[key] = data;
        return data;
    }

    const { sections, ...stored } = data;
    state.responses[key] = stored;

    // Evict the oldest non-pinned, non-just-inserted entry. If everything
    // is pinned or only the just-inserted key is unpinned, the loop falls
    // through and the cache is allowed to exceed `limit` — pinning is a
    // hard guarantee, not a hint.
    if (state.limit && Object.keys(state.responses).length > state.limit) {
        const pinned = state.pinned || new Set();
        for (const k of Object.keys(state.responses)) {
            if (k !== key && !pinned.has(k)) {
                delete state.responses[k];
                break;
            }
        }
    }

    return stored;
};

const withSharedSections = (state, data) => {
    if (!data || typeof data !== 'object') return data;
    if (Array.isArray(data.sections)) return data;
    return {
        ...data,
        sections: getSections(state),
    };
};

/// Update the persistent per-section status entry. Call this from
/// `processDashboardData` after plot stripping so `total` reflects
/// the *renderable* plot count — same number the sidebar shows in
/// `(N)` and what `total === 0` means for "gray me out". `withData`
/// is the count of plots with non-empty time-series data; kept for
/// possible future affordances (e.g. distinguishing "no data yet,
/// poll again" from "no plots at all").
const recordSectionStatus = (state, key, status) => {
    if (!state.sectionStatus) state.sectionStatus = {};
    state.sectionStatus[key] = status;
};

const getSectionStatus = (state, key) =>
    (state.sectionStatus || {})[key] || null;

const clearSectionResponses = (state) => {
    Object.keys(state.responses).forEach((key) => delete state.responses[key]);
};

const clearNonServiceResponses = (state) => {
    Object.keys(state.responses).forEach((key) => {
        if (!key.startsWith('service/')) {
            delete state.responses[key];
        }
    });
};

const resetSectionCacheState = (state) => {
    clearSectionResponses(state);
    state.sections = [];
};

export {
    createSectionCacheState,
    storeSectionResponse,
    storeSharedSections,
    getSections,
    withSharedSections,
    clearSectionResponses,
    clearNonServiceResponses,
    resetSectionCacheState,
    setSectionCacheLimit,
    pinSectionKey,
    recordSectionStatus,
    getSectionStatus,
};
