// Shared chart control buttons (Expand / Select) used by both the
// regular Group component and the cgroup section renderer.

import { isSelected, toggleSelection } from './selection.js';

/**
 * Compact per-chart toggle rendered in the chart header when compare
 * mode is active and the chart style supports it (currently: heatmap
 * style only, for the `diff` toggle).
 *
 * @param {object} spec - plot spec (reads spec.opts.id and spec.opts.style)
 * @param {object} state - { compareMode, toggles, setChartToggle }
 */
export const compareToggle = (spec, state) => {
    if (!state || !state.compareMode) return null;
    const style = spec?.opts?.style || spec?._resolvedStyle;
    if (style !== 'heatmap') return null;
    const chartId = spec?.opts?.id;
    if (!chartId) return null;
    const current = state.toggles && state.toggles[chartId];
    const checked = !!(current && current.diff);
    return m('label.compare-toggle', {
        title: 'Show experiment − baseline diff instead of side-by-side',
    }, [
        m('input[type=checkbox]', {
            checked,
            onchange: (e) => {
                const v = e.target.checked;
                if (typeof state.setChartToggle === 'function') {
                    state.setChartToggle(chartId, 'diff', v);
                }
            },
        }),
        m('span', 'diff'),
    ]);
};

const EXPAND_ICON_PATH = 'M10 1h5v5h-1.5V3.56L9.78 7.28 8.72 6.22l3.72-3.72H10V1zM1 6V1h5v1.5H3.56l3.72 3.72-1.06 1.06L2.5 3.56V6H1zm5 4H1v5h5v-1.5H3.56l3.72-3.72-1.06-1.06L2.5 12.44V10zm4 0v1.5h2.44l-3.72 3.72 1.06 1.06 3.72-3.72V15H15v-5h-5z';

export const expandLink = (spec, sectionRoute) => {
    if (!spec.promql_query) return null;
    const prefix = (typeof m !== 'undefined' && m.route && m.route.prefix) || '';
    const href = `${prefix}${sectionRoute}/chart/${encodeURIComponent(spec.opts.id)}`;
    return m('a.chart-expand', {
        href, target: '_blank', title: 'Open in new tab',
        onclick: (e) => e.stopPropagation(),
    }, [
        'Expand ',
        m('svg', { width: 12, height: 12, viewBox: '0 0 16 16', fill: 'currentColor' },
            m('path', { d: EXPAND_ICON_PATH }),
        ),
    ]);
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
    }, selected ? 'Selected' : 'Select');
};
