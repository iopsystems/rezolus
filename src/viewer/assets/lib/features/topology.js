// ── Data transformation ──────────────────────────────────────────────

const buildHierarchy = (topology) => {
    const packages = new Map();
    for (const entry of topology) {
        if (!packages.has(entry.package)) packages.set(entry.package, new Map());
        const pkg = packages.get(entry.package);
        if (!pkg.has(entry.die)) pkg.set(entry.die, new Map());
        const die = pkg.get(entry.die);
        if (!die.has(entry.core)) die.set(entry.core, []);
        die.get(entry.core).push(entry);
    }
    return packages;
};

const buildCacheStructures = (caches) => {
    let l1dSize = null;
    let l1iSize = null;
    const levels = {};

    for (const cache of caches) {
        const level = cache.level.toLowerCase();
        if (level === 'l1d') { l1dSize = cache.size; continue; }
        if (level === 'l1i') { l1iSize = cache.size; continue; }

        const groups = new Map();
        const cpuToGroup = new Map();
        for (let gi = 0; gi < (cache.shared_cpus || []).length; gi++) {
            groups.set(gi, { size: cache.size, cpus: new Set(cache.shared_cpus[gi]) });
            for (const cpuId of cache.shared_cpus[gi]) {
                cpuToGroup.set(cpuId, gi);
            }
        }
        levels[level] = { size: cache.size, groups, cpuToGroup };
    }

    return { l1dSize, l1iSize, levels };
};

// ── Toggle descriptions ──────────────────────────────────────────────

const DESCRIPTIONS = {
    threads: 'Threads (SMT): Hardware threads sharing a physical core and its L1/L2 caches.',
    caches: 'Cache Hierarchy: Shared cache boundaries between cores at each level (L1d, L1i, L2, L3).',
    numa: 'NUMA Topology: Memory locality domains. Each node has local memory and a set of CPUs.',
};

// ── Layout helpers ───────────────────────────────────────────────────

/**
 * Pick the best columns-per-row so cores divide evenly (no dangling last row).
 * Searches [maxCols .. maxCols/2] for the largest exact divisor of totalCores.
 * Falls back to maxCols if no clean split exists.
 */
const bestCols = (totalCores, maxCols) => {
    if (totalCores <= maxCols) return totalCores;
    const minCols = Math.max(1, Math.ceil(maxCols / 2));
    for (let c = maxCols; c >= minCols; c--) {
        if (totalCores % c === 0) return c;
    }
    return maxCols;
};

// ── Cache level helpers ──────────────────────────────────────────────

const getCacheLevels = (levels) => {
    const sorted = Object.keys(levels).sort();
    if (sorted.length === 0) return { mid: [], llc: null };
    const llc = sorted[sorted.length - 1];
    const mid = sorted.slice(0, -1);
    return { mid, llc };
};

const buildTooltip = (coreId, cpus, l1dSize, l1iSize) => {
    const parts = [`Core ${coreId}`];
    parts.push(`CPU ${cpus.map(e => e.cpu).sort((a, b) => a - b).join(', ')}`);
    if (l1dSize) parts.push(`L1d: ${l1dSize}`);
    if (l1iSize) parts.push(`L1i: ${l1iSize}`);
    const numa = cpus[0]?.numa_node;
    if (numa != null) parts.push(`NUMA ${numa}`);
    return parts.join(' · ');
};

// ── Compute NUMA groups ─────────────────────────────────────────────
// Regroup cores by NUMA node within a package, merging dies that share
// the same NUMA node. Returns array of { numaNode, cores: Map<coreId, [entries]> }

const groupByNuma = (dieMap) => {
    const numaMap = new Map();
    for (const [, coreMap] of dieMap) {
        for (const [coreId, entries] of coreMap) {
            const n = entries[0]?.numa_node ?? 0;
            if (!numaMap.has(n)) numaMap.set(n, new Map());
            numaMap.get(n).set(coreId, entries);
        }
    }
    return [...numaMap.entries()]
        .sort(([a], [b]) => a - b)
        .map(([numaNode, cores]) => ({ numaNode, cores }));
};

// ── Layout sizing ────────────────────────────────────────────────────

/**
 * Compute layout parameters from container width and core count.
 */
