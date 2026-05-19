// SQL Query Explorer and SingleChartView.
//
// Pre-purge this module ran PromQL through the dead backend; this is
// the SQL-native rebuild. The backend already accepts arbitrary
// DuckDB SQL via `/api/v1/query_range?strict=true` — this module is
// pure frontend.

import { Chart } from '../charts/chart.js';
import { ViewerApi } from '../viewer_api.js';
import { applyResultToPlot, fetchHeatmapForPlot } from '../data.js';
import { isHistogramPlot, buildHistogramHeatmapSpec } from '../charts/metric_types.js';
import { readHistory, pushHistory } from './sql_history.js';

// Re-export for tests + outside callers.
export { trimAndDedupe } from './sql_history.js';

const EXAMPLES = [
    {
        title: 'CPU usage rate per CPU',
        sql: `SELECT timestamp::DOUBLE/1e9 AS t,
       regexp_extract(col, '/([0-9]+)$', 1) AS id,
       irate_1s(v, timestamp) AS v
FROM (UNPIVOT (SELECT timestamp, COLUMNS('^cpu_usage/user/[0-9]+$') FROM _src)
        ON COLUMNS('^cpu_usage/user/[0-9]+$') INTO NAME col VALUE v)
ORDER BY t, id`,
    },
    {
        title: 'TCP packet latency p99 (single quantile)',
        sql: `WITH d AS (
  SELECT timestamp,
         h2_delta("tcp_packet_latency:buckets",
                  LAG("tcp_packet_latency:buckets") OVER (ORDER BY timestamp)) AS d
  FROM _src
)
SELECT timestamp::DOUBLE/1e9 AS t,
       h2_quantile(d, 0.99)::DOUBLE AS v
FROM d
WHERE d IS NOT NULL`,
    },
    {
        title: 'Multiple histogram percentiles in one query',
        sql: `WITH d AS (
  SELECT timestamp,
         h2_delta("scheduler_runqueue_latency:buckets",
                  LAG("scheduler_runqueue_latency:buckets") OVER (ORDER BY timestamp)) AS d
  FROM _src
)
SELECT timestamp::DOUBLE/1e9 AS t,
       q::VARCHAR AS quantile,
       h2_quantile(d, q)::DOUBLE AS v
FROM d, (VALUES (0.5), (0.9), (0.99)) qs(q)
WHERE d IS NOT NULL`,
    },
    {
        title: 'Counter delta between two CPUs',
        sql: `SELECT timestamp::DOUBLE/1e9 AS t,
       "cpu_usage/user/0"::DOUBLE - "cpu_usage/user/1"::DOUBLE AS v
FROM _src`,
    },
    {
        title: 'UNPIVOT regex spread to per-state series',
        sql: `SELECT timestamp::DOUBLE/1e9 AS t, col AS state,
       irate_1s(v, timestamp) AS v
FROM (UNPIVOT (SELECT timestamp, COLUMNS('^cpu_usage/[a-z]+$') FROM _src)
        ON COLUMNS('^cpu_usage/[a-z]+$') INTO NAME col VALUE v)`,
    },
];

const buildPlotSpecFromResult = (result, title, unit) => {
    const plot = {
        opts: {
            title,
            id: `explorer-${Math.random().toString(36).slice(2, 10)}`,
            type: 'gauge',
            format: { unit_system: unit || null, precision: 2 },
        },
        data: [],
    };
    applyResultToPlot(plot, result);
    return plot;
};

/**
 * Mithril component: the SQL Query Explorer. Owns a textarea, an
 * Execute button (Ctrl+Enter), a unit-system selector, a history
 * dropdown, an error panel, and a result chart pane.
 */
