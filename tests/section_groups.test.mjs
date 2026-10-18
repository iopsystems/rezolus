// The sidebar groups the navigation by route, so a recording's section
// template that renames the Overview or Exceptions keeps them in their
// places.
import test from 'node:test';
import assert from 'node:assert/strict';
import { groupSections } from '../src/viewer/assets/lib/ui/section_groups.js';

const sections = [
    { name: 'Summary', route: '/overview' },
    { name: 'vllm', route: '/service/vllm' },
    { name: 'Problems', route: '/exceptions' },
    { name: 'Query Explorer', route: '/query' },
    { name: 'Processors', route: '/cpu' },
    { name: 'cgroups', route: '/cgroups' },
    { name: 'source: app', route: '/source/app' },
];

test('built-in sections are found by route, whatever their names', () => {
    const g = groupSections(sections, false);
    assert.equal(g.overview.name, 'Summary');
    assert.equal(g.exceptions.name, 'Problems');
    assert.equal(g.queryExplorer.route, '/query');
    assert.deepEqual(g.services.map((s) => s.route), ['/service/vllm']);
    assert.deepEqual(g.metrics.map((s) => s.route), ['/source/app']);
    assert.deepEqual(g.samplers.map((s) => s.route), ['/cpu', '/cgroups']);
});

test('a section named like a built-in is a sampler section', () => {
    const g = groupSections([{ name: 'Overview', route: '/cpu' }], false);
    assert.equal(g.overview, undefined);
    assert.deepEqual(g.samplers.map((s) => s.route), ['/cpu']);
});

test('compare mode hides the cgroup section', () => {
    const g = groupSections(sections, true);
    assert.deepEqual(g.samplers.map((s) => s.route), ['/cpu']);
});
