import { TimeRangeBar, GranularitySelector } from './controls.js';
import { selectionStore, reportStore, importJSON } from './selection.js';
import { toggleTheme, currentTheme } from './theme.js';
import { collectGroupPlots } from './group_utils.js';

// Format utilities
const formatSize = (bytes) => {
    if (!bytes) return '';
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
    if (bytes < 1024 * 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
    return (bytes / (1024 * 1024 * 1024)).toFixed(1) + ' GB';
};

// Mobile sidebar drawer state
let sidebarOpen = false;

// Top navigation bar component
const TopNav = {
    view({ attrs }) {
        const liveMode = attrs.liveMode;
        const recording = attrs.recording;
        const compareMode = attrs.compareMode;

        return m('div#topnav', [
            m('button.hamburger-btn', {
                onclick: () => { sidebarOpen = !sidebarOpen; },
                title: 'Toggle navigation',
            }, m.trust('<svg width="20" height="20" viewBox="0 0 20 20" fill="currentColor"><rect x="2" y="4" width="16" height="2" rx="1"/><rect x="2" y="9" width="16" height="2" rx="1"/><rect x="2" y="14" width="16" height="2" rx="1"/></svg>')),
            m('div.logo', [
                'REZOLUS',
                liveMode && m('span.live-indicator', {
                    class: recording ? 'recording' : 'stopped',
                }, recording ? 'REC' : 'STOPPED'),
            ]),
            // Always render the compare badge in compare mode — in WASM
            // mode the Load buttons inside each row are naturally skipped
            // because onLoad handlers aren't provided, but the dots +
            // filenames stay visible so the user can see which captures
            // are being compared.
            compareMode && (() => {
                const pickFile = (onPick) => () => {
                    const input = document.createElement('input');
                    input.type = 'file';
                    input.accept = '.parquet,application/octet-stream';
                    input.onchange = async () => {
                        const f = input.files && input.files[0];
                        if (f) await onPick(f);
                    };
                    input.click();
                };
                const row = (cls, label, fname, onLoad) => m('div.compare-capture', [
                    m(`span.compare-dot.${cls}`, '\u25CF'),
                    m('span.compare-capture-label', label),
                    m('span.compare-capture-name', {
                        title: fname || 'No file loaded',
                    }, fname || '—'),
                    onLoad && m(`button.compare-load.${cls}`, {
                        onclick: pickFile(onLoad),
                        title: `Replace ${label} parquet`,
                    }, m.trust('<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 11v2a1 1 0 001 1h10a1 1 0 001-1v-2"/><path d="M8 2v8m0-8l-3 3m3-3l3 3"/></svg>')),
                ]);
                return m('div.compare-badge', [
                    row('compare-baseline-dot', 'baseline', attrs.filename, attrs.onLoadBaseline),
                    row('compare-experiment-dot', 'experiment', attrs.experimentFilename, attrs.onLoadExperiment),
                ]);
            })(),
            // Node selector / filename display. In compare mode the
            // filename info is in the compare badge above, so suppress the
            // duplicate single-capture source label (multi-node selects
            // still render so the user can pin to one node).
            (() => {
                const nodes = attrs.nodeList || [];
                const selNode = attrs.selectedNode;
                const hasMultiNode = nodes.length > 1;

                const displayLabel = selNode || attrs.filename;
                if (!displayLabel) return null;

                if (hasMultiNode) {
                    return m('div.topnav-source', [
                        m('select.topnav-node-select', {
                            value: selNode,
                            onchange: (e) => {
                                attrs.onNodeChange(e.target.value);
                            },
                        }, nodes.map(n => m('option', { value: n }, n))),
                    ]);
                }

                if (compareMode) return null;

                return m('div.topnav-source', [
                    m('span.topnav-source-name', displayLabel),
                ]);
            })(),
            m('div.topnav-actions', [
                // Upload parquet (file mode only, when handler provided).
                // Hidden in compare mode — use the per-capture Load buttons
                // in the compare badge instead.
                attrs.onUploadParquet && !liveMode && !compareMode && m('button.transport-btn.import-btn', {
                    onclick: () => {
                        const input = document.createElement('input');
                        input.type = 'file';
                        input.accept = '.parquet,application/octet-stream';
                        input.onchange = async () => {
                            const file = input.files && input.files[0];
                            if (file && attrs.onUploadParquet) {
                                await attrs.onUploadParquet(file);
                            }
                        };
                        input.click();
                    },
                    title: 'Upload parquet file',
                }, [
                    m('span', 'Load Parquet'),
                    m.trust('<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 11v2a1 1 0 001 1h10a1 1 0 001-1v-2"/><path d="M8 2v8m0-8l-3 3m3-3l3 3"/></svg>'),
                ]),
                // Import report JSON (server viewer only). Hidden in
                // compare mode — reports are single-capture today.
                attrs.onUploadParquet && !compareMode && m('button.transport-btn.import-btn', {
                    class: attrs.filename ? 'parquet-loaded' : '',
                    disabled: !attrs.filename,
                    onclick: () => importJSON(attrs.fileChecksum),
                    title: attrs.filename
                        ? (reportStore.loadedFrom
                            ? `Loaded: ${reportStore.loadedFrom} — click to replace`
                            : 'Import report JSON')
                        : 'Load a parquet file first',
                }, [
                    m('span', 'Load Report'),
                    m.trust('<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 11v2a1 1 0 001 1h10a1 1 0 001-1v-2"/><path d="M8 2v8m0-8l-3 3m3-3l3 3"/></svg>'),
                ]),
                // Transport controls (live mode only)
                liveMode && m('div.transport-controls', [
                    m('button.transport-btn.record-btn', {
                        onclick: attrs.onStartRecording,
                        title: 'Start new recording (clears current data)',
                        disabled: recording,
                    }, m.trust('<svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor"><circle cx="8" cy="8" r="6"/></svg>')),
                    m('button.transport-btn.stop-btn', {
                        onclick: attrs.onStopRecording,
                        title: 'Stop recording',
                        disabled: !recording,
                    }, m.trust('<svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor"><rect x="2" y="2" width="12" height="12" rx="1"/></svg>')),
                    m('button.transport-btn.save-btn', {
                        onclick: attrs.onSaveCapture,
                        title: 'Save capture as parquet',
                    }, m.trust('<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M8 2v8m0 0l-3-3m3 3l3-3"/><path d="M2 11v2a1 1 0 001 1h10a1 1 0 001-1v-2"/></svg>')),
                ]),
            ]),
            // Global time range bar (right-aligned, hidden on systeminfo)
            (attrs.start_time != null && attrs.end_time != null) &&
                m(TimeRangeBar, {
                    start_time: attrs.start_time,
                    end_time: attrs.end_time,
                    chartsState: attrs.chartsState,
                    compareMode: !!attrs.compareMode,
                    hidden: attrs.sectionRoute === '/systeminfo' || attrs.sectionRoute === '/report',
                }),
            // Granularity (step) selector
            (attrs.start_time != null && attrs.end_time != null) &&
                m(GranularitySelector, {
                    value: attrs.granularity,
                    onChange: attrs.onGranularityChange,
                    hidden: attrs.sectionRoute === '/systeminfo' || attrs.sectionRoute === '/report',
                }),
            // Theme toggle — rightmost element in navbar
            m('button.transport-btn.theme-toggle-btn', {
                onclick: toggleTheme,
                title: currentTheme() === 'dark' ? 'Switch to light theme' : 'Switch to dark theme',
            }, currentTheme() === 'dark'
                ? m.trust('<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="5"/><line x1="12" y1="1" x2="12" y2="3"/><line x1="12" y1="21" x2="12" y2="23"/><line x1="4.22" y1="4.22" x2="5.64" y2="5.64"/><line x1="18.36" y1="18.36" x2="19.78" y2="19.78"/><line x1="1" y1="12" x2="3" y2="12"/><line x1="21" y1="12" x2="23" y2="12"/><line x1="4.22" y1="19.78" x2="5.64" y2="18.36"/><line x1="18.36" y1="5.64" x2="19.78" y2="4.22"/></svg>')
                : m.trust('<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/></svg>'),
            ),
        ]);
    },
};

// Count plots with non-empty data across groups and their subgroups.
// `collectGroupPlots` is imported from ./group_utils.js.
const countCharts = (groups) => {
    let total = 0;
    let withData = 0;
    for (const group of groups || []) {
        for (const plot of collectGroupPlots(group)) {
            total++;
            if (plot.data && plot.data.length >= 1 && plot.data[0] && plot.data[0].length > 0) {
                withData++;
            }
        }
    }
    return { total, withData };
};

// Sidebar component
const Sidebar = {
    view({ attrs }) {
        const sectionResponseCache = attrs.sectionResponseCache;

        // In compare mode, cgroup section is hidden from navigation (v1 scope).
        const visibleSections = attrs.compareMode
            ? attrs.sections.filter((s) => s.route !== '/cgroups')
            : attrs.sections;

        // Separate special sections from sampler sections
        const queryExplorer = visibleSections.find(
            (s) => s.name === 'Query Explorer',
        );
        const overviewSection = visibleSections.find((s) => s.name === 'Overview');
        const serviceSections = visibleSections.filter((s) => s.route.startsWith('/service/'));
        const samplerSections = visibleSections.filter(
            (s) => s.name !== 'Query Explorer' && s.name !== 'Overview' && !s.route.startsWith('/service/'),
        );

        return [
        // Backdrop overlay for mobile drawer
        m('div.sidebar-backdrop', {
            class: sidebarOpen ? 'visible' : '',
            onclick: () => { sidebarOpen = false; },
        }),
        m('div#sidebar', {
            class: sidebarOpen ? 'open' : '',
            onclick: (e) => {
                // Close drawer when a link is clicked (mobile navigation)
                if (e.target.closest('a')) sidebarOpen = false;
            },
        }, [
            // Report (shown only when imported)
            reportStore.entries.length > 0 && m(
                m.route.Link,
                {
                    class: attrs.activeSection?.route === '/report'
                        ? 'selected selection-link report-link'
                        : 'selection-link report-link',
                    href: '/report',
                },
                `Report (${reportStore.entries.length})`,
            ),

            // Selection section (shown only when entries exist)
            selectionStore.entries.length > 0 && m(
                m.route.Link,
                {
                    class: attrs.activeSection?.route === '/selection'
                        ? 'selected selection-link'
                        : 'selection-link',
                    href: '/selection',
                },
                `Selection (${selectionStore.entries.length})`,
            ),

            // Overview section first (if exists)
            overviewSection && m(
                m.route.Link,
                {
                    class: attrs.activeSection === overviewSection ? 'selected' : '',
                    href: overviewSection.route,
                },
                overviewSection.name,
            ),

            // Services group header (shown only when service sections exist)
            serviceSections.length > 0 && m('div.sidebar-label', 'Services'),

            // Service sections (one per source)
            serviceSections.map((section) => {
                const sectionKey = section.route.replace(/^\//, '');
                const cached = sectionResponseCache[sectionKey];
                const count = cached ? countCharts(cached.groups) : null;
                const label = count ? `${section.name} (${count.withData})` : section.name;
                return m(
                    m.route.Link,
                    {
                        class: attrs.activeSection?.route === section.route ? 'selected' : '',
                        href: section.route,
                    },
                    label,
                );
            }),

            // Samplers label
            samplerSections.length > 0 && m('div.sidebar-label', 'Samplers'),

            // Sampler sections
            samplerSections.map((section) => {
                const sectionKey = section.route.replace(/^\//, '');
                const cached = sectionResponseCache[sectionKey];
                const count = cached ? countCharts(cached.groups) : null;
                const label = count ? `${section.name} (${count.withData})` : section.name;
                return m(
                    m.route.Link,
                    {
                        class:
                            attrs.activeSection === section ? 'selected' : '',
                        href: section.route,
                    },
                    label,
                );
            }),

            // Separator and Query Explorer if it exists
            queryExplorer && [
                m('div.sidebar-separator'),
                m(
                    m.route.Link,
                    {
                        class:
                            attrs.activeSection === queryExplorer
                                ? 'selected query-explorer-link'
                                : 'query-explorer-link',
                        href: queryExplorer.route,
                    },
                    [m('span.arrow', '→'), ' ', queryExplorer.name],
                ),
            ],

            // System Info link (below Query Explorer)
            attrs.hasSystemInfo && [
                m('div.sidebar-separator'),
                m(
                    m.route.Link,
                    {
                        class:
                            attrs.activeSection?.route === '/systeminfo'
                                ? 'selected systeminfo-link'
                                : 'systeminfo-link',
                        href: '/systeminfo',
                    },
                    [m('span.arrow', '→'), ' System Info'],
                ),
            ],

            // Metadata link (below System Info)
            attrs.hasFileMetadata && [
                m(
                    m.route.Link,
                    {
                        class:
                            attrs.activeSection?.route === '/metadata'
                                ? 'selected metadata-link'
                                : 'metadata-link',
                        href: '/metadata',
                    },
                    [m('span.arrow', '→'), ' Metadata'],
                ),
            ],
        ])];
    },
};

export {
    TopNav,
    Sidebar,
    countCharts,
    formatSize,
};
