// Shared viewer components and helpers used by both the server-backed
// viewer (script.js) and the static site viewer (site script.js).

import { Chart } from './charts/chart.js';
import { expandLink, selectButton, compareToggle } from './chart_controls.js';
import { isHistogramPlot, buildHistogramHeatmapSpec, resolvedStyle } from './charts/metric_types.js';
import { renderCompareChart } from './charts/compare.js';
import {
    queryRangeForCapture, buildEffectiveQuery,
    promqlResultToHeatmapTriples, promqlResultToLinePair, promqlResultToSeriesMap,
    getStepOverride, CAPTURE_BASELINE, CAPTURE_EXPERIMENT,
    fetchQuantileSpectrumForPlot,
} from './data.js';
import { canonicalQuantileLabel } from './charts/util/compare_math.js';
import { quantilesForKind } from './charts/util/spectrum_quantiles.js';
import { heatmapTriplesMinMax } from './charts/util/heatmap_data.js';
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
            cap.timeData = []; cap.heatmapData = [];
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
    return cap;
};

/**
 * Mithril component that fetches experiment data asynchronously and
 * then delegates rendering to the compare adapter. Renders a loading
 * placeholder until the experiment fetch resolves; an error
 * placeholder on fetch failure. When the fetch succeeds but the
 * strategy returns `false` (unsupported / not enough data), falls
 * back to rendering baseline-only.
 */
// Effective experiment-query step: explicit user override (granularity
// selector) > caller-supplied step prop > range-derived auto-step.
const effectiveExperimentStep = (attrs, range) => {
    const override = getStepOverride();
    if (override && override > 0) return override;
    if (attrs.step && attrs.step > 0) return attrs.step;
    return range.step;
};

// Fetch the experiment-side PromQL result and stash it on vnode.state.
// Records `_lastFetchedStep` so the component's view can detect when
// the granularity selector has moved and trigger another fetch.
const fetchExperimentResult = (vnode) => {
    const { spec, sectionRoute } = vnode.attrs;
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
    // Category templates supply a per-side experiment query via
    // spec.promql_query_experiment. When present, route it through
    // buildEffectiveQuery instead of spec.promql_query so the same
    // histogram/counter rewrites and step substitutions apply.
    const baseQuery = spec.promql_query_experiment || spec.promql_query;
    const query = buildEffectiveQuery(
        { ...spec, promql_query: baseQuery },
        { sectionRoute, crossCapture: true },
    );
    if (query == null) {
        vnode.state.error = 'compare: query skipped (unresolved cgroup pattern)';
        return;
    }
    vnode.state._fetchInFlight = true;
    (async () => {
        try {
            // Range cached at compare-mode entry; fall back to a one-off
            // metadata fetch if absent (legacy entry paths).
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
                vnode.state._fetchInFlight = false;
                m.redraw();
                return;
            }
            const step = effectiveExperimentStep(vnode.attrs, range);
            const res = await queryRangeForCapture(
                CAPTURE_EXPERIMENT, query, range.start, range.end, step,
            );
            vnode.state.experimentResult = res;
            vnode.state._lastFetchedStep = step;
            // Invalidate the memoized capture so view() re-extracts
            // against the freshly fetched result.
            vnode.state._capExpResult = null;
        } catch (e) {
            // Some rejection paths throw primitives (null for bare
            // aborts, undefined for empty responses). String(e) would
            // surface "null"/"undefined" to the user; prefer a readable
            // message and log the raw value.
            console.error('[compare] experiment query failed', e);
            vnode.state.error = e?.message
                || (typeof e === 'string' && e)
                || 'experiment query failed';
        } finally {
            vnode.state._fetchInFlight = false;
            m.redraw();
        }
    })();
};

// Fire baseline + experiment spectrum fetches in parallel. On both
// resolving, write into vnode.state._spectrumByKind[kind] and trigger
// a redraw. On either failing or returning no data, leave the cache
// empty for that kind and clear the pending flag so the toggle path
// falls through to FALLBACK (5-percentile split).
const kickOffSpectrumFetch = (vnode, spec, kind) => {
    const range = vnode.attrs.experimentQueryRange;
    const quantiles = quantilesForKind(kind);
    const plotForFetch = { promql_query: spec.promql_query, opts: spec.opts };
    // Snapshot the step this fetch was launched at. If the user
    // changes granularity (which mutates _lastFetchedStep on the next
    // experiment refetch), we discard this fetch's result so we don't
    // write stale data into the kind cache after invalidation.
    const stepAtLaunch = vnode.state._lastFetchedStep;
    Promise.all([
        fetchQuantileSpectrumForPlot(plotForFetch, quantiles, CAPTURE_BASELINE),
        fetchQuantileSpectrumForPlot(plotForFetch, quantiles, CAPTURE_EXPERIMENT, range),
    ])
        .then(([base, exp]) => {
            // Discard if granularity has changed since launch (the
            // cache has already been invalidated for the new step;
            // a fresh fetch will fire on the next view()).
            if (vnode.state._spectrumCachedStep !== stepAtLaunch) return;
            if (vnode.state._spectrumPending === kind) {
                vnode.state._spectrumPending = null;
            }
            if (!base || !exp) {
                m.redraw();
                return;
            }
            vnode.state._spectrumByKind = vnode.state._spectrumByKind || {};
            vnode.state._spectrumByKind[kind] = { baseline: base, experiment: exp };
            m.redraw();
        })
        .catch((err) => {
            if (vnode.state._spectrumCachedStep !== stepAtLaunch) return;
            console.error('[compare] spectrum fetch failed', err);
            if (vnode.state._spectrumPending === kind) {
                vnode.state._spectrumPending = null;
            }
            vnode.state.error = err?.message || 'spectrum fetch failed';
            m.redraw();
        });
};

