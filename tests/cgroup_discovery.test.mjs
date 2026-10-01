// Cgroup discovery for the cgroups view's selector (cgroup_discovery.js):
// cgroup_cpu_usage first, then the union of the other per-cgroup series when
// a recording has none of it (cpu_usage with cgroup_attribution off).
import test from 'node:test';
import assert from 'node:assert/strict';
import { discoverCgroups } from '../src/viewer/assets/lib/features/cgroup_discovery.js';

const result = (...names) => ({
    status: 'success',
    data: { resultType: 'vector', result: names.map((name) => ({ metric: { name }, value: [0, '1'] })) },
});
const empty = { status: 'success', data: { resultType: 'vector', result: [] } };

// A fake query engine: each metric named in a query answers with its names.
const engine = (byMetric, log = []) => async (query) => {
    log.push(query);
    for (const [metric, names] of Object.entries(byMetric)) {
        if (new RegExp(`\\b${metric}\\b`).test(query)) {
            if (names === 'throw') throw new Error('metric not found');
            return result(...names);
        }
    }
    return empty;
};

test('cgroup_cpu_usage answers alone when the recording has it', async () => {
    const log = [];
    const names = await discoverCgroups(engine({
        cgroup_cpu_usage: ['/a', '/b'],
        cgroup_syscall: ['/c'],
    }, log));
    assert.deepEqual([...names].sort(), ['/a', '/b']);
    assert.equal(log.length, 1, 'stops at the first query that finds names');
});

test('without cgroup_cpu_usage the other per-cgroup series are unioned', async () => {
    const names = await discoverCgroups(engine({
        cgroup_cpu_usage: 'throw',
        cgroup_syscall: ['/a', '/b'],
        cgroup_cpu_throttled: ['/b', '/c'],
    }));
    assert.deepEqual([...names].sort(), ['/a', '/b', '/c']);
});

test('a recording with no per-cgroup series finds none', async () => {
    const names = await discoverCgroups(engine({}));
    assert.equal(names.size, 0);
});
