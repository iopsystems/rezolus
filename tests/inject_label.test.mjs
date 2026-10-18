// injectLabel adds a label matcher to every metric reference in a query:
// the service instance and node pickers filter a chart this way.
import test from 'node:test';
import assert from 'node:assert/strict';
import { injectLabel } from '../src/viewer/assets/lib/data.js';

test('a named selector gains the matcher', () => {
    assert.equal(
        injectLabel('sum(irate(cache_hits{source="cachecannon"}[5s]))', 'instance', 'i-1'),
        'sum(irate(cache_hits{source="cachecannon",instance="i-1"}[5s]))',
    );
});

test('a bare metric and a range selector gain one', () => {
    assert.equal(
        injectLabel('sum(irate(cpu_usage[5m])) / cpu_cores', 'node', 'web-01'),
        'sum(irate(cpu_usage{node="web-01"}[5m])) / cpu_cores{node="web-01"}',
    );
});

test('a selector named by __name__ gains the matcher', () => {
    // The valkey template names `redis/total_commands_processed`, which is
    // not a PromQL identifier, this way.
    assert.equal(
        injectLabel(
            'sum(irate({__name__="redis/total_commands_processed",source="valkey"}[5s]))',
            'instance',
            'i-1',
        ),
        'sum(irate({__name__="redis/total_commands_processed",source="valkey",instance="i-1"}[5s]))',
    );
});

test('label names in a grouping clause are left alone', () => {
    assert.equal(
        injectLabel('sum by (id) (irate(x{a="b"}[1m]))', 'node', 'n'),
        'sum by (id) (irate(x{a="b",node="n"}[1m]))',
    );
});

test('braces inside a string literal are left alone', () => {
    assert.equal(
        injectLabel('label_replace(cpu_usage, "dst", "{$1}", "src", "(.*)")', 'node', 'n'),
        'label_replace(cpu_usage{node="n"}, "dst", "{$1}", "src", "(.*)")',
    );
});

test('a section scope restricts the queries the page builds', async () => {
    const { applyScope } = await import('../src/viewer/assets/lib/data.js');
    assert.equal(
        applyScope('sum by (name) (cgroup_cpu_usage)', [
            { label: 'host', op: '=', value: 'h2' },
            { label: 'role', op: '!=', value: 'x' },
        ]),
        'sum by (name) (cgroup_cpu_usage{host="h2",role!="x"})',
    );
    assert.equal(applyScope('cgroup_cpu_usage', undefined), 'cgroup_cpu_usage');
});
