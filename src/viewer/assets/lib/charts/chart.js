
import {
    configureLineChart
} from './line.js';
import {
    configureScatterChart
} from './scatter.js';
import {
    configureHeatmap
} from './heatmap.js';
import {
    configureHistogramHeatmap
} from './histogram_heatmap.js';
import {
    configureMultiSeriesChart
} from './multi.js';
import globalColorMapper, { COLORS } from './util/colormap.js';
import { themeVersion } from '../theme.js';
import { resolveStyle, resolvedStyle } from './metric_types.js';


export class ChartsState {
    // Zoom state for synchronization across charts.
    // Shape: { start?: 0-100, end?: 0-100, startValue?: ms, endValue?: ms }
    // Treated as a whole — consumers should read via the observable
    // subscribeZoom() callback when they need to react to changes.
    // Direct reads (TimeRangeBar's `.globalZoom`, `isDefaultZoom`, etc.)
    // stay fine, but `zoomLevel` MUST ONLY be mutated via setZoom().
    zoomLevel = null;
    // 'global' (time bar) | 'local' (chart drag/scroll) | null
    zoomSource = null;
    // Global zoom — always percentage-based { start, end } (0-100).
    // Tracks what the time bar shows. Only updated when source === 'global'.
    globalZoom = null;
    // All `Chart` instances, mapped by id
    charts = new Map();
    // Zoom subscribers. Each entry receives (zoom, source) synchronously
    // after setZoom produces a diff.
    _zoomSubs = new Set();
    // Global set of pinned series labels (percentile names like "p50",
    // "p99"). A label lives here once; individual scatter charts
    // intersect this set with their own uniqueNames to compute what
    // they render as pinned. That gives free cross-chart pin sync
    // (pinning p99 on one latency scatter highlights p99 on every
    // latency scatter in the section) while charts that don't carry
    // the label simply ignore it.
    pinnedLabels = new Set();
    _pinSubs = new Set();
    // Global color mapper - for consistent cgroup colors
    colorMapper = globalColorMapper;

    // Resets charts state. It's assumed that each individual chart
    // will be disposed of when it is removed from the DOM.
    clear() {
        this.zoomLevel = null;
        this.zoomSource = null;
        this.globalZoom = null;
        this.charts.clear();
        this._zoomSubs.clear();
        this.pinnedLabels = new Set();
        this._pinSubs.clear();
    }

    isDefaultZoom() {
        return !this.zoomLevel || (this.zoomLevel.start === 0 && this.zoomLevel.end === 100);
    }

    /** Returns true when any chart has a local zoom, frozen tooltip, or pinned series. */
    hasActiveSelection() {
        const hasLocalZoom = this.zoomSource === 'local' && !this.isDefaultZoom();
        if (hasLocalZoom) return true;
        if (this.pinnedLabels.size > 0) return true;
        return Array.from(this.charts.values()).some(c => c._tooltipFrozen);
    }

    /**
     * Subscribe to zoom changes. The callback fires synchronously from
     * inside setZoom() with (zoom, source) whenever setZoom produces an
     * actual change. Idempotent writes (same zoom as current) do not
     * fire subscribers — that's how echo events from echarts' own
     * programmatic dispatches are suppressed by construction.
     * Returns an unsubscribe function.
     */
    subscribeZoom(fn) {
        this._zoomSubs.add(fn);
        return () => { this._zoomSubs.delete(fn); };
    }

    /**
     * The ONE writer for zoomLevel / zoomSource / globalZoom. Any path
     * that changes zoom — user drag on a chart, TimeRangeBar drag,
     * selection restore, reset — goes through here.
     *
     * Diffs against the current zoomLevel. When the proposed zoom is
     * effectively identical to the current one, returns false without
     * notifying subscribers (this is the echo guard). Otherwise writes
     * the new zoom and notifies every zoom subscriber synchronously.
     *
     * @param {{start?: number, end?: number, startValue?: number, endValue?: number} | null} zoom
     * @param {{ source?: 'global' | 'local' | null }} opts
     * @returns {boolean} true when the store was updated, false on no-op.
     */
    setZoom(zoom, { source = this.zoomSource } = {}) {
        const next = normalizeZoom(zoom);
        if (zoomEqual(this.zoomLevel, next)) return false;
        this.zoomLevel = next;
        this.zoomSource = source;
        if (source === 'global') {
            this.globalZoom = next == null
                ? null
                : { start: next.start ?? 0, end: next.end ?? 100 };
        }
        for (const fn of this._zoomSubs) fn(this.zoomLevel, this.zoomSource);
        return true;
    }

