//! Staggered photosite lattices (Fujifilm SuperCCD, DNG `CFALayout` 2-5) for RGB outputs.
//!
//! Raw outputs keep the samples as stored. In a staggered buffer every other row (or
//! column) sits half a photosite to the side of its neighbours, and the photosites are not
//! square: a stored grid developed as is looks squeezed by 2 in one direction. RGB outputs
//! are developed in the stored grid, then resampled onto square output pixels at the true
//! photosite positions:
//!
//! - SuperCCD layout 1 (`fuji_rotate` = `layout1`): stored rows are lines of the 45-degree
//!   lattice. Stored sample `(row, col)` of the active area lies at
//!   `y = row / sqrt2`, `x = (2 col + (row & 1)) / sqrt2` output pixels (odd rows half a
//!   column to the right); the output is `width * sqrt2` by `height / sqrt2`, as many
//!   pixels as the active area holds samples.
//! - SuperCCD layout 0 (`layout0`): the same with rows and columns exchanged (odd columns
//!   half a row lower).
//! - DNG `CFALayout` 2/3 (even columns half a row lower / higher) and 4/5 (even rows half a
//!   column right / left), with `DefaultScale` giving the photosite pitch.

use crate::process::Developed;
use flashraw_core::{FrameInfo, RawMetadata, Rect, SensorLayout, par};

/// Geometry of a staggered buffer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Stagger {
    /// The offset lines are rows (shifted horizontally); otherwise columns (shifted vertically).
    pub rows: bool,
    /// Sensor-buffer index (row or column) of a line that is not offset.
    pub anchor: u32,
    /// Output pixels per stored sample along a line.
    pub along: f64,
    /// Output pixels per line across the lines.
    pub across: f64,
    /// Alternate lines are offset (false for half-size developments, whose pixels average
    /// two lines).
    pub offset: bool,
}

/// The staggered geometry of a frame, `None` for regular grids.
pub(crate) fn of(meta: &RawMetadata, info: &FrameInfo) -> Option<Stagger> {
    if info.layout != SensorLayout::OtherCfa {
        return None;
    }
    let s2 = std::f64::consts::SQRT_2;
    let a = info.active_area;
    match meta.extra("fuji_rotate") {
        Some("layout1") => return Some(Stagger { rows: true, anchor: a.y, along: s2, across: 1.0 / s2, offset: true }),
        Some("layout0") => {
            return Some(Stagger { rows: false, anchor: a.x, along: s2, across: 1.0 / s2, offset: true });
        }
        _ => {}
    }
    let layout: u32 = meta.extra("cfa_layout")?.parse().ok()?;
    let (sx, sy) = meta
        .dng
        .as_ref()
        .and_then(|d| d.default_scale)
        .filter(|&(x, y)| x.is_finite() && y.is_finite() && x > 0.0 && y > 0.0)
        .unwrap_or((1.0, 1.0));
    // Normalise so the output is never smaller than the stored grid in either direction.
    let m = sx.min(sy);
    let (sx, sy) = (sx / m, sy / m);
    // Column/row 0 is "even": layouts 2 and 4 offset it, 3 and 5 its neighbours (the offset
    // of the even lines in the opposite direction is the same lattice shifted).
    match layout {
        2 => Some(Stagger { rows: false, anchor: 1, along: sy, across: sx, offset: true }),
        3 => Some(Stagger { rows: false, anchor: 0, along: sy, across: sx, offset: true }),
        4 => Some(Stagger { rows: true, anchor: 1, along: sx, across: sy, offset: true }),
        5 => Some(Stagger { rows: true, anchor: 0, along: sx, across: sy, offset: true }),
        _ => None,
    }
}

impl Stagger {
    /// Output size for a developed `w x h` window.
    pub fn output_size(&self, w: u32, h: u32) -> (u32, u32) {
        let (n_along, n_lines) = if self.rows { (w, h) } else { (h, w) };
        let along = ((n_along as f64 - 0.5).max(0.0) * self.along).floor() as u32 + 1;
        let across = ((n_lines.saturating_sub(1)) as f64 * self.across).floor() as u32 + 1;
        if self.rows { (along, across) } else { (across, along) }
    }

