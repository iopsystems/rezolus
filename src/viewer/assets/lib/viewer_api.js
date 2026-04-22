// Backend API adapter for src/viewer frontend.
// Defines a transport abstraction that mirrors the site/viewer WASM adapter.

const MAX_UPLOAD_BYTES = 50 * 1024 * 1024; // 50 MB — must match server DefaultBodyLimit

const formatMB = (bytes) => (bytes / (1024 * 1024)).toFixed(1);

const backendRequest = (opts) => m.request({
    withCredentials: true,
    ...opts,
});

const sectionUrl = (section) => `/data/${section}.json`;

const ViewerApi = {
    async getMode() {
        return backendRequest({ method: 'GET', url: '/api/v1/mode' });
    },

    async getMetadata(captureId = 'baseline') {
        const q = captureId === 'experiment' ? '?capture=experiment' : '';
        return backendRequest({ method: 'GET', url: `/api/v1/metadata${q}` });
    },

    async getSystemInfo(captureId = 'baseline') {
        const q = captureId === 'experiment' ? '?capture=experiment' : '';
        return backendRequest({ method: 'GET', url: `/api/v1/systeminfo${q}` });
    },

    async getSelection() {
        return backendRequest({ method: 'GET', url: '/api/v1/selection' });
    },

    async getFileMetadata(captureId = 'baseline') {
        const q = captureId === 'experiment' ? '?capture=experiment' : '';
        return backendRequest({ method: 'GET', url: `/api/v1/file_metadata${q}` });
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

    saveUrl() {
        return '/api/v1/save';
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

};

export { ViewerApi };
