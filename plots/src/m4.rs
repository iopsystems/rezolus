//! M4 line downsampling.
//!
//! To draw a long series into a `width`-pixel-wide plot, M4 divides the x-range
//! into `width` buckets (one per pixel column) and keeps just four samples from
//! each: the first, last, minimum, and maximum. That bounds the output to about
//! `4 * width` points regardless of series length, while preserving the line's
//! visual envelope — every peak and trough that would be visible survives.
//!
//! See https://observablehq.com/@uwdata/m4-scalable-time-series-visualization

#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

pub fn m4(values: &[f64], x0: f64, x1: f64, width: usize) -> Vec<Point> {
    let n = values.len();
    if n == 0 || width == 0 || x1 <= x0 {
        return Vec::new();
    }

    let start = (x0.floor() as i64).max(0) as usize;
    let end = (x1.ceil() as i64).max(0) as usize;
    let end = end.min(n);
    if end <= start {
        return Vec::new();
    }

    if width >= end - start {
        return (start..end)
            .map(|i| Point {
                x: i as f64,
                y: values[i],
            })
            .collect();
    }

    let bucket_of = |x: f64| -> usize {
        let k = (width as f64 * (x - x0) / (x1 - x0)).floor() as i64;
        k.max(0).min(width as i64 - 1) as usize
    };

    #[derive(Clone, Copy)]
    struct Agg {
        first_i: usize,
        last_i: usize,
        min_i: usize,
        max_i: usize,
        min_v: f64,
        max_v: f64,
    }

    let mut buckets = vec![
        Agg {
            first_i: 0,
            last_i: 0,
            min_i: 0,
            max_i: 0,
            min_v: 0.0,
            max_v: 0.0,
        };
        width
    ];
    let mut used = vec![false; width];

    for (i, &v) in values.iter().enumerate().skip(start).take(end - start) {
        let k = bucket_of(i as f64);
        if !used[k] {
            used[k] = true;
            buckets[k] = Agg {
                first_i: i,
                last_i: i,
                min_i: i,
                max_i: i,
                min_v: v,
                max_v: v,
            };
            continue;
        }
        let a = &mut buckets[k];
        a.last_i = i;
        if v < a.min_v {
            a.min_v = v;
            a.min_i = i;
        }
        if v > a.max_v {
            a.max_v = v;
            a.max_i = i;
        }
    }

    let len = end - start;
    let mut keep = vec![false; len];
    keep[0] = true;
    keep[len - 1] = true;
    for k in 0..width {
        if !used[k] {
            continue;
        }
        let a = buckets[k];
        keep[a.first_i - start] = true;
        keep[a.last_i - start] = true;
        keep[a.min_i - start] = true;
        keep[a.max_i - start] = true;
    }

    let mut pts = Vec::with_capacity(len.min(4 * width + 2));
    for (i, &ok) in keep.iter().enumerate() {
        if ok {
            let idx = start + i;
            pts.push(Point {
                x: idx as f64,
                y: values[idx],
            });
        }
    }
    pts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_series() {
        // values: [1, 5, 2, 8, 3], width = 2
        // n=5, start=0, end=5, width(2) < 5
        // buckets: k = floor(2 * i / 5)
        //   i=0 -> 0; i=1 -> 0; i=2 -> 0; i=3 -> 1; i=4 -> 1
        // bucket 0: first=0(1), last=2(2), min=2(2), max=1(5)
        // bucket 1: first=3(8), last=4(3), min=4(3), max=3(8)
        // keep flags: 0,1,2,3,4 all marked => all 5 points returned
        let v = [1.0, 5.0, 2.0, 8.0, 3.0];
        let pts = m4(&v, 0.0, 5.0, 2);
        let xs: Vec<f64> = pts.iter().map(|p| p.x).collect();
        let ys: Vec<f64> = pts.iter().map(|p| p.y).collect();
        assert_eq!(xs, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!(ys, vec![1.0, 5.0, 2.0, 8.0, 3.0]);
    }
}
