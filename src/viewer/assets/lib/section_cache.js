const createSectionCacheState = () => ({
    responses: {},
    sections: [],
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

const clearSectionResponses = (state) => {
    Object.keys(state.responses).forEach((key) => delete state.responses[key]);
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
    resetSectionCacheState,
    setSectionCacheLimit,
    pinSectionKey,
};
