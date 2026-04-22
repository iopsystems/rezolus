// Shared viewer components and helpers used by both the server-backed
// viewer (script.js) and the static site viewer (site script.js).

import { Chart } from './charts/chart.js';
import { expandLink, selectButton, compareToggle } from './chart_controls.js';
import { isHistogramPlot, buildHistogramHeatmapSpec } from './charts/metric_types.js';
import { renderCompareChart, BASELINE_COLOR } from './charts/compare.js';
import { queryRangeForCapture, buildEffectiveQuery } from './data.js';
import { ViewerApi } from './viewer_api.js';

// ── Normalization helpers for compare-mode captures ────────────────

// Convert the baseline plot spec's already-populated data into a
// capture-shaped object keyed by `id: 'baseline'`. The shape depends on
// chart style so the compare strategies can consume it uniformly.
const extractBaselineCapture = (spec) => {
    const style = spec.opts?.style || spec._resolvedStyle;
    const cap = { id: 'baseline' };

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
                    // Normalize pXX label form to match quantile-derived keys
                    // from the experiment query (percentileLabel in compare.js).
                    // PromQL's histogram_percentiles() returns quantile as a
                    // fraction (e.g. "0.5", "0.99"); a value <= 1 is treated
                    // as a fraction, anything larger is assumed already in
                    // percent form (e.g. "50").
                    const m = typeof label === 'string' ? label.match(/^p?(\d+(?:\.\d+)?)$/) : null;
                    if (m) {
                        const q = Number(m[1]);
                        const pct = q <= 1 ? q * 100 : q;
                        label = `p${pct.toFixed(2).replace(/\.?0+$/, '')}`;
                    }
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
        cap.minValue = spec.min_value;
        cap.maxValue = spec.max_value;
        cap.heatmapMatrix = heatmapTriplesToMatrix(cap.heatmapData, cap.timeData.length);
        return cap;
    }

    return cap;
};