const computeLayout = (numaGroups, numaPerRow, containerWidth) => {
    let maxCoresPerNuma = 0;
    for (const { cores } of numaGroups) {
        if (cores.size > maxCoresPerNuma) maxCoresPerNuma = cores.size;
    }

    if (!containerWidth) {
        return { borderW: 2, numaPerRow, maxCoresPerNuma, maxCols: 16 };
    }

    // Subtract padding/borders/gaps: package ~14px, NUMA box ~16px + gap ~12px per column
    const overhead = 48 + numaPerRow * 40;
    const widthPerNuma = (containerWidth - overhead) / numaPerRow;

    const maxCols = Math.max(4, Math.floor(widthPerNuma / 48));
    const cols = Math.min(maxCoresPerNuma, maxCols);
    const total = cols * numaPerRow;

    return { borderW: total > 32 ? 1 : 2, numaPerRow, maxCoresPerNuma, maxCols };
};

// ── Rendering ────────────────────────────────────────────────────────

/**
 * Render a single NUMA group: cores grouped by LLC, stacked with cache bars.
 */
const renderNumaGroup = ({ numaNode, cores }, opts) => {
    const { toggles, cacheStructs, layout } = opts;
    const { l1dSize, l1iSize, levels } = cacheStructs;
    const { mid, llc } = getCacheLevels(levels);
    const showCaches = toggles.caches;

    // Group level: lowest shared cache above L1
    const groupLevel = mid.length > 0 ? mid[0] : llc;
    const groupData = groupLevel ? levels[groupLevel] : null;
    const llcData = llc ? levels[llc] : null;

    // Sort cores
    const sortedCores = [...cores.entries()]
        .sort(([a], [b]) => a - b)
        .map(([coreId, cpus]) => ({
            coreId,
            cpus: cpus.sort((a, b) => a.cpu - b.cpu),
            firstCpu: cpus[0]?.cpu,
        }));

    // Sub-group cores by the grouping level (e.g. L2)
    let subGroups;
    if (groupData) {
        const grouped = new Map();
        const ungrouped = [];
        for (const c of sortedCores) {
            const gi = c.firstCpu != null ? groupData.cpuToGroup.get(c.firstCpu) : undefined;
            if (gi != null) {
                if (!grouped.has(gi)) grouped.set(gi, []);
                grouped.get(gi).push(c);
            } else {
                ungrouped.push(c);
            }
        }
        subGroups = [...grouped.entries()]
            .sort(([a], [b]) => a - b)
            .map(([gi, groupCores]) => ({ groupIdx: gi, cores: groupCores, size: groupData.size }));
        if (ungrouped.length > 0) {
            subGroups.push({ groupIdx: -1, cores: ungrouped, size: null });
        }
    } else {
        subGroups = [{ groupIdx: 0, cores: sortedCores, size: null }];
    }

    // Group subgroups by LLC instance
    let llcGrouped;
    if (llcData && llc !== groupLevel) {
        const map = new Map();
        for (const sg of subGroups) {
            const firstCpu = sg.cores[0]?.firstCpu;
            const llcIdx = firstCpu != null ? llcData.cpuToGroup.get(firstCpu) ?? -1 : -1;
            if (!map.has(llcIdx)) map.set(llcIdx, []);
            map.get(llcIdx).push(sg);
        }
        llcGrouped = [...map.entries()]
            .sort(([a], [b]) => a - b)
            .map(([llcIdx, sgs]) => ({
                llcIdx,
                subGroups: sgs,
                allCores: sgs.flatMap(sg => sg.cores),
            }));
    } else {
        llcGrouped = [{
            llcIdx: 0,
            subGroups,
            allCores: subGroups.flatMap(sg => sg.cores),
        }];
    }

    /** Render core cells + L1 row for a slice of cores. */
    const renderCoreCells = (cores) => {
        const cells = [];
        for (const { coreId, cpus } of cores) {
            cells.push(m('div.topo-core', {
                key: `c${coreId}`,
                title: buildTooltip(coreId, cpus, l1dSize, l1iSize),
            }, [
                m('span.core-id', `C${coreId}`),
                opts.hasSMT && toggles.threads && m('div.topo-threads',
                    cpus.map(e => m('span.topo-thread', `T${e.cpu}`)),
                ),
            ]));
        }
        if (showCaches && (l1dSize || l1iSize)) {
            for (const { coreId } of cores) {
                cells.push(m('div.topo-l1-pair', { key: `l1-${coreId}` }, [
                    l1dSize && m('span.topo-l1', [
                        m('span.topo-cache-name', 'L1d'),
                        m('span.topo-cache-size', l1dSize),
                    ]),
                    l1iSize && m('span.topo-l1', [
                        m('span.topo-cache-name', 'L1i'),
                        m('span.topo-cache-size', l1iSize),
                    ]),
                ]));
            }
        }
        return cells;
    };

    /** Render a cache bar (mid-level or LLC). cls is dot-separated. */
    const renderBar = (cls, label, size, key, style) =>
        m(`div.topo-cache-bar.${cls}`, { key, style: style || '' }, [
            m('span.topo-cache-name', label),
            m('span.topo-cache-size', size),
        ]);

    const { maxCols } = layout;

    /**
     * Render an LLC group.
     *
     * Flat mode (fits in maxCols): single CSS grid with spanning bars.
     * Wrapped mode: each sub-group (L2/CCX) as its own grid, flex-wrapped,
     * with the LLC bar full-width below.
     */
    const renderLLCGroup = ({ llcIdx, allCores: llcCores, subGroups: sgs }) => {
        const numCols = llcCores.length;
        // ── Wrapped layout: explicit row packing ──────────────────
        if (numCols > maxCols) {
            const rowCols = bestCols(numCols, maxCols);

            // Group sub-groups by core count to infer type (P-core vs E-core).
            // Each type is packed independently so rows are even within type.
            const sgsByType = new Map();
            for (const sg of sgs) {
                const key = sg.cores.length;
                if (!sgsByType.has(key)) sgsByType.set(key, []);
                sgsByType.get(key).push(sg);
            }

            const allRowDivs = [];
            let rowIdx = 0;

            // Process each type, largest sub-groups first
            for (const [, typeSgs] of [...sgsByType.entries()].sort(([a], [b]) => b - a)) {
                const totalTypeCores = typeSgs.reduce((s, sg) => s + sg.cores.length, 0);
                const typeTarget = bestCols(totalTypeCores, rowCols);

                const elems = [];
                for (const { groupIdx, cores: sgCores, size } of typeSgs) {
                    if (sgCores.length <= typeTarget) {
                        const children = renderCoreCells(sgCores);
                        if (showCaches && size && groupLevel) {
                            children.push(renderBar('topo-mid-cache',
                                groupLevel.toUpperCase(), size,
                                `mid-${groupIdx}`,
                                `grid-column:1/-1`));
                        }
                        elems.push({ cols: sgCores.length, el: m('div.topo-subgroup-grid', {
                            key: `sg-${groupIdx}`,
                            style: `grid-template-columns:repeat(${sgCores.length},1fr);flex:${sgCores.length} 1 0`,
                        }, children) });
                    } else {
                        const chunkCols = bestCols(sgCores.length, typeTarget);
                        for (let i = 0; i < sgCores.length; i += chunkCols) {
                            const slice = sgCores.slice(i, i + chunkCols);
                            elems.push({ cols: slice.length, el: m('div.topo-subgroup-grid', {
                                key: `sg-${groupIdx}-${i}`,
                                style: `grid-template-columns:repeat(${slice.length},1fr);flex:${slice.length} 1 0`,
                            }, renderCoreCells(slice)) });
                        }
                        if (showCaches && size && groupLevel) {
                            elems.push({ cols: Infinity, el: renderBar('topo-mid-cache.topo-full-width',
                                groupLevel.toUpperCase(), size,
                                `mid-full-${groupIdx}`, '') });
                        }
                    }
                }

                // Pack this type's elements into balanced rows
                let curRow = [], curCols = 0;
                for (const e of elems) {
                    if (e.cols === Infinity) {
                        if (curRow.length) { allRowDivs.push(m('div.topo-llc-row', { key: `row-${rowIdx++}` }, curRow.map(x => x.el))); curRow = []; curCols = 0; }
                        allRowDivs.push(e.el);
                    } else if (curCols + e.cols > typeTarget && curRow.length) {
                        allRowDivs.push(m('div.topo-llc-row', { key: `row-${rowIdx++}` }, curRow.map(x => x.el)));
                        curRow = [e]; curCols = e.cols;
                    } else {
                        curRow.push(e); curCols += e.cols;
                    }
                }
                if (curRow.length) {
                    allRowDivs.push(m('div.topo-llc-row', { key: `row-${rowIdx++}` }, curRow.map(x => x.el)));
                }
            }

            // LLC bar (when separate from group level)
            if (showCaches && llc && llc !== groupLevel && llcData) {
                allRowDivs.push(renderBar('topo-llc.topo-full-width',
                    llc.toUpperCase(), llcData.size,
                    `llc-${llcIdx}`, ''));
            }

            return m('div.topo-llc-group-wrap', { key: llcIdx }, allRowDivs);
        }

        // ── Flat layout: single grid with spanning cache bars ─────
        const coreColIdx = new Map();
        llcCores.forEach((c, i) => coreColIdx.set(c.coreId, i));

        const children = renderCoreCells(llcCores);

        if (showCaches && groupLevel) {
            for (const { groupIdx, cores: sgCores, size } of sgs) {
                if (!size) continue;
                const startCol = coreColIdx.get(sgCores[0].coreId) + 1;
                const span = sgCores.length;
                children.push(renderBar('topo-mid-cache',
                    groupLevel.toUpperCase(), size,
                    `mid-${groupIdx}`,
                    `grid-column:${startCol}/span ${span}` ));
            }
        }

        if (showCaches && llc && llc !== groupLevel && llcData) {
            children.push(renderBar('topo-llc',
                llc.toUpperCase(), llcData.size,
                `llc-${llcIdx}`,
                `grid-column:1/-1` ));
        }

        return m('div.topo-llc-group', {
            key: llcIdx,
            style: `grid-template-columns:repeat(${numCols},1fr)`,
        }, children);
    };

    const numaChildren = [];
    if (toggles.numa) {
        numaChildren.push(m('div.topo-numa-header', `NUMA ${numaNode}`));
    }
    numaChildren.push(m('div.topo-llc-groups', llcGrouped.map(renderLLCGroup)));

    return m('div.topo-numa-box', {
        class: toggles.numa ? 'numa-highlighted' : '',
    }, numaChildren);
};

