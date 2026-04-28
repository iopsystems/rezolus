import test from 'node:test';
import assert from 'node:assert/strict';
import {
    createSectionCacheState,
    storeSectionResponse,
    storeSharedSections,
    getSections,
    withSharedSections,
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
