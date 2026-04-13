// Selection & Report stores — curated collections of charts with annotations.
// Selection: built interactively by selecting charts (write mode).
// Report: loaded from JSON import or parquet metadata (read-only mode).

import { ChartsState, Chart } from './charts/chart.js';
import { executePromQLRangeQuery, applyResultToPlot } from './data.js';
import { notify, showSaveModal } from './overlays.js';
import { isHistogramPlot } from './charts/metric_types.js';

// ── UUIDv7 (RFC 9562) ──────────────────────────────────────────────

const uuidv7 = () => {
    const ms = Date.now();
    const bytes = new Uint8Array(16);
    crypto.getRandomValues(bytes);
    bytes[0] = (ms / 2**40) & 0xff;
    bytes[1] = (ms / 2**32) & 0xff;
    bytes[2] = (ms / 2**24) & 0xff;
    bytes[3] = (ms / 2**16) & 0xff;
    bytes[4] = (ms / 2**8) & 0xff;
    bytes[5] = ms & 0xff;
    bytes[6] = (bytes[6] & 0x0f) | 0x70; // version 7
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 10
    const hex = [...bytes].map(b => b.toString(16).padStart(2, '0')).join('');
    return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
};

// ── Stores ───────────────────────────────────────────────────────

const selectionStore = {
    tagline: '',
    entries: [],
    zoom: null,
};

const reportStore = {
    tagline: '',
    entries: [],
    zoom: null,
    loadedFrom: null,    // filename of the imported JSON
    reportId: null,       // UUIDv7 from the imported report
    savedAt: null,        // ISO timestamp
    sourceFilename: null, // original parquet filename
    fileChecksum: null,   // SHA-256 of the parquet
    timeRange: null,      // { start_ms, end_ms }
    rezolusVersion: null,
};

// ── LocalStorage persistence ─────────────────────────────────────

let REPORT_STORAGE_KEY = 'rezolus_report';
let SELECTION_STORAGE_KEY = 'rezolus_selection';

/**
 * Scope localStorage keys by a file fingerprint so each parquet file
 * gets its own selection/report state. Call after loading a new file.
 *
 * @param {{ filename?: string, minTime?: number, maxTime?: number, numSeries?: number }} info
 */
const setStorageScope = (info) => {
    const parts = [
        info.filename || '',
        info.minTime || 0,
        info.maxTime || 0,
        info.numSeries || 0,
    ].join('|');
    const suffix = Array.from(new TextEncoder().encode(parts))
        .reduce((h, b) => ((h << 5) - h + b) | 0, 0)
        .toString(36);
    REPORT_STORAGE_KEY = `rezolus_report_${suffix}`;
    SELECTION_STORAGE_KEY = `rezolus_selection_${suffix}`;
    // Restore from the scoped keys
    clearStore(selectionStore);
    clearStore(reportStore);
    restoreStore(REPORT_STORAGE_KEY, reportStore);
    restoreStore(SELECTION_STORAGE_KEY, selectionStore);
};

const persistStore = (key, store) => {
    try {
        const data = {
            tagline: store.tagline,
            zoom: store.zoom,
            loadedFrom: store.loadedFrom || undefined,
            reportId: store.reportId || undefined,
            savedAt: store.savedAt || undefined,
            sourceFilename: store.sourceFilename || undefined,
            fileChecksum: store.fileChecksum || undefined,
            timeRange: store.timeRange || undefined,
            rezolusVersion: store.rezolusVersion || undefined,
            entries: store.entries.map(e => ({
                chartId: e.chartId,
                section: e.section,
                sectionName: e.sectionName,
                promql_query: e.promql_query,
                note: e.note,
                chartOpts: e.chartOpts,
            })),
        };
        localStorage.setItem(key, JSON.stringify(data));
    } catch (e) {
        console.warn('[selection] failed to persist:', e);
    }
};

