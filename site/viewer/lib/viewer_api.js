// WASM API adapter for site/viewer frontend.
// Mirrors the server viewer's transport layer; queries run via WASM.

let registry = null;

const ensureRegistry = () => {
    if (!registry) throw new Error('No parquet file loaded');
};

const ensureAttached = (captureId) => {
    ensureRegistry();
    if (!registry.has(captureId)) {
        throw new Error(`capture '${captureId}' not attached`);
    }
};

// A template carried in a recording's metadata that did not parse falls back
// to the built-in one; the server viewer logs that to its terminal, and the
// browser logs it to the console. `init_templates` returns the capture's
// notices on every call, and a capture's templates are initialised more than
// once in a session (attach, compare, re-attach), so each distinct notice is
// logged once.
const loggedTemplateNotices = new Set();
const logTemplateNotices = (noticesJson) => {
    try {
        for (const notice of JSON.parse(noticesJson || '[]')) {
            if (loggedTemplateNotices.has(notice)) continue;
            loggedTemplateNotices.add(notice);
            console.warn(notice);
        }
    } catch (_) {
        // Notices are advisory; a malformed list is not worth failing a load.
    }
};

// A built-in section the dashboard template could not expand arrives as an
// empty view whose `metadata.template_error` says why; each distinct message
// is logged once. Mirrored in the server copy.
const loggedSectionErrors = new Set();
const logSectionTemplateError = (view) => {
    const message = view && view.metadata && view.metadata.template_error;
    if (typeof message !== 'string' || loggedSectionErrors.has(message)) return;
    loggedSectionErrors.add(message);
    console.warn(message);
};

const ViewerApi = {
    setRegistry(instance) {
        registry = instance;
    },

    registry() {
        return registry;
    },

    async attachBaseline(data, filename) {
        ensureRegistry();
        registry.attach('baseline', data, filename);
    },

    async attachExperiment(file) {
        ensureRegistry();
        const data = new Uint8Array(await file.arrayBuffer());
        registry.attach('experiment', data, file.name || 'experiment.parquet');
    },

    // Attach an experiment capture from raw bytes (no File wrapper). Used
    // by the demo URL flow to feed pre-fetched parquet buffers into the
    // WASM registry.
    async attachExperimentBytes(bytes, filename) {
        ensureRegistry();
        registry.attach('experiment', bytes, filename || 'experiment.parquet');
    },

    // Whether a capture slot currently holds anything. The registry fills
    // BOTH slots on its own when an attached `.rez` holds two recordings, so
    // the caller cannot know from what it passed in.
    hasCapture(captureId) {
        return !!registry && registry.has(captureId);
    },

    // Anything the last attach wants said out loud — currently only "this
    // archive holds more recordings than are being shown". Returns an array of
    // strings, empty when there is nothing to report.
    attachNotices() {
        if (!registry || typeof registry.notices !== 'function') return [];
        try {
            const parsed = JSON.parse(registry.notices());
            return Array.isArray(parsed) ? parsed : [];
        } catch (_) {
            return [];
        }
    },

    async detachExperiment() {
        ensureRegistry();
        registry.detach('experiment');
    },

    // The attached captures, anchor first: `[{ id, alias }, ...]`. Mirrors the
    // server copy's `/api/v1/captures`; the WASM registry answers directly.
    async getCaptures() {
        if (!registry || typeof registry.captures !== 'function') return [];
        try {
            const parsed = JSON.parse(registry.captures());
            return Array.isArray(parsed) ? parsed : [];
        } catch (_) {
            return [];
        }
    },

    async getMetadata(captureId = 'baseline') {
        ensureAttached(captureId);
        const response = JSON.parse(registry.metadata(captureId));
        if (response.status !== 'success') {
            throw new Error('Failed to get metadata');
        }
        return response;
    },

    async getSystemInfo(captureId = 'baseline') {
        ensureAttached(captureId);
        const sysinfo = registry.systeminfo(captureId);
        return sysinfo ? JSON.parse(sysinfo) : null;
    },

    async getSelection(captureId = 'baseline') {
        ensureAttached(captureId);
        const selection = registry.selection(captureId);
        return selection ? JSON.parse(selection) : null;
    },

    async getFileMetadata(captureId = 'baseline') {
        ensureAttached(captureId);
        const json = registry.file_metadata_json(captureId);
        return json ? JSON.parse(json) : {};
    },

    async getSection(section, background = false, captureId = 'baseline') {
        ensureAttached(captureId);
        const json = registry.get_section(captureId, section);
        if (!json) throw new Error(`Unknown section: ${section}`);
        const view = JSON.parse(json);
        logSectionTemplateError(view);
        return view;
    },

    async getSections(captureId = 'baseline') {
        ensureAttached(captureId);
        const json = registry.get_sections(captureId);
        return json ? JSON.parse(json) : [];
    },

    // `signal` is accepted (unused — in-process) to keep positional parity with
    // the server adapter so data.js can call both the same way.
    async queryRange(query, start, end, step, captureId = 'baseline', signal = undefined, rateMode = 'grid') {
        ensureAttached(captureId);
        return JSON.parse(registry.query_range(captureId, query, start, end, step, rateMode || 'grid'));
    },

    // Display-mode range query. The WASM backend returns the SAME compact binary
    // body as the server (both via dashboard::display_wire), so the shared
    // frontend decodes it identically. In-process, so there's no network to
    // abort — `signal`/`captureId` opts match the server adapter's shape but the
    // registry call is synchronous. A non-series result throws, so the caller
    // falls back to the JSON query path exactly like the server adapter.
    async queryRangeDisplay(query, start, end, step, { points = 500, band = null, captureId = 'baseline', rateMode = 'grid' } = {}) {
        ensureAttached(captureId);
        const bytes = registry.query_range_display(captureId, query, start, end, step, points, band || '', rateMode || 'grid');
        // wasm-bindgen hands back a fresh Uint8Array; hand up a plain ArrayBuffer
        // so decodeDisplayBinary / decodeHeatmapBinary can view it zero-copy.
        return { buffer: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) };
    },

    async getInfo(captureId = 'baseline') {
        ensureAttached(captureId);
        return JSON.parse(registry.info(captureId));
    },

    async getMetrics(source = null, captureId = 'baseline') {
        ensureAttached(captureId);
        return JSON.parse(registry.metrics(captureId, source));
    },

    async getTimestamps(source = null, captureId = 'baseline') {
        ensureAttached(captureId);
        return JSON.parse(registry.timestamps(captureId, source));
    },

    initTemplates(templatesJson, captureId = 'baseline') {
        ensureAttached(captureId);
        logTemplateNotices(registry.init_templates(captureId, templatesJson));
    },

    regenerateCombined(templatesJson, categoryName) {
        ensureRegistry();
        registry.regenerate_combined(
            templatesJson,
            categoryName ?? undefined,
        );
    },

    // Mirror of the server's POST /api/v1/save_with_selection. The
    // WASM registry projects the source bytes locally and returns
    // either a parquet (single capture) or a `*.parquet.ab.tar`
    // (compare mode). The Blob's MIME is best-effort; the file
    // extension is the load-bearing signal for the browser download.
    async saveWithSelection(payload) {
        ensureAttached('baseline');
        const bytes = registry.save_with_selection(JSON.stringify(payload));
        // The registry decides the output shape: `.dendro` for a dendro
        // source, `.rez` for a `.rez` source or a parquet compare, `.parquet`
        // for a single parquet.
        const extension = registry.report_extension();
        return { bytes, mime: 'application/octet-stream', extension };
    },
};

export { ViewerApi };
