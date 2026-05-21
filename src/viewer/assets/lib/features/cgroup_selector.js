// Attrs:
//   groups: array of group objects with plots
//   executeQuery: (sql) => Promise<result> — runs a SQL range query
//                 against the active capture (returns Prometheus
//                 matrix-shape JSON). Used for cgroup name discovery
//                 and per-plot refresh on selection change.
//   applyResultToPlot: (plot, result) => void — paints a query result
//                      onto a plot's data structure.
//   setSelectedCgroups: (names: string[]) => void — informs the
//                       capture registry that the user's cgroup
//                       selection changed. viewer-sql substitutes
//                       `__SELECTED_CGROUPS__` server-side from this
//                       state, so plots only need to re-fetch — no
//                       client-side query rewriting.

import globalColorMapper from '../charts/util/colormap.js';
import { collectGroupPlots } from './group_utils.js';

/** Extract cgroup names from a PromQL query result's metric labels. */
const extractCgroupNames = (result) => {
    const names = new Set();
    if (result.status !== 'success' || !result.data?.result?.length) return names;

    for (const series of result.data.result) {
        if (!series.metric) continue;
        for (const [key, value] of Object.entries(series.metric)) {
            if ((key === 'name' || key.includes('cgroup') || key === 'container') && value) {
                names.add(value);
            }
        }
    }
    return names;
};

/** Render a custom multi-select list with a color swatch per item.
 *  Supports shift+click range selection via lastClicked / setLastClicked. */
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
                    class: selectionSet.has(item) ? 'selected' : '',
                    onclick: (e) => {
                        if (e.shiftKey && lastClicked != null) {
                            const from = items.indexOf(lastClicked);
                            const to = items.indexOf(item);
                            if (from !== -1 && to !== -1) {
                                const lo = Math.min(from, to);
                                const hi = Math.max(from, to);
                                for (let i = lo; i <= hi; i++) {
                                    selectionSet.add(items[i]);
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

        // When re-initialized with persisted selections (e.g. after granularity
        // or node change unmounts/remounts the component tree), re-fetch the
        // individual-side plots so they reflect the current selection.
        if (persistedSelectedCgroups.size > 0) {
            // Push the selection back into the registry — the component's
            // state survives remount, but registry state may have been
            // rebuilt (e.g. on parquet reload).
            vnode.attrs.setSelectedCgroups?.(Array.from(persistedSelectedCgroups));
            this.debouncedUpdateQueries(vnode);
        }
    },

    async fetchAvailableCgroups(vnode) {
        const { executeQuery } = vnode.attrs;
        // The registry pre-populates a `_cgroup_index` table at parquet
        // load time keyed on (metric, column_name, name, id, labels).
        // One SQL query against it gives us every distinct cgroup name
        // — replaces the legacy four PromQL probes.
        //
        // The `t` projection is constant; query_range's outer wrap
        // requires a `t` column for the Prom-matrix shaper, but the
        // value doesn't matter for name discovery.
        const sql = `SELECT 0::DOUBLE AS t, name AS name, COUNT(*)::DOUBLE AS v
                     FROM _cgroup_index WHERE name IS NOT NULL
                     GROUP BY name`;
        try {
            const result = await executeQuery(sql);
            const cgroups = extractCgroupNames(result);
            if (cgroups.size === 0) vnode.state.error = 'No cgroup data found';
            vnode.state.availableCgroups = cgroups;
        } catch (error) {
            console.error('Failed to fetch available cgroups:', error);
            vnode.state.error = 'Failed to load cgroups: ' + error.message;
            vnode.state.availableCgroups = new Set();
        }
        vnode.state.loading = false;
        m.redraw();
    },

    async updateQueries(vnode) {
        const { executeQuery, applyResultToPlot, setSelectedCgroups } = vnode.attrs;

        if (vnode.state.updateInProgress) {
            vnode.state.cancelUpdate = true;
            return;
        }
        vnode.state.updateInProgress = true;
        vnode.state.cancelUpdate = false;

        // Inform the capture registry of the new selection. viewer-sql
        // substitutes `__SELECTED_CGROUPS__` server-side from this
        // state on every subsequent query; the registry's result cache
        // key includes the selection vector, so re-fetched plots will
        // miss the cache and pick up fresh data.
        const selected = Array.from(vnode.state.selectedCgroups);
        setSelectedCgroups?.(selected);

        const generation = ++vnode.state.updateGeneration || 1;
        vnode.state.updateGeneration = generation;

        // Collect plots whose SQL contains the cgroup placeholder.
        // Plots without it are unaffected by selection and don't need
        // refetching. (Same membership check as the legacy PromQL
        // path, just over `sql_query` instead of the snapshotted
        // PromQL originals — no per-plot string rewriting needed.)
        const plotsToUpdate = [];
        for (const group of (vnode.attrs.groups || [])) {
            for (const plot of collectGroupPlots(group)) {
                if (plot.sql_query?.includes('__SELECTED_CGROUPS__')) {
                    plotsToUpdate.push(plot);
                }
            }
        }

        // Same batched, generation-tokened orchestration as the legacy
        // path. The query string is unchanged across selections (the
        // registry handles substitution); only the cache miss + fetch
        // is what's new.
        //
        // Redraw after EACH plot's data lands rather than after the
        // entire batch — cgroup queries can be ~100–300 ms each, so
        // the user sees per-cgroup charts appear one-by-one instead
        // of all-at-once after the slowest finishes.
        const BATCH_SIZE = 5;
        for (let i = 0; i < plotsToUpdate.length; i += BATCH_SIZE) {
            if (vnode.state.cancelUpdate || vnode.state.updateGeneration !== generation) {
                vnode.state.updateInProgress = false;
                return;
            }
            const batch = plotsToUpdate.slice(i, i + BATCH_SIZE);
            await Promise.all(batch.map(async (plot) => {
                try {
                    const result = await executeQuery(plot.sql_query);
                    if (vnode.state.updateGeneration !== generation) return;
                    applyResultToPlot(plot, result);
                    m.redraw();
                } catch (error) {
                    console.error(`Failed query for ${plot.opts.title}:`, error);
                    plot.data = [];
                    plot.series_names = [];
                    m.redraw();
                }
            }));
        }
        vnode.state.updateInProgress = false;
    },

    debouncedUpdateQueries(vnode) {
        if (vnode.state.updateTimer) clearTimeout(vnode.state.updateTimer);
        vnode.state.updateTimer = setTimeout(() => this.updateQueries(vnode), 300);
    },

    /** Move items between selected/available and trigger a query update. */
    transfer(vnode, items, direction) {
        const op = direction === 'add' ? 'add' : 'delete';
        for (const cg of items) {
            vnode.state.selectedCgroups[op](cg);
        }
        persistedSelectedCgroups = new Set(vnode.state.selectedCgroups);
        vnode.state.leftSelected.clear();
        vnode.state.rightSelected.clear();
        this.debouncedUpdateQueries(vnode);
    },

    view(vnode) {
        const st = vnode.state;
        const unselected = Array.from(st.availableCgroups)
            .filter((cg) => !st.selectedCgroups.has(cg))
            .sort();
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
                        unselected.length === 0,
                        () => this.transfer(vnode, unselected, 'add')),
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
