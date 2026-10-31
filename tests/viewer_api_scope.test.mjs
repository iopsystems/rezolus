// A query's scope reaches the backend as both transports send it: the
// WASM adapter passes it to the registry (null for none), and the server
// adapter puts it in the display query's URL (absent for none). An empty
// array or a value of another type is refused before it is sent.
import test from 'node:test';
import assert from 'node:assert/strict';
import { ViewerApi } from '../src/viewer/assets/lib/viewer_api.js';
import { ViewerApi as WasmViewerApi } from '../site/viewer/lib/viewer_api.js';

// A registry that records the scope argument of each query.
const fakeRegistry = () => {
    const scopes = [];
    return {
        scopes,
        has: () => true,
        query_range: (...args) => {
            scopes.push(args[6]);
            return '{"status":"success","data":{"resultType":"matrix","result":[]}}';
        },
        query_range_display: (...args) => {
            scopes.push(args[8]);
            return new Uint8Array(8);
        },
    };
};

test('the WASM adapter passes a scope to the registry', async () => {
    const registry = fakeRegistry();
    WasmViewerApi.setRegistry(registry);
    await WasmViewerApi.queryRange('up', 0, 1, 1);
    await WasmViewerApi.queryRange('up', 0, 1, 1, 'baseline', undefined, 'grid', ['a', 'b']);
    await WasmViewerApi.queryRange('up', 0, 1, 1, 'baseline', undefined, 'grid', 'c');
    await WasmViewerApi.queryRangeDisplay('up', 0, 1, 1, { scope: ['u'] });
    await WasmViewerApi.queryRangeDisplay('up', 0, 1, 1, {});
    assert.deepEqual(registry.scopes, [null, 'a,b', 'c', 'u', null]);
    for (const bad of [[], 5, {}]) {
        await assert.rejects(
            WasmViewerApi.queryRange('up', 0, 1, 1, 'baseline', undefined, 'grid', bad),
            TypeError,
        );
    }
});

test('the server adapter puts a display query scope in its URL', async () => {
    const urls = [];
    const saved = globalThis.fetch;
    globalThis.fetch = async (url) => {
        urls.push(new URL(url, 'http://viewer'));
        return { ok: true, headers: { get: () => 'application/octet-stream' }, arrayBuffer: async () => new ArrayBuffer(8) };
    };
    try {
        await ViewerApi.queryRangeDisplay('up', 0, 1, 1, { scope: ['u1', 'u2'] });
        await ViewerApi.queryRangeDisplay('up', 0, 1, 1, {});
        await assert.rejects(ViewerApi.queryRangeDisplay('up', 0, 1, 1, { scope: [] }), TypeError);
    } finally {
        globalThis.fetch = saved;
    }
    assert.equal(urls.length, 2);
    assert.equal(urls[0].searchParams.get('scope'), 'u1,u2');
    assert.equal(urls[1].searchParams.has('scope'), false);
});
