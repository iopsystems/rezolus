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
 * Decides labeled vs compact mode, border width, and compact cell size.
 */
const computeLayout = (numaGroups, numaPerRow, containerWidth) => {
    let maxCoresPerNuma = 0;
    for (const { cores } of numaGroups) {
        if (cores.size > maxCoresPerNuma) maxCoresPerNuma = cores.size;
    }

    const effectiveCores = maxCoresPerNuma * numaPerRow;

    let labeled, cellWidth;
    if (containerWidth) {
        // Subtract padding/borders/gaps: package ~14px, NUMA box ~16px + gap ~12px per column
        const overhead = 48 + numaPerRow * 40;
        cellWidth = (containerWidth - overhead) / effectiveCores;
        labeled = cellWidth >= 48;
    } else {
        labeled = effectiveCores <= 24;
        cellWidth = null;
    }

    const borderW = effectiveCores > 32 ? 1 : labeled ? 2 : 1;

    // In compact mode, cap cell size for comfortable proportions
    const cellSize = !labeled && cellWidth ? Math.min(cellWidth, 32) : null;

    return { labeled, borderW, numaPerRow, maxCoresPerNuma, cellSize };
};

// ── Rendering ────────────────────────────────────────────────────────

/**
 * Render a single NUMA group: cores grouped by LLC, stacked with cache bars.
 */
const renderNumaGroup = ({ numaNode, cores }, opts) => {
    const { toggles, cacheStructs, layout } = opts;
    const { labeled, cellSize } = layout;
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

    /**
     * Render an LLC group as a CSS grid:
     *   Row 1: core cells (one per column)
     *   Row 2: L1 cells (one per column)
     *   Row 3: mid-level cache bars (each spanning its shared cores)
     *   Row 4: LLC bar (spanning all columns)
     */
    const renderLLCGroup = ({ llcIdx, allCores: llcCores, subGroups: sgs }) => {
        const numCols = llcCores.length;
        const coreColIdx = new Map();
        llcCores.forEach((c, i) => coreColIdx.set(c.coreId, i));

        // Compact mode heights
        const coreH = cellSize ? `height:${Math.round(cellSize)}px` : '';
        const barH = cellSize ? `height:${Math.max(8, Math.round(cellSize / 2))}px` : '';

        const children = [
            // Row: cores
            ...llcCores.map(({ coreId, cpus }) =>
                m('div.topo-core', {
                    key: `c${coreId}`,
                    title: buildTooltip(coreId, cpus, l1dSize, l1iSize),
                    style: labeled ? '' : coreH,
                }, labeled ? [
                    m('span.core-id', `C${coreId}`),
                    opts.hasSMT && toggles.threads && m('div.topo-threads',
                        cpus.map(e => m('span.topo-thread', `T${e.cpu}`)),
                    ),
                ] : []),
            ),
        ];

        // Row: L1
        if (showCaches && (l1dSize || l1iSize)) {
            children.push(...llcCores.map(({ coreId }) =>
                labeled
                    ? m('div.topo-l1-pair', { key: `l1-${coreId}` }, [
                        l1dSize && m('span.topo-l1', [
                            m('span.topo-cache-name', 'L1d'),
                            m('span.topo-cache-size', l1dSize),
                        ]),
                        l1iSize && m('span.topo-l1', [
                            m('span.topo-cache-name', 'L1i'),
                            m('span.topo-cache-size', l1iSize),
                        ]),
                    ])
                    : m('div.topo-l1-pair.compact', { key: `l1-${coreId}`, style: barH }),
            ));
        }

        // Row: mid-level cache bars (each spanning its shared cores)
        if (showCaches && groupLevel) {
            for (const { groupIdx, cores: sgCores, size } of sgs) {
                if (!size) continue;
                const startCol = coreColIdx.get(sgCores[0].coreId) + 1;
                const span = sgCores.length;
                const style = `grid-column:${startCol}/span ${span}` + (barH ? `;${barH}` : '');
                children.push(m('div.topo-cache-bar.topo-mid-cache', {
                    key: `mid-${groupIdx}`,
                    style,
                }, labeled ? [
                    m('span.topo-cache-name', groupLevel.toUpperCase()),
                    m('span.topo-cache-size', size),
                ] : []));
            }
        }

        // Row: LLC bar spanning all columns
        if (showCaches && llc && llc !== groupLevel && llcData) {
            const style = `grid-column:1/-1` + (barH ? `;${barH}` : '');
            children.push(m('div.topo-cache-bar.topo-llc', {
                key: `llc-${llcIdx}`,
                style,
            }, labeled ? [
                m('span.topo-cache-name', llc.toUpperCase()),
                m('span.topo-cache-size', llcData.size),
            ] : []));
        }

        const colSize = cellSize ? `${Math.round(cellSize)}px` : '1fr';
        return m('div.topo-llc-group', {
            key: llcIdx,
            style: `grid-template-columns:repeat(${numCols},${colSize})`,
        }, children);
    };

    return m('div.topo-numa-box', {
        class: toggles.numa ? 'numa-highlighted' : '',
    }, [
        toggles.numa && m('div.topo-numa-header', `NUMA ${numaNode}`),
        m('div.topo-llc-groups', llcGrouped.map(renderLLCGroup)),
    ]);
};

