import { TimeRangeBar } from './controls.js';
import { selectionStore, reportStore, importJSON } from './selection.js';
import { toggleTheme, currentTheme } from './theme.js';

// Format utilities
const formatSize = (bytes) => {
    if (!bytes) return '';
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
    if (bytes < 1024 * 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
    return (bytes / (1024 * 1024 * 1024)).toFixed(1) + ' GB';
};

const formatInterval = (secs) => {
    if (!secs) return '';
    if (secs < 0.001) return (secs * 1000000).toFixed(0) + 'us';
    if (secs < 1) return (secs * 1000).toFixed(0) + 'ms';
    return secs.toFixed(0) + 's';
};

const formatDuration = (secs) => {
    if (!secs && secs !== 0) return '';
    if (secs < 60) return secs.toFixed(0) + 's';
    if (secs < 3600) return (secs / 60).toFixed(1) + 'm';
    if (secs < 86400) return (secs / 3600).toFixed(1) + 'h';
    return (secs / 86400).toFixed(1) + 'd';
};

// Collapsible metadata state (private to TopNav)
let metadataExpanded = false;

// Mobile sidebar drawer state
let sidebarOpen = false;

document.addEventListener('click', (e) => {
    if (metadataExpanded && !e.target.closest('.topnav-source')) {
        metadataExpanded = false;
        m.redraw();
    }
});

// Top navigation bar component
const TopNav = {
    view({ attrs }) {
        const liveMode = attrs.liveMode;
        const recording = attrs.recording;

        // Build metadata key/value pairs
        const metaEntries = [];
        if (attrs.source) metaEntries.push(['Source', attrs.source]);
        if (attrs.version) metaEntries.push(['Version', attrs.version]);
        if (attrs.interval) metaEntries.push(['Interval', formatInterval(attrs.interval)]);
        if (!liveMode && attrs.filesize) metaEntries.push(['Size', formatSize(attrs.filesize)]);
        if (attrs.start_time != null && attrs.end_time != null) {
            metaEntries.push(['Duration', formatDuration((attrs.end_time - attrs.start_time) / 1000)]);
        }
        if (attrs.num_series != null) metaEntries.push(['Series', attrs.num_series.toLocaleString()]);

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
            attrs.filename && m('div.topnav-source', {
                onclick: () => { metadataExpanded = !metadataExpanded; },
            }, [
                m('span.topnav-source-name', attrs.filename),
                m('span.topnav-source-chevron', { class: metadataExpanded ? 'expanded' : '' }, '\u25BE'),
                metadataExpanded && metaEntries.length > 0 && m('div.topnav-meta-table', [
                    m('div.topnav-meta-row.topnav-meta-header',
                        metaEntries.map(([key]) => m('span', key)),
                    ),
                    m('div.topnav-meta-row.topnav-meta-values',
                        metaEntries.map(([, val]) => m('span', val)),
                    ),
                ]),
            ]),
            m('div.topnav-actions', [
                // Theme toggle
                m('button.transport-btn.theme-toggle-btn', {
                    onclick: toggleTheme,
                    title: currentTheme() === 'dark' ? 'Switch to light theme' : 'Switch to dark theme',
                }, currentTheme() === 'dark'
                    ? m.trust('<svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor"><path d="M8 1a7 7 0 100 14A7 7 0 008 1zm0 1.5a5.5 5.5 0 110 11 5.5 5.5 0 010-11zM8 3a1 1 0 00-1 1v1a1 1 0 002 0V4a1 1 0 00-1-1zm4 4h-1a1 1 0 000 2h1a1 1 0 000-2zM4 8H3a1 1 0 000 2h1a1 1 0 000-2zm7.07-3.07a1 1 0 00-1.41 0l-.71.71a1 1 0 001.41 1.41l.71-.71a1 1 0 000-1.41zM5.64 10.36a1 1 0 00-1.41 0l-.71.71a1 1 0 001.41 1.41l.71-.71a1 1 0 000-1.41zm5.65 0l-.71.71a1 1 0 001.41 1.41l.71-.71a1 1 0 00-1.41-1.41zM4.93 4.93l-.71.71A1 1 0 005.64 7.05l.71-.71a1 1 0 00-1.42-1.41zM8 11a1 1 0 00-1 1v1a1 1 0 002 0v-1a1 1 0 00-1-1z"/></svg>')
                    : m.trust('<svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor"><path d="M6 .278a.768.768 0 01.08.858 7.208 7.208 0 00-.878 3.46c0 4.021 3.278 7.277 7.318 7.277.527 0 1.04-.055 1.533-.16a.787.787 0 01.81.316.733.733 0 01-.031.893A8.349 8.349 0 018.344 16C3.734 16 0 12.286 0 7.71 0 4.266 2.114 1.312 5.124.06A.752.752 0 016 .278z"/></svg>'),
                ),
                // Upload parquet (file mode only)
                !liveMode && m('button.transport-btn.import-btn', {
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
                // Import report JSON
                m('button.transport-btn.import-btn', {
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
                    hidden: attrs.sectionRoute === '/systeminfo' || attrs.sectionRoute === '/report',
                }),
        ]);
    },
};

// Count plots with non-empty data across groups.
const countCharts = (groups) => {
    let total = 0;
    let withData = 0;
    for (const group of groups || []) {
        for (const plot of group.plots || []) {
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

        // Separate Query Explorer from other sections
        const regularSections = attrs.sections.filter(
            (s) => s.name !== 'Query Explorer',
        );
        const queryExplorer = attrs.sections.find(
            (s) => s.name === 'Query Explorer',
        );

        // Find the first non-overview section to use as samplers header
        const overviewSection = regularSections.find((s) => s.name === 'Overview');
        const samplerSections = regularSections.filter((s) => s.name !== 'Overview');

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
        ])];
    },
};

export {
    TopNav,
    Sidebar,
    countCharts,
    formatSize,
};