    // Reset zoom level on all charts — same writer path as every other
    // zoom change, so subscribers see a single {start:0, end:100}
    // notification rather than an ad-hoc forEach dispatch.
    resetZoom() {
        this.setZoom({ start: 0, end: 100 }, { source: 'global' });
    }

    /**
     * Subscribe to pinned-label changes. Callback fires synchronously
     * from setPins() with the new (cloned) `Set<string>` of pinned
     * labels whenever setPins produces an actual change.
     * Returns an unsubscribe function.
     */
    subscribePins(fn) {
        this._pinSubs.add(fn);
        return () => { this._pinSubs.delete(fn); };
    }

    /**
     * The single writer for `pinnedLabels`. Diffs against the current
     * set; on no-op returns false without notifying. On a change,
     * writes a fresh clone and notifies every subscriber.
     */
    setPins(labels) {
        const next = labels instanceof Set ? new Set(labels) : new Set(labels ?? []);
        if (setsEqual(this.pinnedLabels, next)) return false;
        this.pinnedLabels = next;
        for (const fn of this._pinSubs) fn(this.pinnedLabels);
        return true;
    }

    // Reset zoom and clear all pin selections and frozen tooltips
    // (preserves heatmap/percentile toggle)
    resetAll() {
        this.resetZoom();
        // Pin clear routes through the observable setter; subscribers
        // (scatter charts) rebuild their legend + series styling and
        // drop their derived pinnedSet. No explicit forEach reconfigure
        // needed — that's the whole point of the subscribe model.
        this.setPins(new Set());
        this.charts.forEach(chart => {
            if (chart._tooltipFrozen) {
                chart._toggleTooltipFreeze(false);
            }
            // Hide any visible tooltips and axis pointer lines
            chart.dispatchAction({ type: 'hideTip' });
            chart.dispatchAction({ type: 'updateAxisPointer', currTrigger: 'leave' });
        });
    }
}

// Shallow Set equality — fine for the modest pin-label sets (handful of
// percentile names at most).
function setsEqual(a, b) {
    if (a === b) return true;
    if (a.size !== b.size) return false;
    for (const x of a) if (!b.has(x)) return false;
    return true;
}

// Normalize a caller-supplied zoom value into the canonical shape
// stored on ChartsState. null means "no zoom"; an object with only
// NaN/undefined fields also collapses to null so callers don't have
// to pre-sanitize.
function normalizeZoom(zoom) {
    if (zoom == null) return null;
    const out = {};
    if (Number.isFinite(zoom.start)) out.start = zoom.start;
    if (Number.isFinite(zoom.end)) out.end = zoom.end;
    if (Number.isFinite(zoom.startValue)) out.startValue = zoom.startValue;
    if (Number.isFinite(zoom.endValue)) out.endValue = zoom.endValue;
    if (Object.keys(out).length === 0) return null;
    return out;
}

// Zoom equality with a tiny epsilon, so programmatic echoes that
// round-trip through echarts' internal arithmetic don't count as a
// real change. Prefer percentage comparison when both sides have it.
const ZOOM_EPS = 1e-6;
function zoomEqual(a, b) {
    if (a === b) return true;
    if (a == null || b == null) return false;
    const near = (x, y) => Math.abs(x - y) <= ZOOM_EPS;
    if (Number.isFinite(a.start) && Number.isFinite(b.start)
        && Number.isFinite(a.end) && Number.isFinite(b.end)) {
        return near(a.start, b.start) && near(a.end, b.end);
    }
    if (Number.isFinite(a.startValue) && Number.isFinite(b.startValue)
        && Number.isFinite(a.endValue) && Number.isFinite(b.endValue)) {
        return near(a.startValue, b.startValue) && near(a.endValue, b.endValue);
    }
    return false;
}

