// metric_sort.js — pure multi-key sort state + comparator for the metric
// browser table. Kept dependency-free so it's unit-testable under `node --test`
// (see tests/metric_sort.test.mjs); the component wires the click handlers and
// renders the indicators.

export const SORTABLE_COLUMNS = ['name', 'type', 'series', 'labels', 'description'];

// Default when no explicit sort is active: group by type, then name.
export const DEFAULT_SORT = [{ col: 'type', dir: 'asc' }, { col: 'name', dir: 'asc' }];

// Per-column cell value. `series` is numeric; everything else is a string.
const cellValue = (row, col) => {
    switch (col) {
        case 'series': return row.series_count ?? 0;
        case 'labels': return (row.label_keys || []).join(',');
        case 'type': return row.metric_type || '';
        case 'description': return row.description || '';
        case 'name':
        default: return row.name || '';
    }
};

// Return a NEW sort-key list after clicking `col`.
// - shift=false (plain click): single-column sort cycling asc -> desc -> off,
//   where "off" reverts to DEFAULT_SORT (the table is never left unsorted).
// - shift=true: toggle `col` within the multi-key list (asc -> desc -> remove),
//   preserving the other keys and their priority order. Removing the last key
//   reverts to DEFAULT_SORT.
export function cycleSortKeys(keys, col, shift) {
    const existing = keys.find((k) => k.col === col);
    if (shift) {
        let next;
        if (!existing) next = [...keys, { col, dir: 'asc' }];
        else if (existing.dir === 'asc') next = keys.map((k) => (k.col === col ? { col, dir: 'desc' } : k));
        else next = keys.filter((k) => k.col !== col);
        return next.length ? next : DEFAULT_SORT.slice();
    }
    const isOnly = keys.length === 1 && existing;
    if (!isOnly) return [{ col, dir: 'asc' }];
    if (existing.dir === 'asc') return [{ col, dir: 'desc' }];
    return DEFAULT_SORT.slice();
}

// Stable multi-key sort. Returns a new array; does not mutate `rows`.
// (Array.prototype.sort is stable in modern engines, so equal keys keep order.)
export function sortMetrics(rows, keys) {
    const eff = keys && keys.length ? keys : DEFAULT_SORT;
    return [...rows].sort((a, b) => {
        for (const { col, dir } of eff) {
            const av = cellValue(a, col);
            const bv = cellValue(b, col);
            let d;
            if (col === 'series') d = av - bv;
            else d = String(av).localeCompare(String(bv));
            if (d !== 0) return dir === 'desc' ? -d : d;
        }
        return 0;
    });
}
