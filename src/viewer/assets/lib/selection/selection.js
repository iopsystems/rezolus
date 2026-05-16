// Selection & Report stores — curated collections of charts with annotations.
// Selection: built interactively by selecting charts (write mode).
// Report: loaded from JSON import or parquet metadata (read-only mode).

import { ChartsState, Chart } from '../charts/chart.js';
import { CompareChartWrapper } from '../viewer_core.js';
import { compareToggle } from '../ui/chart_controls.js';
import { executePromQLRangeQuery, applyResultToPlot, buildEffectiveQuery, CAPTURE_BASELINE, CAPTURE_EXPERIMENT } from '../data.js';
import { notify, showSaveModal } from '../ui/overlays.js';
import { isHistogramPlot } from '../charts/metric_types.js';
import { migrateSelection, SELECTION_SCHEMA_VERSION } from './selection_migration.js';
import { ViewerApi } from '../viewer_api.js';
import { eventsStore } from '../events/events_store.js';
import { renderMarkdown, renderMarkdownInline } from '../ui/markdown.js';

const PIN_ICON_PATH = 'M9.828.722a.5.5 0 0 1 .354.146l4.95 4.95a.5.5 0 0 1 0 .707c-.48.48-1.072.588-1.503.588-.177 0-.335-.018-.46-.039l-3.134 3.134a5.927 5.927 0 0 1 .16 1.013c.046.702-.032 1.687-.72 2.375a.5.5 0 0 1-.707 0l-2.829-2.828-3.182 3.182c-.195.195-1.219.902-1.414.707-.195-.195.512-1.22.707-1.414l3.182-3.182-2.828-2.829a.5.5 0 0 1 0-.707c.688-.688 1.673-.767 2.375-.72a5.922 5.922 0 0 1 1.013.16l3.134-3.133a2.772 2.772 0 0 1-.04-.461c0-.43.108-1.022.589-1.503a.5.5 0 0 1 .353-.146z';

const sectionTitlePin = () => m('svg.section-title-pin', {
    width: 18, height: 18, viewBox: '0 0 16 16', fill: 'currentColor',
    'aria-hidden': 'true',
}, m('path', { d: PIN_ICON_PATH }));

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

const notebookStore = {
    tagline: '',
    entries: [],
    zoom: null,
    stepOverride: null,   // query step in seconds (null = auto)
    anchors: { baseline: 0, experiment: 0 }, // compare-mode offsets in ms
    chartToggles: {},      // per-chart compare-mode toggles, e.g. { chartId: { diff: true } }
    compare: null,         // { baseline_alias, experiment_alias } when set in compare mode
};

const reportStore = {
    tagline: '',
    entries: [],
    zoom: null,
    stepOverride: null,   // query step in seconds (null = auto)
    anchors: { baseline: 0, experiment: 0 },
    chartToggles: {},
    compare: null,         // { baseline_alias, experiment_alias } when set in compare mode
    loadedFrom: null,    // filename of the imported JSON
    reportId: null,       // UUIDv7 from the imported report
    savedAt: null,        // ISO timestamp
    sourceFilename: null, // original parquet filename
    fileChecksum: null,   // SHA-256 of the parquet
    timeRange: null,      // { start_ms, end_ms }
    rezolusVersion: null,
};

const loadedSelectionStore = {
    tagline: '',
    entries: [],
    zoom: null,
    stepOverride: null,
    anchors: { baseline: 0, experiment: 0 },
    chartToggles: {},
    compare: null,
    loadedFrom: null,  // filename of the dropped JSON
};

// Per-card "notes textarea expanded" tracker for NotebookView.
// Module-level rather than view-local because removeEntry,
// clearStore, and openInNotebook all need to clear it before the
// view next mounts. UI-only state — never persisted.
const expandedNotes = new Set();
// Notes/preamble default to raw edit mode in the Notebook; entry.id
// (or the page-preamble sentinel) is present here only while that
// field is toggled to rendered preview. UI-only, never persisted.
const previewMarkdown = new Set();
const PREAMBLE_KEY = '__preamble__';
// entry.id present while its chart title is being edited (Notebook).
const editingTitle = new Set();

// A Markdown field with an Edit ⇄ Preview toggle. Preview renders
// sanitized HTML via renderMarkdown; Edit shows a raw textarea.
// `key` is the previewMarkdown identity (entry id or PREAMBLE_KEY).
const markdownField = ({ key, cls, label, value, placeholder, onInput, onEmptyBlur }) => {
    const editing = !previewMarkdown.has(key);
    const toggle = m('button.md-toggle', {
        type: 'button',
        onclick: () => {
            if (editing) previewMarkdown.add(key);
            else previewMarkdown.delete(key);
            m.redraw();
        },
    }, editing ? 'Preview' : 'Edit');

    const header = m('div.md-field-header', [
        label ? m('label.md-field-label', label) : null,
        toggle,
    ]);

    if (editing) {
        return m('div', { class: cls }, [
            header,
            m('textarea.md-textarea', {
                placeholder,
                value,
                oninput: (e) => onInput(e.target.value),
                onblur: (e) => { if (!e.target.value && onEmptyBlur) onEmptyBlur(); },
            }),
        ]);
    }
    const body = value
        ? m('div.md-rendered', m.trust(renderMarkdown(value)))
        : m('div.md-empty', {
            onclick: () => { previewMarkdown.delete(key); m.redraw(); },
        }, placeholder || 'Nothing here yet — click Edit');
    return m('div', { class: cls }, [header, body]);
};