// Cheap same-shape heuristic for detecting "structurally identical"
// data arrays. Compare-mode strategies rebuild triples/matrix arrays
// on every Mithril redraw even when the underlying capture data hasn't
// changed; comparing array reference alone would force a reconfigure
// (and wipe the zoom) on every render. Sample length + first/last
// element; if those match we treat the arrays as equivalent for the
// purposes of triggering a re-configure. False positives just mean
// "zoom preserved through a genuine data swap" which is still
// recoverable via the Reset button.
function shallowSameShape(a, b) {
    if (a === b) return true;
    if (!Array.isArray(a) || !Array.isArray(b)) return false;
    if (a.length !== b.length) return false;
    if (a.length === 0) return true;
    return sameHead(a[0], b[0]) && sameHead(a[a.length - 1], b[b.length - 1]);
}
function sameHead(a, b) {
    if (a === b) return true;
    if (Array.isArray(a) && Array.isArray(b)) {
        if (a.length !== b.length) return false;
        for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
        return true;
    }
    return false;
}

// Chart component - uses echarts to render a chart
export class Chart {
    constructor(vnode) {
        this.domNode = null; // not available until oncreate
        this.chartId = vnode.attrs.spec.opts.id;
        this.spec = vnode.attrs.spec;
        this.chartsState = vnode.attrs.chartsState;
        this.interval = vnode.attrs.interval; // sampling interval in seconds
        this.resizeObserver = null;
        this.observer = null;
        this.echart = null;
        this._themeVersion = themeVersion;
    }

    oncreate(vnode) {
        this.domNode = vnode.dom;

        // Set up the Intersection Observer to lazy load the chart
        const observer = new IntersectionObserver((entries) => {
            const hasIntersection = entries.some(entry => entry.isIntersecting);
            if (hasIntersection) {
                this.initEchart();
                // Once initialized, we can stop observing
                observer.unobserve(this.domNode);
            }
        }, {
            root: null, // Use viewport as root
            rootMargin: '1px', // Load when within 1px of viewport
            threshold: 0.01 // Trigger when at least 1% visible
        });

        // Start observing the chart element
        observer.observe(this.domNode);

        // Resize charts when their container size changes (window resize, zoom, etc.)
        this.resizeObserver = new ResizeObserver(() => {
            if (this.echart) {
                this.echart.resize();
            }
        });
        this.resizeObserver.observe(this.domNode);
        this.observer = observer;
    }

    onupdate(vnode) {
        // Update the spec and interval references
        const oldSpec = this.spec;
        this.spec = vnode.attrs.spec;
        this.interval = vnode.attrs.interval;

        // Re-render if data changed, format changed, or theme was toggled.
        // A new spec.data *reference* alone isn't enough to justify a
        // full reconfigure — compare-mode strategies rebuild the triples
        // array on every Mithril redraw, which would otherwise wipe the
        // echarts zoom/cursor state on the experiment slot after every
        // tooltip / scroll / hover. Treat equal-length arrays with
        // identical head elements as "same data" to skip the reconfigure
        // in the common case. Theme/format changes still force it.
        const themeChanged = this._themeVersion !== themeVersion;
        const formatChanged = oldSpec.opts?.format !== this.spec.opts?.format;
        const dataChanged = oldSpec.data !== this.spec.data
            && !shallowSameShape(oldSpec.data, this.spec.data);
        if (this.echart && (dataChanged || formatChanged || themeChanged)) {
            this._themeVersion = themeVersion;
            this.configureChartByType();

            // Restore zoom state after re-render (notMerge wipes the
            // dataZoom range). In compare mode each Mithril redraw
            // hands in a fresh spec object and therefore triggers a
            // full reconfigure, which would otherwise clear the user's
            // zoom on the non-source slot. _applyZoom is the single
            // entrypoint for writing the zoom onto echarts; skip when
            // there's no zoom set or we're at default.
            const z = this.chartsState.zoomLevel;
            if (z != null && !(z.start === 0 && z.end === 100)) {
                this._applyZoom(z);
            }
            // Re-arm drag-to-zoom. applyChartOption inside
            // configureChartByType already dispatches takeGlobalCursor,
            // but the subsequent dataZoom restore above can leave
            // echarts' internal cursor state in a stale position
            // (noticeable on heatmaps, where the toolbox rectangle
            // select stops responding). Re-arm unconditionally.
            this.echart.dispatchAction({
                type: 'takeGlobalCursor',
                key: 'dataZoomSelect',
                dataZoomSelectActive: true,
            });
        }
    }

