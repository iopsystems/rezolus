// WASM API adapter for site/viewer frontend.
// Mirrors the server viewer's transport layer — the only difference
// is that queries run via WASM instead of HTTP.

let viewer = null;

const ensureViewer = () => {
    if (!viewer) throw new Error('No parquet file loaded');
};

const ViewerApi = {
    setViewer(instance) {
        viewer = instance;
    },

    async getMetadata() {
        ensureViewer();
        const response = JSON.parse(viewer.metadata());
        if (response.status !== 'success') {
            throw new Error('Failed to get metadata');
        }
        return response;
    },

    async getSystemInfo() {
        ensureViewer();
        const sysinfo = viewer.systeminfo();
        return sysinfo ? JSON.parse(sysinfo) : null;
    },

    async getSelection() {
        ensureViewer();
        const selection = viewer.selection();
        return selection ? JSON.parse(selection) : null;
    },

    async getFileMetadata() {
        ensureViewer();
        if (typeof viewer.file_metadata_json === 'function') {
            return JSON.parse(viewer.file_metadata_json());
        }
        return {};
    },

    async getSection(section) {
        ensureViewer();
        const json = viewer.get_section(section);
        if (!json) throw new Error(`Unknown section: ${section}`);
        return JSON.parse(json);
    },

    async getSections() {
        ensureViewer();
        return JSON.parse(viewer.get_sections());
    },

    async queryRange(query, start, end, step) {
        ensureViewer();
        return JSON.parse(viewer.query_range(query, start, end, step));
    },
};

export { ViewerApi };
