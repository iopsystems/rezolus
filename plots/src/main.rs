//! Server for the downsampled time-series demo.
//!
//! Two endpoints render large series at exactly the resolution the browser asks
//! for, so the client only ever receives a screen's worth of data per view:
//!
//! * `GET /api/line` — an M4-downsampled line (the [`m4`] module): for each pixel
//!   column it keeps the first, last, min, and max sample, so every visible peak
//!   and trough survives.
//! * `GET /api/raster` — a density heatmap answered from a summed-area table (the
//!   [`sat`] module): each visible tile is one constant-time rectangle sum,
//!   mapped through a tone curve to a palette index.
//!
//! Both take the same [`ViewQuery`] (a world-space rectangle + a pixel size) and
//! return their data normalized into a unit `[0,1]` frame, which is what lets the
//! two plots share one coordinate system and zoom together. Everything they serve
//! is synthetic — see "Demo data" at the bottom of this file.

mod m4;
mod sat;

use axum::{Json, Router, extract::Query, routing::get};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tower_http::services::ServeDir;

use crate::m4::{Point, m4};
use crate::sat::{Extent as SExt, Size as SSize, SummedAreaTable, scale_pixels};

// ============================================================================
// Entry point — build the demo data, then serve the two APIs plus static files.
// ============================================================================