    onremove() {
        // Clean up chart instance and event handlers
        if (this.observer) {
            this.observer.disconnect();
        }

        if (this.resizeObserver) {
            this.resizeObserver.disconnect();
        }

        if (this._freezeKeyCleanup) this._freezeKeyCleanup();
        if (this._pinCleanup) this._pinCleanup();

        // Drop our zoom subscription so setZoom stops notifying a
        // disposed echart. Also remove ourselves from the charts
        // registry so iterations in resetAll / hasActiveSelection /
        // similar don't walk stale entries.
        if (this._unsubZoom) { this._unsubZoom(); this._unsubZoom = null; }
        this.chartsState.charts.delete(this.chartId);

        if (this.echart) {
            this.echart.dispose();
            this.echart = null;
        }
    }

    view() {
        return m('div.chart');
    }

    /**
     * Dispatch an action to the echart instance if it is initialized.
     */
    dispatchAction(action) {
        if (this.echart) {
            this.echart.dispatchAction(action);
        }
    }

    /**
     * Apply a zoom to THIS chart's echart. The single place that calls
     * dispatchAction({type: 'dataZoom', …}). Called both from the
     * chartsState subscribeZoom callback (when any writer updates the
     * shared zoom) and from the onupdate reconfigure path (where
     * applyChartOption's notMerge wipes the dataZoom component and we
     * need to restore it).
     *
     * Building the payload prefers percentages; falls back to absolute
     * axis values. Passing an undefined field to dispatchAction would
     * clear that slot on the component (which for heatmaps snaps back
     * to the data range), so only populate the pair we actually have.
     */
    _applyZoom(zoom) {
        if (!this.echart) return;
        const payload = { type: 'dataZoom' };
        if (zoom == null) {
            payload.start = 0;
            payload.end = 100;
        } else if (Number.isFinite(zoom.start) && Number.isFinite(zoom.end)) {
            payload.start = zoom.start;
            payload.end = zoom.end;
        } else if (Number.isFinite(zoom.startValue) && Number.isFinite(zoom.endValue)) {
            payload.startValue = zoom.startValue;
            payload.endValue = zoom.endValue;
        } else {
            // No usable zoom info — nothing to apply.
            return;
        }
        this.echart.dispatchAction(payload);
        this._rescaleYAxis();
    }

    /**
     * Toggle tooltip freeze state. When frozen, the tooltip stays at its
     * current position until unfrozen.
     * @param {boolean} [force] - if provided, set frozen state explicitly
     */
    _toggleTooltipFreeze(force) {
        if (!this.echart) return;
        const freeze = force !== undefined ? force : !this._tooltipFrozen;
        this._tooltipFrozen = freeze;

        if (freeze) {
            // Stop tooltip from following the mouse
            this.echart.setOption({
                tooltip: { triggerOn: 'none' },
            });
        } else {
            // Restore normal tooltip behavior
            this.echart.setOption({
                tooltip: { triggerOn: 'mousemove|click' },
            });
        }

        // Directly patch the tooltip footer DOM since the formatter won't
        // re-run on an already-visible tooltip.
        const footer = this.domNode.querySelector('.tooltip-freeze-footer');
        if (footer) {
            footer.textContent = freeze ? 'FROZEN \u00b7 click to unfreeze' : 'click to freeze';
            footer.style.color = freeze ? COLORS.accent : COLORS.fgMuted;
        }
    }