const restoreStore = (key, store) => {
    try {
        const raw = localStorage.getItem(key);
        if (!raw) return;
        const data = JSON.parse(raw);
        if (!data.entries || !Array.isArray(data.entries)) return;
        store.tagline = data.tagline || '';
        store.zoom = data.zoom || null;
        if (data.loadedFrom !== undefined) store.loadedFrom = data.loadedFrom;
        if (data.reportId !== undefined) store.reportId = data.reportId;
        if (data.savedAt !== undefined) store.savedAt = data.savedAt;
        if (data.sourceFilename !== undefined) store.sourceFilename = data.sourceFilename;
        if (data.fileChecksum !== undefined) store.fileChecksum = data.fileChecksum;
        if (data.timeRange !== undefined) store.timeRange = data.timeRange;
        if (data.rezolusVersion !== undefined) store.rezolusVersion = data.rezolusVersion;
        store.entries = data.entries.map(e => ({
            id: crypto.randomUUID(),
            chartId: e.chartId,
            section: e.section,
            sectionName: e.sectionName,
            promql_query: e.promql_query,
            note: e.note || '',
            chartOpts: e.chartOpts,
        }));
    } catch (e) {
        console.warn('[selection] failed to restore:', e);
    }
};

const persistReport = () => persistStore(REPORT_STORAGE_KEY, reportStore);
const persistSelection = () => persistStore(SELECTION_STORAGE_KEY, selectionStore);

// Stores are restored when setStorageScope() is called with a file fingerprint,
// or eagerly here for the default (unscoped) keys as a fallback.
restoreStore(REPORT_STORAGE_KEY, reportStore);
restoreStore(SELECTION_STORAGE_KEY, selectionStore);

// ── Selection API (write mode) ───────────────────────────────────

const toggleSelection = (spec, sectionKey, sectionName) => {
    const idx = selectionStore.entries.findIndex(e => e.chartId === spec.opts.id);
    if (idx >= 0) {
        selectionStore.entries.splice(idx, 1);
        persistSelection();
        return false;
    }
    selectionStore.entries.push({
        id: crypto.randomUUID(),
        chartId: spec.opts.id,
        section: sectionKey,
        sectionName,
        promql_query: spec.promql_query,
        note: '',
        chartOpts: JSON.parse(JSON.stringify(spec.opts)),
    });
    persistSelection();
    return true;
};

const isSelected = (chartId) => selectionStore.entries.some(e => e.chartId === chartId);

const removeEntry = (store, id) => {
    const idx = store.entries.findIndex(e => e.id === id);
    if (idx >= 0) {
        store.entries.splice(idx, 1);
        if (store === selectionStore) persistSelection();
        else if (store === reportStore) persistReport();
    }
};

const clearStore = (store) => {
    store.entries.length = 0;
    store.tagline = '';
    store.zoom = null;
    if (store === reportStore) {
        store.loadedFrom = null;
        store.reportId = null;
        store.savedAt = null;
        store.sourceFilename = null;
        store.fileChecksum = null;
        store.timeRange = null;
        store.rezolusVersion = null;
        localStorage.removeItem(REPORT_STORAGE_KEY);
    } else if (store === selectionStore) {
        localStorage.removeItem(SELECTION_STORAGE_KEY);
    }
};

// ── Export / Import / Parquet ─────────────────────────────────────

const buildPayload = (store, attrs) => ({
    report_id: uuidv7(),
    rezolus_version: attrs.version || 'unknown',
    saved_at: new Date().toISOString(),
    source: attrs.source || '',
    filename: attrs.filename || '',
    file_checksum: attrs.fileChecksum || null,
    time_range: {
        start_ms: attrs.start_time || 0,
        end_ms: attrs.end_time || 0,
    },
    zoom: attrs.chartsState?.zoomLevel || null,
    tagline: store.tagline,
    entries: store.entries.map(e => ({
        chartId: e.chartId,
        section: e.section,
        sectionName: e.sectionName,
        promql_query: e.promql_query,
        note: e.note,
        chartOpts: e.chartOpts,
    })),
});

const exportJSON = async (store, attrs) => {
    const defaultPrefix = `report-${Date.now()}`;
    const result = await showSaveModal(defaultPrefix, '.json');
    if (!result) return;
    const filename = result.filename;
    const payload = buildPayload(store, attrs);
    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
    notify('info', `Exported ${store.entries.length} chart(s) to ${filename}`);
};