#[tokio::main]
async fn main() {
    // Eagerly populate the demo data so startup failures surface immediately.
    let _ = line_data();
    let _ = sat_data();

    let app = Router::new()
        .route("/api/line", get(line_handler))
        .route("/api/raster", get(raster_handler))
        .fallback_service(ServeDir::new("static"));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .unwrap();
    println!("listening on http://{}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

// ============================================================================
// HTTP contract — the request query and the JSON shapes the handlers return.
// These field names are exactly what the browser's renderers read; see
// static/components/*.js.
// ============================================================================

/// The visible window the client wants drawn: a world-space rectangle
/// (`x0..x1`, `y0..y1`, each optional) plus the pixel `width`/`height` to render
/// it at. Missing or non-finite bounds fall back to the full domain per axis.
#[derive(Deserialize, Default)]
struct ViewQuery {
    x0: Option<f64>,
    x1: Option<f64>,
    y0: Option<f64>,
    y1: Option<f64>,
    width: Option<usize>,
    height: Option<usize>,
}

/// An axis-aligned rectangle in some coordinate frame (data domain, world, or
/// the unit `[0,1]` render frame, depending on where it appears).
#[derive(Serialize, Clone, Copy)]
struct Extent {
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
}

/// A size in device pixels.
#[derive(Serialize, Clone, Copy)]
struct Size {
    width: usize,
    height: usize,
}

/// The render frame: which `extent` the pixels cover and how many pixels wide/tall.
#[derive(Serialize, Clone, Copy)]
struct View {
    extent: Extent,
    size: Size,
}

/// One SVG path: an id (for d3's data-join) plus its geometry and stroke style.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PathSpec {
    id: String,
    d: String,
    stroke: String,
    stroke_width: f64,
}

/// `/api/line` response: the render frame, the full data `domain` (for axes),
/// and the downsampled paths.
#[derive(Serialize)]
struct LinePlotData {
    view: View,
    domain: Extent,
    paths: Vec<PathSpec>,
}

/// A heatmap image: its world `extent` (snapped to tile boundaries), the tile
/// grid `size`, and base64-encoded palette-index bytes (one per tile).
#[derive(Serialize)]
struct RasterData {
    extent: Extent,
    size: Size,
    data: String,
}

/// `/api/raster` response: the render frame, the full data `domain` (for axes),
/// and the tile image.
#[derive(Serialize)]
struct RasterPlotData {
    view: View,
    domain: Extent,
    raster: RasterData,
}

// ============================================================================
// Request handlers — turn a ViewQuery into a downsampled render.
// ============================================================================

/// Every render is returned in a unit `[0,1]×[0,1]` frame at the requested pixel
/// size; the client maps that frame back into world space.
fn unit_view(w: usize, h: usize) -> View {
    View {
        extent: Extent { x0: 0.0, x1: 1.0, y0: 0.0, y1: 1.0 },
        size: Size { width: w, height: h },
    }
}

/// Serialize M4 points into an SVG path string, normalizing x into `[0,1]` over
/// `[x0,x1]` and y (flipped, so larger values sit higher) over `[ymin,ymax]`.
fn build_d(points: &[Point], x0: f64, x1: f64, ymin: f64, ymax: f64) -> String {
    let xspan = x1 - x0;
    let yspan = (ymax - ymin).max(f64::MIN_POSITIVE);
    let mut out = String::with_capacity(points.len() * 20);
    for (i, p) in points.iter().enumerate() {
        let xn = (p.x - x0) / xspan;
        let yn = 1.0 - (p.y - ymin) / yspan;
        if i == 0 {
            out.push_str(&format!("M {:.6} {:.6}", xn, yn));
        } else {
            out.push_str(&format!(" L {:.6} {:.6}", xn, yn));
        }
    }
    out
}

async fn line_handler(Query(q): Query<ViewQuery>) -> Json<LinePlotData> {
    let ld = line_data();
    let width = q.width.unwrap_or(400);
    let height = q.height.unwrap_or(100);
    let n = ld.n as f64;
    let x0 = q.x0.filter(|v| v.is_finite()).unwrap_or(0.0).clamp(0.0, n);
    let x1 = q.x1.filter(|v| v.is_finite()).unwrap_or(n).clamp(0.0, n);
    let (x0, x1) = if x1 > x0 { (x0, x1) } else { (0.0, n) };

    let i0 = (x0.floor() as usize).min(ld.n);
    let i1 = (x1.ceil() as usize).min(ld.n).max(i0);
    let slice = &ld.series[i0..i1];
    let pts = m4(slice, x0 - i0 as f64, x1 - i0 as f64, width);
    // Shift points back into full-domain x coordinates so the SVG `d` stays
    // in the same world frame regardless of which sub-range was served.
    let shifted: Vec<Point> = pts
        .iter()
        .map(|p| Point { x: p.x + i0 as f64, y: p.y })
        .collect();
    let d = build_d(&shifted, 0.0, n, ld.ymin, ld.ymax);
    Json(LinePlotData {
        view: unit_view(width, height),
        domain: Extent { x0: 0.0, x1: n, y0: ld.ymin, y1: ld.ymax },
        paths: vec![PathSpec {
            id: "sine".to_string(),
            d,
            stroke: "#4af".to_string(),
            stroke_width: 1.0,
        }],
    })
}

async fn raster_handler(Query(q): Query<ViewQuery>) -> Json<RasterPlotData> {
    let (sat, n_rows, n_cols) = sat_data();
    let nr = *n_rows as f64;
    let nc = *n_cols as f64;
    let width = q.width.unwrap_or(400);
    let height = q.height.unwrap_or(100);
    let x0 = q.x0.filter(|v| v.is_finite()).unwrap_or(0.0).clamp(0.0, nr);
    let x1 = q.x1.filter(|v| v.is_finite()).unwrap_or(nr).clamp(0.0, nr);
    let y0 = q.y0.filter(|v| v.is_finite()).unwrap_or(0.0).clamp(0.0, nc);
    let y1 = q.y1.filter(|v| v.is_finite()).unwrap_or(nc).clamp(0.0, nc);
    let (x0, x1) = if x1 > x0 { (x0, x1) } else { (0.0, nr) };
    let (y0, y1) = if y1 > y0 { (y0, y1) } else { (0.0, nc) };
    let screen = SSize { width, height };
    let res = sat.query_tiles(SExt { x0, y0, x1, y1 }, screen);

    // max density across tiles
    let mut max_density = 0.0f64;
    for &v in &res.values {
        if v > 0 {
            let d = v as f64 / res.tile_area;
            if d > max_density {
                max_density = d;
            }
        }
    }

    let bytes = scale_pixels(&res, max_density);
    let data = B64.encode(&bytes);

    // Normalize snapped extent into [0,1] over the source dimensions.
    let ext = Extent {
        x0: res.extent.x0 / nr,
        x1: res.extent.x1 / nr,
        y0: res.extent.y0 / nc,
        y1: res.extent.y1 / nc,
    };

    Json(RasterPlotData {
        view: unit_view(width, height),
        domain: Extent { x0: 0.0, y0: 0.0, x1: nr, y1: nc },
        raster: RasterData {
            extent: ext,
            size: Size { width: res.size.width, height: res.size.height },
            data,
        },
    })
}

// ============================================================================
// Demo data — synthetic inputs the handlers serve. Nothing below is part of the
// rendering path; it just fabricates large, multi-scale series so there's
// something with structure at every zoom level to explore. Built once, lazily,
// and cached in the statics below.
// ============================================================================

// The line series and the raster's time axis share one length, so a given x maps
// to the same place in both plots — which is what lets them zoom and pan as a
// single linked view on the client.
const SERIES_LEN: usize = 150_000;

static LINE: OnceLock<LineData> = OnceLock::new();
static SAT: OnceLock<(SummedAreaTable, usize, usize)> = OnceLock::new();

// Cheap LCG used only to sprinkle pseudo-random grain onto the synthetic data,
// so statistical quality doesn't matter.
fn lcg(state: &mut u64) -> f64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*state >> 33) as f64) / (u32::MAX as f64) - 0.5
}

// --- Line series ---

struct LineData {
    series: Vec<f64>,
    n: usize,
    ymin: f64,
    ymax: f64,
}

