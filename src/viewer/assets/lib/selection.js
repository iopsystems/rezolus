// Selection & Report stores — curated collections of charts with annotations.
// Selection: built interactively by selecting charts (write mode).
// Report: loaded from JSON import or parquet metadata (read-only mode).

import { ChartsState, Chart } from './charts/chart.js';
import { executePromQLRangeQuery, applyResultToPlot } from './data.js';

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
    loadedFrom: null, // filename of the imported JSON
};

// ── LocalStorage persistence ─────────────────────────────────────

const REPORT_STORAGE_KEY = 'rezolus_report';
const SELECTION_STORAGE_KEY = 'rezolus_selection';

const persistStore = (key, store) => {
    try {
        const data = {
            tagline: store.tagline,
            zoom: store.zoom,
            loadedFrom: store.loadedFrom || undefined,
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
        localStorage.removeItem(REPORT_STORAGE_KEY);
    } else if (store === selectionStore) {
        localStorage.removeItem(SELECTION_STORAGE_KEY);
    }
};

// ── Export / Import / Parquet ─────────────────────────────────────

const buildPayload = (store, attrs) => ({
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

const exportJSON = (store, attrs) => {
    const defaultName = `selection-${Date.now()}.json`;
    const filename = prompt('File name:', defaultName);
    if (!filename) return;
    const payload = buildPayload(store, attrs);
    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename.endsWith('.json') ? filename : filename + '.json';
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
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
    if (store === reportStore) persistReport();
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
            const payload = JSON.parse(text);
            if (!payload.entries || !Array.isArray(payload.entries)) {
                console.error('[selection] invalid JSON: missing entries array');
                return;
            }
            if (payload.file_checksum && currentChecksum && payload.file_checksum !== currentChecksum) {
                const ok = confirm(
                    `This selection was saved from a different parquet file` +
                    (payload.filename ? ` ("${payload.filename}")` : '') +
                    `. Charts may not render correctly. Load anyway?`
                );
                if (!ok) return;
            }
            loadPayloadIntoStore(reportStore, payload);
            reportStore.loadedFrom = file.name;
            persistReport();
            if (m.route.get() === '/report') {
                ReportView._needsReload = true;
                m.redraw();
            } else {
                m.route.set('/report');
            }
        } catch (e) {
            console.error('[selection] failed to import JSON:', e);
        }
    };
    input.click();
};

const saveToParquet = async (store, attrs) => {
    const defaultName = (attrs.filename || 'rezolus-capture').replace(/\.parquet$/, '') + '-annotated.parquet';
    const filename = prompt('File name:', defaultName);
    if (!filename) return;
    const payload = buildPayload(store, attrs);
    try {
        const xhr = new XMLHttpRequest();
        xhr.open('POST', '/api/v1/save_with_selection', true);
        xhr.setRequestHeader('Content-Type', 'application/json');
        xhr.responseType = 'blob';
        xhr.onload = () => {
            if (xhr.status !== 200) {
                console.error('[selection] save failed:', xhr.status);
                return;
            }
            const blob = xhr.response;
            const url = URL.createObjectURL(blob);
            const a = document.createElement('a');
            a.href = url;
            a.download = filename.endsWith('.parquet') ? filename : filename + '.parquet';
            document.body.appendChild(a);
            a.click();
            document.body.removeChild(a);
            URL.revokeObjectURL(url);
        };
        xhr.send(JSON.stringify(payload));
    } catch (e) {
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
        const hasLocalZoom = cs?.zoomSource === 'local' && !cs?.isDefaultZoom();
        const hasChartSelection = hasLocalZoom ||
            Array.from(cs?.charts?.values() || []).some(c => c._tooltipFrozen || (c.pinnedSet && c.pinnedSet.size > 0));
        const hasHistograms = selectionStore.entries.some(e => e.promql_query && e.promql_query.includes('histogram_percentiles'));

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
                m('button.selection-btn', { onclick: () => exportJSON(selectionStore, attrs) }, 'Export JSON'),
                m('button.selection-btn', { onclick: () => saveToParquet(selectionStore, attrs) }, 'Annotate Parquet'),
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
        const hasLocalZoom = cs?.zoomSource === 'local' && !cs?.isDefaultZoom();
        const hasChartSelection = hasLocalZoom ||
            Array.from(cs?.charts?.values() || []).some(c => c._tooltipFrozen || (c.pinnedSet && c.pinnedSet.size > 0));
        const hasHistograms = reportStore.entries.some(e => e.promql_query && e.promql_query.includes('histogram_percentiles'));

        const header = m('div.selection-header', [
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
    toggleSelection,
    isSelected,
    clearStore,
    importJSON,
    loadPayloadIntoStore,
    SelectionView,
    ReportView,
};