const loadPayloadIntoStore = (store, payload) => {
    store.tagline = payload.tagline || '';
    store.zoom = payload.zoom || null;
    store.entries = payload.entries.map(e => ({
        id: crypto.randomUUID(),
        chartId: e.chartId,
        section: e.section,
        sectionName: e.sectionName,
        promql_query: e.promql_query,
        note: e.note || '',
        chartOpts: e.chartOpts,
    }));
    if (store === reportStore) {
        store.reportId = payload.report_id || null;
        store.savedAt = payload.saved_at || null;
        store.sourceFilename = payload.filename || null;
        store.fileChecksum = payload.file_checksum || null;
        store.timeRange = payload.time_range || null;
        store.rezolusVersion = payload.rezolus_version || null;
        persistReport();
    }
};

const importJSON = (currentChecksum) => {
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = '.json';
    input.onchange = async () => {
        const file = input.files[0];
        if (!file) return;
        try {
            const text = await file.text();
            let payload;
            try {
                payload = JSON.parse(text);
            } catch {
                notify('error', 'Not a valid Rezolus report');
                return;
            }
            if (!payload.entries || !Array.isArray(payload.entries)) {
                notify('error', 'Not a valid Rezolus report');
                return;
            }
            if (payload.file_checksum && currentChecksum && payload.file_checksum !== currentChecksum) {
                const ok = confirm(
                    `This report was saved from a different parquet file` +
                    (payload.filename ? ` ("${payload.filename}")` : '') +
                    `. Charts may not render correctly. Load anyway?`
                );
                if (!ok) return;
                notify('warn', 'Report was saved from a different parquet file');
            }
            loadPayloadIntoStore(reportStore, payload);
            reportStore.loadedFrom = file.name;
            notify('info', `Loaded report (${reportStore.entries.length} charts)`);
            if (m.route.get() === '/report') {
                ReportView._needsReload = true;
                m.redraw();
            } else {
                m.route.set('/report');
            }
        } catch (e) {
            notify('error', 'Failed to import report');
            console.error('[selection] failed to import JSON:', e);
        }
    };
    input.click();
};

const saveToParquet = async (store, attrs) => {
    const defaultPrefix = (attrs.filename || 'rezolus-capture').replace(/\.parquet$/, '') + '-annotated';
    const cs = attrs.chartsState;
    const hasZoom = cs && !cs.isDefaultZoom();
    const checkboxes = hasZoom
        ? [{ key: 'trim', label: 'Trim to selected time range', checked: false }]
        : [];
    const result = await showSaveModal(defaultPrefix, '.parquet', checkboxes);
    if (!result) return;
    const filename = result.filename;
    const trimToSelection = result.trim;
    const payload = buildPayload(store, attrs);

    // When trimming, compute the absolute time range (ms) from the zoom percentage
    if (trimToSelection) {
        const zoom = cs?.globalZoom || cs?.zoomLevel;
        if (zoom && attrs.start_time != null && attrs.end_time != null) {
            const total = attrs.end_time - attrs.start_time;
            payload.trim_range_ms = {
                start: attrs.start_time + (zoom.start / 100) * total,
                end: attrs.start_time + (zoom.end / 100) * total,
            };
        }
    }

    try {
        const xhr = new XMLHttpRequest();
        xhr.open('POST', '/api/v1/save_with_selection', true);
        xhr.setRequestHeader('Content-Type', 'application/json');
        xhr.responseType = 'blob';
        xhr.onload = () => {
            if (xhr.status !== 200) {
                notify('error', `Failed to save parquet (HTTP ${xhr.status})`);
                return;
            }
            const blob = xhr.response;
            const url = URL.createObjectURL(blob);
            const a = document.createElement('a');
            a.href = url;
            a.download = filename;
            document.body.appendChild(a);
            a.click();
            document.body.removeChild(a);
            URL.revokeObjectURL(url);
            notify('info', `Saved ${filename}`);
        };
        xhr.send(JSON.stringify(payload));
    } catch (e) {
        notify('error', 'Failed to save parquet');
        console.error('[selection] failed to save to parquet:', e);
    }
};

// ── Shared chart loading logic ───────────────────────────────────