    initEchart() {
        if (this.echart) {
            return;
        }

        // Initialize the chart
        this.echart = echarts.init(this.domNode);
        // Only log initial chart creation in debug mode
        if (window.location.search.includes('debug')) {
            const startTime = new Date();
            this.echart.on('finished', () => {
                this.echart.off('finished');
                console.log(`Chart ${this.chartId} rendered in ${new Date() - startTime}ms`);
            });
        }

        let timeData = null;
        if (this.spec.data && this.spec.data.length > 0) {
            if (this.spec.data[0] && Array.isArray(this.spec.data[0])) {
                timeData = this.spec.data[0];
            }
        } else if (this.spec.time_data) {
            timeData = this.spec.time_data;
        }

        // Calculate sample interval and minimum zoom percentage
        // Minimum zoom is 5x the sample interval
        if (timeData && timeData.length >= 2) {
            const sampleInterval = timeData[1] - timeData[0]; // in seconds
            const totalDuration = timeData[timeData.length - 1] - timeData[0];
            const minVisibleDuration = sampleInterval * 5;
            // Convert to percentage of total duration
            this.minZoomPercent = Math.max(0.1, (minVisibleDuration / totalDuration) * 100);
        } else {
            this.minZoomPercent = 0.1; // fallback minimum
        }

        // Store chart instance for cleanup and to prevent re-initialization
        this.chartsState.charts.set(this.chartId, this);

        // Subscribe to zoom-state changes. Every writer — TimeRangeBar
        // drag, selection restore, ChartsState.resetZoom, and other
        // charts' datazoom handlers via setZoom — notifies subscribers
        // with the new zoom. We just apply it to our echart; the diff
        // guard inside setZoom ensures idempotent echoes don't fire us.
        this._unsubZoom = this.chartsState.subscribeZoom((zoom) => this._applyZoom(zoom));

        // Perform the main echarts configuration work, and set up any chart-specific dynamic behavior.
        this.configureChartByType();

        // Match existing zoom state on first mount. Equivalent to
        // replaying the last setZoom against only this chart.
        const existingZoom = this.chartsState.zoomLevel;
        if (existingZoom != null && !(existingZoom.start === 0 && existingZoom.end === 100)) {
            this._applyZoom(existingZoom);
        }

        this.echart.on('datazoom', (event) => {
            // 'datazoom' events triggered by the user vs dispatched by us have different formats:
            // User-triggered events have a batch property with the details under it.
            // (We don't want to trigger on our own dispatched zoom actions, so this is convenient.)
            if (!event.batch) {
                return;
            }

            const details = event.batch[0];

            let { start, end, startValue, endValue } = details;

            // Toolbox drag-to-zoom sometimes only emits startValue /
            // endValue (absolute axis coords) and omits the percentage
            // pair. Downstream code (TimeRangeBar's Match Selection,
            // onupdate's zoom restore) relies on the percentage form,
            // and in compare mode the absolute-coord fallback is
            // broken because the chart axis is in relative ms but the
            // TimeRangeBar's start_time is wall-clock ms. Derive the
            // percentages from the chart's own time range when missing.
            //
            // The axis time-reference depends on chart style:
            //   - Line compare:   multiSeries[*].timeData (rebased)
            //   - Line single:    spec.data[0] (absolute)
            //   - Heatmap any:    spec.time_data (matches the rendered axis)
            // Pick the widest reference so the percentage math lines up
            // with whatever echarts used to draw the axis.
            if ((start === undefined || Number.isNaN(start))
                && (end === undefined || Number.isNaN(end))
                && startValue !== undefined
                && endValue !== undefined) {
                let td = this.spec.time_data;
                if (!td && Array.isArray(this.spec.multiSeries) && this.spec.multiSeries.length > 0) {
                    td = this.spec.multiSeries.reduce(
                        (a, s) => (s.timeData && s.timeData.length > (a?.length || 0) ? s.timeData : a),
                        this.spec.multiSeries[0].timeData,
                    );
                }
                if (!td && Array.isArray(this.spec.data)) {
                    td = this.spec.data[0];
                }
                if (td && td.length >= 2) {
                    const axisMin = td[0] * 1000;
                    const axisMax = td[td.length - 1] * 1000;
                    const total = axisMax - axisMin;
                    if (total > 0) {
                        start = Math.max(0, Math.min(100, ((startValue - axisMin) / total) * 100));
                        end = Math.max(0, Math.min(100, ((endValue - axisMin) / total) * 100));
                    }
                }
            }

            // Enforce minimum zoom level (5x sample interval)
            const zoomRange = end - start;
            const minZoom = this.minZoomPercent || 0.1;

            if (zoomRange < minZoom) {
                // Zoom is too tight, clamp it
                const center = (start + end) / 2;
                start = Math.max(0, center - minZoom / 2);
                end = Math.min(100, center + minZoom / 2);

                // Adjust if we hit the boundaries
                if (start === 0) {
                    end = Math.min(100, minZoom);
                } else if (end === 100) {
                    start = Math.max(0, 100 - minZoom);
                }

                // Clear startValue/endValue since we're using percentages
                startValue = undefined;
                endValue = undefined;
            }

            // Reject events whose batch exists but carries no usable
            // zoom info (all four fields undefined or NaN). echarts
            // emits these in edge cases — notably when
            // setOption({series:[...]}) re-fires datazoom after a
            // heatmap downsample swap — and without this guard they'd
            // normalize to null, diff as "changed" against an active
            // zoom, and fan out a 0..100% reset to every subscriber.
            const hasPct = Number.isFinite(start) && Number.isFinite(end);
            const hasValues = Number.isFinite(startValue) && Number.isFinite(endValue);
            if (!hasPct && !hasValues) return;

            // Route the user-initiated zoom through the single
            // chartsState.setZoom writer. setZoom's diff check short-
            // circuits echoes: the dispatch fan-out below will trigger
            // secondary datazoom events on sibling heatmaps (via
            // heatmap.js's downsample-swap setOption), those events
            // re-enter this handler with effectively the same values
            // we just proposed, and setZoom returns false instead of
            // re-notifying. No skip-self, no write-gate, no re-entry
            // flag — the diff is the echo guard by construction.
            const changed = this.chartsState.setZoom(
                { start, end, startValue, endValue },
                { source: 'local' },
            );
            if (changed) m.redraw();
        });

        // Enable drag-to-zoom
        // This requires the toolbox to be enabled. See the comment for the toolbox configuration in base.js for more details.
        this.echart.dispatchAction({
            type: 'takeGlobalCursor',
            key: 'dataZoomSelect',
            dataZoomSelectActive: true,
        });

        // Tooltip freeze: click on chart to freeze, click again to unfreeze.
        // Escape also unfreezes.
        this._tooltipFrozen = false;

        this.echart.getZr().on('click', (e) => {
            // Only freeze when clicking within the plot area, not on legend/title
            if (this.echart.containPixel('grid', [e.offsetX, e.offsetY])) {
                this._toggleTooltipFreeze();
            }
        });

        const onEscKey = (e) => {
            if (e.key === 'Escape' && this._tooltipFrozen) {
                e.preventDefault();
                this._toggleTooltipFreeze(false);
            }
        };
        document.addEventListener('keydown', onEscKey, true);
        this._freezeKeyCleanup = () => {
            document.removeEventListener('keydown', onEscKey, true);
        };

        // Double click on a chart -> reset zoom level
        // https://github.com/apache/echarts/issues/18195#issuecomment-1399583619
        this.echart.getZr().on('dblclick', () => {
            this.chartsState.resetAll();
            m.redraw();
        })
    }

