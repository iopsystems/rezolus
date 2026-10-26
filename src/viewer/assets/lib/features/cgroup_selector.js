// Attrs:
//   groups: array of group objects with plots
//   executeQuery: (query) => Promise<result> — runs a PromQL range query
//   applyResultToPlot: (plot, result) => void — applies PromQL result to a plot
//   substitutePattern: (query, selection) => string — substitutes cgroup placeholder
//   setActiveCgroupPattern: (selection) => void — sets the global active cgroup
//     selection (resolveCgroupSelection's shape, or null)
//   sectionRoute: the section's route; groups are kept per route, since each
//     recording of a combined archive has its own

import globalColorMapper from '../charts/util/colormap.js';
import { collectGroupPlots } from './group_utils.js';
import { writeViewState } from '../ui/url_state.js';
import { discoverCgroups } from './cgroup_discovery.js';
import {
    cgroupUnselectableReason,
    cgroupGroupUnselectableReason,
    isCgroupGroupKey,
    deriveCgroupGroups,
    cgroupTree,
    coveringGroup,
    addCgroupEntries,
    resolveCgroupSelection,
    normalizeCgroupSelection,
    cgroupQueryVariants,
    runCgroupVariants,
} from './cgroup_selection.js';

export { cgroupPattern, cgroupUnselectableReason } from './cgroup_selection.js';

/** Render a custom multi-select list with a color swatch per item.
 *  Supports shift+click range selection via lastClicked / setLastClicked.
 *  `describe(item)` gives `{ label, depth, reason, note }`: the text shown,
 *  how far it is indented (a group's members sit under it), why it cannot be
 *  marked or null, and a title for an item that can. An item with a reason
 *  is drawn disabled, with the reason as its title. */
const selectList = (title, items, selectionSet, onToggle, emptyLabel, lastClicked, setLastClicked, describe) =>
    m('div.selector-column', [
        m('h4', title),
        m('ul.cgroup-select', {
            role: 'listbox',
            'aria-multiselectable': 'true',
        }, items.length === 0
            ? m('li.cgroup-select-empty', emptyLabel)
            : items.map((item) => {
                const { label, depth, reason, note } = describe(item);
                return m('li.cgroup-select-item', {
                    key: item,
                    role: 'option',
                    'aria-selected': selectionSet.has(item) ? 'true' : 'false',
                    'aria-disabled': reason ? 'true' : undefined,
                    title: reason || note || undefined,
                    class: [
                        reason ? 'disabled' : (selectionSet.has(item) ? 'selected' : ''),
                        isCgroupGroupKey(item) ? 'group' : '',
                    ].filter(Boolean).join(' '),
                    style: depth > 0 ? { paddingLeft: `${0.5 + depth * 1.25}rem` } : undefined,
                    onclick: (e) => {
                        if (reason) return;
                        if (e.shiftKey && lastClicked != null) {
                            const from = items.indexOf(lastClicked);
                            const to = items.indexOf(item);
                            if (from !== -1 && to !== -1) {
                                const lo = Math.min(from, to);
                                const hi = Math.max(from, to);
                                for (let i = lo; i <= hi; i++) {
                                    if (!describe(items[i]).reason) selectionSet.add(items[i]);
                                }
                            }
                        } else {
                            onToggle(item);
                        }
                        setLastClicked(item);
                    },
                }, [
                    m('span.cgroup-select-swatch', {
                        style: { background: globalColorMapper.getColorByName(item) },
                    }),
                    m('span.cgroup-select-name', label),
                ]);
            }),
        ),
    ]);

/** Render a transfer button with directional arrows (horizontal on desktop, vertical on mobile). */
const transferBtn = (lrLabel, udLabel, title, disabled, onclick) =>
    m('button', { title, disabled, onclick }, [
        m('span.arrow-lr', lrLabel),
        m('span.arrow-ud', udLabel),
    ]);