const chartLoaderMixin = (store, component) => ({
    oninit() {
        this.specs = new Map();
        this.loading = true;
        this._zoomApplied = false;
        this._loadCharts();
    },

    async _loadCharts() {
        const promises = store.entries.map(async (entry) => {
            if (!entry.promql_query) return;
            try {
                const spec = {
                    opts: { ...entry.chartOpts },
                    promql_query: entry.promql_query,
                    data: [],
                };
                const result = await executePromQLRangeQuery(entry.promql_query);
                if (result) {
                    applyResultToPlot(spec, result);
                }
                this.specs.set(entry.chartId, spec);
            } catch (e) {
                console.warn('[selection] failed to load chart:', entry.chartId, e);
            }
        });
        await Promise.all(promises);
        this.loading = false;
        m.redraw();
    },

    _checkReload() {
        if (component._needsReload) {
            component._needsReload = false;
            this.specs = new Map();
            this.loading = true;
            this._zoomApplied = false;
            this._loadCharts();
        }
    },

    _applyZoom(attrs) {
        if (!this._zoomApplied && !this.loading && store.zoom && attrs.chartsState) {
            this._zoomApplied = true;
            const z = store.zoom;
            attrs.chartsState.zoomLevel = z;
            attrs.chartsState.zoomSource = 'global';
            attrs.chartsState.charts.forEach(chart => {
                chart.dispatchAction({
                    type: 'dataZoom',
                    start: z.start,
                    end: z.end,
                    startValue: z.startValue,
                    endValue: z.endValue,
                });
            });
        }
    },
});

// ── SelectionView (write mode) ───────────────────────────────────

const SelectionView = {
    _needsReload: false,
};
Object.assign(SelectionView, chartLoaderMixin(selectionStore, SelectionView), {
    view({ attrs }) {
        this._checkReload();
        this._applyZoom(attrs);

        const interval = attrs.interval || 1;
        const cs = attrs.chartsState;
        const hasChartSelection = cs?.hasActiveSelection();
        const hasHistograms = selectionStore.entries.some(e => isHistogramPlot(e));

        const header = m('div.selection-header', [
            m('div.section-header-row', [
                m('h1.section-title', attrs.title || 'Selection'),
                m('div.section-actions', [
                    hasChartSelection && m('button.section-action-btn', {
                        onclick: () => { cs.resetAll(); m.redraw(); },
                    }, 'RESET SELECTION'),
                    hasHistograms &&
                    m('button.section-action-btn', {
                        onclick: attrs.onToggleHeatmap,
                        disabled: attrs.heatmapLoading,
                    }, attrs.heatmapLoading ? 'LOADING...' : (attrs.heatmapEnabled ? 'SHOW PERCENTILES' : 'SHOW HEATMAPS')),
                ]),
            ]),
            m('input.selection-tagline', {
                type: 'text',
                placeholder: 'Add a tagline\u2026',
                value: selectionStore.tagline,
                oninput: (e) => { selectionStore.tagline = e.target.value; persistSelection(); },
            }),
        ]);

        if (this.loading) {
            return m('div#section-content.selection-section', [header, m('p', 'Loading charts\u2026')]);
        }

        return m('div#section-content.selection-section', [
            header,

            m('div.selection-actions', [
                m('button.selection-btn', { onclick: () => exportJSON(selectionStore, attrs) }, [
                    'Export JSON ',
                    m.trust('<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 11v2a1 1 0 001 1h10a1 1 0 001-1v-2"/><path d="M8 10V2m0 8l-3-3m3 3l3-3"/></svg>'),
                ]),
                m('button.selection-btn', {
                    onclick: () => saveToParquet(selectionStore, attrs),
                }, [
                    'Annotate Parquet ',
                    m.trust('<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 11v2a1 1 0 001 1h10a1 1 0 001-1v-2"/><path d="M8 10V2m0 8l-3-3m3 3l3-3"/></svg>'),
                ]),
                m('button.selection-btn.selection-btn-danger', {
                    onclick: () => { clearStore(selectionStore); m.redraw(); },
                }, 'Clear All'),
            ]),

            selectionStore.entries.map((entry) => {
                const spec = this.specs.get(entry.chartId);
                if (!spec) return null;
                return m('div.selection-card', [
                    m('div.selection-card-body', [
                        m('div.selection-card-chart', [
                            m('button.selection-card-remove', {
                                onclick: () => { removeEntry(selectionStore, entry.id); m.redraw(); },
                                title: 'Remove',
                            }, 'X'),
                            m('div.chart-wrapper', [
                                m('div.chart-header', [
                                    m('span.chart-title', spec.opts.title),
                                    spec.opts.description && m('span.chart-subtitle', spec.opts.description),
                                ]),
                                m(Chart, { spec, chartsState: attrs.chartsState, interval }),
                            ]),
                        ]),
                        m('div.selection-card-notes', [
                            m('label.selection-notes-label', 'Notes'),
                            m('textarea.selection-notes', {
                                placeholder: 'Add notes\u2026',
                                value: entry.note,
                                oninput: (e) => { entry.note = e.target.value; persistSelection(); },
                            }),
                        ]),
                    ]),
                ]);
            }),
        ]);
    },
});

