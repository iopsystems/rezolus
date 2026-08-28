// GPU selector — a two-panel picker (available GPUs on the left, selected on
// the right) that matches the cgroup selector's UI. Selecting GPUs filters the
// GPU section's non-per-GPU charts to those `id`s (empty = all GPUs). Per-GPU
// charts always show all GPUs and are unaffected (handled in data.js by
// skipping queries that group `by (id)`).
//
// A GPU is identified by `(vendor, id)`, not `id` alone: every vendor's sampler
// numbers its own devices from 0, so a host with an NVIDIA card and an Intel
// iGPU has two distinct GPUs both labelled `id="0"`. The list is keyed on the
// pair, and the pair is what `onChange` reports.
//
// Attrs:
//   gpus: {id, vendor?}[]          — available GPUs, from the section metadata
//   ids: number[]                  — legacy id-only list, used when `gpus` is absent
//   selected: {vendor, id}[]       — currently selected GPUs
//   onChange: (gpus: {vendor, id}[]) => void — called when the selection changes
//   details: {index, name, vendor, memory_bytes}[] — System Info GPU details,
//         used to show each GPU's model name next to its id

import globalColorMapper from '../charts/util/colormap.js';

// Set key for one GPU. NUL cannot occur in a label value, so it cannot collide
// with a vendor or id that merely contains the separator.
const gpuKey = (g) => `${g.vendor ?? ''}\u0000${g.id}`;
const parseKey = (key) => {
    const [vendor, id] = key.split('\u0000');
    return { vendor: vendor || null, id };
};

// Two GPUs can share an id, so the id alone is not a label. Name the vendor
// whenever the recording has more than one.
const gpuLabel = (key, showVendor) => {
    const { vendor, id } = parseKey(key);
    return showVendor && vendor ? `${vendor} GPU ${id}` : `GPU ${id}`;
};

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
 *  Items are GPU keys (see `gpuKey`); `labelFn(key)` renders the display name
 *  and `detailFn(key)` returns the model/details string shown under it. */
const selectList = (title, items, selectionSet, onToggle, emptyLabel, lastClicked, setLastClicked, detailFn, labelFn) =>
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
                    title: detail ? `${labelFn(item)} — ${detail}` : labelFn(item),
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
                        style: { background: globalColorMapper.getColorByName(labelFn(item)) },
                    }),
                    m('span.cgroup-select-name', [
                        m('span.gpu-select-id', labelFn(item)),
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
        // selectedGpus: the GPUs on the right (filtered), held as `gpuKey`
        // strings so a Set can hold (vendor, id) pairs. Seeded from attrs,
        // accepting either pairs or bare ids.
        vnode.state.selectedGpus = new Set(
            (vnode.attrs.selected || []).map((g) => (g !== null && typeof g === 'object')
                ? gpuKey({ vendor: g.vendor ?? null, id: String(g.id) })
                : gpuKey({ vendor: null, id: String(g) })),
        );
        vnode.state.leftSelected = new Set();  // highlighted-but-not-transferred (left)
        vnode.state.rightSelected = new Set();
        vnode.state.lastClickedLeft = null;
        vnode.state.lastClickedRight = null;
    },

    commit(vnode) {
        // Report {vendor, id} pairs, not the internal key strings.
        vnode.attrs.onChange(Array.from(vnode.state.selectedGpus).map(parseKey));
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

        // Prefer the (vendor, id) pairs; fall back to the id-only list for a
        // recording whose metadata predates them.
        const entries = Array.isArray(vnode.attrs.gpus) && vnode.attrs.gpus.length > 0
            ? vnode.attrs.gpus.map((g) => ({ vendor: g.vendor ?? null, id: String(g.id) }))
            : (vnode.attrs.ids || []).map((id) => ({ vendor: null, id: String(id) }));

        // Only qualify names with the vendor when more than one is present;
        // "intel GPU 0" is noise on a single-vendor host.
        const showVendor = new Set(entries.map((g) => g.vendor).filter(Boolean)).size > 1;
        const labelFn = (key) => gpuLabel(key, showVendor);

        const all = entries.map(gpuKey);
        const available = all.filter((k) => !st.selectedGpus.has(k));
        const selected = all.filter((k) => st.selectedGpus.has(k));

        const toggleMark = (set, item) => {
            if (set.has(item)) set.delete(item);
            else set.add(item);
        };

        // Map GPU -> details string ("AMD Radeon ... · 32 GB") from System Info.
        // Keyed on (vendor, index): System Info's `index` is the same
        // vendor-local number as the metric `id`, so on a mixed-vendor host the
        // vendor is what keeps two `index: 0` entries apart. Fall back to a
        // match on index alone when the entry carries no vendor.
        const details = vnode.attrs.details || [];
        const byPair = new Map(
            details.map((g) => [gpuKey({ vendor: g.vendor ?? null, id: String(g.index) }), g]),
        );
        const byIndexOnly = new Map(details.map((g) => [String(g.index), g]));
        const detailFn = (key) => {
            const { vendor, id } = parseKey(key);
            const g = byPair.get(key)
                || (vendor ? undefined : byIndexOnly.get(id))
                || byIndexOnly.get(id);
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
                    labelFn,
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
                    labelFn,
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
