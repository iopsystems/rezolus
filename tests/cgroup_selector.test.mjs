import test from 'node:test';
import assert from 'node:assert/strict';
// The selector imports the chart palette; supply its CSS reads in Node.
globalThis.document = { documentElement: {} };
globalThis.getComputedStyle = () => ({ getPropertyValue: () => '#000000' });
const { CgroupSelector } = await import('../src/viewer/assets/lib/features/cgroup_selector.js');

const result = (names) => ({
    status: 'success',
    data: { result: names.map(name => ({ metric: { name } })) },
});

test('cgroup discovery includes memory-only groups even when CPU groups exist', async () => {
    globalThis.m = { redraw() {} };
    const vnode = {
        attrs: {
            executeQuery: async (query) => result(query.includes('cgroup_memory_usage')
                ? ['/shared', '/idle'] : ['/shared', '/cpu-only']),
        },
        state: { selectedCgroups: new Set() },
    };
    await CgroupSelector.fetchAvailableCgroups(vnode);
    assert.deepEqual([...vnode.state.availableCgroups].sort(), ['/cpu-only', '/idle', '/shared']);
    assert.equal(vnode.state.loading, false);
    assert.equal(vnode.state.error, undefined);
});

test('cgroup discovery works with only the memory sampler enabled', async () => {
    globalThis.m = { redraw() {} };
    const vnode = {
        attrs: {
            executeQuery: async (query) => result(query.includes('cgroup_memory_usage') ? ['/memory-only'] : []),
        },
        state: { selectedCgroups: new Set() },
    };
    await CgroupSelector.fetchAvailableCgroups(vnode);
    assert.deepEqual([...vnode.state.availableCgroups], ['/memory-only']);
    assert.equal(vnode.state.error, undefined);
});
