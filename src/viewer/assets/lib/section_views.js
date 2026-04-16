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

        return m('div.systeminfo-section', [
            m('h1.section-title', 'System Info'),
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
        ]);
    },
});

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

export { createSystemInfoView, renderCgroupSection };
