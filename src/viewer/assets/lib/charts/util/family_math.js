// A statistic band over a family of captures, for compare mode with more
// than one baseline member. Every member is a series already rebased to
// relative time (its own anchor at 0). Members are resampled onto one
// reference grid so each contributes one value per bucket whatever its own
// cadence, then each bucket gets a mean and standard deviation, or a min
// and max, and the count of members that had a value there.
//
// Pure: no DOM, no echarts, node-tested.

// The band and its mean line: a neutral hue, so the experiment's own color
// stays the one thing that means "the run under test".
export const FAMILY_COLOR = '#94a3b8';

// Linear interpolation of `v` (sampled at sorted `t`) onto `grid`. A grid
// point outside [t[0], t[last]] is null, as is one between two samples of
// which either is null: a band must not invent a value where a member had
// none.
export const resampleLinear = (grid, t, v) => {
    const out = new Array(grid.length).fill(null);
    if (!Array.isArray(t) || !Array.isArray(v) || t.length === 0 || t.length !== v.length) return out;
    let j = 0;
    const last = t.length - 1;
    for (let i = 0; i < grid.length; i++) {
        const x = grid[i];
        if (!Number.isFinite(x) || x < t[0] || x > t[last]) continue;
        // The grid is usually sorted, so `j` only moves forward; a grid that
        // steps back restarts the search.
        if (x < t[j]) j = 0;
        while (j < last && t[j + 1] < x) j++;
        const exact = t[j] === x ? j : (j < last && t[j + 1] === x ? j + 1 : -1);
        if (exact >= 0) {
            const y = v[exact];
            out[i] = Number.isFinite(y) ? y : null;
            continue;
        }
        if (j >= last) continue;
        const t0 = t[j];
        const t1 = t[j + 1];
        const y0 = v[j];
        const y1 = v[j + 1];
        if (!Number.isFinite(y0) || !Number.isFinite(y1) || !(t1 > t0)) continue;
        out[i] = y0 + ((x - t0) / (t1 - t0)) * (y1 - y0);
    }
    return out;
};

/**
 * Build the band.
 *
 * `members`: `[{ t, v }]`, each rebased to relative seconds. The first
 * member's grid is the reference grid. `kind` is `'sigma'` (mean ± k·sd,
 * sample sd, a bucket with fewer than two members has no band) or
 * `'envelope'` (min..max, one member suffices).
 *
 * Returns `null` with fewer than two members, else
 * `{ t, lower, upper, mean, n, kind, k, members }`.
 */
export const familyBand = (members, { kind = 'sigma', k = 2 } = {}) => {
    const list = (Array.isArray(members) ? members : [])
        .filter((m) => m && Array.isArray(m.t) && Array.isArray(m.v) && m.t.length > 0 && m.t.length === m.v.length);
    if (list.length < 2) return null;
    const grid = Array.from(list[0].t);
    const columns = list.map((m) => resampleLinear(grid, m.t, m.v));
    const n = new Array(grid.length).fill(0);
    const lower = new Array(grid.length).fill(null);
    const upper = new Array(grid.length).fill(null);
    const mean = new Array(grid.length).fill(null);
    const sigma = kind === 'sigma';
    const kk = Number.isFinite(k) && k > 0 ? k : 2;
    for (let i = 0; i < grid.length; i++) {
        const vals = [];
        for (const col of columns) if (col[i] != null) vals.push(col[i]);
        n[i] = vals.length;
        if (vals.length === 0) continue;
        let sum = 0;
        for (const x of vals) sum += x;
        const mu = sum / vals.length;
        mean[i] = mu;
        if (sigma) {
            if (vals.length < 2) continue;
            let ss = 0;
            for (const x of vals) ss += (x - mu) * (x - mu);
            const sd = Math.sqrt(ss / (vals.length - 1));
            lower[i] = mu - kk * sd;
            upper[i] = mu + kk * sd;
        } else {
            lower[i] = Math.min(...vals);
            upper[i] = Math.max(...vals);
        }
    }
    return { t: grid, lower, upper, mean, n, kind: sigma ? 'sigma' : 'envelope', k: sigma ? kk : null, members: list.length };
};

/** The legend text for a band: what it is and how many members fed it. */
export const familyBandLabel = (band) => {
    if (!band) return '';
    const spread = band.kind === 'sigma' ? `mean ± ${band.k}σ` : 'min..max';
    return `family ${spread} (${band.members} members)`;
};
