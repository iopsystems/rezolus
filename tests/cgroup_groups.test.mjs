import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
    isCgroupGroupKey,
    cgroupTree,
    deriveCgroupGroups,
    resolveGroupKey,
    normalizeCgroupSelection,
    coveringGroup,
    addCgroupEntries,
    resolveCgroupSelection,
    cgroupQueryVariants,
    runCgroupVariants,
} from '../src/viewer/assets/lib/features/cgroup_selection.js';
import { substituteCgroupPattern, createDataApi, setDisplayMode, setStepOverride } from '../src/viewer/assets/lib/data.js';
import { pinnedCompares } from '../src/viewer/assets/lib/features/group_utils.js';

// #1544: a systemd unit with several cgroups below it (a libvirt guest's
// vCPU threads, a service's children) can be selected as one entry.
//
// Names are in the agent's form (process_cgroup_info in src/agent/bpf/mod.rs):
// a cgroup more than three levels deep is `.../<grandparent>/<parent>/<name>`,
// and `\x2d` is written `-`. A libvirt guest's vCPU threads are four deep.

const VM = '.../machine-qemu-2-vm1.scope';
const USER = '.../user@1000.service';
const NAMES = [
    `${VM}/libvirt/emulator`,
    `${VM}/libvirt/vcpu0`,
    `${VM}/libvirt/vcpu1`,
    '/',
    '/system.slice/docker-abc.scope',
    '/system.slice/ssh.service',
    '.../user-1000.slice/user@1000.service/init.scope',
    `${USER}/app.slice/a.scope`,
    `${USER}/app.slice/b.scope`,
    '.../app.slice/b.scope/x',
    '.../app.slice/b.scope/y',
];

test('a unit with two or more recorded cgroups that name it is a group', () => {
    const groups = deriveCgroupGroups(new Set(NAMES));
    assert.deepEqual([...groups.keys()].sort(), [
        `${VM}/`,
        '.../user-1000.slice/user@1000.service/',
        `${USER}/app.slice/b.scope/`,
    ]);
    assert.deepEqual(groups.get(`${VM}/`), NAMES.slice(0, 3));
    // The unit's truncated keys are one group: `.../user@1000.service/` is
    // merged into the key with more context.
    assert.deepEqual(groups.get('.../user-1000.slice/user@1000.service/'), [
        '.../user-1000.slice/user@1000.service/init.scope',
        `${USER}/app.slice/a.scope`,
        `${USER}/app.slice/b.scope`,
    ]);
    // b.scope's children are recorded without user@1000.service, so they are
    // in b.scope's group (under its key with the most context) and not in
    // the service's.
    assert.deepEqual(groups.get(`${USER}/app.slice/b.scope/`), [
        '.../app.slice/b.scope/x', '.../app.slice/b.scope/y', `${USER}/app.slice/b.scope`,
    ]);
});

test('the root cgroup is a name, not a group key', () => {
    assert.equal(isCgroupGroupKey('/'), false);
    assert.equal(isCgroupGroupKey(`${VM}/`), true);
    const groups = deriveCgroupGroups(new Set(NAMES));
    const r = resolveCgroupSelection(['/'], groups);
    assert.equal(r.leaf, '/');
    assert.equal(r.excluded, '/');
});

test("a guest's full and truncated keys merge when the scope's own cgroups are recorded", () => {
    const groups = deriveCgroupGroups([
        '/machine.slice/machine-qemu-2-vm1.scope/libvirt',
        `${VM}/libvirt/vcpu0`,
        `${VM}/libvirt/vcpu1`,
    ]);
    assert.deepEqual([...groups.keys()], ['/machine.slice/machine-qemu-2-vm1.scope/']);
    assert.equal(groups.get('/machine.slice/machine-qemu-2-vm1.scope/').length, 3);
});

test('a truncated key that two longer keys end with stays apart', () => {
    const groups = deriveCgroupGroups([
        '/a.slice/x.service/c1', '/a.slice/x.service/c2',
        '/b.slice/x.service/c1', '/b.slice/x.service/c2',
        '.../x.service/d/e1', '.../x.service/d/e2',
    ]);
    assert.ok(groups.has('.../x.service/'));
    assert.equal(groups.get('/a.slice/x.service/').length, 2);
});

