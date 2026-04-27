const createSectionCacheState = () => ({
    responses: {},
    sections: [],
});

const getSections = (state) => state.sections || [];

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
    getSections,
    withSharedSections,
    clearSectionResponses,
    resetSectionCacheState,
};
