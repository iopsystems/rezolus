// Site viewer data wrapper.
// Re-exports the shared data pipeline from data_base.js (src/viewer/assets/lib/data.js).

export {
    executePromQLRangeQuery,
    applyResultToPlot,
    fetchHeatmapForPlot,
    fetchHeatmapsForGroups,
    substituteCgroupPattern,
    processDashboardData,
} from './data_base.js';