const CompareChartWrapper = {
    oninit(vnode) {
        vnode.state.experimentResult = null;
        vnode.state.error = null;
        // The step at which experimentResult was last fetched. View-side
        // checks this against the current effective step on every redraw
        // and re-fetches when they diverge (granularity selector change).
        vnode.state._lastFetchedStep = null;
        vnode.state._fetchInFlight = false;
        // Kick off the initial fetch.
        fetchExperimentResult(vnode);
    },

    view(vnode) {
        // Re-fetch if the user changed the granularity selector since the
        // last fetch. CompareChartWrapper's oninit only runs once per
        // component lifetime; without this check the cached result from
        // the previous step would render forever.
        if (!vnode.state.error && !vnode.state._fetchInFlight) {
            const want = effectiveExperimentStep(vnode.attrs, { step: 0 });
            if (want > 0 && vnode.state._lastFetchedStep != null
                && want !== vnode.state._lastFetchedStep) {
                fetchExperimentResult(vnode);
            }
        }

        const { spec, chartsState, interval, anchors, toggles, setChartToggle, captureLabels } = vnode.attrs;

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

        // Compare-mode spectrum fetch: when a percentile chart has a
        // Full or Tail toggle on, we need the full/tail spectrum data
        // for both captures. Fetched in parallel, cached on vnode.state
        // by kind, invalidated when granularity changes (mirrors the
        // existing experiment refetch path).
        const chartId = spec?.opts?.id;
        const chartToggles = chartId && toggles ? toggles[chartId] : null;
        const spectrumKind = chartToggles?.spectrumKind || null;

        // Invalidate the spectrum cache when granularity changes
        // (we already track _lastFetchedStep for the experiment fetch;
        // reuse the same trigger).
        if (vnode.state._spectrumCachedStep !== vnode.state._lastFetchedStep) {
            vnode.state._spectrumByKind = {};
            vnode.state._spectrumPending = null;
            vnode.state._spectrumCachedStep = vnode.state._lastFetchedStep;
        }

        if (spectrumKind && resolvedStyle(spec) === 'scatter') {
            const cached = vnode.state._spectrumByKind?.[spectrumKind];
            if (!cached && vnode.state._spectrumPending !== spectrumKind) {
                vnode.state._spectrumPending = spectrumKind;
                kickOffSpectrumFetch(vnode, spec, spectrumKind);
                return m('div.chart-loading', 'Loading spectrum\u2026');
            }
            if (!cached) {
                return m('div.chart-loading', 'Loading spectrum\u2026');
            }
            // Augment captures with spectrum fields for the strategies.
            baselineCap.spectrumTimeData = cached.baseline.time_data;
            baselineCap.spectrumData = cached.baseline.data;
            baselineCap.spectrumSeriesNames = cached.baseline.series_names;
            baselineCap.spectrumColorMinAnchor = cached.baseline.color_min_anchor;
            experimentCap.spectrumTimeData = cached.experiment.time_data;
            experimentCap.spectrumData = cached.experiment.data;
            experimentCap.spectrumSeriesNames = cached.experiment.series_names;
            experimentCap.spectrumColorMinAnchor = cached.experiment.color_min_anchor;
        } else {
            // Toggle off (or chart is non-scatter): scrub any spectrum
            // fields left on the memoized capture objects from a prior
            // toggled-on render. Otherwise downstream consumers would
            // see stale truthy fields after the user disables the
            // spectrum view.
            for (const cap of [baselineCap, experimentCap]) {
                if (!cap) continue;
                cap.spectrumTimeData = undefined;
                cap.spectrumData = undefined;
                cap.spectrumSeriesNames = undefined;
                cap.spectrumColorMinAnchor = undefined;
            }
        }

        const result = renderCompareChart({
            spec,
            captures: [baselineCap, experimentCap],
            anchors: anchors || { baseline: 0, experiment: 0 },
            toggles: toggles || {},
            setChartToggle,
            chartsState,
            interval,
            Chart,
            captureLabels,
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
                baselineAlias, experimentAlias,
            } = state;
            const captureLabels = {
                baseline: baselineAlias || 'baseline',
                experiment: experimentAlias || 'experiment',
            };
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
                    // Pass the user-effective step. CompareChartWrapper
                    // also consults _stepOverride internally, but
                    // including it in attrs keeps the value visible in
                    // the vnode.attrs trail for any future debugging.
                    step: getStepOverride() || interval,
                    experimentQueryRange,
                    captureLabels,
                })
                : m(Chart, { spec: renderSpec, chartsState, interval });

            const renderChart = (spec) => {
                const isHistogramChart = isHistogramPlot(spec);
                // In compare mode, every chart takes the full chart-grid
                // row. Single line overlays benefit from the wider x-axis
                // to distinguish blue/green traces; split multi/scatter
                // and side-by-side heatmaps need the room structurally.
                // Histogram charts (percentile scatter, bucket heatmap,
                // quantile heatmap) also go full-width — the x-axis
                // density and the heatmap legend bar both need the room.
                const wrapperClass = (spec.width === 'full' || compareMode || isHistogramChart)
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
export function buildClientOnlySectionView(Main, sectionResponseCache, getSections, activeSection, getCompareMode) {
    return {
        view() {
            const anyCached = Object.values(sectionResponseCache)[0];
            return m(Main, {
                activeSection,
                groups: [],
                sections: typeof getSections === 'function' ? getSections() : [],
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