// ── Component ────────────────────────────────────────────────────────

const CpuTopology = {
    oninit() {
        this.toggles = { threads: true, caches: true, numa: true };
        this.containerWidth = null;
    },

    oncreate(vnode) {
        const state = this;
        state.containerWidth = vnode.dom.offsetWidth;
        state._ro = new ResizeObserver(([entry]) => {
            const w = Math.round(entry.contentRect.width);
            if (w !== state.containerWidth) {
                state.containerWidth = w;
                m.redraw();
            }
        });
        state._ro.observe(vnode.dom);
        m.redraw();
    },

    onremove() {
        if (this._ro) { this._ro.disconnect(); this._ro = null; }
    },

    view({ attrs }) {
        const info = attrs.data;
        if (!info || !info.cpu_topology || info.cpu_topology.length === 0) return null;

        const packages = buildHierarchy(info.cpu_topology);
        const cacheStructs = buildCacheStructures(info.caches || []);
        const toggles = this.toggles;

        // Detect SMT: any core with more than one hardware thread
        let hasSMT = false;
        outer: for (const dieMap of packages.values()) {
            for (const cores of dieMap.values()) {
                for (const cpus of cores.values()) {
                    if (cpus.length > 2) {
                        console.warn('[topo] unexpected: %d threads per core', cpus.length);
                    }
                    if (cpus.length > 1) { hasSMT = true; break outer; }
                }
            }
        }

        const pkgNumaGroups = [...packages.entries()]
            .sort(([a], [b]) => a - b)
            .map(([pkgId, dieMap]) => ({
                pkgId,
                numaGroups: groupByNuma(dieMap),
            }));

        const totalNumaGroups = pkgNumaGroups.reduce((s, p) => s + p.numaGroups.length, 0);

        const allNumaGroups = pkgNumaGroups.flatMap(p => p.numaGroups);
        const numaPerRow = Math.min(totalNumaGroups, 2);
        const layout = computeLayout(allNumaGroups, numaPerRow, this.containerWidth);

        const renderOpts = { hasSMT, toggles, cacheStructs, layout };

        const activeDescs = Object.entries(toggles)
            .filter(([key, on]) => on && (key !== 'threads' || hasSMT))
            .map(([key]) => DESCRIPTIONS[key]);

        const innerChildren = [
            m('div.topo-toggles', Object.entries(toggles)
                .filter(([key]) => key !== 'threads' || hasSMT)
                .map(([key, on]) =>
                    m(`button.topo-toggle.topo-toggle-${key}`, {
                        class: on ? 'active' : '',
                        onclick: () => { this.toggles[key] = !on; },
                    }, key === 'numa' ? 'NUMA' : key.charAt(0).toUpperCase() + key.slice(1)),
                ),
            ),
            m('div.topo-packages', pkgNumaGroups.map(({ pkgId, numaGroups }) =>
                m('div.topo-package', { key: pkgId }, [
                    m('div.topo-package-header', `Package ${pkgId}`),
                    m('div.topo-numa-grid', numaGroups.map(ng =>
                        renderNumaGroup(ng, renderOpts),
                    )),
                ]),
            )),
        ];

        if (activeDescs.length > 0) {
            innerChildren.push(m('div.topo-descriptions',
                activeDescs.map(text => m('p.topo-desc', text)),
            ));
        }

        return m('div.topo-canvas', {
            style: `--topo-border:${layout.borderW}px;--topo-numa-cols:${numaPerRow}`,
        }, [m('div.topo-inner', innerChildren)]);
    },
};

export { CpuTopology };