export const QueryExplorer = {
    oninit(vnode) {
        vnode.state.sql = '';
        vnode.state.unit = '';
        vnode.state.history = readHistory();
        vnode.state.error = null;
        vnode.state.plot = null;
        vnode.state.loading = false;
        vnode.state.chartsState = vnode.attrs.chartsState;
    },
    async submit(vnode) {
        const sql = vnode.state.sql.trim();
        if (!sql || vnode.state.loading) return;
        vnode.state.loading = true;
        vnode.state.error = null;
        vnode.state.plot = null;
        try {
            // No meaningful start/end on the explorer side; the SQL
            // body controls the time window via its own WHERE
            // clause if the user wants one. Pass placeholder zeros
            // (the backend ignores `start/end/step` on the SQL path).
            const result = await ViewerApi.queryRange(sql, 0, 0, 1, 'baseline', { strict: true });
            if (result && result.status === 'success') {
                vnode.state.plot = buildPlotSpecFromResult(result, 'Result', vnode.state.unit);
                vnode.state.history = pushHistory(sql);
            } else {
                vnode.state.error = result?.error || 'unknown query failure';
            }
        } catch (e) {
            vnode.state.error = (e && e.message) || String(e);
        } finally {
            vnode.state.loading = false;
            m.redraw();
        }
    },
    view(vnode) {
        const onKeyDown = (e) => {
            // Ctrl/Cmd+Enter submits without losing focus.
            if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
                e.preventDefault();
                this.submit(vnode);
            }
        };
        const onHistoryPick = (e) => {
            const idx = Number(e.target.value);
            if (Number.isFinite(idx) && vnode.state.history[idx]) {
                vnode.state.sql = vnode.state.history[idx].sql;
                m.redraw();
            }
        };
        return m('div.query-explorer', [
            m('h2', 'Query Explorer'),
            m('p.subtitle', 'Write DuckDB SQL against the loaded capture. ',
                m('code', '_src'), ' is the parquet table; ',
                m('code', 't'), ' (DOUBLE seconds) + ', m('code', 'v'),
                ' is the schema the chart consumes.'),
            m('div.explorer-toolbar', [
                m('select.history-dropdown', {
                    value: '',
                    onchange: onHistoryPick,
                    disabled: vnode.state.history.length === 0,
                }, [
                    m('option', { value: '' }, vnode.state.history.length === 0 ? 'No history yet' : 'History'),
                    ...vnode.state.history.map((entry, i) =>
                        m('option', { value: String(i) }, entry.sql.split('\n')[0].slice(0, 80)),
                    ),
                ]),
                m('select', {
                    value: vnode.state.unit,
                    onchange: (e) => { vnode.state.unit = e.target.value; },
                }, [
                    m('option', { value: '' }, 'Unit: default'),
                    m('option', { value: 'time' }, 'time'),
                    m('option', { value: 'rate' }, 'rate'),
                    m('option', { value: 'bytes' }, 'bytes'),
                    m('option', { value: 'percentage' }, 'percentage'),
                ]),
                m('button.execute', {
                    onclick: () => this.submit(vnode),
                    disabled: vnode.state.loading || !vnode.state.sql.trim(),
                }, vnode.state.loading ? 'Running…' : 'Execute (Ctrl+Enter)'),
            ]),
            m('textarea.explorer-sql', {
                rows: 10,
                spellcheck: false,
                placeholder: 'SELECT timestamp::DOUBLE/1e9 AS t, "cpu_usage/user/0"::DOUBLE AS v FROM _src',
                value: vnode.state.sql,
                oninput: (e) => { vnode.state.sql = e.target.value; },
                onkeydown: onKeyDown,
            }),
            vnode.state.error && m('div.explorer-error', [
                m('strong', 'SQL error: '),
                m('pre', vnode.state.error),
            ]),
            vnode.state.plot && m('div.explorer-result', [
                m(Chart, {
                    spec: vnode.state.plot,
                    chartsState: vnode.state.chartsState,
                    interval: 1,
                }),
            ]),
            m('details.explorer-examples', [
                m('summary', 'Example queries'),
                m('ul', EXAMPLES.map((ex) => m('li', [
                    m('button.example-link', {
                        onclick: () => { vnode.state.sql = ex.sql; m.redraw(); },
                    }, ex.title),
                ]))),
            ]),
        ]);
    },
};

