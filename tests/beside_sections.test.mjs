// A recording shown beside a comparison: its sections render as one
// capture (`single_capture` in their metadata), and the badge names it
// after the arms (`besideNames`, from the sections that carry an instance).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isSingleCapture, besideNames } from '../src/viewer/assets/lib/features/group_utils.js';

test('a section marked single_capture renders as one capture', () => {
    assert.equal(isSingleCapture({ single_capture: 'h' }), true);
    assert.equal(isSingleCapture({}), false);
    assert.equal(isSingleCapture(undefined), false);
});

test('the badge names each beside recording once, in nav order', () => {
    const sections = [
        { route: '/cpu', instance: null },
        { route: '/cpu@h', instance: 'h' },
        { route: '/memory@h', instance: 'h' },
        { route: '/cpu@g', instance: 'g' },
        { route: '/memory' },
    ];
    assert.deepEqual(besideNames(sections), ['h', 'g']);
    assert.deepEqual(besideNames(undefined), []);
});