// ── ReportView (read-only mode) ──────────────────────────────────

const ReportView = {
    _needsReload: false,
};
Object.assign(ReportView, chartLoaderMixin(reportStore, ReportView), {
    view({ attrs }) {
        this._checkReload();
        this._applyZoom(attrs);

        const interval = attrs.interval || 1;
        const cs = attrs.chartsState;
        const hasChartSelection = cs?.hasActiveSelection();
        const hasHistograms = reportStore.entries.some(e => isHistogramPlot(e));

        const fmtTs = (ms) => {
            const d = new Date(ms);
            return `${d.toISOString().slice(0, 10)} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}:${String(d.getSeconds()).padStart(2, '0')}`;
        };

        const meta = [];
        if (reportStore.reportId) meta.push(['Report ID', reportStore.reportId]);
        if (reportStore.savedAt) meta.push(['Saved', reportStore.savedAt.replace('T', ' ').replace(/\.\d+Z$/, ' UTC')]);
        if (reportStore.sourceFilename) {
            const name = reportStore.sourceFilename;
            const cksum = reportStore.fileChecksum ? ` (${reportStore.fileChecksum.slice(0, 6)})` : '';
            meta.push(['Source', name + cksum]);
        }
        if (reportStore.timeRange && reportStore.timeRange.start_ms && reportStore.timeRange.end_ms) {
            meta.push(['Time Range', `${fmtTs(reportStore.timeRange.start_ms)} \u2013 ${fmtTs(reportStore.timeRange.end_ms)}`]);
        }
        if (reportStore.rezolusVersion) meta.push(['Rezolus', reportStore.rezolusVersion]);

        const header = m('div.selection-header', [
            meta.length > 0 && m('div.report-meta', meta.map(([label, value]) =>
                m('span.report-meta-item', [
                    m('span.report-meta-label', label + ': '),
                    value,
                ]),
            )),
            m('div.section-header-row', [
                m('h1.section-title', attrs.title || 'Report'),
                m('div.section-actions', [
                    hasChartSelection && m('button.section-action-btn', {
                        onclick: () => { cs.resetAll(); m.redraw(); },
                    }, 'RESET SELECTION'),
                    hasHistograms &&
                    m('button.section-action-btn', {
                        onclick: attrs.onToggleHeatmap,
                        disabled: attrs.heatmapLoading,
                    }, attrs.heatmapLoading ? 'LOADING...' : (attrs.heatmapEnabled ? 'SHOW PERCENTILES' : 'SHOW HEATMAPS')),
                ]),
            ]),
            reportStore.tagline && m('p.selection-tagline-text', reportStore.tagline),
        ]);

        if (this.loading) {
            return m('div#section-content.selection-section', [header, m('p', 'Loading charts\u2026')]);
        }

        return m('div#section-content.selection-section', [
            header,

            reportStore.entries.map((entry) => {
                const spec = this.specs.get(entry.chartId);
                if (!spec) return null;
                return m('div.selection-card', [
                    m('div.selection-card-body', [
                        m('div.selection-card-chart', [
                            m('div.chart-wrapper', [
                                m('div.chart-header', [
                                    m('span.chart-title', spec.opts.title),
                                    spec.opts.description && m('span.chart-subtitle', spec.opts.description),
                                ]),
                                m(Chart, { spec, chartsState: attrs.chartsState, interval }),
                            ]),
                        ]),
                        entry.note && m('div.selection-card-notes', [
                            m('label.selection-notes-label', 'Notes'),
                            m('p.selection-notes-text', entry.note),
                        ]),
                    ]),
                ]);
            }),
        ]);
    },
});

export {
    selectionStore,
    reportStore,
    setStorageScope,
    toggleSelection,
    isSelected,
    clearStore,
    importJSON,
    loadPayloadIntoStore,
    SelectionView,
    ReportView,
};