test('the selector lists each group above its members', () => {
    const groups = deriveCgroupGroups(new Set(NAMES));
    const tree = cgroupTree(NAMES, groups).map(({ item, depth }) => `${'  '.repeat(depth)}${item}`);
    assert.deepEqual(tree, [
        `${VM}/`,
        `  ${VM}/libvirt/emulator`,
        `  ${VM}/libvirt/vcpu0`,
        `  ${VM}/libvirt/vcpu1`,
        '.../user-1000.slice/user@1000.service/',
        '  .../user-1000.slice/user@1000.service/init.scope',
        `  ${USER}/app.slice/a.scope`,
        `${USER}/app.slice/b.scope/`,
        '  .../app.slice/b.scope/x',
        '  .../app.slice/b.scope/y',
        `  ${USER}/app.slice/b.scope`,
        '/',
        '/system.slice/docker-abc.scope',
        '/system.slice/ssh.service',
    ]);
});

test("a unit's own cgroup sits under its group", () => {
    const names = ['/m.slice/x.scope', '/m.slice/x.scope/a', '/m.slice/x.scope-b'];
    const tree = cgroupTree(names, deriveCgroupGroups(names));
    assert.deepEqual(tree, [
        { item: '/m.slice/x.scope-b', depth: 0 },
        { item: '/m.slice/x.scope/', depth: 0 },
        { item: '/m.slice/x.scope', depth: 1 },
        { item: '/m.slice/x.scope/a', depth: 1 },
    ]);
});

test('a pinned chart that draws cgroup groups is not compared', () => {
    const spec = { promql_query: 'q' };
    assert.equal(pinnedCompares(true, { cgroup_queries: [{ query: 'q', label: null }] }, spec), true);
    assert.equal(pinnedCompares(true, { cgroup_queries: [{ query: 'q', label: `${VM}/` }] }, spec), false);
});

test("a unit's own cgroup is a member of its group", () => {
    const groups = deriveCgroupGroups(['/a.service', '/a.service/child']);
    assert.deepEqual(groups.get('/a.service/'), ['/a.service', '/a.service/child']);
});

test('a container whose cgroup is its scope, and a slice, are not groups', () => {
    const groups = deriveCgroupGroups(['/system.slice/docker-abc.scope', '/system.slice/ssh.service']);
    assert.equal(groups.size, 0);
});

test('selecting a group replaces its selected members and locks the rest', () => {
    const groups = deriveCgroupGroups(new Set(NAMES));
    let sel = addCgroupEntries(new Set(), [`${VM}/libvirt/vcpu0`, '/system.slice/ssh.service'], groups);
    sel = addCgroupEntries(sel, [`${VM}/`], groups);
    assert.deepEqual([...sel].sort(), [`${VM}/`, '/system.slice/ssh.service']);
    assert.equal(coveringGroup(`${VM}/libvirt/vcpu1`, sel, groups), `${VM}/`);
    assert.equal(coveringGroup('/system.slice/docker-abc.scope', sel, groups), null);
    // A locked member is not added.
    assert.deepEqual([...addCgroupEntries(sel, [`${VM}/libvirt/vcpu1`], groups)].sort(), [...sel].sort());
});

test('a nested group is locked by the group around it, and replaced by it', () => {
    const groups = deriveCgroupGroups([
        '/s.slice/a.service/b.scope', '/s.slice/a.service/c', '.../a.service/b.scope/x',
    ]);
    const outer = '/s.slice/a.service/';
    // b.scope's own cgroup and its child x: the child does not name a.service.
    const inner = '/s.slice/a.service/b.scope/';
    assert.deepEqual(groups.get(inner), ['.../a.service/b.scope/x', '/s.slice/a.service/b.scope']);
    // x names a.service (its grandparent), so the outer group holds all three.
    assert.equal(groups.get(outer).length, 3);
    const sel = addCgroupEntries(new Set([inner]), [outer], groups);
    assert.deepEqual([...sel], [outer]);
    assert.equal(coveringGroup(inner, sel, groups), outer);
    // The outer group is not covered by the inner one; selecting it
    // replaces the inner.
    assert.equal(coveringGroup(outer, new Set([inner]), groups), null);
});

