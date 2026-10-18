// The page mounts its own views (the query explorer and the client-only
// sections) by route. A recording's section template can name a section
// anything, so a section named like a client view at another route is an
// ordinary section whose groups render.
import test from 'node:test';
import assert from 'node:assert/strict';
import { clientViewFor, CLIENT_SECTIONS } from '../src/viewer/assets/lib/ui/client_views.js';

test('every client route has its view', () => {
    assert.equal(clientViewFor('/query'), 'query');
    assert.equal(clientViewFor('/systeminfo'), 'systeminfo');
    assert.equal(clientViewFor('/metadata'), 'metadata');
    assert.equal(clientViewFor('/notebook'), 'notebook');
    assert.equal(clientViewFor('/selection'), 'selection');
    assert.equal(clientViewFor('/report'), 'report');
});

test('the descriptors route to their own views', () => {
    for (const [view, section] of Object.entries(CLIENT_SECTIONS)) {
        assert.equal(clientViewFor(section.route), view);
    }
});

test('a section named like a client view elsewhere renders its groups', () => {
    for (const { name } of Object.values(CLIENT_SECTIONS)) {
        assert.equal(clientViewFor(`/${name.toLowerCase().replace(' ', '')}x`), null, name);
    }
    assert.equal(clientViewFor('/nb'), null);
    assert.equal(clientViewFor('/overview'), null);
    assert.equal(clientViewFor('/cpu'), null);
    assert.equal(clientViewFor('/service/vllm'), null);
});
