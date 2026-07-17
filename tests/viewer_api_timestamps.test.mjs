import test from 'node:test';
import assert from 'node:assert/strict';
import { ViewerApi } from '../src/viewer/assets/lib/viewer_api.js';
import { ViewerApi as WasmViewerApi } from '../site/viewer/lib/viewer_api.js';

test('ViewerApi exposes getTimestamps', () => {
    assert.equal(typeof ViewerApi.getTimestamps, 'function');
});

test('WASM ViewerApi exposes getTimestamps', () => {
    assert.equal(typeof WasmViewerApi.getTimestamps, 'function');
});
