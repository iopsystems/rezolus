// Shared chart control buttons (Expand / Select) used by both the
// regular Group component and the cgroup section renderer.

import { isSelected, toggleSelection } from './selection.js';

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
