// Render a decoded display-mode boxplot series (see data.js
// `decodeDisplayBinary`) as echarts series: a robust median line, plus two
// nested filled bands — the inner `[lo,hi]` typical-spread band and the outer
// `[min,max]` extremes band. The outer band is what keeps a decimated spike
// visible; the median line does not chase it.
//
// Bands use the standard echarts "stacked area" idiom: an invisible baseline
// line at the lower bound, then a line carrying (upper - lower) stacked on top
// whose `areaStyle` fills the gap. No `sampling` — the data is already
// decimated server-side, so every point should render.

const zipMs = (t, col) => {
    const n = t.length;
    const out = new Array(n);
    for (let i = 0; i < n; i++) out[i] = [t[i] * 1000, col[i]]; // s -> ms
    return out;
};

const zipDiffMs = (t, base, top) => {
    const n = t.length;
    const out = new Array(n);
    for (let i = 0; i < n; i++) out[i] = [t[i] * 1000, top[i] - base[i]];
    return out;
};

// Whether an uncertainty band carries at least one drawable point (both edges
// finite). uncLo/uncHi use NaN to mark points with no band; an all-NaN band
// should draw nothing rather than an empty, invisible series pair.
const hasFiniteBand = (lo, hi) => {
    if (!lo || !hi) return false;
    const n = Math.min(lo.length, hi.length);
    for (let i = 0; i < n; i++) {
        if (Number.isFinite(lo[i]) && Number.isFinite(hi[i])) return true;
    }
    return false;
};

// Distinct colors per series. Band fills are the series color at low opacity
// (applied by echarts via areaStyle.opacity, so any CSS color works).
const PALETTE = ['#4e79a7', '#e15759', '#59a14f', '#f28e2b', '#76b7b2', '#af7aa1', '#edc948', '#ff9da7'];

// Build the echarts series array for one decoded boxplot series.
// `s`: { t, min, lo, median, hi, max } (arrays / Float64Arrays).
//
// `opts.stackId` MUST be unique per series in a chart — echarts stacks every
// series sharing a stack string, so two series with the same name (e.g. a
// per-CPU counter, 12 series all named `cpu_cycles`) would otherwise sum into
// one garbled band. Default derives from the name, which is only safe for a
// single-series chart; multi-series callers pass a unique id (the index).
export function buildBoxplotSeries(s, opts = {}) {
    const {
        name = s.metric?.__name__ || 'series',
        stackId = name,
        lineColor = PALETTE[0],
        // Band fills are the SERIES color at these opacities. We let echarts
        // apply the opacity to `lineColor` directly rather than baking an rgba,
        // because `lineColor` is often a CSS-var-resolved value (rgb()/hsl()),
        // not a #hex — so hand-parsing it produced a broken, near-invisible
        // fill that didn't match the line.
        innerOpacity = 0.45,
        outerOpacity = 0.28,
        // Percentile charts want just the min/max envelope + median (the inner
        // p25/p75 band is redundant there and adds clutter across 4-5 series).
        outerOnly = false,
        // Multi-series line charts draw their own median lines (with gap/clamp/
        // step handling) and only want the band from here — skip the median.
        noMedian = false,
        // Draw-order offset so a caller with N series can stack them
        // consistently: a higher zBase draws on top. Series' internal levels
        // span zBase+1..zBase+3, so callers should stride zBase by ≥4.
        zBase = 0,
    } = opts;

    // Invisible baseline line that only establishes the stack floor. Hidden
    // from the tooltip so only the median row shows.
    const base = (data, stack) => ({
        type: 'line',
        data,
        stack,
        symbol: 'none',
        silent: true,
        tooltip: { show: false },
        lineStyle: { opacity: 0 },
        z: zBase + 1,
    });
    // Line carrying (upper - lower); its areaStyle fills from the stack floor,
    // in the series color at `opacity`.
    const fill = (data, stack, opacity) => ({
        type: 'line',
        data,
        stack,
        symbol: 'none',
        silent: true,
        tooltip: { show: false },
        lineStyle: { opacity: 0 },
        areaStyle: { color: lineColor, opacity },
        z: zBase + 1,
    });

    const out = [
        // outer extremes band [min, max] — the spike envelope
        base(zipMs(s.t, s.min), `${stackId} outer`),
        fill(zipDiffMs(s.t, s.min, s.max), `${stackId} outer`, outerOpacity),
    ];
    if (!outerOnly) {
        // inner typical-spread band [lo, hi]
        out.push(
            base(zipMs(s.t, s.lo), `${stackId} inner`),
            fill(zipDiffMs(s.t, s.lo, s.hi), `${stackId} inner`, innerOpacity),
        );
    }
    // Measurement-uncertainty ribbon [uncLo, uncHi] — the aggregated acquisition-
    // window band (median of per-sample interval edges), i.e. how precisely the
    // median VALUE is known, which is a different thing from the value SPREAD the
    // bands above show. So it gets a distinct treatment: a light fill with thin
    // visible borders at both edges, drawn above the spread fills but below the
    // median line, hugging the line. At native resolution (spread bands collapse
    // to the line) this ribbon is the only band left. NaN marks a point with no
    // band; echarts renders it as a gap. Skipped entirely when no point has one.
    if (Array.isArray(s.uncLo) === false && !(s.uncLo instanceof Float64Array)) {
        // no uncertainty columns on this series
    } else if (hasFiniteBand(s.uncLo, s.uncHi)) {
        const uStack = `${stackId} unc`;
        const border = { color: lineColor, width: 1, opacity: 0.65 };
        const uCommon = {
            type: 'line',
            stack: uStack,
            symbol: 'none',
            silent: true,
            tooltip: { show: false },
            connectNulls: false,
            z: zBase + 2,
        };
        out.push(
            // baseline rides uncLo (thin visible lower border)
            { ...uCommon, data: zipMs(s.t, s.uncLo), lineStyle: border },
            // (uncHi - uncLo) stacked on the baseline → top edge lands on uncHi;
            // its areaStyle fills the ribbon, its lineStyle draws the upper border.
            {
                ...uCommon,
                data: zipDiffMs(s.t, s.uncLo, s.uncHi),
                lineStyle: border,
                areaStyle: { color: lineColor, opacity: 0.22 },
            },
        );
    }
    // robust median line on top (skipped when the caller draws its own line)
    if (!noMedian) {
        out.push({
            name,
            type: 'line',
            data: zipMs(s.t, s.median),
            symbol: 'none',
            lineStyle: { color: lineColor, width: 1.5 },
            // Legend/tooltip markers read `itemStyle.color`, NOT `lineStyle.color`
            // — without this echarts colors the marker from its default palette,
            // so the legend swatch wouldn't match the drawn line.
            itemStyle: { color: lineColor },
            emphasis: { focus: 'series' },
            z: zBase + 3,
        });
    }
    return out;
}

