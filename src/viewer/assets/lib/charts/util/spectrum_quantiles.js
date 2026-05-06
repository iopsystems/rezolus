// Build the list of quantiles to query for each spectrum kind. Use
// integer math so 100 evenly-spaced steps don't accumulate float drift
// (especially relevant for the tail's 0.0001-wide steps).
//
// Shared by scatter.js (single-capture toggle) and compare.js (A/B
// strategies) so both modes always issue the same quantile set.
export function quantilesForKind(kind) {
    const out = [];
    if (kind === 'tail') {
        for (let i = 1; i <= 100; i++) out.push((9900 + i) / 10000);
    } else {
        for (let i = 1; i <= 100; i++) out.push(i / 100);
    }
    return out;
}
