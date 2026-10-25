// The cgroup selection: which names are selectable, the groups a recording's
// names form, and the patterns and queries a selection turns into. No DOM and
// no mithril, so the node tests import it directly.

// Why a cgroup cannot be selected, or null when it can. A selection reaches
// the query engine as a regex matcher, `name=~"a|b"`: promql-parser unquotes
// the string and requires it to compile as a regex, then metriken-storage
// `match_pattern` compares each `|`-separated alternative with the name
// literally, after turning `\.` into `.`. A name made of letters, digits,
// `/ _ . : @ - = , ~ %` and `\xHH` escapes (systemd's escaped characters)
// compiles and compares equal to itself; those characters cover systemd
// units, container ids and pod names. Any other name, and the empty name, is
// not offered, including a few the engine could match (`{`, `"`).
const SELECTABLE = /^(?:[A-Za-z0-9/_.:@=,~%-]|\\x[0-9A-Fa-f]{2})+$/;
export const cgroupUnselectableReason = (name) => (SELECTABLE.test(name)
    ? null
    : 'Cannot be selected: the name has characters outside those the selector can quote safely');

// A name inside a PromQL double-quoted string: backslashes and quotes escaped,
// so the string unquotes to the name itself (a systemd `\x2d` stays `\x2d`).
const quoteForPromql = (name) => name.replace(/\\/g, '\\\\').replace(/"/g, '\\"');

// The pattern substituted for `__SELECTED_CGROUPS__`: the selectable names,
// quoted, joined by `|` and parenthesised when there are several. null when
// none can be selected.
export const cgroupPattern = (names) => {
    const usable = names.filter((n) => !cgroupUnselectableReason(n)).map(quoteForPromql);
    return usable.length > 1 ? '(' + usable.join('|') + ')' : usable[0] || null;
};

// A group is keyed by its unit's path with a trailing `/`, the subtree below
// it, and the link's `cgroup=` carries the key. The root cgroup is `/`; no
// other recorded name ends in `/`, so a key cannot be a name.
export const isCgroupGroupKey = (key) => typeof key === 'string' && key.length > 1 && key.endsWith('/');

const isUnit = (component) => /\.(scope|service)$/.test(component);

// The agent records a cgroup more than three levels deep by its last three
// path components, as `.../<grandparent>/<parent>/<name>`
// (`process_cgroup_info` in src/agent/bpf/mod.rs). A unit is therefore known
// only from names that still carry it: its own cgroup and those up to two
// levels below it.

/**
 * The groups among `names`: Map of key -> sorted member names. A group is a
 * `*.scope` or `*.service` path component with at least two recorded names
 * at or below it, as far as the names show (see above). Nested units each
 * form their own group. A truncated key (`.../x.scope/`) is merged into the
 * one longer key that ends with the same components, taken to be the same
 * unit. That is right for a system unit, whose name is unique, and wrong
 * when a user manager's unit deeper in the tree has the same name: the two
 * are then one group. With more than one such key it stays apart. Of two
 * groups with the same members, only the one with the longer key is kept.
 */
export const deriveCgroupGroups = (names) => {
    const raw = new Map();
    for (const name of names) {
        const parts = name.split('/');
        // Every prefix ending in a unit, including the name itself: a unit's
        // own cgroup is a member of its group.
        for (let i = 1; i <= parts.length; i++) {
            if (!isUnit(parts[i - 1])) continue;
            const key = parts.slice(0, i).join('/') + '/';
            if (!raw.has(key)) raw.set(key, new Set());
            raw.get(key).add(name);
        }
    }
    const keys = [...raw.keys()].sort((a, b) => b.length - a.length);
    const canonical = new Map();
    for (const key of keys) {
        let target = key;
        if (key.startsWith('.../')) {
            const tail = key.slice(3);
            const longer = new Set(keys
                .filter((k) => k.length > key.length && k.endsWith(tail))
                .map((k) => canonical.get(k)));
            if (longer.size === 1) target = [...longer][0];
        }
        canonical.set(key, target);
    }
    const groups = new Map();
    for (const [key, members] of raw) {
        const target = canonical.get(key);
        if (!groups.has(target)) groups.set(target, new Set());
        for (const n of members) groups.get(target).add(n);
    }
    for (const [key, members] of groups) {
        if (members.size < 2) groups.delete(key);
        else groups.set(key, [...members].sort());
    }
    const byMembers = new Map();
    for (const [key, members] of groups) {
        const id = members.join('\n');
        const other = byMembers.get(id);
        if (other === undefined) {
            byMembers.set(id, key);
        } else {
            const drop = other.length >= key.length ? key : other;
            groups.delete(drop);
            byMembers.set(id, drop === key ? other : key);
        }
    }
    return groups;
};

/**
 * The key among `groups` that `key` names, or null. A key from another
 * recording can carry more or less of the unit's path than this recording's
 * (`/machine.slice/x.scope/` against `.../x.scope/`); it names the one group
 * whose key ends with its components after `...`, or whose components after
 * `...` it ends with, when there is exactly one. As with the merge in
 * deriveCgroupGroups, a system unit's key can resolve to a user manager's
 * unit of the same name.
 */
export const resolveGroupKey = (key, groups) => {
    if (groups.has(key)) return key;
    const tail = (k) => (k.startsWith('.../') ? k.slice(3) : null);
    const matches = [...groups.keys()].filter((k) => (tail(key) && k.endsWith(tail(key)))
        || (tail(k) && key.endsWith(tail(k))));
    return matches.length === 1 ? matches[0] : null;
};

/**
 * `names` and the group keys in display order, each with its depth: every
 * entry follows the smallest group holding it (of two the same size, one
 * whose key starts the entry's path, then the longer key), and entries with
 * the same parent are sorted by text.
 */
export const cgroupTree = (names, groups) => {
    // Higher is the better parent: fewer members, then a key that starts the
    // entry's path, then a longer key.
    const rank = (key, members, entry) => [
        -members.length,
        entry.startsWith(key) || `${entry}/` === key ? 1 : 0,
        key.length,
    ];
    const higher = (a, b) => {
        for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return a[i] > b[i];
        return false;
    };
    const entries = [...new Set([...names, ...groups.keys()])];
    const parentOf = new Map();
    for (const entry of entries) {
        const own = isCgroupGroupKey(entry) ? groups.get(entry) || [] : [entry];
        let parent = null;
        for (const [key, members] of groups) {
            if (key === entry || own.length === 0) continue;
            const set = new Set(members);
            if (!own.every((n) => set.has(n))) continue;
            if (isCgroupGroupKey(entry) && members.length === own.length) continue;
            if (!parent || higher(rank(key, members, entry), rank(parent, groups.get(parent), entry))) parent = key;
        }
        parentOf.set(entry, parent);
    }
    const children = new Map();
    for (const entry of entries) {
        const p = parentOf.get(entry);
        if (!children.has(p)) children.set(p, []);
        children.get(p).push(entry);
    }
    const out = [];
    const walk = (parent, depth) => {
        for (const entry of (children.get(parent) || []).sort()) {
            out.push({ item: entry, depth });
            walk(entry, depth + 1);
        }
    };
    walk(null, 0);
    return out;
};

/** Why a group cannot be selected, or null when it can. */
export const cgroupGroupUnselectableReason = (members) => (
    members.some((n) => cgroupUnselectableReason(n))
        ? 'Cannot be selected: a cgroup in this group has characters outside those the selector can quote safely'
        : null
);

/** The names an entry covers: a group's members, or the name itself. */
export const entryMembers = (key, groups) => (isCgroupGroupKey(key) ? groups.get(key) || [] : [key]);

/**
 * The selected group that blocks `key`, or null. An entry is blocked by a
 * selected group other than itself that it shares a name with, unless the
 * entry holds every member of that group (selecting it then replaces the
 * group). Selecting a blocked entry would count the shared names twice.
 * Groups can overlap without nesting when the agent truncated their names:
 * `.../svc.service/app.slice/b.scope` is in svc.service's group and in
 * b.scope's.
 */
export const coveringGroup = (key, selected, groups) => {
    const own = new Set(entryMembers(key, groups));
    if (own.size === 0) return null;
    for (const sel of selected) {
        if (sel === key || !isCgroupGroupKey(sel)) continue;
        const members = groups.get(sel) || [];
        if (members.some((n) => own.has(n)) && !members.every((n) => own.has(n))) return sel;
    }
    return null;
};

/**
 * `selected` with `keys` added. A group takes the place of every selected
 * entry whose names are all its members; an entry a selected group blocks
 * (coveringGroup) is skipped. Groups are added before names, so a range
 * that spans a group and its members selects the group.
 */
export const addCgroupEntries = (selected, keys, groups) => {
    const out = new Set(selected);
    const ordered = [...keys].sort((a, b) => Number(isCgroupGroupKey(b)) - Number(isCgroupGroupKey(a)));
    for (const key of ordered) {
        if (out.has(key) || coveringGroup(key, out, groups)) continue;
        if (isCgroupGroupKey(key) && groups.has(key)) {
            const members = new Set(groups.get(key));
            for (const other of [...out]) {
                if (entryMembers(other, groups).every((n) => members.has(n))) out.delete(other);
            }
        }
        out.add(key);
    }
    return out;
};

/**
 * A selection made consistent with `groups`: each group key resolved to this
 * recording's key (resolveGroupKey), then the entries added as if selected
 * one after another (addCgroupEntries), so a group replaces the members it
 * holds. A group key with no match is kept, for resolveCgroupSelection and
 * the selector to drop.
 */
export const normalizeCgroupSelection = (keys, groups) => {
    const resolved = [...keys].map((k) => (isCgroupGroupKey(k) ? resolveGroupKey(k, groups) || k : k));
    return addCgroupEntries(new Set(), resolved, groups);
};

/**
 * A selection of keys as the patterns its queries take:
 * - `leaf`: the individually selected names, for `name=~` (null if none);
 * - `excluded`: every selected name and group member, for `name!~`;
 * - `groups`: `{ key, pattern }` per selected group, in key order.
 * A group key not in `groups` (a link from another recording) is left out.
 */
export const resolveCgroupSelection = (keys, groups) => {
    const leaves = [];
    const selectedGroups = [];
    const excluded = new Set();
    for (const key of [...keys].sort()) {
        if (isCgroupGroupKey(key)) {
            const members = groups.get(key);
            if (!members || cgroupGroupUnselectableReason(members)) continue;
            selectedGroups.push({ key, pattern: cgroupPattern(members) });
            for (const n of members) excluded.add(n);
        } else if (!cgroupUnselectableReason(key)) {
            leaves.push(key);
            excluded.add(key);
        }
    }
    return {
        leaf: cgroupPattern(leaves),
        excluded: cgroupPattern([...excluded].sort()),
        groups: selectedGroups,
    };
};

const SUM_BY_NAME = /sum\s+by\s*\(\s*name\s*\)\s*/g;

/**
 * The queries an individual cgroup chart runs for a selection: the template
 * itself for the individually selected names, and per selected group the
 * template with `sum by (name)` made `sum`, labelled with the group's key.
 * Each comes with the selection to substitute into it. A template without a
 * `name=~` placeholder, or a selection with no groups, gives the template
 * alone.
 *
 * A group's IPC is sum(instructions) / sum(cycles) over its members, which
 * is why a group is its own query rather than a sum of member lines.
 */
export const cgroupQueryVariants = (template, selection) => {
    if (!selection || typeof selection === 'string' || selection.groups.length === 0
        || !template.includes('name=~"__SELECTED_CGROUPS__"')) {
        return [{ query: template, label: null, selection }];
    }
    const out = [];
    if (selection.leaf) out.push({ query: template, label: null, selection });
    if (template.match(SUM_BY_NAME)) {
        const summed = template.replace(SUM_BY_NAME, 'sum ');
        for (const g of selection.groups) {
            out.push({
                query: summed,
                label: g.key,
                selection: { leaf: g.pattern, excluded: selection.excluded, groups: [] },
            });
        }
    }
    return out;
};

// One query's result relabelled for a cgroup group: each series is named by
// the group's key. A group's query sums its members, so it has one series.
export const relabelMatrix = (res, label) => (label == null ? res : {
    ...res,
    data: {
        ...res.data,
        result: (res.data?.result || []).map((s) => ({ ...s, metric: { name: label } })),
    },
});

// The results of a chart's cgroup query variants as one result, series in
// variant order. A variant that fails fails the whole: a chart missing a
// group's line would read as that group being idle.
export const mergeMatrices = (results) => {
    if (results.length === 1) return results[0];
    for (const r of results) {
        if (r?.status !== 'success') return r;
    }
    return {
        ...results[0],
        data: {
            ...results[0].data,
            result: results.flatMap((r) => r.data?.result || []),
        },
    };
};

// Run each `{ query, label }` with `fetch` and merge the relabelled results.
export const runCgroupVariants = async (queries, fetch) => mergeMatrices(
    await Promise.all(queries.map(async ({ query, label }) => relabelMatrix(await fetch(query), label))),
);