// Hydrate an entries list from a JSON payload. Used by all three
// load paths (restoreStore, loadPayloadIntoStore, loadJsonIntoSelection)
// so the entry shape stays in one place.
const entriesFromPayload = (entries) => (entries || []).map(e => ({
    id: crypto.randomUUID(),
    chartId: e.chartId,
    section: e.section,
    sectionName: e.sectionName,
    groupName: e.groupName || '',
    promql_query: e.promql_query,
    // Optional on pre-fix payloads; null degrades to single-query compare.
    promql_query_experiment: e.promql_query_experiment || null,
    category_members: e.category_members || null,
    note: e.note || '',
    // Optional inline-Markdown title override; '' = use derived title.
    titleOverride: e.titleOverride || '',
    chartOpts: e.chartOpts,
}));

// ── LocalStorage persistence ─────────────────────────────────────

let REPORT_STORAGE_KEY = 'rezolus_report';
let NOTEBOOK_STORAGE_KEY = 'rezolus_notebook';
let EVENTS_STORAGE_KEY = 'rezolus_events';

// True once a localStorage working set was restored for the active
// scope — seedEventsFromMetadata then becomes a no-op so the persisted
// (possibly-edited) set wins over the parquet footer, mirroring how
// Notebook localStorage overrides embedded report state.
let eventsRestoredFromStorage = false;
// Guards persistEvents during restore / seed so we only ever persist
// genuine user mutations (add / delete), never the initial hydration.
let suspendEventsPersist = false;

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
    NOTEBOOK_STORAGE_KEY = `rezolus_notebook_${suffix}`;
    EVENTS_STORAGE_KEY = `rezolus_events_${suffix}`;
    // In-memory reset only — must NOT purge localStorage at the
    // (just-set) scoped key, otherwise we wipe the file's persisted
    // notebook on every page load before restoring it.
    // loadedSelectionStore is in-memory only; reset so a Selection
    // imported against a previous parquet doesn't survive into the
    // new file's session (its chart specs may not even resolve).
    resetStoreState(notebookStore);
    resetStoreState(reportStore);
    resetStoreState(loadedSelectionStore);
    restoreStore(REPORT_STORAGE_KEY, reportStore);
    restoreStore(NOTEBOOK_STORAGE_KEY, notebookStore);
    // Drop the previous file's in-memory events, then either restore
    // this file's persisted working set or leave it empty for
    // seedEventsFromMetadata to fill from the footer.
    suspendEventsPersist = true;
    eventsStore.clear();
    suspendEventsPersist = false;
    restoreEvents();
};

const persistEvents = () => {
    if (suspendEventsPersist) return;
    try {
        localStorage.setItem(
            EVENTS_STORAGE_KEY,
            JSON.stringify({ version: SELECTION_SCHEMA_VERSION, events: eventsStore.all() }),
        );
    } catch (e) {
        console.warn('[selection] failed to persist events:', e);
    }
};

const restoreEvents = () => {
    eventsRestoredFromStorage = false;
    try {
        const raw = localStorage.getItem(EVENTS_STORAGE_KEY);
        if (!raw) return;
        const parsed = JSON.parse(raw);
        if (!parsed || !Array.isArray(parsed.events)) return;
        suspendEventsPersist = true;
        eventsStore.replaceAll(parsed.events);
        suspendEventsPersist = false;
        eventsRestoredFromStorage = true;
    } catch (e) {
        console.warn('[selection] failed to restore events:', e);
    }
};

// Seed the events store from the parquet footer — unless a persisted
// working set was already restored for this scope (then the persisted
// set is authoritative, so unsaved adds/deletes survive a refresh).
// Suspends persistence so the footer-only seed never lands in
// localStorage; only genuine user edits get persisted thereafter.
export const seedEventsFromMetadata = (fileMetadata) => {
    if (eventsRestoredFromStorage) return;
    suspendEventsPersist = true;
    eventsStore.seedFromMetadata(fileMetadata);
    suspendEventsPersist = false;
};

eventsStore.subscribe(persistEvents);

