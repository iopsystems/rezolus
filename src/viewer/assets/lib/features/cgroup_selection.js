// The cgroup selection: which names are selectable, the groups a recording's
// names form, and the patterns and queries a selection turns into. No DOM and
// no mithril, so the node tests import it directly.

// Why a cgroup cannot be selected, or null when it can. A selection reaches
// the query engine as a regex matcher, `name=~"a|b"`, which promql-parser
// unquotes and the engine matches as a regular expression anchored at both
// ends; each name is escaped as a regex (`regexEscape`), so it matches
// itself only. Selectable names are made of letters, digits,
// `/ _ . : @ - = , ~ %` and `\xHH` escapes (systemd's escaped characters),
// which cover systemd units, container ids and pod names, and are the set
// the tests cover. Any other name, and the empty name, is not offered.
const SELECTABLE = /^(?:[A-Za-z0-9/_.:@=,~%-]|\\x[0-9A-Fa-f]{2})+$/;
export const cgroupUnselectableReason = (name) => (SELECTABLE.test(name)
    ? null
    : 'Cannot be selected: the name has characters outside those the selector supports');

// A name as a regular expression that matches it literally: `.` and `\`
// (in a systemd `\x2d`) and the other regex metacharacters escaped.
const regexEscape = (name) => name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

// A pattern inside a PromQL double-quoted string: backslashes and quotes
// escaped, so the string unquotes to the pattern itself.
const quoteForPromql = (pattern) => pattern.replace(/\\/g, '\\\\').replace(/"/g, '\\"');

// The pattern substituted for `__SELECTED_CGROUPS__`: the selectable names,
// each escaped as a regex and quoted, joined by `|` and parenthesised when
// there are several. null when none can be selected.
export const cgroupPattern = (names) => alternatives(names
    .filter((n) => !cgroupUnselectableReason(n))
    .map((n) => quoteForPromql(regexEscape(n))));

// Patterns joined by `|` and parenthesised when there are several; null for
// none.
const alternatives = (patterns) => (patterns.length > 1 ? '(' + patterns.join('|') + ')' : patterns[0] || null);

// A unit's cgroup and every cgroup below it, `<unit>(?:/.*)?`, for a key.
const prefixPattern = (key) => quoteForPromql(regexEscape(key.slice(0, -1))) + '(?:/.*)?';

// The `.../` key a member recorded under a shortened path was grouped by in
// deriveCgroupGroups, given the longer key it was merged into: the
// member's longest `.../` prefix ending in `/` that `key` ends with. null if
// it has none.
const shortenedKey = (member, key) => {
    if (!member.startsWith('.../')) return null;
    for (let end = member.lastIndexOf('/'); end > 3; end = member.lastIndexOf('/', end - 1)) {
        const prefix = member.slice(0, end + 1);
        if (key.endsWith(prefix.slice(3))) return prefix;
    }
    return null;
};

// The pattern of a selected group: its unit's cgroup and every cgroup below
// it, by its key and by each shortened `.../` key merged into it. In the
// recording the group came from, these match exactly its members
// (deriveCgroupGroups); in another recording, such as the other arm of a
// comparison, they match that recording's cgroups under the unit. A member
// under neither, which deriveCgroupGroups does not produce, is matched by
// name.
const groupPattern = (key, members) => {
    const keys = new Set([key]);
    const named = [];
    for (const n of members) {
        if (n === key.slice(0, -1) || n.startsWith(key)) continue;
        const shortened = shortenedKey(n, key);
        if (shortened) keys.add(shortened);
        else named.push(quoteForPromql(regexEscape(n)));
    }
    return alternatives([...[...keys].map(prefixPattern), ...named]);
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
        ? 'Cannot be selected: a cgroup in this group has characters outside those the selector supports'
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
 * - `excluded`: every selected name and group (groupPattern), for `name!~`;
 * - `groups`: `{ key, pattern }` per selected group, in key order.
 * A group key not in `groups` (a link from another recording) is left out.
 */
export const resolveCgroupSelection = (keys, groups) => {
    const leaves = [];
    const selectedGroups = [];
    for (const key of [...keys].sort()) {
        if (isCgroupGroupKey(key)) {
            const members = groups.get(key);
            if (!members || cgroupGroupUnselectableReason(members)) continue;
            selectedGroups.push({ key, pattern: groupPattern(key, members) });
        } else if (!cgroupUnselectableReason(key)) {
            leaves.push(key);
        }
    }
    const leaf = cgroupPattern(leaves);
    return {
        leaf,
        excluded: alternatives([...(leaf ? [leaf] : []), ...selectedGroups.map((g) => g.pattern)]),
        groups: selectedGroups,
    };
};

const SUM_BY_NAME = /sum\s+by\s*\(\s*name\s*\)\s*\(/g;

// The label a member's own name is copied to before `name` is set to its
// group's key, so two members of a group stay two series until summed.
const MEMBER_LABEL = 'cgroup_member';

// The index just past the `)` closing the `(` before `open`, skipping
// strings; -1 if it is not closed.
const closingParen = (query, open) => {
    let depth = 1;
    for (let i = open; i < query.length; i++) {
        const c = query[i];
        if (c === '"' || c === "'" || c === '`') {
            for (i++; i < query.length && query[i] !== c; i++) {
                if (query[i] === '\\' && c !== '`') i++;
            }
        } else if (c === '(') {
            depth++;
        } else if (c === ')' && --depth === 0) {
            return i + 1;
        }
    }
    return -1;
};

// `expr` with each member of each group renamed to its group's key:
// `name` is copied to MEMBER_LABEL, then set to the key where it matches
// the group's pattern.
const renameGroupMembers = (expr, groups) => groups.reduce(
    (inner, g) => `label_replace(${inner}, "name", "${quoteForPromql(g.key)}", "name", "${g.pattern}")`,
    `label_replace(${expr}, "${MEMBER_LABEL}", "$1", "name", "(.*)")`,
);

/**
 * The queries an individual cgroup chart runs for a selection, each with the
 * selection to substitute into it. With groups selected, the template's
 * selector takes every selected name and group (`excluded`), and the
 * argument of each `sum by (name) (...)` has the `name` of each cgroup a
 * group's pattern matches replaced by the group's key (`label_replace`), so
 * the chart's one query draws a line per
 * individually selected name and one per group. A group's IPC is then
 * sum(instructions) / sum(cycles) over its members. A template with groups
 * but no `sum by (name)` cannot sum a group: it runs for the individually
 * selected names, or not at all. A template without a `name=~` placeholder,
 * or a selection with no groups, gives the template alone.
 */
export const cgroupQueryVariants = (template, selection) => {
    if (!selection || typeof selection === 'string' || selection.groups.length === 0
        || !template.includes('name=~"__SELECTED_CGROUPS__"')) {
        return [{ query: template, label: null, selection }];
    }
    if (!template.match(SUM_BY_NAME)) {
        return selection.leaf ? [{ query: template, label: null, selection }] : [];
    }
    let query = '';
    let rest = 0;
    for (const m of template.matchAll(SUM_BY_NAME)) {
        const open = m.index + m[0].length;
        if (open < rest) continue;
        const close = closingParen(template, open);
        if (close < 0) return [];
        query += template.slice(rest, open)
            + renameGroupMembers(template.slice(open, close - 1), selection.groups) + ')';
        rest = close;
    }
    query += template.slice(rest);
    const all = { leaf: selection.excluded, excluded: selection.excluded, groups: [] };
    return [{ query, label: null, selection: all }];
};

// The rest of this file runs the `{ query, label }` lists in a chart's
// `cgroup_queries`, which a chart saved with a query per cgroup group carries.

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
