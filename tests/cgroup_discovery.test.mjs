// Cgroup discovery for the cgroups view's selector (cgroup_discovery.js):
// cgroup_cpu_usage first, then the union of the other per-cgroup series when
// a recording has none of it (cpu_usage with cgroup_attribution off).
import test from 'node:test';
import assert from 'node:assert/strict';
import { discoverCgroups, discoverEveryCapture } from '../src/viewer/assets/lib/features/cgroup_discovery.js';

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

test('a comparison lists the cgroups of every capture', async () => {
    // Two captures, each with a cgroup the other lacks, and a third whose
    // every query is refused.
    const names = await discoverEveryCapture([
        engine({ cgroup_cpu_usage: ['/a', '/b'] }),
        engine({ cgroup_cpu_usage: ['/b', '/c'] }),
        async () => { throw new Error('refused'); },
    ]);
    assert.deepEqual([...names].sort(), ['/a', '/b', '/c']);
});

test('a capture without cgroup_cpu_usage is searched by the other series', async () => {
    // The baseline has cgroup_cpu_usage; the experiment recorded with
    // cgroup_attribution off for cpu_usage, so only cgroup_cpu_cycles names
    // its cgroups. The baseline's answer to the first query must not end
    // the experiment's search.
    const names = await discoverEveryCapture([
        engine({ cgroup_cpu_usage: ['/a'] }),
        engine({ cgroup_cpu_cycles: ['/a', '/only-experiment'] }),
    ]);
    assert.deepEqual([...names].sort(), ['/a', '/only-experiment']);
});