const persistStore = (key, store) => {
    try {
        const data = {
            version: SELECTION_SCHEMA_VERSION,
            tagline: store.tagline,
            zoom: store.zoom,
            stepOverride: store.stepOverride ?? undefined,
            anchors: store.anchors || { baseline: 0, experiment: 0 },
            chartToggles: store.chartToggles || {},
            compare: store.compare || undefined,
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
                groupName: e.groupName || '',
                promql_query: e.promql_query,
                promql_query_experiment: e.promql_query_experiment || undefined,
                category_members: e.category_members || undefined,
                note: e.note,
                titleOverride: e.titleOverride || undefined,
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
        const parsed = JSON.parse(raw);
        if (!parsed.entries || !Array.isArray(parsed.entries)) return;
        const data = migrateSelection(parsed);
        store.tagline = data.tagline || '';
        store.zoom = data.zoom || null;
        store.stepOverride = data.stepOverride ?? null;
        store.anchors = data.anchors || { baseline: 0, experiment: 0 };
        store.chartToggles = data.chartToggles || {};
        if (data.compare !== undefined) store.compare = data.compare;
        if (data.loadedFrom !== undefined) store.loadedFrom = data.loadedFrom;
        if (data.reportId !== undefined) store.reportId = data.reportId;
        if (data.savedAt !== undefined) store.savedAt = data.savedAt;
        if (data.sourceFilename !== undefined) store.sourceFilename = data.sourceFilename;
        if (data.fileChecksum !== undefined) store.fileChecksum = data.fileChecksum;
        if (data.timeRange !== undefined) store.timeRange = data.timeRange;
        if (data.rezolusVersion !== undefined) store.rezolusVersion = data.rezolusVersion;
        store.entries = entriesFromPayload(data.entries);
    } catch (e) {
        if (e?.message?.includes('unsupported selection schema')) {
            console.warn('[selection] dropped stale localStorage entry:', e.message);
            localStorage.removeItem(key);
        } else {
            console.warn('[selection] failed to restore:', e);
        }
    }
};

const persistReport = () => persistStore(REPORT_STORAGE_KEY, reportStore);
const persistNotebook = () => persistStore(NOTEBOOK_STORAGE_KEY, notebookStore);

// ── Anchors + per-chart toggles (compare-mode state) ─────────────

/**
 * Set a compare-mode anchor in milliseconds. Only the `baseline` and
 * `experiment` keys are recognized. Persists + triggers a redraw.
 */
const setAnchor = (captureId, ms) => {
    if (captureId !== CAPTURE_BASELINE && captureId !== CAPTURE_EXPERIMENT) return;
    if (!notebookStore.anchors) notebookStore.anchors = { baseline: 0, experiment: 0 };
    notebookStore.anchors[captureId] = Number(ms) || 0;
    persistNotebook();
    if (typeof m !== 'undefined' && typeof m.redraw === 'function') m.redraw();
};

// Per-chart toggle writer bound to a specific store. `persistFn` is
// optional (loadedSelectionStore is in-memory only).
const chartToggleSetter = (store, persistFn) => (chartId, key, value) => {
    if (!chartId || !key) return;
    if (!store.chartToggles) store.chartToggles = {};
    if (!store.chartToggles[chartId]) store.chartToggles[chartId] = {};
    store.chartToggles[chartId][key] = value;
    if (persistFn) persistFn();
    if (typeof m !== 'undefined' && typeof m.redraw === 'function') m.redraw();
};

const setChartToggle = chartToggleSetter(notebookStore, persistNotebook);
const setReportChartToggle = chartToggleSetter(reportStore, persistReport);
const setLoadedSelectionChartToggle = chartToggleSetter(loadedSelectionStore, null);

// Stores are restored when setStorageScope() is called with a file fingerprint,
// or eagerly here for the default (unscoped) keys as a fallback.
restoreStore(REPORT_STORAGE_KEY, reportStore);
restoreStore(NOTEBOOK_STORAGE_KEY, notebookStore);
restoreEvents();

// Build the displayed chart title for a selection card. The dashboard
// gives charts visual context via section/group breadcrumbs ("CPU >
// TLB Flush > Total Flushes"); the selection cards are flat-listed,
// so we restore that context inline. De-dups when group equals
// section (e.g. /scheduler has a single "Scheduler" group).
const selectionCardTitle = (entry, spec) => {
    const parts = [];
    if (entry.sectionName) parts.push(entry.sectionName);
    if (entry.groupName && entry.groupName !== entry.sectionName) parts.push(entry.groupName);
    parts.push(spec.opts.title);
    return parts.join(': ');
};

// Plain-text effective title (override wins), for non-rendering
// contexts like the unavailable-card placeholder.
const plainCardTitle = (entry, spec) =>
    (entry.titleOverride && entry.titleOverride.trim()) || selectionCardTitle(entry, spec);

// Card title node. In Report/LoadedSelection it's read-only: an
// override renders as inline Markdown, otherwise the derived
// breadcrumb. In the Notebook (`editable`) clicking it (or the ✎)
// swaps to a single-line input; blank clears the override and falls
// back to the derived title.
const cardTitle = (entry, spec, { editable = false } = {}) => {
    const derived = selectionCardTitle(entry, spec);
    const override = entry.titleOverride && entry.titleOverride.trim();

    if (editable && editingTitle.has(entry.id)) {
        return m('input.chart-title-edit', {
            // Prefill with the current effective title (override or
            // derived) so the user edits from it instead of a blank
            // box; persistence still treats empty as "use derived".
            value: entry.titleOverride || derived,
            placeholder: derived,
            oninput: (e) => { entry.titleOverride = e.target.value; persistNotebook(); },
            onblur: () => { editingTitle.delete(entry.id); m.redraw(); },
            onkeydown: (e) => { if (e.key === 'Enter') e.target.blur(); },
            oncreate: (vnode) => vnode.dom.focus(),
        });
    }

    const titleNode = override
        ? m('span.chart-title.md-inline', m.trust(renderMarkdownInline(override)))
        : m('span.chart-title', derived);

    if (!editable) return titleNode;

    const enterEdit = () => { editingTitle.add(entry.id); m.redraw(); };
    return m('span.chart-title-editable', { onclick: enterEdit, title: 'Click to edit title (Markdown)' }, [
        titleNode,
        m('button.chart-title-edit-btn', {
            type: 'button',
            'aria-label': 'Edit title',
            onclick: (e) => { e.stopPropagation(); enterEdit(); },
        }, '✎'),
    ]);
};

// Render a placeholder card for an entry whose chart can't render
// in this capture (metric absent, query errored, no series matched).
// Replaces the previous silent-collapse behavior where the chart's
// .no-data state hid the entire .chart-wrapper, leaving an empty
// gap that diverged from the loaded entry count.
const unavailableCard = (entry, spec, controls = null, dropAttrs = null) =>
    m('div.selection-card', dropAttrs || {}, [
        m('div.chart-wrapper.chart-wrapper-unavailable', [
            m('div.chart-unavailable', [
                m('div.chart-unavailable-title', plainCardTitle(entry, spec)),
                m('div.chart-unavailable-message',
                    spec.unavailableReason
                        ? `Failed to load: ${spec.unavailableReason}`
                        : 'No data available in this capture.'),
            ]),
            controls,
        ]),
    ]);

// ── Selection API (write mode) ───────────────────────────────────

const toggleSelection = (spec, sectionKey, sectionName, groupName, compareMeta = null) => {
    const idx = notebookStore.entries.findIndex(e => e.chartId === spec.opts.id);
    if (idx >= 0) {
        notebookStore.entries.splice(idx, 1);
        persistNotebook();
        return false;
    }
    notebookStore.entries.push({
        id: crypto.randomUUID(),
        chartId: spec.opts.id,
        section: sectionKey,
        sectionName,
        groupName: groupName || '',
        promql_query: spec.promql_query,
        // Bridge fields for category KPIs: without these, Notebook
        // re-renders only the baseline arm (experiment runs the
        // wrong query and returns no data).
        promql_query_experiment: spec.promql_query_experiment || null,
        category_members: compareMeta?.categoryMembers || null,
        note: '',
        chartOpts: JSON.parse(JSON.stringify(spec.opts)),
    });
    // Capture compare metadata on first pin in compare mode. Don't
    // overwrite later pins so the first author's intent wins (e.g.,
    // the user pinned a chart, attached a different experiment, then
    // pinned more — the original baseline_alias stands).
    if (compareMeta && !notebookStore.compare) {
        notebookStore.compare = {
            baseline_alias: compareMeta.baselineAlias || null,
            experiment_alias: compareMeta.experimentAlias || null,
        };
    }
    persistNotebook();
    return true;
};

const isSelected = (chartId) => notebookStore.entries.some(e => e.chartId === chartId);

const removeEntry = (store, id) => {
    const idx = store.entries.findIndex(e => e.id === id);
    if (idx >= 0) {
        store.entries.splice(idx, 1);
        if (store === notebookStore) {
            expandedNotes.delete(id);
            previewMarkdown.delete(id);
            editingTitle.delete(id);
            persistNotebook();
        } else if (store === reportStore) persistReport();
    }
};

// Swap entry at `id` with its neighbor. delta = -1 moves up, +1 down.
// No-op when the move would push past either end of the list.
const moveEntry = (store, id, delta) => {
    const idx = store.entries.findIndex(e => e.id === id);
    const target = idx + delta;
    if (idx < 0 || target < 0 || target >= store.entries.length) return;
    [store.entries[idx], store.entries[target]] = [store.entries[target], store.entries[idx]];
    if (store === notebookStore) persistNotebook();
    else if (store === reportStore) persistReport();
};

// Move `draggedId` to land immediately before `targetId`. Powers the
// drag-and-drop reorder; both directions collapse to "insert before"
// because the splice-then-insert sequence keeps `toIdx` pointing at
// the right slot whether we moved up or down.
const reorderEntry = (store, draggedId, targetId) => {
    if (!draggedId || draggedId === targetId) return;
    const fromIdx = store.entries.findIndex(e => e.id === draggedId);
    const toIdx = store.entries.findIndex(e => e.id === targetId);
    if (fromIdx < 0 || toIdx < 0) return;
    const [item] = store.entries.splice(fromIdx, 1);
    store.entries.splice(toIdx, 0, item);
    if (store === notebookStore) persistNotebook();
    else if (store === reportStore) persistReport();
};

// Module-level drag session state. HTML5 DnD's dataTransfer would also
// work but requires roundtripping through string serialization; a
// single-page drag has no need for that.
let _draggedEntryId = null;
let _dragOverEntryId = null;

// In-memory only — leaves localStorage alone. Used during scope
// re-binding where the next step is restoring from the scoped key.
const resetStoreState = (store) => {
    store.entries.length = 0;
    store.tagline = '';
    store.zoom = null;
    store.stepOverride = null;
    store.anchors = { baseline: 0, experiment: 0 };
    store.chartToggles = {};
    store.compare = null;
    if (store === reportStore) {
        store.loadedFrom = null;
        store.reportId = null;
        store.savedAt = null;
        store.sourceFilename = null;
        store.fileChecksum = null;
        store.timeRange = null;
        store.rezolusVersion = null;
    }
    if (store === loadedSelectionStore) {
        store.loadedFrom = null;
    }
};

// Full purge: in-memory + localStorage. Used by the "Clear All" UI.
const clearStore = (store) => {
    resetStoreState(store);
    if (store === reportStore) {
        localStorage.removeItem(REPORT_STORAGE_KEY);
    } else if (store === notebookStore) {
        expandedNotes.clear();
        previewMarkdown.clear();
        editingTitle.clear();
        localStorage.removeItem(NOTEBOOK_STORAGE_KEY);
        // Events are Notebook-scoped content — clear them alongside it.
        suspendEventsPersist = true;
        eventsStore.clear();
        suspendEventsPersist = false;
        eventsRestoredFromStorage = false;
        localStorage.removeItem(EVENTS_STORAGE_KEY);
    }
    // loadedSelectionStore is in-memory only; no localStorage to clear
};

// Copy a source store (Report or LoadedSelection) into the live
// Notebook with overwrite-confirm. Shallow-spread on entries with
// fresh ids — chartOpts is shared by reference with the source,
// which is safe because the viewer reads chartOpts but never
// mutates it. Switch to structuredClone if that ever changes.
const openInNotebook = (sourceStore, kindLabel) => {
    if (notebookStore.entries.length > 0) {
        if (!confirm(`Notebook has unsaved entries. Discard them and load the ${kindLabel}?`)) return;
    }
    resetStoreState(notebookStore);
    notebookStore.tagline = sourceStore.tagline || '';
    notebookStore.anchors = { ...(sourceStore.anchors || { baseline: 0, experiment: 0 }) };
    notebookStore.chartToggles = { ...(sourceStore.chartToggles || {}) };
    notebookStore.compare = sourceStore.compare ? { ...sourceStore.compare } : null;
    notebookStore.entries = sourceStore.entries.map(e => ({ ...e, id: crypto.randomUUID() }));
    expandedNotes.clear();
    previewMarkdown.clear();
    editingTitle.clear();
    persistNotebook();
    notify('info', `${kindLabel} opened in Notebook`);
    m.route.set('/notebook');
};

const openReportInNotebook = () => openInNotebook(reportStore, 'Report');
const openLoadedSelectionInNotebook = () => openInNotebook(loadedSelectionStore, 'Selection');

// ── Export / Import / Parquet ─────────────────────────────────────

const buildPayload = (store, attrs, { includeNotes = true } = {}) => ({
    version: SELECTION_SCHEMA_VERSION,
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
    step_override: attrs.stepOverride ?? null,
    anchors: store.anchors || { baseline: 0, experiment: 0 },
    chartToggles: store.chartToggles || {},
    compare: store.compare || undefined,
    tagline: store.tagline,
    entries: store.entries.map(e => ({
        chartId: e.chartId,
        section: e.section,
        sectionName: e.sectionName,
        groupName: e.groupName || '',
        promql_query: e.promql_query,
        promql_query_experiment: e.promql_query_experiment || undefined,
        category_members: e.category_members || undefined,
        note: includeNotes ? e.note : '',
        titleOverride: e.titleOverride || undefined,
        chartOpts: e.chartOpts,
    })),
    events: eventsStore.all(),
});

const exportJSON = async (store, attrs) => {
    const defaultPrefix = (attrs.filename || 'rezolus-capture').replace(/\.parquet$/, '') + '-selection';
    const result = await showSaveModal(defaultPrefix, '.json');
    if (!result) return;
    const filename = result.filename;
    const payload = buildPayload(store, attrs, { includeNotes: false });
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
    try {
        const migrated = migrateSelection(payload);
        store.tagline = migrated.tagline || '';
        store.zoom = migrated.zoom || null;
        store.stepOverride = migrated.step_override ?? migrated.stepOverride ?? null;
        store.anchors = migrated.anchors || { baseline: 0, experiment: 0 };
        store.chartToggles = migrated.chartToggles || {};
        store.compare = migrated.compare || null;
        store.entries = entriesFromPayload(payload.entries);
        if (store === reportStore) {
            store.reportId = payload.report_id || null;
            store.savedAt = payload.saved_at || null;
            store.sourceFilename = payload.filename || null;
            store.fileChecksum = payload.file_checksum || null;
            store.timeRange = payload.time_range || null;
            store.rezolusVersion = payload.rezolus_version || null;
            persistReport();
        }
        return true;
    } catch (e) {
        notify('error', `Cannot load: ${e.message}`);
        return false;
    }
};

const importSelection = () => {
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
                notify('error', 'Not a valid Rezolus selection');
                return;
            }
            if (!payload.entries || !Array.isArray(payload.entries)) {
                notify('error', 'Not a valid Rezolus selection');
                return;
            }
            const ok = loadJsonIntoSelection(payload, file.name);
            if (!ok) return;
            notify('info', `Loaded selection (${loadedSelectionStore.entries.length} charts)`);
            if (m.route.get() === '/selection') {
                LoadedSelectionView._needsReload = true;
                m.redraw();
            } else {
                m.route.set('/selection');
            }
        } catch (e) {
            notify('error', 'Failed to import selection');
            console.error('[selection] failed to import JSON:', e);
        }
    };
    input.click();
};

