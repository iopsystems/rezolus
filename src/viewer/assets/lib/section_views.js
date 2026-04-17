import { expandLink, selectButton } from './chart_controls.js';

const createSystemInfoView = ({ CpuTopology, formatBytes }) => ({
    view({ attrs }) {
        const info = attrs.data;
        if (!info) {
            return m('div.systeminfo-section', [
                m('h1.section-title', 'System Info'),
                m('p.systeminfo-empty', 'No system information available in this recording.'),
            ]);
        }

        // Detect multi-node format: top-level values are objects with system fields
        const isMultiNode = !info.hostname && !info.cpus &&
            Object.values(info).some(v => v && typeof v === 'object' && (v.hostname || v.cpus));

        if (isMultiNode) {
            return m('div.systeminfo-section', [
                m('h1.section-title', 'System Info'),
                ...Object.entries(info).map(([nodeName, nodeInfo]) =>
                    renderNodeInfo(nodeName, nodeInfo, CpuTopology, formatBytes)
                ),
            ]);
        }

        // Single-node: render as before
        return m('div.systeminfo-section', [
            m('h1.section-title', 'System Info'),
            renderSingleNodeInfo(info, CpuTopology, formatBytes),
        ]);
    },
});

const renderSingleNodeInfo = (info, CpuTopology, formatBytes) => {
    const rows = (items) => items
        .filter(([, v]) => v != null && v !== '')
        .map(([label, value]) => m('tr', [
            m('td.sysinfo-label', label),
            m('td.sysinfo-value', String(value)),
        ]));

    const table = (title, items) => {
        const filtered = items.filter(([, v]) => v != null && v !== '');
        if (filtered.length === 0) return null;
        return m('div.sysinfo-group', [
            m('h2.sysinfo-group-title', title),
            m('table.sysinfo-table', m('tbody', rows(items))),
        ]);
    };

    return [
        m(CpuTopology, { data: info }),
        m('div.sysinfo-grid', [
            table('System', [
                ['Hostname', info.hostname],
                ['OS', info.os],
                ['Kernel', info.kernel],
                ['Architecture', info.arch],
            ]),
            table('CPU', [
                ['Model', info.cpu_model],
                ['Vendor', info.cpu_vendor],
                ['Logical CPUs', info.cpus],
                ['Physical Cores', info.cores],
                ['Packages', info.packages],
                ['SMT', info.smt != null ? (info.smt ? 'Enabled' : 'Disabled') : null],
            ]),
            table('Memory', [
                ['Total', formatBytes(info.memory_total_bytes)],
                ['NUMA Nodes', info.numa_nodes],
            ]),
            info.caches && info.caches.length > 0 && m('div.sysinfo-group', [
                m('h2.sysinfo-group-title', 'Cache Topology'),
                m('table.sysinfo-table', m('tbody',
                    info.caches.map((c) => m('tr', [
                        m('td.sysinfo-label', c.level),
                        m('td.sysinfo-value', [c.size || '', c.instances > 1 ? ` x ${c.instances}` : ''].join('')),
                    ])),
                )),
            ]),
            info.nics && info.nics.length > 0 && m('div.sysinfo-group', [
                m('h2.sysinfo-group-title', 'Network Interfaces'),
                m('table.sysinfo-table', m('tbody',
                    info.nics.map((nic) => m('tr', [
                        m('td.sysinfo-label', nic.name),
                        m('td.sysinfo-value', [
                            nic.speed ? `${nic.speed} Mbps` : '',
                            nic.driver ? ` (${nic.driver})` : '',
                            nic.numa_node != null ? ` NUMA ${nic.numa_node}` : '',
                        ].join('')),
                    ])),
                )),
            ]),
            info.gpus && info.gpus.length > 0 && m('div.sysinfo-group', [
                m('h2.sysinfo-group-title', 'GPUs'),
                m('table.sysinfo-table', m('tbody',
                    info.gpus.map((gpu) => m('tr', [
                        m('td.sysinfo-label', gpu.name || gpu.vendor),
                        m('td.sysinfo-value', [
                            gpu.memory_bytes ? formatBytes(gpu.memory_bytes) : '',
                            gpu.driver ? ` (${gpu.driver})` : '',
                        ].join('')),
                    ])),
                )),
            ]),
        ]),
        m('div.sysinfo-raw', [
            m('h2.sysinfo-group-title', 'Raw JSON'),
            m('pre.sysinfo-json', JSON.stringify(info, null, 2)),
        ]),
    ];
};