/**
 * Mithril component: pinned single-chart view. Reads section data
 * from the supplied cache and locates a plot by `opts.id`. Caller
 * routes here via `/chart/:section/:chartId`, where `section` is the
 * URL-encoded section path (single-segment or multi-segment).
 *
 * For histogram plots, mirrors the dashboard's "Show Heatmaps"
 * affordance with a per-view toggle: percentile-line view by default,
 * heatmap view when the user clicks the toggle.
 */
export const SingleChartView = {
    oninit(vnode) {
        vnode.state.heatmapMode = false;
        vnode.state.heatmapData = null;
        vnode.state.heatmapLoading = false;
        vnode.state.heatmapPrefetchKicked = false;
    },

    view(vnode) {
        const { section, chartId, sectionResponseCache, chartsState, initialHeatmap } = vnode.attrs;
        const st = vnode.state;
        const cached = sectionResponseCache[section];
        if (!cached) {
            return m('div.single-chart-main', m('p', 'Section data not loaded.'));
        }
        // Prefer the unfiltered plot index that `processDashboardData`
        // stashes on the section payload — that way a deep link
        // resolves even when the parquet has no data for the chart.
        // Fall back to walking the visible group tree for sections
        // whose payload predates the `_allPlots` field.
        let target = (cached._allPlots || []).find((p) => p?.opts?.id === chartId) || null;
        if (!target) {
            for (const g of cached.groups || []) {
                const subgroups = Array.isArray(g.subgroups)
                    ? g.subgroups
                    : [{ plots: g.plots || [] }];
                for (const sg of subgroups) {
                    for (const plot of (sg.plots || [])) {
                        if (plot?.opts?.id === chartId) {
                            target = plot;
                            break;
                        }
                    }
                    if (target) break;
                }
                if (target) break;
            }
        }
        if (!target) {
            return m('div.single-chart-main', m('p', `Chart "${chartId}" not found in section "${section}".`));
        }
        const spec = {
            ...target,
            opts: { ...target.opts },
            width: 'full',
        };
        const isHistogram = isHistogramPlot(target);

        // Honor `?heatmap=1` from the URL once per mount, after the
        // target plot is resolved. We can't fetch in oninit because
        // the section's data may still be loading at that point.
        if (initialHeatmap && isHistogram && !st.heatmapPrefetchKicked) {
            st.heatmapPrefetchKicked = true;
            st.heatmapLoading = true;
            (async () => {
                try {
                    st.heatmapData = await fetchHeatmapForPlot(target);
                    if (st.heatmapData) st.heatmapMode = true;
                } finally {
                    st.heatmapLoading = false;
                    m.redraw();
                }
            })();
        }

        const chartSpec = (st.heatmapMode && st.heatmapData)
            ? buildHistogramHeatmapSpec(spec, st.heatmapData)
            : spec;

        const toggleHeatmap = async () => {
            if (st.heatmapMode) {
                st.heatmapMode = false;
                m.redraw();
                return;
            }
            if (!st.heatmapData) {
                st.heatmapLoading = true;
                m.redraw();
                try {
                    st.heatmapData = await fetchHeatmapForPlot(target);
                } finally {
                    st.heatmapLoading = false;
                }
            }
            if (st.heatmapData) st.heatmapMode = true;
            m.redraw();
        };

        return m('div.single-chart-main',
            m('div.single-chart-view.single-chart-container', [
                m('div.single-chart-header', [
                    m('h2', spec.opts.title),
                    isHistogram && m('button.section-action-btn', {
                        onclick: toggleHeatmap,
                        disabled: st.heatmapLoading,
                    }, st.heatmapLoading
                        ? 'LOADING...'
                        : (st.heatmapMode ? 'SHOW PERCENTILES' : 'SHOW HEATMAP')),
                ]),
                spec.opts.description && m('p.chart-description', spec.opts.description),
                m(Chart, { spec: chartSpec, chartsState, interval: cached.interval || 1 }),
            ]),
        );
    },
};
