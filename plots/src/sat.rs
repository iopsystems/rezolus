//! Density heatmaps from a summed-area table.
//!
//! A summed-area table (a 2D prefix sum) is built once over the whole field,
//! after which the total of any axis-aligned rectangle is four array lookups. To
//! render a view, [`SummedAreaTable::query_tiles`] snaps the requested extent to
//! a grid of power-of-two-sized tiles and returns one rectangle sum per tile;
//! [`scale_pixels`] then maps each tile's density through a tone curve to a
//! palette index. Cost scales with the tile grid (a screenful), not the field
//! size, so every zoom level renders from the same prepared table.

#[derive(Clone, Copy, Debug, Default)]
pub struct Extent {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Size {
    pub width: usize,
    pub height: usize,
}

#[derive(Debug, Default)]
pub struct TileResult {
    pub extent: Extent,
    pub size: Size,
    pub values: Vec<i64>,
    pub tile_area: f64,
}

pub struct SummedAreaTable {
    data: Vec<u64>,
    n_rows: usize,
    n_cols: usize,
}

impl SummedAreaTable {
    pub fn new(rows: &[Vec<u64>]) -> Self {
        let n_rows = rows.len();
        if n_rows == 0 {
            return Self {
                data: Vec::new(),
                n_rows: 0,
                n_cols: 0,
            };
        }
        let n_cols = rows[0].len();
        let mut data = vec![0u64; n_rows * n_cols];
        for (i, row) in rows.iter().enumerate() {
            let off = i * n_cols;
            data[off..off + n_cols].copy_from_slice(row);
            for j in 1..n_cols {
                data[off + j] = data[off + j].wrapping_add(data[off + j - 1]);
            }
        }
        for i in 1..n_rows {
            for j in 0..n_cols {
                data[i * n_cols + j] =
                    data[i * n_cols + j].wrapping_add(data[(i - 1) * n_cols + j]);
            }
        }
        Self {
            data,
            n_rows,
            n_cols,
        }
    }

    pub fn query_tiles(&self, ext: Extent, screen: Size) -> TileResult {
        let sw = screen.width;
        let sh = screen.height;
        if ext.x1 <= ext.x0 || ext.y1 <= ext.y0 || sw == 0 || sh == 0 {
            return TileResult::default();
        }

        let sx = (((ext.x1 - ext.x0) / sw as f64).log2().ceil())
            .exp2()
            .max(1.0);
        let sy = (((ext.y1 - ext.y0) / sh as f64).log2().ceil())
            .exp2()
            .max(1.0);

        let snap_x0 = (ext.x0 / sx).floor() * sx;
        let snap_x1 = (ext.x1 / sx).ceil() * sx;
        let snap_y0 = (ext.y0 / sy).floor() * sy;
        let snap_y1 = (ext.y1 / sy).ceil() * sy;
        let tiles_x = ((snap_x1 - snap_x0) / sx).round() as usize;
        let tiles_y = ((snap_y1 - snap_y0) / sy).round() as usize;

        let boundary_rows: Vec<i64> = (0..=tiles_x)
            .map(|i| snap_x0 as i64 - 1 + (i as i64) * (sx as i64))
            .collect();
        let boundary_cols: Vec<i64> = (0..=tiles_y)
            .map(|i| snap_y0 as i64 - 1 + (i as i64) * (sy as i64))
            .collect();

        // grid[ri][ci]
        let mut grid = vec![vec![0i64; tiles_y + 1]; tiles_x + 1];
        for (ri, &r) in boundary_rows.iter().enumerate() {
            if r < 0 {
                continue;
            }
            let row_idx = (r as usize).min(self.n_rows - 1);
            let row_off = row_idx * self.n_cols;
            for (ci, &c) in boundary_cols.iter().enumerate() {
                if c < 0 {
                    grid[ri][ci] = 0;
                } else {
                    let col_idx = (c as usize).min(self.n_cols - 1);
                    grid[ri][ci] = self.data[row_off + col_idx] as i64;
                }
            }
        }

        let mut values = vec![0i64; tiles_x * tiles_y];
        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                values[ty * tiles_x + tx] =
                    grid[tx + 1][ty + 1] - grid[tx][ty + 1] - grid[tx + 1][ty] + grid[tx][ty];
            }
        }

        TileResult {
            extent: Extent {
                x0: snap_x0,
                y0: snap_y0,
                x1: snap_x1,
                y1: snap_y1,
            },
            size: Size {
                width: tiles_x,
                height: tiles_y,
            },
            values,
            tile_area: sx * sy,
        }
    }
}

/// Map tile densities to colormap indices `[0, 255]`. The tone response is a
/// 50/50 blend of a log curve (lifts the low end, compressing the tonal range)
/// and a linear curve (preserves the dynamic range so darks stay dark and bright
/// peaks pop) — keeping faint structure visible without washing out highlights.
pub fn scale_pixels(result: &TileResult, max_density: f64) -> Vec<u8> {
    let mut pixels = vec![0u8; result.values.len()];
    if max_density <= 0.0 || result.tile_area <= 0.0 {
        return pixels;
    }
    let log_max = max_density.ln_1p();
    for (i, &v) in result.values.iter().enumerate() {
        if v > 0 {
            let density = v as f64 / result.tile_area;
            let log_t = if log_max > 0.0 {
                density.ln_1p() / log_max
            } else {
                0.0
            };
            let lin_t = (density / max_density).min(1.0);
            let t = (log_t + lin_t) / 2.0;
            pixels[i] = (t * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    pixels
}