const renderNodeInfo = (nodeName, info, CpuTopology, formatBytes) => {
    return m('div.systeminfo-node', [
        m('h2.systeminfo-node-title', nodeName),
        ...renderSingleNodeInfo(info, CpuTopology, formatBytes),
    ]);
};

const createMetadataView = () => {
    let rawExpanded = false;
    let descFilter = '';

    return {
        view({ attrs }) {
            const meta = attrs.data;
            if (!meta || Object.keys(meta).length === 0) {
                return m('div.metadata-section', [
                    m('h1.section-title', 'Metadata'),
                    m('p.systeminfo-empty', 'No file metadata available.'),
                ]);
            }

            const psm = meta.per_source_metadata || {};
            const descriptions = meta.descriptions || {};
            const descEntries = Object.entries(descriptions)
                .filter(([k]) => !descFilter || k.toLowerCase().includes(descFilter.toLowerCase()))
                .sort(([a], [b]) => a.localeCompare(b));

            const serviceQueries = [];
            for (const [key, value] of Object.entries(psm)) {
                if (value.service_queries) {
                    serviceQueries.push({ source: key, queries: value.service_queries });
                }
            }

            if (meta.service_queries && serviceQueries.length === 0) {
                serviceQueries.push({ source: meta.source || 'unknown', queries: meta.service_queries });
            }

            return m('div.metadata-section', [
                m('h1.section-title', 'Metadata'),

                // Recording Info
                m('div.sysinfo-group', [
                    m('h2.sysinfo-group-title', 'Recording Info'),
                    m('table.sysinfo-table', m('tbody', [
                        meta.source && m('tr', [
                            m('td.sysinfo-label', 'Source'),
                            m('td.sysinfo-value', Array.isArray(meta.source) ? meta.source.join(', ') : String(meta.source)),
                        ]),
                        meta.sampling_interval_ms && m('tr', [
                            m('td.sysinfo-label', 'Sampling Interval'),
                            m('td.sysinfo-value', `${meta.sampling_interval_ms} ms`),
                        ]),
                        meta.version && m('tr', [
                            m('td.sysinfo-label', 'Version'),
                            m('td.sysinfo-value', String(meta.version)),
                        ]),
                    ].filter(Boolean))),
                ]),

                // Sources (from per_source_metadata)
                Object.keys(psm).length > 0 && m('div.sysinfo-group', [
                    m('h2.sysinfo-group-title', 'Sources'),
                    m('table.sysinfo-table', [
                        m('thead', m('tr', [
                            m('th', 'Key'),
                            m('th', 'Role'),
                            m('th', 'Version'),
                            m('th', 'Node'),
                            m('th', 'Instance'),
                        ])),
                        m('tbody', Object.entries(psm).map(([key, val]) =>
                            m('tr', [
                                m('td', key),
                                m('td', val.role || ''),
                                m('td', val.version || ''),
                                m('td', val.node || ''),
                                m('td', val.instance || ''),
                            ])
                        )),
                    ]),
                ]),

                // Descriptions
                descEntries.length > 0 && m('div.sysinfo-group', [
                    m('h2.sysinfo-group-title', `Metric Descriptions (${descEntries.length})`),
                    m('input.metadata-search', {
                        placeholder: 'Filter metrics...',
                        value: descFilter,
                        oninput: (e) => { descFilter = e.target.value; },
                    }),
                    m('div.metadata-desc-list', [
                        m('table.sysinfo-table', m('tbody',
                            descEntries.slice(0, 200).map(([name, desc]) =>
                                m('tr', [
                                    m('td.sysinfo-label', name),
                                    m('td.sysinfo-value', String(desc)),
                                ])
                            )
                        )),
                        descEntries.length > 200 && m('p.fg-secondary',
                            `Showing 200 of ${descEntries.length}. Use filter to narrow.`
                        ),
                    ]),
                ]),

                // Service Queries
                serviceQueries.length > 0 && m('div.sysinfo-group', [
                    m('h2.sysinfo-group-title', 'Service Queries'),
                    ...serviceQueries.map(sq => [
                        m('h3', sq.source),
                        sq.queries.kpis && m('table.sysinfo-table', [
                            m('thead', m('tr', [m('th', 'Title'), m('th', 'Type'), m('th', 'Query')])),
                            m('tbody', sq.queries.kpis.map(kpi =>
                                m('tr', [
                                    m('td', kpi.title || ''),
                                    m('td', kpi.metric_type || kpi.type || ''),
                                    m('td', m('code', kpi.query || '')),
                                ])
                            )),
                        ]),
                    ]),
                ]),

                // Raw Metadata
                m('div.sysinfo-group', [
                    m('h2.sysinfo-group-title', {
                        onclick: () => { rawExpanded = !rawExpanded; },
                        style: { cursor: 'pointer' },
                    }, [rawExpanded ? '\u25BE' : '\u25B8', ' Raw Metadata']),
                    rawExpanded && m('pre.sysinfo-json', JSON.stringify(meta, null, 2)),
                ]),
            ]);
        },
    };
};