// Persisted state — survives component remount across navigations.
// The selection holds cgroup names and group keys (cgroup_selection.js).
let persistedSelectedCgroups = new Set();
let persistedOriginalQueries = null; // Map<string, string>
// Each section's groups, once discovered: route -> Map of key -> members.
const persistedGroups = new Map();

// Seed the selection before the component first mounts, from the link's
// `cgroup=` keys. Names are validated against the recording once the
// available list arrives (fetchAvailableCgroups); unknown ones are dropped
// there with a warning rather than left to render empty charts.
export const seedSelectedCgroups = (names) => {
    const seeded = (Array.isArray(names) ? names : []).filter((n) => typeof n === 'string' && n !== '');
    const unusable = seeded.filter((n) => cgroupUnselectableReason(n));
    if (unusable.length > 0) {
        console.warn(`[cgroups] dropping selection the selector cannot quote: ${unusable.join(', ')}`);
    }
    persistedSelectedCgroups = new Set(seeded.filter((n) => !cgroupUnselectableReason(n)));
    if (unusable.length > 0) writeViewState({ cgroup: [...persistedSelectedCgroups] });
};

// The selection as resolveCgroupSelection resolves it for the section at
// `route`, or null when nothing is selected. A page that does not mount the
// selector (an expanded chart) reads the selection from here; a group in it
// resolves only once that section's groups are known (loadCgroupGroups).
export const selectedCgroupSelection = (route) => (persistedSelectedCgroups.size > 0
    ? resolveCgroupSelection(
        normalizeCgroupSelection(persistedSelectedCgroups, persistedGroups.get(route) || new Map()),
        persistedGroups.get(route) || new Map(),
    )
    : null);

// Discover the groups of the section at `route` if the selection names one
// and they are not yet known. `executeQuery` runs a PromQL range query
// restricted as that section's charts are.
export const loadCgroupGroups = async (route, executeQuery) => {
    if (persistedGroups.has(route) || ![...persistedSelectedCgroups].some(isCgroupGroupKey)) return;
    persistedGroups.set(route, deriveCgroupGroups(await discoverCgroups(executeQuery)));
};

// Forget every section's groups, for a recording that replaces this one.
export const clearCgroupGroups = () => { persistedGroups.clear(); };

const GROUP_NOTE = 'The cgroups recorded under this unit. The agent names a cgroup more than three '
    + 'levels deep by its last three path components, so one more than two levels below the unit '
    + 'is not included.';

// Why an entry cannot be selected given the selection, or null.
const entryReason = (key, selected, groups) => {
    if (isCgroupGroupKey(key)) {
        const members = groups.get(key) || [];
        const reason = cgroupGroupUnselectableReason(members);
        if (reason) return reason;
    } else {
        const reason = cgroupUnselectableReason(key);
        if (reason) return reason;
    }
    const covering = coveringGroup(key, selected, groups);
    if (!covering) return null;
    const members = new Set(groups.get(covering) || []);
    return isCgroupGroupKey(key) && !(groups.get(key) || []).every((n) => members.has(n))
        ? `Shares cgroups with ${covering}`
        : `Included in ${covering}`;
};

