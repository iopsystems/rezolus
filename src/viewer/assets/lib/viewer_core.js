// Shared viewer components and helpers used by both the server-backed
// viewer (script.js) and the static site viewer (site script.js).

import { Chart } from './charts/chart.js';
import { expandLink, selectButton } from './chart_controls.js';
import { isHistogramPlot, buildHistogramHeatmapSpec } from './charts/metric_types.js';
/**
 * Factory for the Group component.
 *
 * @param {Function} getState - Returns { chartsState, heatmapEnabled,
 *     heatmapLoading, heatmapDataCache } with current values.
 */
export function createGroupComponent(getState) {
    return {
        view({ attrs }) {
            const { chartsState, heatmapEnabled, heatmapLoading, heatmapDataCache } = getState();
            const sectionRoute = attrs.sectionRoute;
            const sectionName = attrs.sectionName;
            const interval = attrs.interval;
            const sectionHeatmapData = heatmapDataCache.get(sectionRoute);
            const isHeatmapMode = heatmapEnabled && !heatmapLoading;

            const isOverview = sectionRoute === '/overview';
            const titlePrefix = isOverview ? attrs.name : sectionName;
            const prefixTitle = (opts) => titlePrefix
                ? { ...opts, title: `${titlePrefix}: ${opts.title}` }
                : opts;

            const chartHeader = (opts) => m('div.chart-header', [
                m('span.chart-title', opts.title),
                opts.description && m('span.chart-subtitle', opts.description),
            ]);

            return m(
                'div.group',
                { id: attrs.id },
                [
                    m('h2', `${attrs.name}`),
                    m(
                        'div.charts',
                        attrs.plots.map((spec) => {
                            const isHistogramChart = isHistogramPlot(spec);

                            if (isHistogramChart && isHeatmapMode && sectionHeatmapData?.has(spec.opts.id)) {
                                const heatmapData = sectionHeatmapData.get(spec.opts.id);
                                const heatmapSpec = buildHistogramHeatmapSpec(spec, heatmapData, prefixTitle(spec.opts));
                                return m('div.chart-wrapper', [
                                    chartHeader(heatmapSpec.opts),
                                    m(Chart, { spec: heatmapSpec, chartsState, interval }),
                                    expandLink(spec, sectionRoute),
                                    selectButton(spec, sectionRoute, sectionName),
                                ]);
                            }

                            const prefixedSpec = { ...spec, opts: prefixTitle(spec.opts), noCollapse: attrs.noCollapse };
                            return m('div.chart-wrapper', [
                                chartHeader(prefixedSpec.opts),
                                m(Chart, { spec: prefixedSpec, chartsState, interval }),
                                expandLink(spec, sectionRoute),
                                selectButton(spec, sectionRoute, sectionName),
                            ]);
                        }),
                    ),
                ],
            );
        },
    };
}

/**
 * Extract interval/version/source from any cached section response.
 */
export function getCachedSectionMeta(sectionResponseCache, interval) {
    const anyCached = Object.values(sectionResponseCache)[0];
    return {
        interval: anyCached?.interval || interval,
        version: anyCached?.version,
        source: anyCached?.source,
        filename: anyCached?.filename,
        start_time: anyCached?.start_time,
        end_time: anyCached?.end_time,
    };
}

/**
 * Build a Mithril component for a client-only section (System Info,
 * Metadata, Selection, Report) that has no backend data of its own.
 */
export function buildClientOnlySectionView(Main, sectionResponseCache, activeSection) {
    return {
        view() {
            const anyCached = Object.values(sectionResponseCache)[0];
            const sections = anyCached?.sections || [];
            return m(Main, {
                activeSection,
                groups: [],
                sections,
                source: anyCached?.source,
                version: anyCached?.version,
                filename: anyCached?.filename,
                interval: anyCached?.interval,
                filesize: anyCached?.filesize,
                start_time: anyCached?.start_time,
                end_time: anyCached?.end_time,
                num_series: anyCached?.num_series,
            });
        },
    };
}

