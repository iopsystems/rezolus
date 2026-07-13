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

    async detachExperiment() {
        ensureRegistry();
        registry.detach('experiment');
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
        return JSON.parse(json);
    },

    async getSections(captureId = 'baseline') {
        ensureAttached(captureId);
        const json = registry.get_sections(captureId);
        return json ? JSON.parse(json) : [];
    },

    async queryRange(query, start, end, step, captureId = 'baseline') {
        ensureAttached(captureId);
        return JSON.parse(registry.query_range(captureId, query, start, end, step));
    },

    // Display-mode range query. The WASM backend returns the SAME compact binary
    // body as the server (both via dashboard::display_wire), so the shared
    // frontend decodes it identically. In-process, so there's no network to
    // abort — `signal`/`captureId` opts match the server adapter's shape but the
    // registry call is synchronous. A non-series result throws, so the caller
    // falls back to the JSON query path exactly like the server adapter.
    async queryRangeDisplay(query, start, end, step, { points = 500, band = null, captureId = 'baseline' } = {}) {
        ensureAttached(captureId);
        const bytes = registry.query_range_display(captureId, query, start, end, step, points, band || '');
        // wasm-bindgen hands back a fresh Uint8Array; hand up a plain ArrayBuffer
        // so decodeDisplayBinary / decodeHeatmapBinary can view it zero-copy.
        return { buffer: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) };
    },

    async getInfo(captureId = 'baseline') {
        ensureAttached(captureId);
        return JSON.parse(registry.info(captureId));
    },

    initTemplates(templatesJson, captureId = 'baseline') {
        ensureAttached(captureId);
        registry.init_templates(captureId, templatesJson);
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
        const hasExperiment = registry.has('experiment');
        return {
            bytes,
            mime: hasExperiment ? 'application/x-tar' : 'application/octet-stream',
            extension: hasExperiment ? '.parquet.ab.tar' : '.parquet',
        };
    },
};

export { ViewerApi };
