// Site viewer data wrapper.
// Reuses src viewer data pipeline and overrides only transport/runtime behavior.

import { createDataApi } from '../../../src/viewer/assets/lib/data.js';
import { ViewerApi } from './viewer_api.js';

const siteDataApi = createDataApi({
    getMetadata: () => ViewerApi.getMetadata(),
    queryRange: (query, start, end, step) => ViewerApi.queryRange(query, start, end, step),
    // Keep site behavior: suppress rejected histogram heatmap fetch logs.
    logHeatmapErrors: false,
});

const {
    executePromQLRangeQuery,
    applyResultToPlot,
    fetchHeatmapForPlot,
    fetchHeatmapsForGroups,
    substituteCgroupPattern,
    processDashboardData,
} = siteDataApi;

export {
    executePromQLRangeQuery,
    applyResultToPlot,
    fetchHeatmapForPlot,
    fetchHeatmapsForGroups,
    substituteCgroupPattern,
    processDashboardData,
};
