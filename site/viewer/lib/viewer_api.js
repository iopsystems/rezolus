// WASM API adapter for site/viewer frontend.
// Defines a transport abstraction that mirrors src/viewer backend adapter.

import { generateSectionData } from './dashboards.js';

let viewer = null;
let viewerInfo = null;

const ensureViewer = () => {
    if (!viewer) throw new Error('No parquet file loaded');
};

const ViewerApi = {
    setViewer(instance) {
        viewer = instance;
    },

    setViewerInfo(info) {
        viewerInfo = info;
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

    async getSection(section) {
        if (!viewerInfo) throw new Error('Viewer info not initialized');
        const data = generateSectionData(section, viewerInfo);
        if (!data) throw new Error(`Unknown section: ${section}`);
        return data;
    },

    async queryRange(query, start, end, step) {
        ensureViewer();
        return JSON.parse(viewer.query_range(query, start, end, step));
    },
};

export { ViewerApi };