// ── Legend ────────────────────────────────────────────────────────────

/**
 * Render the legend: one representative LLC cluster at readable size.
 * Shown only in compact mode when inline labels are omitted.
 */
const renderLegend = (cacheStructs, hasSMT) => {
    const { l1dSize, l1iSize, levels } = cacheStructs;
    const { mid, llc } = getCacheLevels(levels);
    const groupLevel = mid.length > 0 ? mid[0] : llc;
    const groupData = groupLevel ? levels[groupLevel] : null;
    const llcData = llc ? levels[llc] : null;

    // How many cores share one group-level cache (CCX)
    let coresPerGroup = 1;
    if (groupData) {
        for (const [, info] of groupData.groups) {
            coresPerGroup = info.cpus.size;
            break;
        }
    }

    // How many CCXs per LLC cluster
    let coresPerLLC = coresPerGroup;
    if (llcData) {
        for (const [, info] of llcData.groups) {
            coresPerLLC = info.cpus.size;
            break;
        }
    }
    const numSubGroups = llc !== groupLevel && coresPerLLC > coresPerGroup
        ? Math.ceil(coresPerLLC / coresPerGroup) : 1;

    const boxPx = '56px';

    const renderCCX = (si) => {
        const children = [];

        // Core row
        children.push(m('div.topo-core-row', { key: `cores-${si}` },
            Array.from({ length: coresPerGroup }, (_, ci) => {
                const idx = si * coresPerGroup + ci;
                return m('div.topo-core.legend-core', { style: `width:${boxPx};height:${boxPx}` }, [
                    m('span.core-id', `C${idx}`),
                    hasSMT ? m('div.topo-threads', [
                        m('span.topo-thread', `T${idx * 2}`),
                        m('span.topo-thread', `T${idx * 2 + 1}`),
                    ]) : null,
                ].filter(Boolean));
            }),
        ));

        // L1 pairs
        if (l1dSize || l1iSize) {
            children.push(m('div.topo-core-row', { key: `l1-${si}` },
                Array.from({ length: coresPerGroup }, (_, ci) =>
                    m('div.topo-l1-pair', { key: ci, style: `width:${boxPx}` }, [
                        l1dSize ? m('span.topo-l1', [
                            m('span.topo-cache-name', 'L1d'),
                            m('span.topo-cache-size', l1dSize),
                        ]) : null,
                        l1iSize ? m('span.topo-l1', [
                            m('span.topo-cache-name', 'L1i'),
                            m('span.topo-cache-size', l1iSize),
                        ]) : null,
                    ].filter(Boolean)),
                ),
            ));
        }

        // Group-level cache bar (L2 / CCX)
        if (groupLevel && groupData) {
            children.push(m('div.topo-cache-bar.topo-mid-cache', { key: `mid-${si}` }, [
                m('span.topo-cache-name', groupLevel.toUpperCase()),
                m('span.topo-cache-size', groupData.size),
            ]));
        }

        return m('div.topo-subgroup', { key: si }, children);
    };

    const clusterChildren = [];
    clusterChildren.push(m('div.topo-subgroups',
        Array.from({ length: numSubGroups }, (_, si) => renderCCX(si)),
    ));
    if (llc && llc !== groupLevel && llcData) {
        clusterChildren.push(m('div.topo-cache-bar.topo-llc', [
            m('span.topo-cache-name', llc.toUpperCase()),
            m('span.topo-cache-size', llcData.size),
        ]));
    }

    return m('div.topo-legend', [
        m('div.topo-legend-title', 'Legend'),
        m('div.topo-legend-cluster', clusterChildren),
    ]);
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

        // Try 2 NUMA columns, drop to 1 before going compact
        const allNumaGroups = pkgNumaGroups.flatMap(p => p.numaGroups);
        let numaPerRow = Math.min(totalNumaGroups, 2);
        let layout = computeLayout(allNumaGroups, numaPerRow, this.containerWidth);
        if (!layout.labeled && numaPerRow > 1) {
            numaPerRow = 1;
            layout = computeLayout(allNumaGroups, numaPerRow, this.containerWidth);
        }

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

        if (!layout.labeled && toggles.caches) {
            innerChildren.push(renderLegend(cacheStructs, hasSMT));
        }

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
