import test from 'node:test';
import assert from 'node:assert/strict';
import { ViewerApi } from '../src/viewer/assets/lib/viewer_api.js';
import { ViewerApi as WasmViewerApi } from '../site/viewer/lib/viewer_api.js';

test('ViewerApi exposes getSections', () => {
    assert.equal(typeof ViewerApi.getSections, 'function');
});

test('WASM ViewerApi exposes getSections', () => {
    assert.equal(typeof WasmViewerApi.getSections, 'function');
});