// Render a decoded boxplot series as an ENVELOPE OF LINES (no fill): a
// full-weight median line plus faint, thin min and max lines, all in the same
// color. Used by A/B compare mode, where two filled bands would blend into mud
// on overlap — thin bounding lines stay legible when baseline and experiment
// overlap. Only the median carries a name/tooltip; the min/max lines are silent.
export function buildEnvelopeLines(s, opts = {}) {
    // `zBase` orders captures consistently when overlaid: higher draws on top.
    // Bounds sit at zBase+2, the median at zBase+3, so callers should stride
    // zBase by ≥4 per capture.
    const { name = s.metric?.__name__ || 'series', color = PALETTE[0], zBase = 0 } = opts;
    const bound = (col) => ({
        type: 'line',
        data: zipMs(s.t, col),
        symbol: 'none',
        silent: true,
        tooltip: { show: false },
        lineStyle: { color, width: 1, opacity: 0.4 },
        z: zBase + 2,
    });
    return [
        bound(s.min),
        bound(s.max),
        {
            name,
            type: 'line',
            data: zipMs(s.t, s.median),
            symbol: 'none',
            lineStyle: { color, width: 1.5 },
            // Legend/tooltip markers read `itemStyle.color`, not `lineStyle.color`;
            // without it the marker falls back to echarts' palette and the
            // baseline/experiment swatches don't match their overlay lines.
            itemStyle: { color },
            emphasis: { focus: 'series' },
            z: zBase + 3,
        },
    ];
}

// Neutral-filled band between two overlaid median lines (A/B compare mode):
// shades the gap between baseline and experiment so agreement reads as a thin
// line (band collapses to zero) and divergence as a widening ribbon. The fill
// encodes difference by AREA, not a new hue, so it stays colorblind-safe (a
// third color would regress the blue/green pair). `band`: { t (seconds),
// lower, upper } — per-x min/max of the two medians; null entries leave a gap.
// Drawn behind the median/envelope lines via a low `z`.
export function buildDivergenceBand(band, opts = {}) {
    const { color = '#94a3b8', opacity = 0.28, z = 0, stackId = 'divergence' } = opts;
    const n = band.t.length;
    const baseData = new Array(n);
    const fillData = new Array(n);
    for (let i = 0; i < n; i++) {
        const lo = band.lower[i];
        const hi = band.upper[i];
        const gap = (lo == null || hi == null) ? null : hi - lo;
        baseData[i] = [band.t[i] * 1000, lo == null ? null : lo];
        fillData[i] = [band.t[i] * 1000, gap];
    }
    const common = { type: 'line', symbol: 'none', silent: true, tooltip: { show: false }, stack: stackId, z };
    return [
        // invisible baseline at the lower median
        { ...common, data: baseData, lineStyle: { opacity: 0 } },
        // stacked fill spanning up to the higher median
        { ...common, data: fillData, lineStyle: { opacity: 0 }, areaStyle: { color, opacity } },
    ];
}

// Label a series from its distinguishing labels (drop __name__ and the noisy
// endpoint/source), e.g. `cpu_cycles{id=0}`, for a readable legend.
const seriesLabel = (metric, i) => {
    const name = metric?.__name__ || `series ${i}`;
    const rest = Object.entries(metric || {})
        .filter(([k]) => k !== '__name__' && k !== 'endpoint' && k !== 'source')
        .map(([k, v]) => `${k}=${v}`)
        .join(',');
    return rest ? `${name}{${rest}}` : name;
};

// Assemble a full echarts option rendering every series in a decoded display
// response as boxplot bands. Each series gets a UNIQUE stack id (index) and
// its own color, so multi-series queries (e.g. a per-CPU counter) render as
// distinct boxplots instead of collapsing into one stack.
export function boxplotChartOption(decoded, opts = {}) {
    const decodedSeries = decoded.series || [];
    const series = decodedSeries.flatMap((s, i) =>
        buildBoxplotSeries(s, {
            name: seriesLabel(s.metric, i),
            stackId: `s${i}`,
            lineColor: PALETTE[i % PALETTE.length],
            ...opts,
        }),
    );
    const multi = decodedSeries.length > 1;
    return {
        animation: false,
        tooltip: { trigger: 'axis' },
        legend: multi
            ? { type: 'scroll', top: 0, data: decodedSeries.map((s, i) => seriesLabel(s.metric, i)) }
            : undefined,
        grid: { left: 64, right: 20, top: multi ? 34 : 16, bottom: 40 },
        xAxis: { type: 'time' },
        yAxis: { type: 'value', scale: true },
        series,
    };
}
