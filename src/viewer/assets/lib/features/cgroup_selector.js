// Attrs:
//   groups: array of group objects with plots
//   executeQuery: (query) => Promise<result> — runs a PromQL range query
//   applyResultToPlot: (plot, result) => void — applies PromQL result to a plot
//   substitutePattern: (query, pattern) => string — substitutes cgroup placeholder
//   setActiveCgroupPattern: (pattern) => void — sets the global active cgroup pattern

import globalColorMapper from '../charts/util/colormap.js';
import { collectGroupPlots } from './group_utils.js';
import { writeViewState } from '../ui/url_state.js';
import { discoverCgroups } from './cgroup_discovery.js';

// Why a cgroup cannot be selected, or null when it can. A selection reaches
// the query engine as a regex matcher, `name=~"a|b"`: promql-parser unquotes
// the string and requires it to compile as a regex, then metriken-storage
// `match_pattern` compares each `|`-separated alternative with the name
// literally, after turning `\.` into `.`. A name made of letters, digits,
// `/ _ . : @ - = , ~ %` and `\xHH` escapes (systemd's escaped characters)
// compiles and compares equal to itself; those characters cover systemd
// units, container ids and pod names. Any other name is not offered.
const SELECTABLE = /^(?:[A-Za-z0-9/_.:@=,~%-]|\\x[0-9A-Fa-f]{2})*$/;
export const cgroupUnselectableReason = (name) => (SELECTABLE.test(name)
    ? null
    : 'Cannot be selected: the name has characters the query engine\'s matcher cannot express');

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


/** Render a custom multi-select list with a color swatch per item.
 *  Supports shift+click range selection via lastClicked / setLastClicked.
 *  An item cgroupUnselectableReason rejects is drawn disabled, with the
 *  reason as its title, and cannot be marked. */
const selectList = (title, items, selectionSet, onToggle, emptyLabel, lastClicked, setLastClicked) =>
    m('div.selector-column', [
        m('h4', title),
        m('ul.cgroup-select', {
            role: 'listbox',
            'aria-multiselectable': 'true',
        }, items.length === 0
            ? m('li.cgroup-select-empty', emptyLabel)
            : items.map((item) =>
                m('li.cgroup-select-item', {
                    role: 'option',
                    'aria-selected': selectionSet.has(item) ? 'true' : 'false',
                    'aria-disabled': cgroupUnselectableReason(item) ? 'true' : undefined,
                    title: cgroupUnselectableReason(item) || undefined,
                    class: cgroupUnselectableReason(item)
                        ? 'disabled'
                        : (selectionSet.has(item) ? 'selected' : ''),
                    onclick: (e) => {
                        if (cgroupUnselectableReason(item)) return;
                        if (e.shiftKey && lastClicked != null) {
                            const from = items.indexOf(lastClicked);
                            const to = items.indexOf(item);
                            if (from !== -1 && to !== -1) {
                                const lo = Math.min(from, to);
                                const hi = Math.max(from, to);
                                for (let i = lo; i <= hi; i++) {
                                    if (!cgroupUnselectableReason(items[i])) selectionSet.add(items[i]);
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
                    m('span.cgroup-select-name', item),
                ]),
            ),
        ),
    ]);

/** Render a transfer button with directional arrows (horizontal on desktop, vertical on mobile). */
const transferBtn = (lrLabel, udLabel, title, disabled, onclick) =>
    m('button', { title, disabled, onclick }, [
        m('span.arrow-lr', lrLabel),
        m('span.arrow-ud', udLabel),
    ]);

// Persisted state — survives component remount across navigations.
let persistedSelectedCgroups = new Set();
let persistedOriginalQueries = null; // Map<string, string>

// Seed the selection before the component first mounts, from the link's
// `cgroup=` keys. Names are validated against the recording once the
// available list arrives (fetchAvailableCgroups); unknown ones are dropped
// there with a warning rather than left to render empty charts.
export const seedSelectedCgroups = (names) => {
    const seeded = (Array.isArray(names) ? names : []).filter((n) => typeof n === 'string' && n !== '');
    const unusable = seeded.filter((n) => cgroupUnselectableReason(n));
    if (unusable.length > 0) {
        console.warn(`[cgroups] dropping selection the query engine cannot match: ${unusable.join(', ')}`);
    }
    persistedSelectedCgroups = new Set(seeded.filter((n) => !cgroupUnselectableReason(n)));
};

// The selection as the pattern the selector substitutes for
// `__SELECTED_CGROUPS__`, or null when nothing is selected. A page that does
// not mount the selector (an expanded chart) reads the selection from here.
export const selectedCgroupPattern = () => cgroupPattern(Array.from(persistedSelectedCgroups));

export const CgroupSelector = {
    oninit(vnode) {
        vnode.state.selectedCgroups = new Set(persistedSelectedCgroups);
        vnode.state.availableCgroups = new Set();
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
        if (persistedSelectedCgroups.size > 0) {
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

            // A selection that names cgroups this recording does not have
            // (a link from another host, a stale persisted set) would render
            // every individual chart empty with nothing to say why. Drop the
            // unknown names, say so once, and rewrite the link to match.
            if (cgroups.size > 0 && vnode.state.selectedCgroups.size > 0) {
                const unknown = [...vnode.state.selectedCgroups].filter((cg) => !cgroups.has(cg));
                if (unknown.length > 0) {
                    console.warn(`[cgroups] dropping selection not in this recording: ${unknown.join(', ')}`);
                    for (const cg of unknown) vnode.state.selectedCgroups.delete(cg);
                    persistedSelectedCgroups = new Set(vnode.state.selectedCgroups);
                    writeViewState({ cgroup: [...persistedSelectedCgroups] });
                    this.debouncedUpdateQueries(vnode);
                }
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

        const selectedPattern = cgroupPattern(Array.from(vnode.state.selectedCgroups)) || '';

        setActiveCgroupPattern(selectedPattern || null);

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

        // Collect plots whose original query contains the cgroup placeholder
        const plotsToUpdate = [];
        for (const [gi, group] of (vnode.attrs.groups || []).entries()) {
            for (const [pi, plot] of collectGroupPlots(group).entries()) {
                const orig = vnode.state.originalQueries.get(`${gi}-${pi}`);
                if (orig && orig.includes('__SELECTED_CGROUPS__')) {
                    plotsToUpdate.push({
                        plot,
                        query: substitutePattern(orig, selectedPattern || null),
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
            await Promise.all(batch.map(async ({ plot, query }) => {
                plot.promql_query = query;
                try {
                    const result = await executeQuery(query);
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

    /** Move items between selected/available and trigger a query update. */
    transfer(vnode, items, direction) {
        const op = direction === 'add' ? 'add' : 'delete';
        for (const cg of items) {
            if (op === 'add' && cgroupUnselectableReason(cg)) continue;
            vnode.state.selectedCgroups[op](cg);
        }
        persistedSelectedCgroups = new Set(vnode.state.selectedCgroups);
        writeViewState({ cgroup: [...persistedSelectedCgroups] });
        vnode.state.leftSelected.clear();
        vnode.state.rightSelected.clear();
        this.debouncedUpdateQueries(vnode);
    },

    view(vnode) {
        const st = vnode.state;
        const unselected = Array.from(st.availableCgroups)
            .filter((cg) => !st.selectedCgroups.has(cg))
            .sort();
        const selectable = unselected.filter((cg) => !cgroupUnselectableReason(cg));
        const selected = Array.from(st.selectedCgroups).sort();

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
                ),
            ]),
            m('div.selector-info', [
                m('small', `${unselected.length} available, ${selected.length} selected`),
            ]),
        ]);
    },
};
