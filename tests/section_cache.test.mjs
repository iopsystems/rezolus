import test from 'node:test';
import assert from 'node:assert/strict';
import {
    createSectionCacheState,
    storeSectionResponse,
    storeSharedSections,
    getSections,
    withSharedSections,
    setSectionCacheLimit,
    pinSectionKey,
    clearSectionResponses,
    clearNonServiceResponses,
} from '../src/viewer/assets/lib/section_cache.js';

test('storeSectionResponse strips duplicated sections and preserves shared section metadata', () => {
    const state = createSectionCacheState();
    const overview = {
        groups: [{ id: 'cpu' }],
        sections: [
            { name: 'Overview', route: '/overview' },
            { name: 'CPU', route: '/cpu' },
        ],
        interval: 1,
    };

    const storedOverview = storeSectionResponse(state, 'overview', overview);

    assert.equal(state.responses.overview, storedOverview);
    assert.deepEqual(getSections(state), overview.sections);
    assert.equal('sections' in storedOverview, false);
    assert.deepEqual(withSharedSections(state, storedOverview).sections, overview.sections);
});

test('storeSectionResponse reuses the shared sections list for later payloads without embedded sections', () => {
    const state = createSectionCacheState();
    storeSectionResponse(state, 'overview', {
        groups: [],
        sections: [{ name: 'Overview', route: '/overview' }],
    });

    const storedCpu = storeSectionResponse(state, 'cpu', {
        groups: [{ id: 'busy' }],
        interval: 1,
    });

    assert.equal('sections' in storedCpu, false);
    assert.deepEqual(withSharedSections(state, storedCpu).sections, [
        { name: 'Overview', route: '/overview' },
    ]);
});

test('shared sections can be bootstrapped without storing a section body', () => {
    const state = createSectionCacheState();
    storeSharedSections(state, [
        { name: 'Overview', route: '/overview' },
        { name: 'CPU', route: '/cpu' },
    ]);

    assert.deepEqual(getSections(state), [
        { name: 'Overview', route: '/overview' },
        { name: 'CPU', route: '/cpu' },
    ]);
    assert.deepEqual(state.responses, {});
});

test('withSharedSections uses bootstrapped metadata for lean section payloads', () => {
    const state = createSectionCacheState();
    storeSharedSections(state, [{ name: 'Overview', route: '/overview' }]);
    const stitched = withSharedSections(state, { groups: [] });
    assert.deepEqual(stitched.sections, [{ name: 'Overview', route: '/overview' }]);
});

test('clearSectionResponses preserves the bootstrapped sections nav list', () => {
    // Lazy section payloads don't embed `sections`, so dropping it here
    // leaves nothing to restore it.
    const state = createSectionCacheState();
    storeSharedSections(state, [
        { name: 'Overview', route: '/overview' },
        { name: 'CPU', route: '/cpu' },
    ]);
    storeSectionResponse(state, 'cpu', { groups: [{ id: 'busy' }] });
    assert.deepEqual(Object.keys(state.responses), ['cpu']);

    clearSectionResponses(state);

    assert.deepEqual(state.responses, {});
    assert.deepEqual(getSections(state), [
        { name: 'Overview', route: '/overview' },
        { name: 'CPU', route: '/cpu' },
    ]);
});

test('clearNonServiceResponses drops stock entries but keeps service entries and nav', () => {
    const state = createSectionCacheState();
    storeSharedSections(state, [{ name: 'CPU', route: '/cpu' }]);
    storeSectionResponse(state, 'cpu', { groups: [] });
    storeSectionResponse(state, 'memory', { groups: [] });
    storeSectionResponse(state, 'service/vllm', { groups: [] });
    storeSectionResponse(state, 'service/sglang', { groups: [] });

    clearNonServiceResponses(state);

    assert.equal(state.responses.cpu, undefined);
    assert.equal(state.responses.memory, undefined);
    assert.deepEqual(
        Object.keys(state.responses).sort(),
        ['service/sglang', 'service/vllm'],
    );
    assert.deepEqual(getSections(state), [{ name: 'CPU', route: '/cpu' }]);
});

test('bounded section cache evicts oldest non-pinned section', () => {
    const state = createSectionCacheState();
    storeSharedSections(state, [{ name: 'Overview', route: '/overview' }]);
    setSectionCacheLimit(state, 2);
    pinSectionKey(state, 'overview');
    storeSectionResponse(state, 'overview', { groups: [] });
    storeSectionResponse(state, 'cpu', { groups: [] });
    storeSectionResponse(state, 'memory', { groups: [] });
    assert.equal(state.responses.overview.groups.length, 0);
    assert.equal(state.responses.memory.groups.length, 0);
    assert.equal(state.responses.cpu, undefined);
});