fn line_data() -> &'static LineData {
    LINE.get_or_init(|| {
        let n = SERIES_LEN;
        let mut s = 0xC0FFEEu64;
        let mut series = Vec::with_capacity(n);
        let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in 0..n {
            let x = i as f64;
            // A sum of sines at geometrically spaced frequencies with roughly 1/f
            // amplitudes, so the curve carries structure at every zoom level: a
            // broad shape, medium swells, and progressively finer ripples that
            // only resolve as you zoom in (instead of just reading as noise).
            let v = (x * 0.00035).sin()         // broad shape (~8 periods overall)
                + 0.45 * (x * 0.0019).sin()     // medium undulation
                + 0.22 * (x * 0.011).sin()      // fine waviness
                + 0.10 * (x * 0.052).sin()      // detail that resolves on deep zoom
                + 0.05 * lcg(&mut s);
            if v < ymin {
                ymin = v;
            }
            if v > ymax {
                ymax = v;
            }
            series.push(v);
        }
        LineData { series, n, ymin, ymax }
    })
}

// --- Heatmap field ---
//
// hash2 -> value_noise -> fbm build up an isotropic fractal-noise texture, which
// sat_data layers onto a sinusoidal density ridge to produce the raster, then
// folds into a summed-area table for constant-time tile queries.

// Hash a lattice point to a pseudo-random value in [0, 1).
fn hash2(xi: i64, yi: i64) -> f64 {
    let mut h = (xi as u64).wrapping_mul(0x9E3779B97F4A7C15)
        ^ (yi as u64).wrapping_mul(0xC2B2AE3D27D4EB4F);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58476D1CE4E5B9);
    h ^= h >> 32;
    (h >> 11) as f64 / ((1u64 << 53) as f64)
}

// Smooth 2D value noise: lattice hashes blended with a smoothstep falloff.
fn value_noise(x: f64, y: f64) -> f64 {
    let x0 = x.floor();
    let y0 = y.floor();
    let (xi, yi) = (x0 as i64, y0 as i64);
    let sx = {
        let t = x - x0;
        t * t * (3.0 - 2.0 * t)
    };
    let sy = {
        let t = y - y0;
        t * t * (3.0 - 2.0 * t)
    };
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let top = lerp(hash2(xi, yi), hash2(xi + 1, yi), sx);
    let bot = lerp(hash2(xi, yi + 1), hash2(xi + 1, yi + 1), sx);
    lerp(top, bot, sy)
}

// Fractal (1/f) noise: octaves of value noise at halving amplitude. Isotropic
// detail at every scale, with no directional banding.
fn fbm(mut x: f64, mut y: f64) -> f64 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    for _ in 0..5 {
        sum += amp * value_noise(x, y);
        x *= 2.0;
        y *= 2.0;
        amp *= 0.5;
    }
    sum
}

fn sat_data() -> &'static (SummedAreaTable, usize, usize) {
    SAT.get_or_init(|| {
        let n_rows = SERIES_LEN;
        let n_cols = 128usize;
        let mut s = 0xBADC0DEu64;
        let mut rows: Vec<Vec<u64>> = Vec::with_capacity(n_rows);
        for i in 0..n_rows {
            let x = i as f64;
            // A bright ridge of density snakes through the value axis over time,
            // wiggling at geometrically spaced frequencies — from coarse swings
            // down to few-row ripples — so zooming the time axis keeps resolving
            // fresh undulations all the way to the native row scale.
            let center = 64.0
                + 30.0 * (x * 0.0012).sin()
                + 14.0 * (x * 0.0070).sin()
                + 7.0 * (x * 0.0260).sin()
                + 3.5 * (x * 0.0900).sin()
                + 2.0 * (x * 0.3100).sin()
                + 1.1 * (x * 0.8300).sin()
                + 0.6 * (x * 1.9000).sin();
            // The ridge also breathes thicker and thinner over time.
            let width = 14.0 + 7.0 * (x * 0.0040).sin();
            let mirror = n_cols as f64 - center;
            let mut row = Vec::with_capacity(n_cols);
            for j in 0..n_cols {
                let y = j as f64;
                let d0 = (y - center) / width;
                let d1 = (y - mirror) / (width * 1.4);
                // Primary ridge + fainter mirror band.
                let mut v = 220.0 * (-0.5 * d0 * d0).exp();
                v += 70.0 * (-0.5 * d1 * d1).exp();
                // Organic fractal-noise texture: multi-scale detail that stays crisp
                // when zoomed in and softens to clouds when zoomed out, with none of
                // the directional banding a sum of plane waves would give.
                v += 55.0 * fbm(x * 0.02, y * 0.03);
                // Per-cell white-noise grain. Its mean is ~0, so it averages away as
                // tiles cover more cells (zoomed out) but shows as fine speckle once
                // you zoom in toward the native resolution.
                v += 28.0 * lcg(&mut s);
                row.push(v.max(0.0) as u64);
            }
            rows.push(row);
        }
        (SummedAreaTable::new(&rows), n_rows, n_cols)
    })
}
