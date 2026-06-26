// GPU selector — a two-panel picker (available GPUs on the left, selected on
// the right) that matches the cgroup selector's UI. Selecting GPUs filters the
// GPU section's non-per-GPU charts to those `id`s (empty = all GPUs). Per-GPU
// charts always show all GPUs and are unaffected (handled in data.js by
// skipping queries that group `by (id)`).
//
// Attrs:
//   ids: number[]                  — available GPU ids (e.g. [0, 1])
//   selected: (string|number)[]    — currently selected ids
//   onChange: (ids: string[]) => void — called when the selection changes
//   gpus: {index, name, vendor, memory_bytes}[] — System Info GPU details,
//         used to show each GPU's model name next to its id

import globalColorMapper from '../charts/util/colormap.js';

const gpuLabel = (id) => `GPU ${id}`;

// Compact byte formatter for the GPU memory shown in the selector (e.g. "32 GB").
const formatBytes = (bytes) => {
    if (!bytes || bytes <= 0) return '';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let v = bytes;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
    return `${v >= 10 || Number.isInteger(v) ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
};

/** Render one labeled multi-select column with a color swatch per GPU.
 *  Mirrors the cgroup selector's selectList (shift+click range select).
 *  `detailFn(id)` returns the model/details string shown under the id. */
const selectList = (title, items, selectionSet, onToggle, emptyLabel, lastClicked, setLastClicked, detailFn) =>
    m('div.selector-column', [
        m('h4', title),
        m('ul.cgroup-select', {
            role: 'listbox',
            'aria-multiselectable': 'true',
        }, items.length === 0
            ? m('li.cgroup-select-empty', emptyLabel)
            : items.map((item) => {
                const detail = detailFn ? detailFn(item) : '';
                return m('li.cgroup-select-item', {
                    role: 'option',
                    'aria-selected': selectionSet.has(item) ? 'true' : 'false',
                    class: selectionSet.has(item) ? 'selected' : '',
                    title: detail ? `${gpuLabel(item)} — ${detail}` : gpuLabel(item),
                    onclick: (e) => {
                        if (e.shiftKey && lastClicked != null) {
                            const from = items.indexOf(lastClicked);
                            const to = items.indexOf(item);
                            if (from !== -1 && to !== -1) {
                                const lo = Math.min(from, to);
                                const hi = Math.max(from, to);
                                for (let i = lo; i <= hi; i++) selectionSet.add(items[i]);
                            }
                        } else {
                            onToggle(item);
                        }
                        setLastClicked(item);
                    },
                }, [
                    m('span.cgroup-select-swatch', {
                        style: { background: globalColorMapper.getColorByName(gpuLabel(item)) },
                    }),
                    m('span.cgroup-select-name', [
                        m('span.gpu-select-id', gpuLabel(item)),
                        detail && m('span.gpu-select-detail', detail),
                    ]),
                ]);
            }),
        ),
    ]);

/** Transfer button with directional arrows (horizontal desktop / vertical mobile). */
const transferBtn = (lrLabel, udLabel, title, disabled, onclick) =>
    m('button', { title, disabled, onclick }, [
        m('span.arrow-lr', lrLabel),
        m('span.arrow-ud', udLabel),
    ]);

export const GpuSelector = {
    oninit(vnode) {
        // selectedGpus: the ids on the right (filtered). Seeded from attrs.
        vnode.state.selectedGpus = new Set((vnode.attrs.selected || []).map(String));
        vnode.state.leftSelected = new Set();  // highlighted-but-not-transferred (left)
        vnode.state.rightSelected = new Set();
        vnode.state.lastClickedLeft = null;
        vnode.state.lastClickedRight = null;
    },

    commit(vnode) {
        vnode.attrs.onChange(Array.from(vnode.state.selectedGpus));
    },

    transfer(vnode, items, action) {
        const st = vnode.state;
        for (const item of items) {
            if (action === 'add') st.selectedGpus.add(item);
            else st.selectedGpus.delete(item);
        }
        st.leftSelected.clear();
        st.rightSelected.clear();
        this.commit(vnode);
    },

    view(vnode) {
        const st = vnode.state;
        const all = (vnode.attrs.ids || []).map(String);
        const available = all.filter((id) => !st.selectedGpus.has(id));
        const selected = all.filter((id) => st.selectedGpus.has(id));

        const toggleMark = (set, item) => {
            if (set.has(item)) set.delete(item);
            else set.add(item);
        };

        // Map GPU id -> details string ("AMD Radeon ... · 32 GB") from System Info.
        const byIndex = new Map(
            (vnode.attrs.gpus || []).map((g) => [String(g.index), g]),
        );
        const detailFn = (id) => {
            const g = byIndex.get(String(id));
            if (!g) return '';
            const parts = [];
            if (g.name || g.vendor) parts.push(g.name || g.vendor);
            if (g.memory_bytes) parts.push(formatBytes(g.memory_bytes));
            return parts.join(' · ');
        };

        return m('div.cgroup-selector', [
            m('h3', 'GPU Selection'),
            m('div.selector-container', [
                selectList(
                    'Available GPUs (All)',
                    available,
                    st.leftSelected,
                    (item) => toggleMark(st.leftSelected, item),
                    'No GPUs available',
                    st.lastClickedLeft,
                    (item) => { st.lastClickedLeft = item; },
                    detailFn,
                ),

                m('div.selector-controls', [
                    transferBtn('>', '↓', 'Show selected GPUs',
                        st.leftSelected.size === 0,
                        () => this.transfer(vnode, Array.from(st.leftSelected), 'add')),
                    transferBtn('>>', '⇊', 'Show all GPUs individually',
                        available.length === 0,
                        () => this.transfer(vnode, available, 'add')),
                    transferBtn('<<', '⇈', 'Back to aggregate (all)',
                        selected.length === 0,
                        () => this.transfer(vnode, selected, 'remove')),
                    transferBtn('<', '↑', 'Remove selected from filter',
                        st.rightSelected.size === 0,
                        () => this.transfer(vnode, Array.from(st.rightSelected), 'remove')),
                ]),

                selectList(
                    'Selected GPUs',
                    selected,
                    st.rightSelected,
                    (item) => toggleMark(st.rightSelected, item),
                    'No GPUs selected (showing aggregate)',
                    st.lastClickedRight,
                    (item) => { st.lastClickedRight = item; },
                    detailFn,
                ),
            ]),
            m('div.selector-info', [
                m('small', selected.length === 0
                    ? `Showing aggregate across all ${all.length} GPUs`
                    : `${available.length} available, ${selected.length} selected`),
            ]),
        ]);
    },
};
