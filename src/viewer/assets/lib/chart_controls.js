// Shared chart control buttons (Expand / Select) used by both the
// regular Group component and the cgroup section renderer.

import { isSelected, toggleSelection } from './selection.js';
import { resolvedStyle } from './charts/metric_types.js';

/**
 * Compact per-chart toggle rendered in the chart header when compare
 * mode is active and the chart style supports it.
 *
 * - heatmap: single `diff` checkbox (experiment − baseline)
 * - scatter: Full / Tail (mutually exclusive) + Diff (visible only when
 *   one of them is active)
 *
 * @param {object} spec - plot spec (reads spec.opts.id and spec.opts.style)
 * @param {object} state - { compareMode, toggles, setChartToggle }
 */
export const compareToggle = (spec, state) => {
    if (!state || !state.compareMode) return null;
    const style = resolvedStyle(spec);
    const chartId = spec?.opts?.id;
    if (!chartId) return null;
    const current = state.toggles?.[chartId] || {};
    const setToggle = state.setChartToggle;

    if (style === 'heatmap') {
        return m('label.compare-toggle', {
            title: 'Show experiment − baseline diff instead of side-by-side',
        }, [
            m('input[type=checkbox]', {
                checked: !!current.diff,
                onchange: (e) => setToggle?.(chartId, 'diff', e.target.checked),
            }),
            m('span', 'diff'),
        ]);
    }

    if (style === 'scatter') {
        const kind = current.spectrumKind || null;
        const setKind = (v) => {
            // Mutually exclusive: clicking the active kind clears it,
            // clicking the other kind switches.
            const next = (kind === v) ? null : v;
            setToggle?.(chartId, 'spectrumKind', next);
            // When clearing the kind, also clear the diff flag so that
            // re-enabling spectrum mode later starts in side-by-side.
            if (next == null) setToggle?.(chartId, 'diff', false);
        };
        // Two visually-separated groups:
        //   1. Diff — data treatment (experiment − baseline). Only
        //      visible when a spectrum kind is selected, since diff
        //      is meaningless without one (the dispatch ignores diff
        //      when kind is null).
        //   2. Full / Tail — visual style (which quantile slice the
        //      heatmap shows). Mutually exclusive, always visible.
        return m('span.compare-toggle-group', [
            kind && m('label.compare-toggle', {
                title: 'Show experiment − baseline diff instead of side-by-side',
            }, [
                m('input[type=checkbox]', {
                    checked: !!current.diff,
                    onchange: (e) => setToggle?.(chartId, 'diff', e.target.checked),
                }),
                m('span', 'Diff'),
            ]),
            m('label.compare-toggle', {
                // Add gap between the Diff group and Full/Tail only
                // when Diff is actually rendered, so a kind-less chart
                // doesn't show a stray indent before Full.
                style: kind ? 'margin-left: 20px' : '',
                title: 'Show full p1..p100 spectrum heatmap',
            }, [
                m('input[type=checkbox]', {
                    checked: kind === 'full',
                    onchange: () => setKind('full'),
                }),
                m('span', 'Full'),
            ]),
            m('label.compare-toggle', {
                style: 'margin-left: 0.5em',
                title: 'Show tail p99.01..p100 spectrum heatmap',
            }, [
                m('input[type=checkbox]', {
                    checked: kind === 'tail',
                    onchange: () => setKind('tail'),
                }),
                m('span', 'Tail'),
            ]),
        ]);
    }

    return null;
};

const EXPAND_ICON_PATH = 'M10 1h5v5h-1.5V3.56L9.78 7.28 8.72 6.22l3.72-3.72H10V1zM1 6V1h5v1.5H3.56l3.72 3.72-1.06 1.06L2.5 3.56V6H1zm5 4H1v5h5v-1.5H3.56l3.72-3.72-1.06-1.06L2.5 12.44V10zm4 5h5v-5h-1.5V12.44L9.78 8.72 8.72 9.78l3.72 3.72H10V15z';

const PIN_ICON_PATH = 'M9.828.722a.5.5 0 0 1 .354.146l4.95 4.95a.5.5 0 0 1 0 .707c-.48.48-1.072.588-1.503.588-.177 0-.335-.018-.46-.039l-3.134 3.134a5.927 5.927 0 0 1 .16 1.013c.046.702-.032 1.687-.72 2.375a.5.5 0 0 1-.707 0l-2.829-2.828-3.182 3.182c-.195.195-1.219.902-1.414.707-.195-.195.512-1.22.707-1.414l3.182-3.182-2.828-2.829a.5.5 0 0 1 0-.707c.688-.688 1.673-.767 2.375-.72a5.922 5.922 0 0 1 1.013.16l3.134-3.133a2.772 2.772 0 0 1-.04-.461c0-.43.108-1.022.589-1.503a.5.5 0 0 1 .353-.146z';

export const expandLink = (spec, sectionRoute) => {
    if (!spec.promql_query) return null;
    const prefix = (typeof m !== 'undefined' && m.route && m.route.prefix) || '';
    const href = `${prefix}${sectionRoute}/chart/${encodeURIComponent(spec.opts.id)}`;
    return m('a.chart-expand', {
        href, target: '_blank', title: 'Open in new tab',
        onclick: (e) => e.stopPropagation(),
    },
        m('svg', { width: 14, height: 14, viewBox: '0 0 16 16', fill: 'currentColor' },
            m('path', { d: EXPAND_ICON_PATH }),
        ),
    );
};

export const selectButton = (spec, sectionRoute, sectionName) => {
    if (!spec.promql_query) return null;
    const sectionKey = sectionRoute.replace(/^\//, '');
    const selected = isSelected(spec.opts.id);
    return m('button.chart-select', {
        class: selected ? 'chart-selected' : '',
        onclick: (e) => {
            e.stopPropagation();
            toggleSelection(spec, sectionKey, sectionName);
            m.redraw();
        },
        title: selected ? 'Remove from selection' : 'Add to selection',
    },
        m('svg', { width: 14, height: 14, viewBox: '0 0 16 16', fill: 'currentColor' },
            m('path', { d: PIN_ICON_PATH }),
        ),
    );
};