    /**
     * Rescale Y-axis to fit visible data when zoomed in on X-axis.
     * Called after every datazoom event and on zoom reset.
     */
    _rescaleYAxis() {
        if (!this.echart) return;

        const style = resolvedStyle(this.spec)
            || resolveStyle(this.spec.opts.type, this.spec.opts.subtype);
        // Only for chart types with a value/log Y-axis
        if (style === 'heatmap' || style === 'histogram_heatmap') return;

        // In compare-mode overlays, spec.data holds only the baseline's
        // [timeData, valueData]; the experiment values live in
        // spec.multiSeries[1]. Collect all series' (timeData, valueData)
        // pairs so the Y-rescale considers both captures and doesn't
        // clip the higher-valued trace.
        const multi = Array.isArray(this.spec.multiSeries) && this.spec.multiSeries.length > 0
            ? this.spec.multiSeries
            : null;
        const data = this.spec.data;
        const seriesPairs = multi
            ? multi.map((s) => ({ timeData: s.timeData, valueData: s.valueData }))
            : (data && data.length >= 2 && data[0] && data[0].length > 0
                ? (() => {
                    const timeData = data[0];
                    const out = [];
                    for (let i = 1; i < data.length; i++) out.push({ timeData, valueData: data[i] });
                    return out;
                })()
                : []);
        if (seriesPairs.length === 0) return;

        const format = this.spec.opts.format || {};
        const option = this.echart.getOption();
        const isLog = option?.yAxis?.[0]?.type === 'log';

        // When percentile series are pinned, rescale Y to pinned subset only.
        const hasPins = this.pinnedSet && this.pinnedSet.size > 0;
        const labels = this._seriesLabels; // set by scatter chart config

        // If at default zoom and no pins active, restore original bounds
        if (this.chartsState.isDefaultZoom() && !(hasPins && labels)) {
            this.echart.setOption({
                yAxis: { min: null, max: this._oobAxisMax ?? null }
            });
            return;
        }

        // Compute visible time range from zoom state. Use the widest
        // series' timeData as the reference (matches what line.js uses
        // for dataZoom).
        const refTimeData = seriesPairs.reduce(
            (a, s) => (s.timeData.length > a.length ? s.timeData : a),
            seriesPairs[0].timeData,
        );
        const zoom = this.chartsState.zoomLevel;

        let visibleMinMs, visibleMaxMs;
        if (!zoom) {
            visibleMinMs = refTimeData[0] * 1000;
            visibleMaxMs = refTimeData[refTimeData.length - 1] * 1000;
        } else if (zoom.startValue !== undefined && zoom.endValue !== undefined) {
            visibleMinMs = zoom.startValue;
            visibleMaxMs = zoom.endValue;
        } else {
            const totalMinMs = refTimeData[0] * 1000;
            const totalMaxMs = refTimeData[refTimeData.length - 1] * 1000;
            const totalRange = totalMaxMs - totalMinMs;
            visibleMinMs = totalMinMs + (zoom.start / 100) * totalRange;
            visibleMaxMs = totalMinMs + (zoom.end / 100) * totalRange;
        }

        // Scan each series' data for min/max Y in visible range.
        // When percentile series are pinned, only consider pinned series
        // (legacy spec.data path — multiSeries doesn't carry labels).
        let yMin = Infinity;
        let yMax = -Infinity;

        for (let pairIdx = 0; pairIdx < seriesPairs.length; pairIdx++) {
            if (hasPins && labels) {
                const name = labels[pairIdx];
                if (name && !this.pinnedSet.has(name)) continue;
            }
            const { timeData, valueData } = seriesPairs[pairIdx];
            for (let i = 0; i < timeData.length; i++) {
                const tMs = timeData[i] * 1000;
                if (tMs < visibleMinMs) continue;
                if (tMs > visibleMaxMs) break;
                const y = valueData[i];
                if (y !== null && y !== undefined && !isNaN(y)) {
                    if (y < yMin) yMin = y;
                    if (y > yMax) yMax = y;
                }
            }
        }

        if (yMin === Infinity || yMax === -Infinity) return;

        // Handle flat line (all same value)
        if (yMin === yMax) {
            const pad = yMin !== 0 ? Math.abs(yMin * 0.1) : 1;
            yMin -= pad;
            yMax += pad;
        }

        // Add padding — multiplicative for log scale, additive for linear
        let newMin, newMax;
        if (isLog) {
            newMin = yMin / 1.2;
            newMax = yMax * 1.2;
        } else {
            const padding = (yMax - yMin) * 0.05;
            newMin = yMin - padding;
            newMax = yMax + padding;
        }

        // Cap axis max: OOB band takes priority, then range ceiling, then padded max
        let yAxisMax;
        if (this._oobAxisMax) {
            yAxisMax = this._oobAxisMax;
        } else if (format.range?.max != null) {
            yAxisMax = Math.min(newMax, format.range.max);
        } else {
            yAxisMax = newMax;
        }
        this.echart.setOption({
            yAxis: {
                min: newMin,
                max: yAxisMax,
            }
        });
    }