export const CgroupSelector = {
    oninit(vnode) {
        vnode.state.selectedCgroups = new Set(persistedSelectedCgroups);
        vnode.state.availableCgroups = new Set();
        const route = vnode.attrs.sectionRoute || '/cgroups';
        vnode.state.groups = persistedGroups.get(route) || new Map();
        vnode.state.loading = true;
        vnode.state.error = null;
        vnode.state.leftSelected = new Set();
        vnode.state.rightSelected = new Set();
        vnode.state.lastClickedLeft = null;
        vnode.state.lastClickedRight = null;
        vnode.state.originalQueries = persistedOriginalQueries;

        // Force multi-series style on right-side (individual) cgroup plots so that
        // color assignment is always by cgroup name (hash-based) — even when only
        // one cgroup is selected, which would otherwise resolve to a single-series
        // line chart with the accent color.
        for (const group of vnode.attrs.groups || []) {
            if (group.metadata?.side !== 'right') continue;
            for (const plot of collectGroupPlots(group)) {
                if (plot.opts && plot.opts.style !== 'multi') {
                    plot.opts.style = 'multi';
                }
            }
        }

        this.fetchAvailableCgroups(vnode);

        // When initialized with persisted selections (after a granularity or
        // node change remounts the component tree, or seeded from the link
        // before the first mount), re-run queries so the individual cgroup
        // charts get populated. updateQueries snapshots the original queries
        // itself on its first run, so a missing snapshot is no reason to skip.
        // A selection with a group waits until discovery has found the
        // groups' members; fetchAvailableCgroups runs the queries then.
        const waitForGroups = !persistedGroups.has(route) && [...persistedSelectedCgroups].some(isCgroupGroupKey);
        if (persistedSelectedCgroups.size > 0 && !waitForGroups) {
            this.debouncedUpdateQueries(vnode);
        }
    },

    async fetchAvailableCgroups(vnode) {
        const { executeQuery } = vnode.attrs;

        try {
            const cgroups = await discoverCgroups(executeQuery);

            if (cgroups.size === 0) {
                vnode.state.error = 'No cgroup data found';
            }

            vnode.state.availableCgroups = cgroups;
            vnode.state.groups = deriveCgroupGroups(cgroups);
            persistedGroups.set(vnode.attrs.sectionRoute || '/cgroups', vnode.state.groups);

            // A selection that names cgroups or groups this recording does
            // not have (a link from another host, a stale persisted set) would
            // render every individual chart empty with nothing to say why.
            // Drop the unknown entries, say so once, and rewrite the link to
            // match. A group with a member the selector cannot quote is
            // dropped too, rather than plotted as a partial sum.
            let changed = false;
            if (cgroups.size > 0 && vnode.state.selectedCgroups.size > 0) {
                const groups = vnode.state.groups;
                // A link's group key resolves to this recording's key for the
                // unit, and a group takes the place of the members a link
                // also holds, as it does when selected here.
                const before = [...vnode.state.selectedCgroups].sort().join('\n');
                vnode.state.selectedCgroups = normalizeCgroupSelection(vnode.state.selectedCgroups, groups);
                if ([...vnode.state.selectedCgroups].sort().join('\n') !== before) changed = true;
                const unknown = [...vnode.state.selectedCgroups].filter((cg) => (isCgroupGroupKey(cg)
                    ? !groups.has(cg) || cgroupGroupUnselectableReason(groups.get(cg))
                    : !cgroups.has(cg)));
                if (unknown.length > 0) {
                    console.warn(`[cgroups] dropping selection not in this recording: ${unknown.join(', ')}`);
                    for (const cg of unknown) vnode.state.selectedCgroups.delete(cg);
                    changed = true;
                }
                if (changed) {
                    persistedSelectedCgroups = new Set(vnode.state.selectedCgroups);
                    writeViewState({ cgroup: [...persistedSelectedCgroups] });
                }
            }
            // The groups' members are known now; a selection that holds a
            // group, or lost an entry, runs its queries again.
            if (changed || [...vnode.state.selectedCgroups].some(isCgroupGroupKey)) {
                this.debouncedUpdateQueries(vnode);
            }
        } catch (error) {
            console.error('Failed to fetch available cgroups:', error);
            vnode.state.error = 'Failed to load cgroups: ' + error.message;
            vnode.state.availableCgroups = new Set();
        }

        vnode.state.loading = false;
        m.redraw();
    },

    async updateQueries(vnode) {
        const { executeQuery, substitutePattern, setActiveCgroupPattern, applyResultToPlot } = vnode.attrs;

        if (vnode.state.updateInProgress) {
            vnode.state.cancelUpdate = true;
            return;
        }

        vnode.state.updateInProgress = true;
        vnode.state.cancelUpdate = false;

        const selection = vnode.state.selectedCgroups.size > 0
            ? resolveCgroupSelection(vnode.state.selectedCgroups, vnode.state.groups)
            : null;

        setActiveCgroupPattern(selection);

        // Snapshot original queries on first update
        if (!vnode.state.originalQueries) {
            vnode.state.originalQueries = new Map();
            for (const [gi, group] of (vnode.attrs.groups || []).entries()) {
                for (const [pi, plot] of collectGroupPlots(group).entries()) {
                    if (plot.promql_query) {
                        vnode.state.originalQueries.set(`${gi}-${pi}`, plot.promql_query);
                    }
                }
            }
            persistedOriginalQueries = vnode.state.originalQueries;
        }

        const generation = ++vnode.state.updateGeneration || 1;
        vnode.state.updateGeneration = generation;

        // Collect plots whose original query contains the cgroup placeholder.
        // An individual plot runs a query per selected group as well as one
        // for the individually selected names (cgroupQueryVariants); a query
        // left with its placeholder has nothing selected and is not run.
        const plotsToUpdate = [];
        for (const [gi, group] of (vnode.attrs.groups || []).entries()) {
            for (const [pi, plot] of collectGroupPlots(group).entries()) {
                const orig = vnode.state.originalQueries.get(`${gi}-${pi}`);
                if (orig && orig.includes('__SELECTED_CGROUPS__')) {
                    const queries = cgroupQueryVariants(orig, selection)
                        .map((v) => ({ query: substitutePattern(v.query, v.selection), label: v.label }))
                        .filter(({ query }) => !query.includes('__SELECTED_CGROUPS__'));
                    plotsToUpdate.push({
                        plot,
                        queries,
                        query: queries[0]?.query ?? substitutePattern(orig, selection),
                    });
                }
            }
        }

        // Execute in batches to avoid overwhelming the server
        const BATCH_SIZE = 5;
        for (let i = 0; i < plotsToUpdate.length; i += BATCH_SIZE) {
            if (vnode.state.cancelUpdate || vnode.state.updateGeneration !== generation) {
                vnode.state.updateInProgress = false;
                // The selection changed under this run (a transfer, or the
                // availability check pruning a link-seeded name). Run once
                // more so the newer selection is what the charts show;
                // without this the cancelling change was silently lost.
                if (vnode.state.cancelUpdate) this.debouncedUpdateQueries(vnode);
                return;
            }

            const batch = plotsToUpdate.slice(i, i + BATCH_SIZE);
            await Promise.all(batch.map(async ({ plot, query, queries }) => {
                plot.promql_query = query;
                // Every query the plot draws, for a pinned or expanded copy
                // of it; null when its one query is promql_query.
                plot.cgroup_queries = queries.length > 1 || queries[0]?.label ? queries : null;
                try {
                    if (queries.length === 0) {
                        plot.data = [];
                        plot.series_names = [];
                        return;
                    }
                    const result = await runCgroupVariants(queries, executeQuery);
                    if (vnode.state.updateGeneration !== generation) return;
                    applyResultToPlot(plot, result);
                } catch (error) {
                    console.error(`Failed query for ${plot.opts.title}:`, error);
                    plot.data = [];
                    plot.series_names = [];
                }
            }));
        }

        if (vnode.state.updateGeneration === generation) {
            m.redraw();
        }

        vnode.state.updateInProgress = false;
        // A change that arrived during the last batch was not seen by the
        // loop's check; pick it up now.
        if (vnode.state.cancelUpdate) this.debouncedUpdateQueries(vnode);
    },

    debouncedUpdateQueries(vnode) {
        if (vnode.state.updateTimer) clearTimeout(vnode.state.updateTimer);
        vnode.state.updateTimer = setTimeout(() => this.updateQueries(vnode), 300);
    },

    /** Move items between selected/available and trigger a query update.
     *  Adding a group replaces every selected entry it shares a name with. */
    transfer(vnode, items, direction) {
        const st = vnode.state;
        if (direction === 'add') {
            const usable = [...items].filter((cg) => !(isCgroupGroupKey(cg)
                ? cgroupGroupUnselectableReason(st.groups.get(cg) || [])
                : cgroupUnselectableReason(cg)));
            st.selectedCgroups = addCgroupEntries(st.selectedCgroups, usable, st.groups);
        } else {
            for (const cg of items) st.selectedCgroups.delete(cg);
        }
        persistedSelectedCgroups = new Set(vnode.state.selectedCgroups);
        writeViewState({ cgroup: [...persistedSelectedCgroups] });
        vnode.state.leftSelected.clear();
        vnode.state.rightSelected.clear();
        this.debouncedUpdateQueries(vnode);
    },

    view(vnode) {
        const st = vnode.state;
        // Each group above its members, which are indented under it.
        const tree = cgroupTree(st.availableCgroups, st.groups)
            .filter(({ item }) => !st.selectedCgroups.has(item));
        const depthOf = new Map(tree.map(({ item, depth }) => [item, depth]));
        const unselected = tree.map(({ item }) => item);
        // "Move all" selects every cgroup on its own, as it did before groups.
        const selectable = unselected.filter((cg) => !isCgroupGroupKey(cg)
            && !entryReason(cg, st.selectedCgroups, st.groups));
        const selected = Array.from(st.selectedCgroups).sort();
        const describe = (item) => {
            const reason = entryReason(item, st.selectedCgroups, st.groups);
            return {
                label: isCgroupGroupKey(item)
                    ? `${item} (${(st.groups.get(item) || []).length} cgroups)`
                    : item,
                depth: depthOf.get(item) || 0,
                reason,
                note: !reason && isCgroupGroupKey(item) ? GROUP_NOTE : null,
            };
        };
        const describeSelected = (item) => ({ ...describe(item), depth: 0, reason: null });
        const availableNames = unselected.filter((cg) => !isCgroupGroupKey(cg)).length;

        const leftItems = st.loading
            ? [] // selectList will show emptyLabel
            : unselected;

        const toggleMark = (set, item) => {
            if (set.has(item)) set.delete(item);
            else set.add(item);
        };

        return m('div.cgroup-selector', [
            m('h3', 'Cgroup Selection'),
            st.error && m('div.error-message', st.error),
            m('div.selector-container', [
                selectList(
                    'Available Cgroups (Aggregate)',
                    leftItems,
                    st.leftSelected,
                    (item) => toggleMark(st.leftSelected, item),
                    st.loading ? 'Loading cgroups...' : 'No cgroups available',
                    st.lastClickedLeft,
                    (item) => { st.lastClickedLeft = item; },
                    describe,
                ),

                m('div.selector-controls', [
                    transferBtn('>', '↓', 'Move selected to individual',
                        st.leftSelected.size === 0,
                        () => this.transfer(vnode, st.leftSelected, 'add')),
                    transferBtn('>>', '⇊', 'Move all to individual',
                        selectable.length === 0,
                        () => this.transfer(vnode, selectable, 'add')),
                    transferBtn('<<', '⇈', 'Move all to aggregate',
                        selected.length === 0,
                        () => this.transfer(vnode, selected, 'remove')),
                    transferBtn('<', '↑', 'Move selected to aggregate',
                        st.rightSelected.size === 0,
                        () => this.transfer(vnode, st.rightSelected, 'remove')),
                ]),

                selectList(
                    'Individual Cgroups',
                    selected,
                    st.rightSelected,
                    (item) => toggleMark(st.rightSelected, item),
                    'No cgroups selected',
                    st.lastClickedRight,
                    (item) => { st.lastClickedRight = item; },
                    describeSelected,
                ),
            ]),
            m('div.selector-info', [
                m('small', `${availableNames} available, ${selected.length} selected`),
            ]),
        ]);
    },
};