test('a group that shares a cgroup with a selected group is blocked', () => {
    const groups = deriveCgroupGroups(new Set(NAMES));
    const svc = '.../user-1000.slice/user@1000.service/';
    const b = `${USER}/app.slice/b.scope/`;
    assert.equal(coveringGroup(b, new Set([svc]), groups), svc);
    assert.equal(coveringGroup(svc, new Set([b]), groups), b);
    assert.deepEqual([...addCgroupEntries(new Set([svc]), [b], groups)], [svc]);
});

test('a range that spans a group and its members selects the group', () => {
    const groups = deriveCgroupGroups(new Set(NAMES));
    const sel = addCgroupEntries(new Set(), [`${VM}/libvirt/vcpu0`, `${VM}/`, `${VM}/libvirt/vcpu1`], groups);
    assert.deepEqual([...sel], [`${VM}/`]);
});

test('a selection resolves to the individual, excluded and per-group patterns', () => {
    const groups = deriveCgroupGroups(new Set(NAMES));
    const r = resolveCgroupSelection(new Set([`${VM}/`, '/system.slice/ssh.service']), groups);
    assert.equal(r.leaf, '/system.slice/ssh.service');
    assert.equal(r.groups.length, 1);
    assert.equal(r.groups[0].key, `${VM}/`);
    assert.equal(r.groups[0].pattern, `(${NAMES.slice(0, 3).join('|')})`);
    assert.equal(r.excluded, `(${[...NAMES.slice(0, 3), '/system.slice/ssh.service'].sort().join('|')})`);
});

test('a group the recording lacks, or with a member that cannot be quoted, is left out', () => {
    const groups = deriveCgroupGroups(['/a.service/x', '/a.service/y|z']);
    assert.equal(resolveCgroupSelection(['/a.service/'], groups).groups.length, 0);
    assert.equal(resolveCgroupSelection(['/elsewhere.scope/'], groups).groups.length, 0);
    assert.equal(resolveCgroupSelection(['/elsewhere.scope/'], groups).excluded, null);
});

// Every individual query the dashboard generates.
const section = JSON.parse(readFileSync(new URL('../crates/dashboard/sections/rezolus/cgroups.json', import.meta.url)));
const queries = [];
(function walk(o) {
    if (Array.isArray(o)) o.forEach(walk);
    else if (o && typeof o === 'object') {
        if (typeof o.query === 'string' && o.query.includes('__SELECTED_CGROUPS__')) queries.push(o.query);
        Object.values(o).forEach(walk);
    }
})(section);
const individual = queries.filter((q) => q.includes('name=~"__SELECTED_CGROUPS__"'));
const aggregate = queries.filter((q) => q.includes('name!~"__SELECTED_CGROUPS__"'));

test('every individual query gives one summed query per group', () => {
    const groups = deriveCgroupGroups(new Set(NAMES));
    const sel = resolveCgroupSelection([`${VM}/`, '/system.slice/ssh.service'], groups);
    assert.ok(individual.length > 5);
    for (const t of individual) {
        const variants = cgroupQueryVariants(t, sel);
        assert.equal(variants.length, 2, t);
        const [leaf, group] = variants.map((v) => substituteCgroupPattern(v.query, v.selection));
        assert.ok(leaf.includes('name=~"/system.slice/ssh.service"'), leaf);
        assert.match(leaf, /sum by \(name\)/);
        assert.equal(variants[1].label, `${VM}/`);
        assert.ok(group.includes(`name=~"${sel.groups[0].pattern}"`), group);
        assert.doesNotMatch(group, /by \(name\)|__SELECTED_CGROUPS__/);
    }
});