const loadJsonIntoSelection = (json, filename) => {
    try {
        const parsed = typeof json === 'string' ? JSON.parse(json) : json;
        const migrated = migrateSelection(parsed);
        resetStoreState(loadedSelectionStore);
        loadedSelectionStore.tagline = migrated.tagline || '';
        loadedSelectionStore.anchors = migrated.anchors || { baseline: 0, experiment: 0 };
        loadedSelectionStore.chartToggles = migrated.chartToggles || {};
        loadedSelectionStore.compare = migrated.compare || null;
        loadedSelectionStore.entries = entriesFromPayload(migrated.entries);
        loadedSelectionStore.loadedFrom = filename;
        return true;
    } catch (e) {
        notify('error', `Cannot load Selection: ${e.message}`);
        return false;
    }
};

const saveToParquet = async (store, attrs) => {
    const defaultPrefix = (attrs.filename || 'rezolus-capture').replace(/\.parquet$/, '') + '-report';
    const cs = attrs.chartsState;
    const hasZoom = cs && !cs.isDefaultZoom();
    const checkboxes = [
        { key: 'trim_columns', label: 'Trim columns to charts in this report', checked: true },
    ];
    if (hasZoom) {
        checkboxes.push({ key: 'trim', label: 'Trim to selected time range', checked: false });
    }
    const result = await showSaveModal(defaultPrefix, '.parquet', checkboxes);
    if (!result) return;
    const filename = result.filename;
    const trimToSelection = result.trim;
    const payload = buildPayload(store, attrs);
    payload.trim_columns = result.trim_columns !== false;

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
        // Transport-agnostic: the server adapter POSTs to
        // /api/v1/save_with_selection; the WASM adapter projects the
        // source bytes locally. Both return { bytes, mime, extension }.
        const { bytes, mime, extension } = await ViewerApi.saveWithSelection(payload);
        // If the user accepted the default `.parquet` filename, swap the
        // extension to whatever the adapter reported (AB tarballs come
        // back with `.parquet.ab.tar`).
        const finalName = extension && filename.endsWith('.parquet') && extension !== '.parquet'
            ? filename.slice(0, -'.parquet'.length) + extension
            : filename;
        const blob = new Blob([bytes], { type: mime });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = finalName;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        URL.revokeObjectURL(url);
        notify('info', `Saved ${finalName}`);
    } catch (e) {
        notify('error', `Failed to save parquet: ${e?.message ?? e}`);
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
            // Always set a spec so the view can render a placeholder card
            // for entries whose query won't run or returns nothing in this
            // capture — silent collapse confused users (the original entry
            // count and visible cards diverged with no explanation).
            const spec = {
                opts: { ...entry.chartOpts },
                promql_query: entry.promql_query,
                // Bridge: per-side experiment query for category KPIs.
                promql_query_experiment: entry.promql_query_experiment || undefined,
                data: [],
            };
            if (!entry.promql_query) {
                spec.unavailable = true;
                this.specs.set(entry.chartId, spec);
                return;
            }
            try {
                // Histogram metrics need histogram_quantiles / heatmap
                // wrapping; the dashboard pipeline does this via
                // buildEffectiveQuery, but the stored entry only has
                // the raw metric name.
                const effective = buildEffectiveQuery(spec) || entry.promql_query;
                const result = await executePromQLRangeQuery(effective);
                if (result) {
                    applyResultToPlot(spec, result);
                }
                const hasData = result?.status === 'success' && result?.data?.result?.length > 0;
                if (!hasData) spec.unavailable = true;
            } catch (e) {
                console.warn('[selection] failed to load chart:', entry.chartId, e);
                spec.unavailable = true;
                spec.unavailableReason = e.message;
            }
            this.specs.set(entry.chartId, spec);
        });
        await Promise.all(promises);
        // Surface the gap between "loaded N entries" and "visible cards" once
        // per data-load. component._lastUnavailableNotice debounces re-mounts;
        // it's reset by _checkReload when fresh data lands.
        const unavailable = [...this.specs.values()].filter(s => s.unavailable).length;
        const noticeKey = `${this.specs.size}/${unavailable}`;
        if (unavailable > 0 && component._lastUnavailableNotice !== noticeKey) {
            component._lastUnavailableNotice = noticeKey;
            notify('warn',
                `${unavailable} of ${this.specs.size} pinned ${this.specs.size === 1 ? 'chart has' : 'charts have'} no data in this capture`);
        }
        this.loading = false;
        m.redraw();
    },

    _checkReload() {
        if (component._needsReload) {
            component._needsReload = false;
            component._lastUnavailableNotice = null;  // re-arm the notify
            this.specs = new Map();
            this.loading = true;
            this._zoomApplied = false;
            this._loadCharts();
        }
    },

    _applyZoom(attrs) {
        if (!this._zoomApplied && !this.loading && store.zoom && attrs.chartsState) {
            this._zoomApplied = true;
            // Single-writer path: setZoom updates state and fans out
            // to every chart's zoom subscriber. No local forEach.
            attrs.chartsState.setZoom(store.zoom, { source: 'global' });
        }
    },
});

