// Shared viewer components and helpers used by both the server-backed
// viewer (script.js) and the static site viewer (site script.js).

import { Chart } from './charts/chart.js';
import { expandLink, selectButton, compareToggle } from './chart_controls.js';
import { isHistogramPlot, buildHistogramHeatmapSpec, resolvedStyle } from './charts/metric_types.js';
import { renderCompareChart } from './charts/compare.js';
import {
    queryRangeForCapture, buildEffectiveQuery,
    promqlResultToHeatmapTriples, promqlResultToLinePair, promqlResultToSeriesMap,
    CAPTURE_BASELINE, CAPTURE_EXPERIMENT,
} from './data.js';
import { canonicalQuantileLabel } from './charts/util/compare_math.js';
import { ViewerApi } from './viewer_api.js';

// ── Normalization helpers for compare-mode captures ────────────────

// Convert the baseline plot spec's already-populated data into a
// capture-shaped object keyed by `id: 'baseline'`. The shape depends on
// chart style so the compare strategies can consume it uniformly.
const extractBaselineCapture = (spec) => {
    const style = resolvedStyle(spec);
    const cap = { id: CAPTURE_BASELINE };

    if (style === 'line') {
        const data = spec.data;
        if (Array.isArray(data) && data.length >= 2) {
            cap.timeData = data[0] || [];
            cap.valueData = data[1] || [];
        } else {
            cap.timeData = [];
            cap.valueData = [];
        }
        return cap;
    }

    if (style === 'multi' || style === 'scatter') {
        const data = spec.data;
        const names = spec.series_names || [];
        const map = new Map();
        if (Array.isArray(data) && data.length >= 2) {
            const timeData = data[0] || [];
            for (let i = 1; i < data.length; i++) {
                let label = names[i - 1];
                if (style === 'scatter') {
                    const canonical = canonicalQuantileLabel(label);
                    if (canonical) label = canonical;
                }
                if (label == null) continue;
                map.set(String(label), { timeData, valueData: data[i] || [] });
            }
        }
        cap.seriesMap = map;
        return cap;
    }

    if (style === 'heatmap' || style === 'histogram_heatmap') {
        cap.timeData = spec.time_data || [];
        cap.heatmapData = spec.data || [];
        cap.bucketBounds = spec.bucket_bounds;
        cap.heatmapMatrix = heatmapTriplesToMatrix(cap.heatmapData, cap.timeData.length);
        const scanned = heatmapTriplesMinMax(cap.heatmapData);
        cap.minValue = scanned.min != null ? scanned.min : spec.min_value;
        cap.maxValue = scanned.max != null ? scanned.max : spec.max_value;
        return cap;
    }

    return cap;
};

// Convert an experiment PromQL range result (same JSON shape as the
// baseline got through applyResultToPlot) into the same capture shape
// produced by extractBaselineCapture.
const extractExperimentCapture = (spec, promqlResult) => {
    const style = resolvedStyle(spec);
    const cap = { id: CAPTURE_EXPERIMENT };
    const results = promqlResult?.data?.result;
    if (!Array.isArray(results) || results.length === 0) {
        if (style === 'multi' || style === 'scatter') cap.seriesMap = new Map();
        else if (style === 'heatmap' || style === 'histogram_heatmap') {
            cap.timeData = []; cap.heatmapData = []; cap.heatmapMatrix = [];
        } else {
            cap.timeData = []; cap.valueData = [];
        }
        return cap;
    }

    if (style === 'line') {
        const pair = promqlResultToLinePair(results);
        cap.timeData = pair.timeData;
        cap.valueData = pair.valueData;
        return cap;
    }

    if (style === 'multi') {
        // Match baseline's label convention: first non-__name__ metric
        // label's value (see applyResultToPlot's series-name loop).
        cap.seriesMap = promqlResultToSeriesMap(results, (item) => {
            const mm = item.metric || {};
            for (const [k, v] of Object.entries(mm)) {
                if (k !== '__name__') return String(v);
            }
            return null;
        });
        return cap;
    }

    if (style === 'scatter') {
        cap.seriesMap = promqlResultToSeriesMap(results, (item) => canonicalQuantileLabel(item));
        return cap;
    }

    if (style === 'heatmap') {
        const { timestamps, triples, minValue, maxValue } = promqlResultToHeatmapTriples(results);
        cap.timeData = timestamps.map(Number);
        cap.heatmapData = triples;
        cap.heatmapMatrix = heatmapTriplesToMatrix(triples, cap.timeData.length);
        cap.minValue = minValue;
        cap.maxValue = maxValue;
        return cap;
    }

    // histogram_heatmap — handled via a separate endpoint in the real
    // app; for compare mode we best-effort pass the raw PromQL result,
    // understanding the side-by-side strategy may degrade to no-data if
    // the shape doesn't match. Full support is follow-up work.
    cap.timeData = [];
    cap.heatmapData = [];
    cap.heatmapMatrix = [];
    return cap;
};

