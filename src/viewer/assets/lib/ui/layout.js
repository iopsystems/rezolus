import { TimeRangeBar, GranularitySelector, TimeModeSelector } from './controls.js';
import { notebookStore, reportStore, loadedSelectionStore, importSelection } from '../selection/selection.js';
import { toggleTheme, currentTheme } from './theme.js';
import { collectGroupPlots } from '../features/group_utils.js';

const formatSize = (bytes) => {
    if (!bytes) return '';
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
    if (bytes < 1024 * 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
    return (bytes / (1024 * 1024 * 1024)).toFixed(1) + ' GB';
};

// Shared 14px upload arrow used by every Load-parquet trigger.
export const UPLOAD_ICON_SVG = '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 11v2a1 1 0 001 1h10a1 1 0 001-1v-2"/><path d="M8 2v8m0-8l-3 3m3-3l3 3"/></svg>';

const FILE_ICON_SVG = '<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M9 2H4a1 1 0 00-1 1v10a1 1 0 001 1h8a1 1 0 001-1V6"/><path d="M9 2v3a1 1 0 001 1h3M9 2l4 4"/></svg>';

// Open a one-shot hidden <input type=file> and forward the selected
// file to the caller's onPick. Returns an onclick handler so the site
// can wire it directly to a button. Accepts both bare parquets and
// combined-A/B tarballs (`.parquet.ab.tar`).
export const openParquetPicker = (onPick) => () => {
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = '.parquet,.ab.tar,.tar,application/octet-stream,application/x-tar';
    input.onchange = async () => {
        const file = input.files && input.files[0];
        if (file && onPick) await onPick(file);
    };
    input.click();
};

let sidebarOpen = false;

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
                const baselineLabel = attrs.baselineAlias || 'baseline';
                const experimentLabel = attrs.experimentAlias || 'experiment';
                const row = (cls, label, fname, onLoad) => m('div.compare-capture', [
                    m('div.compare-capture-info', [
                        m('div.compare-capture-head', [
                            m(`span.compare-dot.${cls}`, '\u25CF'),
                            m('span.compare-capture-label', label),
                        ]),
                        m('div.compare-capture-name', {
                            title: fname || 'No file loaded',
                        }, fname || '—'),
                    ]),
                    onLoad && m(`button.compare-load.${cls}`, {
                        onclick: openParquetPicker(onLoad),
                        title: `Replace ${label} parquet`,
                    }, m.trust(UPLOAD_ICON_SVG)),
                ]);
                // Summary (always visible): dot + label for each capture.
                // Filenames live in the dropdown card — their lengths are
                // unpredictable and would crowd the navbar.
                return m('details.compare-badge', [
                    m('summary.compare-badge-summary', {
                        title: `Show ${baselineLabel} / ${experimentLabel} details`,
                    }, [
                        m('span.compare-badge-chip', [
                            m('span.compare-dot.compare-baseline-dot', '\u25CF'),
                            m('span.compare-badge-label', baselineLabel),
                        ]),
                        m('span.compare-badge-chip', [
                            m('span.compare-dot.compare-experiment-dot', '\u25CF'),
                            m('span.compare-badge-label', experimentLabel),
                        ]),
                    ]),
                    m('div.compare-capture-list', [
                        row('compare-baseline-dot', baselineLabel, attrs.filename, attrs.onLoadBaseline),
                        row('compare-experiment-dot', experimentLabel, attrs.experimentFilename, attrs.onLoadExperiment),
                    ]),
                ]);
            })(),
            (() => {
                const nodes = attrs.nodeList || [];
                const selNode = attrs.selectedNode;
                if (nodes.length > 1) {
                    return m('div.topnav-source', [
                        m('select.topnav-node-select', {
                            value: selNode,
                            onchange: (e) => {
                                attrs.onNodeChange(e.target.value);
                            },
                        }, nodes.map(n => m('option', { value: n }, n))),
                    ]);
                }
                return null;
            })(),
            // GPU selection lives in the GPU section body (a two-panel picker
            // mirroring the cgroups selector), not in the top nav.
            // Three sibling buttons sit next to REZOLUS in single-capture
            // mode: file metadata (filename text + dropdown), Load Parquet,
            // Load Selection. Compare mode puts filename + per-side Load
            // Parquet inside .compare-badge instead, but Load Selection
            // still appears here.
            m('div.topnav-actions', [
                (() => {
                    const displayLabel = attrs.selectedNode || attrs.filename;
                    if (!displayLabel || compareMode) return null;
                    return m('details.topnav-source', [
                        m('summary.topnav-source-summary', {
                            title: displayLabel,
                        }, [
                            m.trust(FILE_ICON_SVG),
                            m('span.topnav-source-name', displayLabel),
                        ]),
                        m('div.topnav-source-card', [
                            m('div.topnav-source-fullname', displayLabel),
                        ]),
                    ]);
                })(),
                attrs.onUploadParquet && !liveMode && m('button.transport-btn.import-btn', {
                    onclick: openParquetPicker(attrs.onUploadParquet),
                    title: compareMode
                        ? 'Replace current view (parquet or combined-A/B tarball)'
                        : 'Upload parquet or combined-A/B tarball',
                }, [
                    m('span', 'Load Parquet'),
                    m.trust(UPLOAD_ICON_SVG),
                ]),
                attrs.onUploadParquet && m('button.transport-btn.import-btn', {
                    class: attrs.filename ? 'parquet-loaded' : '',
                    disabled: !attrs.filename,
                    onclick: () => importSelection(),
                    title: attrs.filename
                        ? (loadedSelectionStore.loadedFrom
                            ? `Loaded: ${loadedSelectionStore.loadedFrom} — click to replace`
                            : 'Import selection JSON')
                        : 'Load a parquet file first',
                }, [
                    m('span', 'Load Selection'),
                    m.trust(UPLOAD_ICON_SVG),
                ]),
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
            (attrs.start_time != null && attrs.end_time != null) &&
                m(TimeRangeBar, {
                    start_time: attrs.start_time,
                    end_time: attrs.end_time,
                    chartsState: attrs.chartsState,
                    compareMode: !!attrs.compareMode,
                    baselineAlias: attrs.baselineAlias,
                    hidden: attrs.sectionRoute === '/systeminfo' || attrs.sectionRoute === '/report',
                }),
            (attrs.start_time != null && attrs.end_time != null) &&
                m(GranularitySelector, {
                    value: attrs.granularity,
                    onChange: attrs.onGranularityChange,
                    hidden: attrs.sectionRoute === '/systeminfo' || attrs.sectionRoute === '/report',
                }),
            (attrs.start_time != null && attrs.end_time != null) &&
                m(TimeModeSelector, {
                    value: attrs.timeMode,
                    onChange: attrs.onTimeModeChange,
                    disabled: attrs.compareMode,
                    hidden: attrs.sectionRoute === '/systeminfo' || attrs.sectionRoute === '/report',
                }),
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

const Sidebar = {
    view({ attrs }) {
        const sectionResponseCache = attrs.sectionResponseCache;

        // In compare mode, cgroup section is hidden from navigation (v1 scope).
        const visibleSections = attrs.compareMode
            ? attrs.sections.filter((s) => s.route !== '/cgroups')
            : attrs.sections;

        const queryExplorer = visibleSections.find(
            (s) => s.name === 'Query Explorer',
        );
        const overviewSection = visibleSections.find((s) => s.name === 'Overview');
        const exceptionsSection = visibleSections.find((s) => s.name === 'Exceptions');
        const serviceSections = visibleSections.filter((s) => s.route.startsWith('/service/'));
        // Foreign (non-Rezolus) source sections get their own "Metrics" group;
        // "Samplers" is reserved for the Rezolus agent's built-in samplers.
        const metricSections = visibleSections.filter((s) => s.route.startsWith('/source/'));
        const samplerSections = visibleSections.filter(
            (s) => s.name !== 'Query Explorer'
                && s.name !== 'Overview'
                && s.name !== 'Exceptions'
                && !s.route.startsWith('/service/')
                && !s.route.startsWith('/source/'),
        );

        return [
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

            notebookStore.entries.length > 0 && m(
                m.route.Link,
                {
                    class: attrs.activeSection?.route === '/notebook'
                        ? 'selected selection-link notebook-link'
                        : 'selection-link notebook-link',
                    href: '/notebook',
                },
                `Notebook (${notebookStore.entries.length})`,
            ),

            loadedSelectionStore.entries.length > 0 && m(
                m.route.Link,
                {
                    class: attrs.activeSection?.route === '/selection'
                        ? 'selected selection-link loaded-selection-link'
                        : 'selection-link loaded-selection-link',
                    href: '/selection',
                },
                `Selection (${loadedSelectionStore.entries.length})`,
            ),

            overviewSection && m(
                m.route.Link,
                {
                    class: attrs.activeSection === overviewSection ? 'selected' : '',
                    href: overviewSection.route,
                },
                overviewSection.name,
            ),

            serviceSections.length > 0 && m('div.sidebar-label', 'Services'),

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

            // Exceptions sits between Overview/Services and Samplers —
            // it's a cross-sampler triage view, not one of the per-
            // sampler sections.
            exceptionsSection && m(
                m.route.Link,
                {
                    class: attrs.activeSection === exceptionsSection ? 'selected' : '',
                    href: exceptionsSection.route,
                },
                exceptionsSection.name,
            ),

            samplerSections.length > 0 && m('div.sidebar-label', 'Samplers'),

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

            metricSections.length > 0 && m('div.sidebar-label', 'Metrics'),

            metricSections.map((section) => m(
                m.route.Link,
                {
                    class: attrs.activeSection === section ? 'selected' : '',
                    href: section.route,
                },
                section.name,
            )),

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
