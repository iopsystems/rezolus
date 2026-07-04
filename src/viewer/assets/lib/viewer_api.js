// Backend API adapter for src/viewer frontend.
// Transport abstraction mirrors the site/viewer WASM adapter — keep the
// two in sync.

const MAX_UPLOAD_BYTES = 50 * 1024 * 1024; // 50 MB — must match server DefaultBodyLimit

const formatMB = (bytes) => (bytes / (1024 * 1024)).toFixed(1);

const backendRequest = (opts) => m.request({
    withCredentials: true,
    ...opts,
});

const sectionUrl = (section) => `/data/${section}.json`;

// Return '?capture=<id>' for non-baseline captures, '' otherwise. The
// backend treats the absence of the param as "baseline".
const captureQS = (captureId) =>
    (captureId && captureId !== 'baseline') ? `?capture=${captureId}` : '';

const ViewerApi = {
    async getMode() {
        return backendRequest({ method: 'GET', url: '/api/v1/mode' });
    },

    async getMetadata(captureId = 'baseline') {
        return backendRequest({ method: 'GET', url: `/api/v1/metadata${captureQS(captureId)}` });
    },

    async getSystemInfo(captureId = 'baseline') {
        return backendRequest({ method: 'GET', url: `/api/v1/systeminfo${captureQS(captureId)}` });
    },

    async getSelection() {
        return backendRequest({ method: 'GET', url: '/api/v1/selection' });
    },

    async getFileMetadata(captureId = 'baseline') {
        return backendRequest({ method: 'GET', url: `/api/v1/file_metadata${captureQS(captureId)}` });
    },

    async reset() {
        return backendRequest({ method: 'POST', url: '/api/v1/reset', background: true });
    },

    async uploadParquet(file) {
        if (file.size > MAX_UPLOAD_BYTES) {
            throw new Error(
                `File is too large (${formatMB(file.size)} MB). Maximum upload size is ${formatMB(MAX_UPLOAD_BYTES)} MB.`,
            );
        }
        const data = await file.arrayBuffer();
        try {
            return await backendRequest({
                method: 'POST',
                url: '/api/v1/upload',
                body: data,
                serialize: (v) => v,
                headers: {
                    'Content-Type': 'application/octet-stream',
                    'x-rezolus-filename': file.name || 'upload.parquet',
                },
            });
        } catch (e) {
            if (e.code === 413) {
                throw new Error(
                    `File is too large (${formatMB(file.size)} MB). Maximum upload size is ${formatMB(MAX_UPLOAD_BYTES)} MB.`,
                );
            }
            throw e;
        }
    },

    async connectAgent(url) {
        return backendRequest({
            method: 'POST',
            url: '/api/v1/connect',
            body: url,
            serialize: (v) => v,
            headers: {
                'Content-Type': 'text/plain',
            },
        });
    },

    /// Ask the local server to fetch + ingest a remote parquet in one
    /// hop. The bytes never traverse the browser. The server validates
    /// the URL against its --proxy-allow list and 403s if disallowed.
    async loadFromUrl(url, filename = null) {
        return backendRequest({
            method: 'POST',
            url: '/api/v1/load_url',
            body: { url, filename },
            headers: {
                'Content-Type': 'application/json',
            },
        });
    },

    saveUrl() {
        return '/api/v1/save';
    },

    async getSections(captureId = 'baseline') {
        return backendRequest({
            method: 'GET',
            url: `/api/v1/sections${captureQS(captureId)}`,
        });
    },

    async getSection(section, background = false) {
        return backendRequest({
            method: 'GET',
            url: sectionUrl(section),
            background,
        });
    },

    async queryRange(query, start, end, step, captureId = 'baseline') {
        const params = new URLSearchParams({
            query,
            start: String(start),
            end: String(end),
            step: String(step),
        });
        if (captureId && captureId !== 'baseline') {
            params.set('capture', captureId);
        }
        return backendRequest({
            method: 'GET',
            url: `/api/v1/query_range?${params.toString()}`,
            background: true,
        });
    },

    async getMetrics(source = null, captureId = 'baseline') {
        const params = new URLSearchParams();
        if (source) params.set('source', source);
        if (captureId && captureId !== 'baseline') {
            params.set('capture', captureId);
        }
        const qs = params.toString();
        return backendRequest({
            method: 'GET',
            url: `/api/v1/metrics${qs ? `?${qs}` : ''}`,
        });
    },

    async attachExperiment(file) {
        if (file.size > MAX_UPLOAD_BYTES) {
            throw new Error(
                `File is too large (${formatMB(file.size)} MB). Maximum upload size is ${formatMB(MAX_UPLOAD_BYTES)} MB.`,
            );
        }
        const data = await file.arrayBuffer();
        try {
            return await backendRequest({
                method: 'POST',
                url: '/api/v1/captures/experiment',
                body: data,
                serialize: (v) => v,
                headers: {
                    'Content-Type': 'application/octet-stream',
                    'x-rezolus-filename': file.name || 'experiment.parquet',
                },
            });
        } catch (e) {
            if (e.code === 413) {
                throw new Error(
                    `File is too large (${formatMB(file.size)} MB). Maximum upload size is ${formatMB(MAX_UPLOAD_BYTES)} MB.`,
                );
            }
            throw e;
        }
    },

    async detachExperiment() {
        return backendRequest({ method: 'DELETE', url: '/api/v1/captures/experiment' });
    },

    // POST the selection payload and stream the resulting parquet (or
    // *.parquet.ab.tar in compare mode) back as bytes. Mirrors the
    // WASM adapter's `saveWithSelection` shape so callers can stay
    // transport-agnostic. Server's Content-Type / Content-Disposition
    // tell us which extension to use on the download.
    async saveWithSelection(payload) {
        const resp = await fetch('/api/v1/save_with_selection', {
            method: 'POST',
            credentials: 'include',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
        });
        if (!resp.ok) {
            const detail = await resp.text().catch(() => '');
            throw new Error(`save failed (HTTP ${resp.status})${detail ? `: ${detail}` : ''}`);
        }
        const mime = resp.headers.get('content-type') || 'application/octet-stream';
        const extension = mime.includes('x-tar') ? '.parquet.ab.tar' : '.parquet';
        const bytes = new Uint8Array(await resp.arrayBuffer());
        return { bytes, mime, extension };
    },

};

export { ViewerApi };
