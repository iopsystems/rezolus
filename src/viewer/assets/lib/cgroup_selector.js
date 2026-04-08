// cgroup_selector.js - Cgroup selector component for selecting which cgroups to view individually
//
// Attrs:
//   groups: array of group objects with plots
//   executeQuery: (query) => Promise<result> — runs a PromQL range query
//   applyResultToPlot: (plot, result) => void — applies PromQL result to a plot
//   substitutePattern: (query, pattern) => string — substitutes cgroup placeholder
//   setActiveCgroupPattern: (pattern) => void — sets the global active cgroup pattern

// ── Helpers ─────────────────────────────────────────────────────────

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

/** Collect the selected options from a <select> change event into a Set. */
const syncSelection = (set, event) => {
    set.clear();
    for (const opt of event.target.selectedOptions) {
        set.add(opt.value);
    }
};

/** Render a multi-select list with a header. */
const selectList = (title, items, selectionSet, onchange, emptyLabel) =>
    m('div.selector-column', [
        m('h4', title),
        m('select.cgroup-select[multiple]', {
            size: Math.max(2, Math.min(10, items.length || 1)),
            onchange,
        }, items.length === 0
            ? [m('option[disabled]', emptyLabel)]
            : items.map((item) =>
                m('option', { value: item, selected: selectionSet.has(item) }, item),
            ),
        ),
    ]);

/** Render a transfer button with directional arrows (horizontal on desktop, vertical on mobile). */
const transferBtn = (lrLabel, udLabel, title, disabled, onclick) =>
    m('button', { title, disabled, onclick }, [
        m('span.arrow-lr', lrLabel),
        m('span.arrow-ud', udLabel),
    ]);

// ── Persisted state (survives component remount across navigations) ─

let persistedSelectedCgroups = new Set();
let persistedOriginalQueries = null; // Map<string, string>

// ── Component ───────────────────────────────────────────────────────

export const CgroupSelector = {
    oninit(vnode) {
        vnode.state.selectedCgroups = new Set(persistedSelectedCgroups);
        vnode.state.availableCgroups = new Set();
        vnode.state.loading = true;
        vnode.state.error = null;
        vnode.state.leftSelected = new Set();
        vnode.state.rightSelected = new Set();
        vnode.state.originalQueries = persistedOriginalQueries;

        this.fetchAvailableCgroups(vnode);
    },

    async fetchAvailableCgroups(vnode) {
        const { executeQuery } = vnode.attrs;
        const queries = [
            'sum by (name) (cgroup_cpu_usage)',
            'group by (name) (cgroup_cpu_usage)',
            'cgroup_cpu_usage',
            'sum by (name) (rate(cgroup_cpu_usage[1m]))',
        ];

        try {
            let cgroups = new Set();

            for (const query of queries) {
                try {
                    const result = await executeQuery(query);
                    cgroups = extractCgroupNames(result);
                    if (cgroups.size > 0) break;
                } catch (e) {
                    console.warn(`Query failed: ${query}`, e);
                }
            }

            if (cgroups.size === 0) {
                vnode.state.error = 'No cgroup data found';
            }

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
        const { executeQuery, substitutePattern, setActiveCgroupPattern, applyResultToPlot } = vnode.attrs;

        if (vnode.state.updateInProgress) {
            vnode.state.cancelUpdate = true;
            return;
        }

        vnode.state.updateInProgress = true;
        vnode.state.cancelUpdate = false;

        // Build alternation pattern for the selected cgroups
        const selected = Array.from(vnode.state.selectedCgroups);
        const selectedPattern = selected.length > 1
            ? '(' + selected.join('|') + ')'
            : selected[0] || '';

        setActiveCgroupPattern(selectedPattern || null);

        // Snapshot original queries on first update
        if (!vnode.state.originalQueries) {
            vnode.state.originalQueries = new Map();
            for (const [gi, group] of (vnode.attrs.groups || []).entries()) {
                for (const [pi, plot] of (group.plots || []).entries()) {
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
            for (const [pi, plot] of (group.plots || []).entries()) {
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

        return m('div.cgroup-selector', [
            m('h3', 'Cgroup Selection'),
            st.error && m('div.error-message', st.error),
            m('div.selector-container', [
                // Available (aggregate) list
                selectList(
                    'Available Cgroups (Aggregate)',
                    leftItems,
                    st.leftSelected,
                    (e) => syncSelection(st.leftSelected, e),
                    st.loading ? 'Loading cgroups...' : 'No cgroups available',
                ),

                // Transfer buttons
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

                // Selected (individual) list
                selectList(
                    'Individual Cgroups',
                    selected,
                    st.rightSelected,
                    (e) => syncSelection(st.rightSelected, e),
                    'No cgroups selected',
                ),
            ]),
            m('div.selector-info', [
                m('small', `${unselected.length} available, ${selected.length} selected`),
            ]),
        ]);
    },
};