    /// Resample `d` (developed linear RGB of the sensor window `win`) onto square pixels.
    pub fn resample(&self, d: &Developed, win: Rect, parallel: bool) -> Developed {
        let (w, h) = (d.width as usize, d.height as usize);
        let (ow, oh) = self.output_size(d.width, d.height);
        let mut out = vec![0f32; ow as usize * oh as usize * 3];
        if w == 0 || h == 0 {
            return Developed { width: ow, height: oh, rgb: out };
        }
        // Lines and positions along them in the developed window.
        let (n_lines, n_along) = if self.rows { (h, w) } else { (w, h) };
        let first_line = if self.rows { win.y } else { win.x };
        let offset = |l: usize| {
            if self.offset { ((first_line as i64 + l as i64 - self.anchor as i64) & 1) as f64 * 0.5 } else { 0.0 }
        };
        let px = |l: usize, k: usize| {
            let i = if self.rows { l * w + k } else { k * w + l };
            [d.rgb[i * 3], d.rgb[i * 3 + 1], d.rgb[i * 3 + 2]]
        };
        // Sample of line `l` at continuous position `t` (stored samples) along it.
        let along = |l: usize, t: f64| {
            let t = (t - offset(l)).clamp(0.0, (n_along - 1) as f64);
            let k0 = t.floor() as usize;
            let k1 = (k0 + 1).min(n_along - 1);
            let f = (t - k0 as f64) as f32;
            let (a, b) = (px(l, k0), px(l, k1));
            [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]
        };
        let row_len = ow as usize * 3;
        par::for_each_row_chunk(&mut out, row_len, 16, parallel, |first, chunk| {
            for (i, row) in chunk.chunks_mut(row_len).enumerate() {
                let y = first + i;
                for (x, o) in row.chunks_exact_mut(3).enumerate() {
                    let (u, v) = if self.rows { (x, y) } else { (y, x) };
                    let lf = (v as f64 / self.across).clamp(0.0, (n_lines - 1) as f64);
                    let l0 = lf.floor() as usize;
                    let l1 = (l0 + 1).min(n_lines - 1);
                    let f = (lf - l0 as f64) as f32;
                    let t = u as f64 / self.along;
                    let (a, b) = (along(l0, t), along(l1, t));
                    for c in 0..3 {
                        o[c] = a[c] + (b[c] - a[c]) * f;
                    }
                }
            }
        });
        Developed { width: ow, height: oh, rgb: out }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn developed(w: u32, h: u32, f: impl Fn(u32, u32) -> f32) -> Developed {
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                rgb.extend_from_slice(&[f(x, y); 3]);
            }
        }
        Developed { width: w, height: h, rgb }
    }

    #[test]
    fn superccd_layout1_geometry() {
        let s = Stagger {
            rows: true,
            anchor: 0,
            along: std::f64::consts::SQRT_2,
            across: std::f64::consts::FRAC_1_SQRT_2,
            offset: true,
        };
        // As many output pixels as samples, aspect 2 : 1 relative to the stored grid.
        let (ow, oh) = s.output_size(1000, 2000);
        assert_eq!((ow, oh), (1414, 1414));
        // A field that is linear in the true photosite position stays linear: the stored
        // value of sample (row, col) is its output x coordinate.
        let x_of = |c: u32, r: u32| (2.0 * c as f32 + (r & 1) as f32) / std::f32::consts::SQRT_2;
        let d = developed(100, 200, x_of);
        let o = s.resample(&d, Rect::new(0, 0, 100, 200), false);
        for y in [3usize, 50, 120] {
            for x in [10usize, 40, 130] {
                let v = o.rgb[(y * o.width as usize + x) * 3];
                assert!((v - x as f32).abs() < 1e-3, "({x}, {y}) -> {v}");
            }
        }
        // The window parity follows the sensor position: an odd first row swaps the offsets.
        let d = developed(100, 200, |c, r| x_of(c, r + 1));
        let o = s.resample(&d, Rect::new(0, 1, 100, 200), false);
        let v = o.rgb[(10 * o.width as usize + 40) * 3];
        assert!((v - 40.0).abs() < 1e-3, "{v}");
    }

    #[test]
    fn layout0_is_layout1_transposed() {
        let s1 = Stagger { rows: true, anchor: 0, along: 1.5, across: 0.75, offset: true };
        let s0 = Stagger { rows: false, ..s1 };
        let f = |a: u32, b: u32| (a * 7 + b * 3) as f32 + (a % 3) as f32;
        let (w, h) = (31u32, 17u32);
        let a = s1.resample(&developed(w, h, f), Rect::new(0, 0, w, h), true);
        let b = s0.resample(&developed(h, w, |x, y| f(y, x)), Rect::new(0, 0, h, w), true);
        assert_eq!((a.width, a.height), (b.height, b.width));
        for y in 0..a.height as usize {
            for x in 0..a.width as usize {
                assert_eq!(a.rgb[(y * a.width as usize + x) * 3], b.rgb[(x * b.width as usize + y) * 3]);
            }
        }
    }
}