// ── NotebookView (write mode) ────────────────────────────────────

const NotebookView = {
    _needsReload: false,
};
Object.assign(NotebookView, chartLoaderMixin(notebookStore, NotebookView), {
    view({ attrs }) {
        this._checkReload();
        this._applyZoom(attrs);

        const interval = attrs.interval || 1;
        const cs = attrs.chartsState;
        const hasChartSelection = cs?.hasActiveSelection();
        const hasHistograms = notebookStore.entries.some(e => isHistogramPlot(e));
        const hasAnyNote = notebookStore.entries.some(e => e.note && e.note.length > 0);
        const inTwoFileCompare = attrs.compareMode && !attrs.combinedAB;
        const downloadIcon = m.trust('<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 11v2a1 1 0 001 1h10a1 1 0 001-1v-2"/><path d="M8 10V2m0 8l-3-3m3 3l3-3"/></svg>');

        const header = m('div.selection-header', [
            m('div.section-header-row', [
                m('h1.section-title', [
                    sectionTitlePin(),
                    attrs.title || 'Notebook',
                ]),
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
            markdownField({
                key: PREAMBLE_KEY,
                cls: 'selection-preamble',
                label: 'Overview',
                value: notebookStore.tagline,
                placeholder: 'Add an overview / preamble (Markdown supported)\u2026',
                onInput: (v) => { notebookStore.tagline = v; persistNotebook(); },
            }),
        ]);

        if (this.loading) {
            return m('div#section-content.selection-section', [header, m('p', 'Loading charts\u2026')]);
        }

        return m('div#section-content.selection-section', [
            header,

            m('div.selection-actions', [
                m('button.selection-btn', {
                    disabled: inTwoFileCompare,
                    title: inTwoFileCompare
                        ? 'Two-file A/B mode has no single parquet to embed in. Use `parquet combine --ab` first.'
                        : 'Embed selection + notes in the loaded parquet',
                    onclick: () => saveToParquet(notebookStore, attrs),
                }, [
                    'Save as Report (parquet, Selection & Notes) ',
                    downloadIcon,
                ]),
                m('button.selection-btn.selection-btn-indigo', {
                    disabled: hasAnyNote,
                    title: hasAnyNote
                        ? 'Selection has notes \u2014 use Save as Report (parquet, Selection & Notes) to keep them with the data, or clear notes first.'
                        : 'Download a JSON pattern (charts + toggles only)',
                    onclick: () => exportJSON(notebookStore, attrs),
                }, [
                    'Save as Selection (JSON, no Notes) ',
                    downloadIcon,
                ]),
                m('button.selection-btn.selection-btn-danger', {
                    disabled: !hasAnyNote,
                    title: hasAnyNote
                        ? 'Clear notes from all pinned charts (charts and toggles preserved)'
                        : 'No notes to clear',
                    onclick: () => {
                        if (!confirm('Clear notes from all pinned charts? This cannot be undone.')) return;
                        notebookStore.entries.forEach(e => { e.note = ''; });
                        expandedNotes.clear();
                        previewMarkdown.clear();
                        persistNotebook();
                        m.redraw();
                    },
                }, 'Clear Notes'),
                m('button.selection-btn.selection-btn-danger', {
                    onclick: () => { clearStore(notebookStore); m.redraw(); },
                }, 'Clear All'),
            ]),

            notebookStore.entries.map((entry, idx, arr) => {
                const spec = this.specs.get(entry.chartId);
                if (!spec) return null;
                const cardControls = m('div.selection-card-controls', [
                    // Only the handle is draggable; clicks/drags inside
                    // the chart body and notes textarea behave normally.
                    m('span.selection-card-drag-handle', {
                        draggable: true,
                        title: 'Drag to reorder',
                        ondragstart: (e) => {
                            _draggedEntryId = entry.id;
                            e.dataTransfer.effectAllowed = 'move';
                            // Some browsers refuse to start a drag when
                            // dataTransfer is empty.
                            try { e.dataTransfer.setData('text/plain', entry.id); } catch (_) {}
                        },
                        ondragend: () => {
                            _draggedEntryId = null;
                            _dragOverEntryId = null;
                            m.redraw();
                        },
                    }, '\u22ee\u22ee'),
                    m('button.selection-card-move', {
                        onclick: () => { moveEntry(notebookStore, entry.id, -1); m.redraw(); },
                        title: 'Move up',
                        disabled: idx === 0,
                    }, '\u2191'),
                    m('button.selection-card-move', {
                        onclick: () => { moveEntry(notebookStore, entry.id, 1); m.redraw(); },
                        title: 'Move down',
                        disabled: idx === arr.length - 1,
                    }, '\u2193'),
                    m('button.selection-card-remove', {
                        onclick: () => { removeEntry(notebookStore, entry.id); m.redraw(); },
                        title: 'Remove from Notebook',
                    }, '\u00d7'),
                ]);
                // Drop handlers live on the card root so the entire
                // card surface is a drop target \u2014 only the handle
                // initiates a drag.
                const dropAttrs = {
                    ondragover: (e) => {
                        if (!_draggedEntryId || _draggedEntryId === entry.id) return;
                        e.preventDefault(); // marks this card as a valid drop target
                        e.dataTransfer.dropEffect = 'move';
                        if (_dragOverEntryId !== entry.id) {
                            _dragOverEntryId = entry.id;
                            m.redraw();
                        }
                    },
                    ondragleave: () => {
                        if (_dragOverEntryId === entry.id) {
                            _dragOverEntryId = null;
                            m.redraw();
                        }
                    },
                    ondrop: (e) => {
                        e.preventDefault();
                        reorderEntry(notebookStore, _draggedEntryId, entry.id);
                        _draggedEntryId = null;
                        _dragOverEntryId = null;
                        m.redraw();
                    },
                    class: _dragOverEntryId === entry.id ? 'drag-over' : null,
                };
                if (spec.unavailable) {
                    return unavailableCard(entry, spec, cardControls, dropAttrs);
                }
                // In compare mode, render through CompareChartWrapper so
                // pinned charts mirror the live A/B view (side-by-side,
                // diff, etc.). Service-template charts that lack a
                // promql_query fall through to the single-capture path.
                const captureLabels = {
                    baseline: attrs.baselineAlias || 'baseline',
                    experiment: attrs.experimentAlias || 'experiment',
                };
                // Use the entry's home route so buildEffectiveQuery's
                // /service/* gate (skip node injection) still applies
                // — otherwise the baseline's hostname gets injected
                // into bridge queries and returns zero matches.
                const originalSectionRoute = entry.section ? `/${entry.section}` : '/notebook';
                const chartBody = (attrs.compareMode && spec.promql_query)
                    ? m(CompareChartWrapper, {
                        spec,
                        chartsState: attrs.chartsState,
                        interval,
                        anchors: attrs.anchors,
                        toggles: attrs.toggles,
                        setChartToggle: attrs.setChartToggle,
                        sectionRoute: originalSectionRoute,
                        step: interval,
                        experimentQueryRange: attrs.experimentQueryRange,
                        captureLabels,
                        // Stored on the pinned entry so the bridge
                        // survives even though /notebook has no
                        // category_members metadata of its own.
                        categoryMembers: entry.category_members,
                    })
                    : m(Chart, { spec, chartsState: attrs.chartsState, interval });
                return m('div.selection-card', dropAttrs, [
                    m('div.chart-wrapper', [
                        m('div.chart-header', [
                            m('div.chart-title-row', [
                                cardTitle(entry, spec, { editable: true }),
                                attrs.compareMode && compareToggle(spec, {
                                    compareMode: attrs.compareMode,
                                    toggles: attrs.toggles,
                                    setChartToggle: attrs.setChartToggle,
                                }),
                            ]),
                            spec.opts.description && m('span.chart-subtitle', spec.opts.description),
                        ]),
                        chartBody,
                        cardControls,
                    ]),
                    (() => {
                        const hasNote = entry.note && entry.note.length > 0;
                        const expanded = expandedNotes.has(entry.id);
                        if (!hasNote && !expanded) {
                            return m('button.notes-affordance', {
                                onclick: () => {
                                    // Edit is the default mode, so just
                                    // surfacing the field is enough.
                                    expandedNotes.add(entry.id);
                                    m.redraw();
                                },
                            }, '+ Add note');
                        }
                        return markdownField({
                            key: entry.id,
                            cls: 'selection-card-notes',
                            label: 'Notes',
                            value: entry.note,
                            placeholder: 'Add notes (Markdown supported)\u2026',
                            onInput: (v) => { entry.note = v; persistNotebook(); },
                            onEmptyBlur: () => {
                                expandedNotes.delete(entry.id);
                                previewMarkdown.delete(entry.id);
                                m.redraw();
                            },
                        });
                    })(),
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
                    reportStore.entries.length > 0 && m('button.section-action-btn', {
                        onclick: openReportInNotebook,
                        title: 'Copy this Report into the Notebook for editing',
                    }, 'OPEN IN NOTEBOOK'),
                ]),
            ]),
            reportStore.tagline && m('div.selection-preamble-text.md-rendered', m.trust(renderMarkdown(reportStore.tagline))),
        ]);

        if (this.loading) {
            return m('div#section-content.selection-section', [header, m('p', 'Loading charts\u2026')]);
        }

        return m('div#section-content.selection-section', [
            header,

            reportStore.entries.map((entry) => {
                const spec = this.specs.get(entry.chartId);
                if (!spec) return null;
                if (spec.unavailable) return unavailableCard(entry, spec);
                const captureLabels = {
                    baseline: attrs.baselineAlias || 'baseline',
                    experiment: attrs.experimentAlias || 'experiment',
                };
                const originalSectionRoute = entry.section ? `/${entry.section}` : '/report';
                const chartBody = (attrs.compareMode && spec.promql_query)
                    ? m(CompareChartWrapper, {
                        spec,
                        chartsState: attrs.chartsState,
                        interval,
                        anchors: reportStore.anchors,
                        toggles: reportStore.chartToggles,
                        setChartToggle: setReportChartToggle,
                        sectionRoute: originalSectionRoute,
                        step: interval,
                        experimentQueryRange: attrs.experimentQueryRange,
                        captureLabels,
                        categoryMembers: entry.category_members,
                    })
                    : m(Chart, { spec, chartsState: attrs.chartsState, interval });
                return m('div.selection-card', [
                    m('div.chart-wrapper', [
                        m('div.chart-header', [
                            m('div.chart-title-row', [
                                cardTitle(entry, spec),
                                attrs.compareMode && compareToggle(spec, {
                                    compareMode: attrs.compareMode,
                                    toggles: reportStore.chartToggles,
                                    setChartToggle: setReportChartToggle,
                                }),
                            ]),
                            spec.opts.description && m('span.chart-subtitle', spec.opts.description),
                        ]),
                        chartBody,
                    ]),
                    entry.note && m('div.report-card-notes', [
                        m('label.report-notes-label', 'Notes'),
                        m('div.report-notes-text.md-rendered', m.trust(renderMarkdown(entry.note))),
                    ]),
                ]);
            }),
        ]);
    },
});

// ── LoadedSelectionView (read-only mode for dropped JSON) ──────

const LoadedSelectionView = {
    _needsReload: false,
};
Object.assign(LoadedSelectionView, chartLoaderMixin(loadedSelectionStore, LoadedSelectionView), {
    view({ attrs }) {
        this._checkReload();
        this._applyZoom(attrs);

        const interval = attrs.interval || 1;

        const header = m('div.selection-header', [
            m('div.section-header-row', [
                m('h1.section-title', attrs.title || 'Selection'),
                m('div.section-actions', [
                    loadedSelectionStore.entries.length > 0 && m('button.section-action-btn', {
                        onclick: openLoadedSelectionInNotebook,
                        title: 'Copy this Selection into the Notebook for editing',
                    }, 'OPEN IN NOTEBOOK'),
                ]),
            ]),
            loadedSelectionStore.loadedFrom && m('p.selection-source',
                `Loaded from: ${loadedSelectionStore.loadedFrom}`),
            loadedSelectionStore.tagline && m('div.selection-preamble-text.md-rendered', m.trust(renderMarkdown(loadedSelectionStore.tagline))),
        ]);

        if (this.loading) {
            return m('div#section-content.selection-section', [header, m('p', 'Loading charts\u2026')]);
        }

        return m('div#section-content.selection-section', [
            header,

            loadedSelectionStore.entries.map((entry) => {
                const spec = this.specs.get(entry.chartId);
                if (!spec) return null;
                if (spec.unavailable) return unavailableCard(entry, spec);
                const captureLabels = {
                    baseline: attrs.baselineAlias || 'baseline',
                    experiment: attrs.experimentAlias || 'experiment',
                };
                const originalSectionRoute = entry.section ? `/${entry.section}` : '/selection';
                const chartBody = (attrs.compareMode && spec.promql_query)
                    ? m(CompareChartWrapper, {
                        spec,
                        chartsState: attrs.chartsState,
                        interval,
                        anchors: loadedSelectionStore.anchors,
                        toggles: loadedSelectionStore.chartToggles,
                        setChartToggle: setLoadedSelectionChartToggle,
                        sectionRoute: originalSectionRoute,
                        step: interval,
                        experimentQueryRange: attrs.experimentQueryRange,
                        captureLabels,
                        categoryMembers: entry.category_members,
                    })
                    : m(Chart, { spec, chartsState: attrs.chartsState, interval });
                return m('div.selection-card', [
                    m('div.chart-wrapper', [
                        m('div.chart-header', [
                            m('div.chart-title-row', [
                                cardTitle(entry, spec),
                                attrs.compareMode && compareToggle(spec, {
                                    compareMode: attrs.compareMode,
                                    toggles: loadedSelectionStore.chartToggles,
                                    setChartToggle: setLoadedSelectionChartToggle,
                                }),
                            ]),
                            spec.opts.description && m('span.chart-subtitle', spec.opts.description),
                        ]),
                        chartBody,
                    ]),
                ]);
            }),
        ]);
    },
});

export {
    notebookStore,
    reportStore,
    loadedSelectionStore,
    setStorageScope,
    persistNotebook,
    toggleSelection,
    isSelected,
    clearStore,
    importSelection,
    loadPayloadIntoStore,
    loadJsonIntoSelection,
    setAnchor,
    setChartToggle,
    NotebookView,
    ReportView,
    LoadedSelectionView,
    migrateSelection,
    SELECTION_SCHEMA_VERSION,
};
