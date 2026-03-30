import { TimeRangeBar } from './controls.js';

// Format utilities
const formatSize = (bytes) => {
    if (!bytes) return '';
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
    return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
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
                    hidden: attrs.sectionRoute === '/systeminfo',
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

        return m('div#sidebar', [
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
        ]);
    },
};

// Status bar component
const StatusBar = {
    view({ attrs }) {
        const { source, version, interval, filesize, liveMode } = attrs;
        return m('div#status-bar', [
            source && m('span.status-item', [
                m('span.status-label', 'Source'),
                source,
            ]),
            version && m('span.status-item', [
                m('span.status-label', 'Version'),
                version,
            ]),
            interval && m('span.status-item', [
                m('span.status-label', 'Interval'),
                formatInterval(interval),
            ]),
            !liveMode && filesize && m('span.status-item', [
                m('span.status-label', 'Size'),
                formatSize(filesize),
            ]),
        ]);
    },
};

export {
    TopNav,
    Sidebar,
    StatusBar,
    countCharts,
    formatSize,
    formatInterval,
    formatDuration,
};