test("the IPC chart's group query divides summed instructions by summed cycles", () => {
    const ipc = individual.find((q) => q.includes('cgroup_cpu_instructions'));
    const groups = deriveCgroupGroups(new Set(NAMES));
    const sel = resolveCgroupSelection([`${VM}/`], groups);
    const [v] = cgroupQueryVariants(ipc, sel);
    const out = substituteCgroupPattern(v.query, v.selection);
    const p = sel.groups[0].pattern;
    assert.equal(out,
        `sum (irate(cgroup_cpu_instructions{name=~"${p}"}[5m])) / sum (irate(cgroup_cpu_cycles{name=~"${p}"}[5m]))`);
});

test('only groups selected: no query for individual names', () => {
    const groups = deriveCgroupGroups(new Set(NAMES));
    const sel = resolveCgroupSelection([`${VM}/`], groups);
    for (const t of individual) {
        const variants = cgroupQueryVariants(t, sel);
        assert.deepEqual(variants.map((v) => v.label), [`${VM}/`]);
    }
});

test('the aggregate excludes every selected name and group member', () => {
    const groups = deriveCgroupGroups(new Set(NAMES));
    const sel = resolveCgroupSelection([`${VM}/`, '/system.slice/ssh.service'], groups);
    for (const n of [...NAMES.slice(0, 3), '/system.slice/ssh.service']) {
        assert.ok(sel.excluded.split(/[(|)]/).includes(n), n);
    }
    assert.ok(!sel.excluded.includes('docker-abc'));
    for (const t of aggregate) {
        const variants = cgroupQueryVariants(t, sel);
        assert.equal(variants.length, 1);
        const out = substituteCgroupPattern(variants[0].query, variants[0].selection);
        assert.ok(out.includes(`name!~"${sel.excluded}"`), out);
        assert.ok(!out.includes('__SELECTED_CGROUPS__'), out);
    }
});

test('variant results merge with each group named by its key', async () => {
    const series = (metric, v) => ({ metric, values: [[1, String(v)]] });
    const responses = {
        a: { status: 'success', data: { resultType: 'matrix', result: [series({ name: '/x' }, 1)] } },
        b: { status: 'success', data: { resultType: 'matrix', result: [series({}, 2)] } },
    };
    const merged = await runCgroupVariants(
        [{ query: 'a', label: null }, { query: 'b', label: '/g.scope/' }],
        async (q) => responses[q],
    );
    assert.deepEqual(merged.data.result.map((s) => s.metric), [{ name: '/x' }, { name: '/g.scope/' }]);
});

test('a failed variant fails the merged result', async () => {
    const ok = { status: 'success', data: { resultType: 'matrix', result: [] } };
    const bad = { status: 'error', error: 'boom' };
    const merged = await runCgroupVariants(
        [{ query: 'a', label: null }, { query: 'b', label: '/g.scope/' }],
        async (q) => (q === 'a' ? ok : bad),
    );
    assert.equal(merged.status, 'error');
    await assert.rejects(runCgroupVariants([{ query: 'a', label: '/g/' }], async () => { throw new Error('x'); }));
});

test('buildEffectiveQueries runs a query per group through the usual transforms', () => {
    const api = createDataApi({ getMetadata: async () => ({ status: 'success', data: {} }) });
    const groups = deriveCgroupGroups(new Set(NAMES));
    const sel = resolveCgroupSelection([`${VM}/`, '/system.slice/ssh.service'], groups);
    const plot = { promql_query: individual[0], opts: { type: 'line' } };
    const out = api.buildEffectiveQueries(plot, { activeCgroupPattern: sel, sectionRoute: '/cgroups' });
    assert.deepEqual(out.map((q) => q.label), [null, `${VM}/`]);
    assert.ok(out.every(({ query }) => !query.includes('__SELECTED_CGROUPS__')));
    // Nothing selected: an individual chart has nothing to run.
    assert.equal(api.buildEffectiveQueries(plot, { activeCgroupPattern: null }), null);
    // A plain string pattern still works as one query.
    assert.deepEqual(
        api.buildEffectiveQueries(plot, { activeCgroupPattern: '/a' }).map((q) => q.label),
        [null],
    );
});