// Scan a flat [timeIdx, y, value] triple array for the numeric value
// bounds. Returns { min: null, max: null } when there are no numeric
// samples. Used once at extract time so unifiedHeatmapRange can just
// Math.min/Math.max the two pre-computed pairs.
function heatmapTriplesMinMax(triples) {
    if (!Array.isArray(triples) || triples.length === 0) return { min: null, max: null };
    let lo = Infinity;
    let hi = -Infinity;
    for (const t of triples) {
        const v = Array.isArray(t) ? t[2] : null;
        if (v == null || Number.isNaN(v)) continue;
        if (v < lo) lo = v;
        if (v > hi) hi = v;
    }
    if (!Number.isFinite(lo) || !Number.isFinite(hi)) return { min: null, max: null };
    return { min: lo, max: hi };
}

// Build a rows × bins matrix from a flat [timeIdx, y, value] triple
// array. Gaps fill with null. Used by the diff-heatmap strategy.
function heatmapTriplesToMatrix(triples, binCount) {
    if (!Array.isArray(triples) || triples.length === 0) return [];
    let maxY = -1;
    for (const t of triples) {
        const y = Number(t?.[1]);
        if (Number.isFinite(y) && y > maxY) maxY = y;
    }
    if (maxY < 0) return [];
    const rows = maxY + 1;
    const cols = Math.max(1, binCount || 0);
    const matrix = Array.from({ length: rows }, () =>
        new Array(cols).fill(null));
    for (const [ti, y, v] of triples) {
        const r = Number(y);
        const c = Number(ti);
        if (!Number.isFinite(r) || !Number.isFinite(c)) continue;
        if (r < 0 || r >= rows || c < 0 || c >= cols) continue;
        matrix[r][c] = (v === null || v === undefined) ? null : Number(v);
    }
    return matrix;
}

/**
 * Mithril component that fetches experiment data asynchronously and
 * then delegates rendering to the compare adapter. Renders a loading
 * placeholder until the experiment fetch resolves; an error
 * placeholder on fetch failure. When the fetch succeeds but the
 * strategy returns `false` (unsupported / not enough data), falls
 * back to rendering baseline-only.
 */
