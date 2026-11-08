import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { substituteCgroupPattern } from '../src/viewer/assets/lib/data.js';

// #1289: the aggregate plots are the sum over the cgroups not selected.

// Every cgroup query the dashboard generates, so a new query shape is covered.
// Written by crates/dashboard's cgroup_queries_fixture_matches_the_generator.
const queries = JSON.parse(readFileSync(new URL('./fixtures/cgroup_queries.json', import.meta.url)));
const aggregate = queries.filter((q) => q.includes('name!~"__SELECTED_CGROUPS__"'));
const individual = queries.filter((q) => q.includes('name=~"__SELECTED_CGROUPS__"'));

test('the section has both aggregate and individual cgroup queries', () => {
    assert.ok(aggregate.length > 5, `aggregate queries: ${aggregate.length}`);
    assert.ok(individual.length > 5, `individual queries: ${individual.length}`);
});

test('a selection is excluded from the aggregate and selected in the individual plots', () => {
    const pattern = '(/system.slice/ssh.service|/init.scope)';
    for (const q of aggregate) {
        const out = substituteCgroupPattern(q, pattern);
        assert.ok(out.includes(`name!~"${pattern}"`), out);
        assert.ok(!out.includes('__SELECTED_CGROUPS__'), out);
    }
    for (const q of individual) {
        const out = substituteCgroupPattern(q, pattern);
        assert.ok(out.includes(`name=~"${pattern}"`), out);
    }
});

test('with nothing selected the aggregate covers every cgroup and keeps its other matchers', () => {
    for (const q of aggregate) {
        const out = substituteCgroupPattern(q, null);
        assert.ok(!out.includes('__SELECTED_CGROUPS__'), out);
        assert.ok(!/name!~/.test(out), out);
        assert.ok(!/\{\s*,|,\s*\}|\{\s*\}/.test(out), `malformed selector: ${out}`);
    }
    assert.equal(
        substituteCgroupPattern('sum(irate(cgroup_cpu_usage{state="user",name!~"__SELECTED_CGROUPS__"}[5m]))', null),
        'sum(irate(cgroup_cpu_usage{state="user"}[5m]))',
    );
    assert.equal(
        substituteCgroupPattern('sum(irate(cgroup_cpu_usage{name!~"__SELECTED_CGROUPS__"}[5m]))', null),
        'sum(irate(cgroup_cpu_usage[5m]))',
    );
});

test('a name!~ matcher that is not the placeholder is kept', () => {
    const q = 'sum(irate(cgroup_cpu_usage{name!~"/init.scope",state="user"}[5m]))';
    assert.equal(substituteCgroupPattern(q, null), q);
});