test('a link key resolves to the key this recording has for the unit', () => {
    const short = deriveCgroupGroups([`${VM}/libvirt/vcpu0`, `${VM}/libvirt/vcpu1`]);
    const full = deriveCgroupGroups([
        '/machine.slice/machine-qemu-2-vm1.scope/libvirt', `${VM}/libvirt/vcpu0`, `${VM}/libvirt/vcpu1`,
    ]);
    assert.equal(resolveGroupKey('/machine.slice/machine-qemu-2-vm1.scope/', short), `${VM}/`);
    assert.equal(resolveGroupKey(`${VM}/`, full), '/machine.slice/machine-qemu-2-vm1.scope/');
    assert.equal(resolveGroupKey('.../other.scope/', short), null);
    // Two keys end with the same components: not resolved.
    const two = deriveCgroupGroups(['/a.slice/x.service/c1', '/a.slice/x.service/c2',
        '/b.slice/x.service/c1', '/b.slice/x.service/c2']);
    assert.equal(resolveGroupKey('.../x.service/', two), null);
});

test('two group keys the recording lacks are both kept for the selector to drop', () => {
    const out = normalizeCgroupSelection(['/p.scope/', '/q.scope/', '/leaf'], new Map());
    assert.deepEqual([...out].sort(), ['/leaf', '/p.scope/', '/q.scope/']);
});

test('a link holding a group and one of its members normalizes to the group', () => {
    const groups = deriveCgroupGroups(new Set(NAMES));
    const out = normalizeCgroupSelection(
        ['/machine.slice/machine-qemu-2-vm1.scope/', `${VM}/libvirt/vcpu0`, '/'], groups);
    assert.deepEqual([...out].sort(), [`${VM}/`, '/']);
});

test("a unit's own cgroup goes under its own group when two groups tie", () => {
    const names = [
        '/user.slice/user-1000.slice/user@1000.service',
        `${USER}/app.slice/b.scope`,
        '.../app.slice/b.scope/x',
    ];
    const groups = deriveCgroupGroups(names);
    const tree = cgroupTree(names, groups);
    const under = (item) => {
        const i = tree.findIndex((e) => e.item === item);
        for (let j = i - 1; j >= 0; j--) if (tree[j].depth < tree[i].depth) return tree[j].item;
        return null;
    };
    assert.equal(under(`${USER}/app.slice/b.scope`), `${USER}/app.slice/b.scope/`);
});

test('of two groups with the same members only the longer key is kept', () => {
    const groups = deriveCgroupGroups(['/a.service/b.scope/x', '/a.service/b.scope/y']);
    assert.deepEqual([...groups.keys()], ['/a.service/b.scope/']);
});

test("a refetch (zoom) of a chart the selector updated runs the selector's queries", async (t) => {
    setDisplayMode(true);
    t.after(() => setDisplayMode(false));
    setStepOverride(null);
    const calls = [];
    const api = createDataApi({
        getMetadata: async () => ({ status: 'success', data: { minTime: 0, maxTime: 100, interval: 1 } }),
        queryRange: async (query) => {
            calls.push(query);
            return {
                status: 'success',
                data: { resultType: 'matrix', result: [{ metric: query.includes('sum by') ? { name: '/a' } : {}, values: [[1, '1']] }] },
            };
        },
    });
    const groups = deriveCgroupGroups(new Set(NAMES));
    const sel = resolveCgroupSelection([`${VM}/`, '/system.slice/ssh.service'], groups);
    const template = individual[0];
    const queries = cgroupQueryVariants(template, sel)
        .map((v) => ({ query: substituteCgroupPattern(v.query, v.selection), label: v.label }));
    // As cgroup_selector.js leaves the plot after a selection.
    const plot = { promql_query: queries[0].query, cgroup_queries: queries, opts: { type: 'delta_counter', style: 'multi' } };
    const data = { groups: [{ name: 'g', subgroups: [{ plots: [plot] }] }] };
    await api.processDashboardData(data, sel, '/cgroups');
    assert.deepEqual(calls.sort(), queries.map((q) => q.query).sort());
    assert.deepEqual(plot.series_names, ['/a', `${VM}/`]);
});