// Convert an experiment PromQL range result (same JSON shape as the
// baseline got through applyResultToPlot) into the same capture shape
// produced by extractBaselineCapture.
const extractExperimentCapture = (spec, promqlResult) => {
    const style = spec.opts?.style || spec._resolvedStyle;
    const cap = { id: 'experiment' };
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
        const first = results[0];
        const values = Array.isArray(first?.values) ? first.values : [];
        cap.timeData = values.map((pair) => Number(pair[0]));
        cap.valueData = values.map((pair) => {
            const v = pair[1];
            if (v === null || v === undefined) return null;
            const n = Number(v);
            return Number.isNaN(n) ? null : n;
        });
        return cap;
    }

    if (style === 'multi') {
        // Match baseline's label convention: series_names[i] is the first
        // non-__name__ metric label's value (see applyResultToPlot's
        // allData branch in data.js). For a gauge like cpu_usage with
        // labels {cpu="0"}, both sides will produce "0".
        const map = new Map();
        for (const item of results) {
            const mm = item.metric || {};
            let key = null;
            for (const [k, v] of Object.entries(mm)) {
                if (k !== '__name__') { key = String(v); break; }
            }
            if (key == null) continue;
            const values = Array.isArray(item.values) ? item.values : [];
            map.set(key, {
                timeData: values.map((p) => Number(p[0])),
                valueData: values.map((p) => {
                    const v = p[1];
                    if (v === null || v === undefined) return null;
                    const n = Number(v);
                    return Number.isNaN(n) ? null : n;
                }),
            });
        }
        cap.seriesMap = map;
        return cap;
    }

    if (style === 'scatter') {
        const map = new Map();
        for (const item of results) {
            const q = Number(item.metric && item.metric.quantile);
            if (!Number.isFinite(q)) continue;
            // Always present as percent form ("p50", "p99.9") to match
            // the baseline normalizer in extractBaselineCapture.
            const pct = q <= 1 ? q * 100 : q;
            const canonical = `p${pct.toFixed(2).replace(/\.?0+$/, '')}`;
            const values = Array.isArray(item.values) ? item.values : [];
            map.set(canonical, {
                timeData: values.map((p) => Number(p[0])),
                valueData: values.map((p) => {
                    const v = p[1];
                    if (v === null || v === undefined) return null;
                    const n = Number(v);
                    return Number.isNaN(n) ? null : n;
                }),
            });
        }
        cap.seriesMap = map;
        return cap;
    }

    if (style === 'heatmap') {
        // Build a flat-triple table + 2D matrix from the PromQL result,
        // using the same transform applyResultToPlot uses for baseline.
        const timeSet = new Set();
        for (const item of results) {
            for (const [ts] of item.values || []) timeSet.add(ts);
        }
        const timestamps = Array.from(timeSet).sort((a, b) => Number(a) - Number(b));
        const timeIndex = new Map();
        timestamps.forEach((ts, idx) => timeIndex.set(ts, idx));
        const heatmapData = [];
        for (let idx = 0; idx < results.length; idx++) {
            const item = results[idx];
            let cpuId = idx;
            if (item.metric && item.metric.id != null) {
                const parsed = parseInt(item.metric.id, 10);
                if (!Number.isNaN(parsed)) cpuId = parsed;
            }
            for (const [ts, val] of item.values || []) {
                const ti = timeIndex.get(ts);
                if (ti === undefined) continue;
                const v = val === null || val === undefined ? null : parseFloat(val);
                heatmapData.push([ti, cpuId, v]);
            }
        }
        cap.timeData = timestamps.map(Number);
        cap.heatmapData = heatmapData;
        cap.heatmapMatrix = heatmapTriplesToMatrix(heatmapData, cap.timeData.length);
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
            injectNodeLabel: false,
            injectInstanceLabel: false,
        });
        if (query == null) {
            vnode.state.error = 'compare: query skipped (unresolved cgroup pattern)';
            return;
        }
        // Query the experiment over its own time range. `getFileMetadata`
        // returns start/end/duration for the specified capture; fall
        // back to a wide default when unavailable.
        (async () => {
            try {
                let start = 0;
                let end = 0;
                let step = vnode.attrs.step || 1;
                // /api/v1/metadata returns minTime/maxTime in SECONDS (PromQL
                // native format, same as plot.data[0] timestamps). Do not
                // divide by 1000.
                try {
                    const meta = await ViewerApi.getMetadata('experiment');
                    const data = meta?.data ?? meta;
                    const minT = data?.minTime ?? data?.min_time ?? data?.start_time;
                    const maxT = data?.maxTime ?? data?.max_time ?? data?.end_time;
                    if (minT != null && maxT != null) {
                        start = Number(minT);
                        end = Number(maxT);
                        const dur = Math.max(1, end - start);
                        if (!step || step <= 0) step = Math.max(1, Math.floor(dur / 500));
                    }
                } catch (_) { /* best effort; fall through to request anyway */ }
                if (end <= start) {
                    vnode.state.error = 'experiment metadata missing time range';
                    m.redraw();
                    return;
                }
                vnode.state.experimentQuery = query;
                vnode.state.experimentWindow = { start, end, step };
                const res = await queryRangeForCapture('experiment', query, start, end, step);
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

        const baselineCap = extractBaselineCapture(spec);
        const experimentCap = extractExperimentCapture(spec, vnode.state.experimentResult);

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

        if (result === false || result == null) {
            // Fall through to baseline-only rendering.
            return m(Chart, { spec, chartsState, interval });
        }
        // `_splitSpecs` marker: render each sub-spec as its own Chart in
        // a grid wrapper so a multi/scatter split lays out cleanly.
        if (result._splitSpecs) {
            const specs = result._splitSpecs;
            if (!specs || specs.length === 0) {
                const bKeys = [...(extractBaselineCapture(spec).seriesMap?.keys() || [])];
                const eKeys = [...(extractExperimentCapture(spec, vnode.state.experimentResult).seriesMap?.keys() || [])];
                const resultsLen = vnode.state.experimentResult?.data?.result?.length ?? 'n/a';
                const win = vnode.state.experimentWindow;
                return m('div.chart-error', [
                    m('div', 'compare: no shared labels between captures'),
                    m('div', { style: 'font-size:11px;opacity:0.7;margin-top:4px' },
                        `baseline: [${bKeys.join(', ') || '(empty)'}]`),
                    m('div', { style: 'font-size:11px;opacity:0.7' },
                        `experiment: [${eKeys.join(', ') || '(empty)'}]`),
                    m('div', { style: 'font-size:11px;opacity:0.7;margin-top:4px;word-break:break-all' },
                        `query: ${vnode.state.experimentQuery || '(n/a)'}`),
                    m('div', { style: 'font-size:11px;opacity:0.7' },
                        `window: ${win ? `[${win.start}, ${win.end}] step=${win.step}` : '(n/a)'}, raw series: ${resultsLen}`),
                ]);
            }
            return m('div.compare-split-subgroup',
                specs.map((s) => m('div.chart-wrapper',
                    m(Chart, { spec: s, chartsState, interval }))));
        }
        // Mithril vnode (has a `tag` or is an array) — render directly.
        if (Array.isArray(result) || (result && (result.tag || result.view || result.children))) {
            return result;
        }
        // Otherwise treat as a transformed spec.
        return m(Chart, { spec: result, chartsState, interval });
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
                compareMode, toggles, setChartToggle, anchors,
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
                m('span.chart-title', opts.title),
                opts.description && m('span.chart-subtitle', opts.description),
                compareMode && spec && compareToggle(spec, {
                    compareMode, toggles, setChartToggle,
                }),
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

            const renderChart = (spec) => {
                const isHistogramChart = isHistogramPlot(spec);
                const wrapperClass = spec.width === 'full'
                    ? 'div.chart-wrapper.full-width'
                    : 'div.chart-wrapper';

                if (isHistogramChart && isHeatmapMode && sectionHeatmapData?.has(spec.opts.id)) {
                    const heatmapData = sectionHeatmapData.get(spec.opts.id);
                    const heatmapSpec = buildHistogramHeatmapSpec(spec, heatmapData, prefixTitle(spec.opts));
                    return m(wrapperClass, [
                        chartHeader(heatmapSpec.opts, heatmapSpec),
                        compareMode && spec.promql_query
                            ? m(CompareChartWrapper, {
                                spec: heatmapSpec,
                                chartsState,
                                interval,
                                anchors,
                                toggles,
                                setChartToggle,
                                sectionRoute,
                                step: interval,
                            })
                            : m(Chart, { spec: heatmapSpec, chartsState, interval }),
                        expandLink(spec, sectionRoute),
                        selectButton(spec, sectionRoute, sectionName),
                    ]);
                }

                const prefixedSpec = { ...spec, opts: prefixTitle(spec.opts), noCollapse };

                if (compareMode && spec.promql_query) {
                    return m(wrapperClass, [
                        chartHeader(prefixedSpec.opts, prefixedSpec),
                        m(CompareChartWrapper, {
                            spec: prefixedSpec,
                            chartsState,
                            interval,
                            anchors,
                            toggles,
                            setChartToggle,
                            sectionRoute,
                            step: interval,
                        }),
                        expandLink(spec, sectionRoute),
                        selectButton(spec, sectionRoute, sectionName),
                    ]);
                }

                return m(wrapperClass, [
                    chartHeader(prefixedSpec.opts, prefixedSpec),
                    m(Chart, { spec: prefixedSpec, chartsState, interval }),
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

// silence lint for unused re-export BASELINE_COLOR (retained for future use)
void BASELINE_COLOR;
