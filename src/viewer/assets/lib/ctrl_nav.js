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

// Global time range bar — interactive minimap for zoom selection.
// Displays full experiment duration with a draggable selection window.
const TimeRangeBar = {
    oninit(vnode) {
        vnode.state.barStart = 0;
        vnode.state.barEnd = 100;
        vnode.state.editing = null; // 'start' | 'end' | null
        vnode.state.editValue = '';
    },

    oncreate(vnode) {
        vnode.state.dragging = null; // 'left' | 'right' | 'move' | 'create'
        vnode.state.dragStartX = 0;
        vnode.state.dragStartLeft = 0;
        vnode.state.dragStartRight = 0;

        const chartsState = vnode.attrs.chartsState;
        const bar = vnode.dom;
        const track = bar.querySelector('.time-track');
        const getPercent = (clientX) => {
            const rect = track.getBoundingClientRect();
            return Math.max(0, Math.min(100, ((clientX - rect.left) / rect.width) * 100));
        };

        const getBarZoom = () => ({ start: vnode.state.barStart, end: vnode.state.barEnd });

        vnode.state.applyZoom = (start, end) => {
            // Enforce minimum 0.5% range
            if (end - start < 0.5) return;
            vnode.state.barStart = start;
            vnode.state.barEnd = end;
            chartsState.zoomLevel = { start, end };
            chartsState.zoomSource = 'global';
            chartsState.charts.forEach(chart => {
                chart.dispatchAction({ type: 'dataZoom', start, end });
                chart._rescaleYAxis();
            });
            m.redraw();
        };

        const applyZoom = vnode.state.applyZoom;

        const onMouseDown = (e) => {
            if (e.button !== 0) return;
            e.preventDefault();
            const pct = getPercent(e.clientX);
            const zoom = getBarZoom();
            const handleWidth = 3; // percent tolerance for handle hit

            if (Math.abs(pct - zoom.start) < handleWidth) {
                vnode.state.dragging = 'left';
            } else if (Math.abs(pct - zoom.end) < handleWidth) {
                vnode.state.dragging = 'right';
            } else if (pct > zoom.start && pct < zoom.end) {
                vnode.state.dragging = 'move';
                vnode.state.dragStartX = pct;
                vnode.state.dragStartLeft = zoom.start;
                vnode.state.dragStartRight = zoom.end;
            } else {
                // Click outside selection — start creating a new one
                vnode.state.dragging = 'create';
                vnode.state.dragStartX = pct;
                applyZoom(pct, pct + 0.5);
            }

            document.addEventListener('mousemove', onMouseMove);
            document.addEventListener('mouseup', onMouseUp);
        };

        const onMouseMove = (e) => {
            const pct = getPercent(e.clientX);
            const zoom = getBarZoom();

            if (vnode.state.dragging === 'left') {
                applyZoom(Math.min(pct, zoom.end - 0.5), zoom.end);
            } else if (vnode.state.dragging === 'right') {
                applyZoom(zoom.start, Math.max(pct, zoom.start + 0.5));
            } else if (vnode.state.dragging === 'move') {
                const delta = pct - vnode.state.dragStartX;
                let newStart = vnode.state.dragStartLeft + delta;
                let newEnd = vnode.state.dragStartRight + delta;
                const range = newEnd - newStart;
                if (newStart < 0) { newStart = 0; newEnd = range; }
                if (newEnd > 100) { newEnd = 100; newStart = 100 - range; }
                applyZoom(newStart, newEnd);
            } else if (vnode.state.dragging === 'create') {
                const anchor = vnode.state.dragStartX;
                applyZoom(Math.min(anchor, pct), Math.max(anchor, pct));
            }
        };

        const onMouseUp = () => {
            vnode.state.dragging = null;
            document.removeEventListener('mousemove', onMouseMove);
            document.removeEventListener('mouseup', onMouseUp);
        };

        bar.addEventListener('mousedown', onMouseDown);
        vnode.state.cleanup = () => {
            bar.removeEventListener('mousedown', onMouseDown);
            document.removeEventListener('mousemove', onMouseMove);
            document.removeEventListener('mouseup', onMouseUp);
        };
    },

    onremove(vnode) {
        if (vnode.state.cleanup) vnode.state.cleanup();
    },

    view(vnode) {
        const chartsState = vnode.attrs.chartsState;
        const start = vnode.state.barStart;
        const end = vnode.state.barEnd;
        const startTime = vnode.attrs.start_time;
        const endTime = vnode.attrs.end_time;

        const formatTime = (ms) => {
            const d = new Date(ms);
            return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
        };
        const formatDate = (ms) => {
            const d = new Date(ms);
            return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
        };

        const totalDuration = endTime - startTime;
        const selectedStartMs = startTime + (start / 100) * totalDuration;
        const selectedEndMs = startTime + (end / 100) * totalDuration;

        // Show date labels only when the selected start and end fall on different days
        const startDate = new Date(selectedStartMs);
        const endDate = new Date(selectedEndMs);
        const showDates = startDate.toDateString() !== endDate.toDateString();

        // Parse a user-entered time string and return ms since epoch, or null on failure.
        // Accepts "HH:MM:SS" or "MMM DD HH:MM:SS" (e.g. "Mar 29 14:30:00").
        const parseTimeInput = (text, referenceMs) => {
            const ref = new Date(referenceMs);
            // Try "HH:MM:SS" — keep the date from the reference
            const timeOnly = text.match(/^(\d{1,2}):(\d{2}):(\d{2})$/);
            if (timeOnly) {
                const d = new Date(ref);
                d.setHours(parseInt(timeOnly[1], 10), parseInt(timeOnly[2], 10), parseInt(timeOnly[3], 10), 0);
                return d.getTime();
            }
            // Try "MMM DD HH:MM:SS"
            const full = text.match(/^(\w+)\s+(\d{1,2})\s+(\d{1,2}):(\d{2}):(\d{2})$/);
            if (full) {
                const months = ['jan','feb','mar','apr','may','jun','jul','aug','sep','oct','nov','dec'];
                const monthIdx = months.indexOf(full[1].toLowerCase());
                if (monthIdx === -1) return null;
                const d = new Date(ref);
                d.setMonth(monthIdx, parseInt(full[2], 10));
                d.setHours(parseInt(full[3], 10), parseInt(full[4], 10), parseInt(full[5], 10), 0);
                return d.getTime();
            }
            return null;
        };

        const commitEdit = (which) => {
            const text = vnode.state.editValue.trim();
            const refMs = which === 'start' ? selectedStartMs : selectedEndMs;
            const parsed = parseTimeInput(text, refMs);
            vnode.state.editing = null;
            if (parsed === null) return; // invalid input, discard

            // Clamp to recording bounds
            const clampedMs = Math.max(startTime, Math.min(endTime, parsed));
            const pct = ((clampedMs - startTime) / totalDuration) * 100;

            if (which === 'start') {
                vnode.state.applyZoom(Math.min(pct, end - 0.5), end);
            } else {
                vnode.state.applyZoom(start, Math.max(pct, start + 0.5));
            }
        };

        const startEditing = (which, currentMs) => {
            vnode.state.editing = which;
            vnode.state.editValue = showDates
                ? `${formatDate(currentMs)} ${formatTime(currentMs)}`
                : formatTime(currentMs);
        };

        const editInput = (which) => m('input.time-label-input', {
            value: vnode.state.editValue,
            oninput: (e) => { vnode.state.editValue = e.target.value; },
            onkeydown: (e) => {
                if (e.key === 'Enter') commitEdit(which);
                if (e.key === 'Escape') { vnode.state.editing = null; }
            },
            onblur: () => commitEdit(which),
            oncreate: (v) => { v.dom.focus(); v.dom.select(); },
        });

        const timeLabel = (which, ms) => {
            if (vnode.state.editing === which) {
                return editInput(which);
            }
            return m('span.time-label', {
                ondblclick: () => startEditing(which, ms),
                title: 'Double-click to edit',
            }, [
                showDates && m('span.time-label-date', formatDate(ms)),
                formatTime(ms),
            ]);
        };

        return m('div.time-range-bar', [
            timeLabel('start', selectedStartMs),
            m('div.time-track', [
                // Dimmed regions outside selection
                start > 0 && m('div.time-dim', { style: { left: '0%', width: `${start}%` } }),
                end < 100 && m('div.time-dim', { style: { left: `${end}%`, width: `${100 - end}%` } }),
                // Selection window
                m('div.time-selection', {
                    style: { left: `${start}%`, width: `${end - start}%` },
                }, [
                    m('div.time-handle.time-handle-left'),
                    m('div.time-handle.time-handle-right'),
                ]),
            ]),
            timeLabel('end', selectedEndMs),
            m('button.time-reset-btn', {
                onclick: (e) => {
                    e.stopPropagation();
                    vnode.state.barStart = 0;
                    vnode.state.barEnd = 100;
                    chartsState.resetAll();
                    m.redraw();
                },
                title: 'Reset to full time range',
                style: { visibility: (start > 0.1 || end < 99.9) ? 'visible' : 'hidden' },
            }, 'Reset'),
        ]);
    },
};

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
            // Global time range bar (right-aligned)
            (attrs.start_time != null && attrs.end_time != null) &&
                m(TimeRangeBar, { start_time: attrs.start_time, end_time: attrs.end_time, chartsState: attrs.chartsState }),
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