const CompareChartWrapper = {
    oninit(vnode) {
        const { spec, sectionRoute } = vnode.attrs;
        vnode.state.experimentResult = null;
        vnode.state.error = null;
        if (!spec.promql_query) {
            vnode.state.error = 'no PromQL query';
            return;
        }
        // Apply the same transforms the baseline path applies (histogram
        // wrap, counter rewrite, cgroup substitution), but deliberately
        // SKIP node/instance label injection: those labels are tied to the
        // baseline's topology and would return zero matches on the
        // experiment in the common case where the two recordings have
        // different hostnames or instance IDs.
        const query = buildEffectiveQuery(spec, {
            sectionRoute,
            crossCapture: true,
        });
        if (query == null) {
            vnode.state.error = 'compare: query skipped (unresolved cgroup pattern)';
            return;
        }
        // Query the experiment over its own time range. The compare-mode
        // bootstrap cached {start, end, step} at attach time; fall back
        // to a one-off metadata fetch only if the cache is missing
        // (e.g. legacy entry paths that don't thread it through).
        (async () => {
            try {
                let range = vnode.attrs.experimentQueryRange;
                if (!range) {
                    try {
                        const meta = await ViewerApi.getMetadata(CAPTURE_EXPERIMENT);
                        const data = meta?.data ?? meta;
                        const minT = data?.minTime ?? data?.min_time ?? data?.start_time;
                        const maxT = data?.maxTime ?? data?.max_time ?? data?.end_time;
                        if (minT != null && maxT != null) {
                            const start = Number(minT);
                            const end = Number(maxT);
                            if (Number.isFinite(start) && Number.isFinite(end) && end > start) {
                                range = { start, end, step: Math.max(1, Math.floor((end - start) / 500)) };
                            }
                        }
                    } catch (_) { /* best effort */ }
                }
                if (!range) {
                    vnode.state.error = 'experiment metadata missing time range';
                    m.redraw();
                    return;
                }
                const step = vnode.attrs.step && vnode.attrs.step > 0 ? vnode.attrs.step : range.step;
                const res = await queryRangeForCapture(CAPTURE_EXPERIMENT, query, range.start, range.end, step);
                vnode.state.experimentResult = res;
                m.redraw();
            } catch (e) {
                vnode.state.error = e?.message || String(e);
                m.redraw();
            }
        })();
    },

    view(vnode) {
        const { spec, chartsState, interval, anchors, toggles, setChartToggle } = vnode.attrs;

        if (vnode.state.error) {
            return m('div.chart-error', `compare error: ${vnode.state.error}`);
        }
        if (!vnode.state.experimentResult) {
            return m('div.chart-loading', 'Loading experiment\u2026');
        }

        // Memoize both captures on vnode.state keyed by spec.data /
        // experimentResult identity. Each extractor walks the raw data
        // (heatmap extract does an O(rows×bins) normalize), and nothing
        // else mutates spec.data or experimentResult between redraws —
        // tooltip/hover/zoom just re-run view() with the same inputs.
        if (vnode.state._capData !== spec.data) {
            vnode.state._capData = spec.data;
            vnode.state._baselineCap = extractBaselineCapture(spec);
        }
        if (vnode.state._capExpResult !== vnode.state.experimentResult) {
            vnode.state._capExpResult = vnode.state.experimentResult;
            vnode.state._experimentCap = extractExperimentCapture(spec, vnode.state.experimentResult);
        }
        const baselineCap = vnode.state._baselineCap;
        const experimentCap = vnode.state._experimentCap;

        const result = renderCompareChart({
            spec,
            captures: [baselineCap, experimentCap],
            anchors: anchors || { baseline: 0, experiment: 0 },
            toggles: toggles || {},
            setChartToggle,
            chartsState,
            interval,
            Chart,
        });

        switch (result && result.kind) {
            case 'spec':
                return m(Chart, { spec: result.spec, chartsState, interval });
            case 'vnode':
                return result.vnode;
            case 'split': {
                const specs = result.specs;
                if (!specs || specs.length === 0) {
                    return m('div.chart-error', 'compare: no shared labels between captures');
                }
                return m('div.compare-split-subgroup',
                    specs.map((s) => m('div.chart-wrapper', [
                        s._splitLabel && m('div.compare-split-label', s._splitLabel),
                        m(Chart, { spec: s, chartsState, interval }),
                    ])));
            }
            case 'fallback':
            default:
                return m(Chart, { spec, chartsState, interval });
        }
    },
};

/**
 * Factory for the Group component.
 *
 * @param {Function} getState - Returns { chartsState, heatmapEnabled,
 *     heatmapLoading, heatmapDataCache, compareMode?, toggles?,
 *     setChartToggle?, anchors? } with current values.
 */