const renderCgroupSection = ({
    attrs,
    titleText,
    interval,
    chartsState,
    Chart,
    CgroupSelector,
    executePromQLRangeQuery,
    applyResultToPlot,
    substituteCgroupPattern,
    setActiveCgroupPattern,
    globalColorMapper,
}) => {
    const sectionRoute = '/cgroups';
    const sectionName = 'Cgroups';

    const leftGroups = attrs.groups.filter((g) => g.metadata?.side === 'left');
    const rightGroups = attrs.groups.filter((g) => g.metadata?.side === 'right');

    const leftPlots = leftGroups.flatMap((g) => g.plots || []);
    const rightPlots = rightGroups.flatMap((g) => g.plots || []);
    const rightByTitle = new Map(rightPlots.map((p) => [p.opts?.title, p]));
    const paired = new Set();
    const pairs = [];
    for (const left of leftPlots) {
        const title = left.opts?.title;
        const right = rightByTitle.get(title);
        if (right) paired.add(title);
        pairs.push({ left, right: right || null });
    }
    for (const right of rightPlots) {
        if (!paired.has(right.opts?.title)) {
            pairs.push({ left: null, right });
        }
    }

    const renderCgroupChart = (spec, label, legend) => {
        if (!spec) return null;
        const prefixedSpec = { ...spec, opts: { ...spec.opts }, noCollapse: true, compactGrid: true };
        return m('div.cgroup-chart', [
            m('span.cgroup-chart-label', label),
            m('div.chart-wrapper', [
                m(Chart, { spec: prefixedSpec, chartsState, interval }),
                expandLink(spec, sectionRoute),
                selectButton(spec, sectionRoute, sectionName),
            ]),
            legend,
        ]);
    };

    return m('div#section-content.cgroups-section', [
        m('h1.section-title', titleText),
        m(CgroupSelector, {
            groups: attrs.groups,
            executeQuery: executePromQLRangeQuery,
            applyResultToPlot: applyResultToPlot,
            substitutePattern: substituteCgroupPattern,
            setActiveCgroupPattern: (p) => { setActiveCgroupPattern(p); },
        }),
        m('div.cgroup-pairs',
            pairs.map((pair) => {
                const title = pair.left?.opts?.title || pair.right?.opts?.title || '';
                const description = pair.left?.opts?.description || pair.right?.opts?.description;
                // Only show a legend when the right-side chart has actual series data;
                // otherwise stale series_names can linger as "ghost" entries.
                const rightData = pair.right?.data;
                const hasData = Array.isArray(rightData) && rightData.length > 1
                    && Array.isArray(rightData[0]) && rightData[0].length > 0;
                const seriesNames = hasData ? (pair.right?.series_names || []) : [];
                const legend = seriesNames.length > 0 && m('div.cgroup-pair-legend',
                    seriesNames.map((name) =>
                        m('span.cgroup-legend-item', [
                            m('span.cgroup-legend-swatch', {
                                style: { background: globalColorMapper.getColorByName(name) },
                            }),
                            name,
                        ]),
                    ),
                );
                return m('div.cgroup-pair', [
                    m('div.cgroup-pair-header', [
                        m('span.chart-title', title),
                        description && m('span.chart-subtitle', description),
                    ]),
                    m('div.cgroup-pair-charts', [
                        renderCgroupChart(pair.left, 'Aggregate'),
                        renderCgroupChart(pair.right, 'Individual', legend),
                    ]),
                ]);
            }),
        ),
    ]);
};

export { createSystemInfoView, createMetadataView, renderCgroupSection };
