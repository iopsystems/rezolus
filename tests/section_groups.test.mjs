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

// An archive of two hosts of one run: the built-in sections render once per
// host, the second host's routes keyed `@h2`, every entry naming its host.
const twoHosts = [
    { name: 'Overview', route: '/overview', instance: 'h1' },
    { name: 'svc', route: '/service/svc' },
    { name: 'Exceptions', route: '/exceptions', instance: 'h1' },
    { name: 'Query Explorer', route: '/query' },
    { name: 'CPU', route: '/cpu', instance: 'h1' },
    { name: 'cgroups', route: '/cgroups', instance: 'h1' },
    { name: 'Overview', route: '/overview@h2', instance: 'h2' },
    { name: 'Exceptions', route: '/exceptions@h2', instance: 'h2' },
    { name: 'CPU', route: '/cpu@h2', instance: 'h2' },
    { name: 'cgroups', route: '/cgroups@h2', instance: 'h2' },
];

test('each recording of a combined archive gets its own group', () => {
    const g = groupSections(twoHosts, false);
    assert.equal(g.queryExplorer.route, '/query');
    assert.equal(g.overview, undefined);
    assert.deepEqual(g.samplers, []);
    assert.deepEqual(g.services.map((s) => s.route), ['/service/svc']);
    assert.deepEqual(
        g.instances.map((i) => [i.name, i.overview.route, i.exceptions.route,
            i.samplers.map((s) => s.route)]),
        [
            ['h1', '/overview', '/exceptions', ['/cpu', '/cgroups']],
            ['h2', '/overview@h2', '/exceptions@h2', ['/cpu@h2', '/cgroups@h2']],
        ],
    );
});

test('compare mode hides every instance of the cgroup section', () => {
    const g = groupSections(twoHosts, true);
    assert.deepEqual(
        g.instances.map((i) => i.samplers.map((s) => s.route)),
        [['/cpu'], ['/cpu@h2']],
    );
});

test('one recording has no instance groups', () => {
    assert.deepEqual(groupSections(sections, false).instances, []);
});