export function createGroupComponent(getState) {
    return {
        view({ attrs }) {
            const state = getState();
            const {
                chartsState, heatmapEnabled, heatmapLoading, heatmapDataCache,
                compareMode, toggles, setChartToggle, anchors, experimentQueryRange,
            } = state;
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

            const chartHeader = (opts, spec) => m('div.chart-header', [
                m('div.chart-title-row', [
                    m('span.chart-title', opts.title),
                    compareMode && spec && compareToggle(spec, {
                        compareMode, toggles, setChartToggle,
                    }),
                ]),
                opts.description && m('span.chart-subtitle', opts.description),
            ]);

            const noCollapse = attrs.noCollapse || attrs.metadata?.no_collapse;

            // Compat shim: if the incoming JSON still uses the legacy
            // `plots` shape (an array directly on the group), promote it
            // to a single unnamed subgroup so rendering stays uniform.
            const subgroups = attrs.subgroups
                ? attrs.subgroups
                : [{ name: null, description: null, plots: attrs.plots || [] }];

            // Whether a plot has any series data to show. Used to suppress
            // the group title + subgroup headers when the whole cluster is
            // empty (e.g. a section querying metrics that don't exist on
            // the host, like GPU on a CPU-only box).
            const plotHasData = (plot) =>
                Array.isArray(plot.data) && plot.data.some((series) =>
                    Array.isArray(series) && series.length > 0
                );
            const subgroupHasData = (sg) => (sg.plots || []).some(plotHasData);
            const groupHasData = subgroups.some(subgroupHasData);

            if (!groupHasData) return null;

            // Build the chart-body vnode for a given (maybe-prefixed) spec.
            // Picks the compare wrapper when in compare mode AND the spec has
            // a promql_query (service-template charts without one fall through
            // to the single-capture path even in compare mode).
            const chartBody = (renderSpec, sourceSpec) => (compareMode && sourceSpec.promql_query)
                ? m(CompareChartWrapper, {
                    spec: renderSpec,
                    chartsState,
                    interval,
                    anchors,
                    toggles,
                    setChartToggle,
                    sectionRoute,
                    step: interval,
                    experimentQueryRange,
                })
                : m(Chart, { spec: renderSpec, chartsState, interval });

            const renderChart = (spec) => {
                const isHistogramChart = isHistogramPlot(spec);
                // In compare mode, every chart takes the full chart-grid
                // row. Single line overlays benefit from the wider x-axis
                // to distinguish blue/green traces; split multi/scatter
                // and side-by-side heatmaps need the room structurally.
                const wrapperClass = (spec.width === 'full' || compareMode)
                    ? 'div.chart-wrapper.full-width'
                    : 'div.chart-wrapper';

                let renderSpec;
                if (isHistogramChart && isHeatmapMode && sectionHeatmapData?.has(spec.opts.id)) {
                    renderSpec = buildHistogramHeatmapSpec(spec, sectionHeatmapData.get(spec.opts.id), prefixTitle(spec.opts));
                } else {
                    renderSpec = { ...spec, opts: prefixTitle(spec.opts), noCollapse };
                }

                return m(wrapperClass, [
                    chartHeader(renderSpec.opts, renderSpec),
                    chartBody(renderSpec, spec),
                    expandLink(spec, sectionRoute),
                    selectButton(spec, sectionRoute, sectionName),
                ]);
            };

            return m(
                'div.group',
                { id: attrs.id },
                [
                    m('h2', `${attrs.name}`),
                    subgroups.map((sg) => {
                        const hasData = subgroupHasData(sg);
                        return m('div.subgroup', [
                            hasData && sg.name && m('h3.subgroup-title', sg.name),
                            hasData && sg.description && m('p.subgroup-description', sg.description),
                            m('div.charts', (sg.plots || []).map(renderChart)),
                        ]);
                    }),
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
export function buildClientOnlySectionView(Main, sectionResponseCache, activeSection, getCompareMode) {
    return {
        view() {
            const anyCached = Object.values(sectionResponseCache)[0];
            const sections = anyCached?.sections || [];
            return m(Main, {
                activeSection,
                groups: [],
                sections,
                compareMode: typeof getCompareMode === 'function' ? getCompareMode() : false,
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

