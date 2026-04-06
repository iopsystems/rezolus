// Dashboard definitions — auto-generated from src/viewer/dashboard/*.rs
// Do not edit manually. Run `cargo xtask generate-dashboards` to update.

const cache = {};

async function loadDashboard(sectionKey) {
    if (cache[sectionKey]) return cache[sectionKey];
    const resp = await fetch(`dashboards/${sectionKey}.json`);
    if (!resp.ok) throw new Error(`Failed to load dashboard: ${sectionKey}`);
    cache[sectionKey] = await resp.json();
    return cache[sectionKey];
}

/**
 * Generate a section's View-compatible data structure.
 * Loads pre-generated JSON and merges in runtime viewer info.
 */
export async function generateSectionData(sectionKey, viewerInfo) {
    const dashboard = await loadDashboard(sectionKey);

    return {
        ...dashboard,
        // Override runtime fields from viewerInfo
        interval: viewerInfo.interval,
        source: viewerInfo.source,
        version: viewerInfo.version,
        filename: viewerInfo.filename,
        start_time: viewerInfo.minTime * 1000,
        end_time: viewerInfo.maxTime * 1000,
        num_series: (viewerInfo.counter_names?.length || 0) +
                    (viewerInfo.gauge_names?.length || 0) +
                    (viewerInfo.histogram_names?.length || 0),
    };
}