    configureChartByType() {
        // Use explicit style (query explorer), resolved style (from query result),
        // or infer from metric type/subtype before data arrives.
        const style = resolvedStyle(this.spec)
            || resolveStyle(this.spec.opts.type, this.spec.opts.subtype);

        // Clean up heatmap DOM legend bar when switching to a different chart type
        // (e.g. heatmap-mode toggle off). The legend lives inside chart.domNode
        // now, so clear it from the same scope.
        if (style !== 'histogram_heatmap' && style !== 'heatmap') {
            this.domNode?.querySelector('.heatmap-legend-bar')?.remove();
        }

        // Handle different chart types by delegating to specialized modules
        if (style === 'line') {
            configureLineChart(this, this.spec, this.chartsState);
        } else if (style === 'heatmap') {
            configureHeatmap(this, this.spec, this.chartsState);
        } else if (style === 'histogram_heatmap') {
            configureHistogramHeatmap(this, this.spec, this.chartsState);
        } else if (style === 'scatter') {
            configureScatterChart(this, this.spec, this.chartsState);
        } else if (style === 'multi') {
            configureMultiSeriesChart(this, this.spec, this.chartsState);
        } else {
            throw new Error(`Unknown chart style: ${style}`);
        }

        // Compact layout for cgroup paired charts: same grid for both, hide echarts legend
        if (this.spec.compactGrid && this.echart) {
            this.echart.setOption({
                grid: { top: '20' },
                legend: { show: false },
            });
        }
    }
}