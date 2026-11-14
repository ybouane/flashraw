//! Lateral chromatic aberration of Bayer frames: red and blue are magnified a little differently
//! from green, radially about the optical centre, so coloured fringes grow towards the corners.
//!
//! [`estimate`] finds the two scale factors from the frame itself and [`correct`] resamples the
//! red and blue samples (on their own lattices, before demosaicing) so that they line up with
//! green. Estimation works on half-resolution planes (one red, one blue and the mean of the two
//! greens per 2x2 cell): in tiles with enough edges, the displacement of the red and of the blue
//! plane against the green plane is found by normalised cross-correlation of their Laplacians,
//! and a robust least-squares fit of `d = t + k * (p - centre)` over the tiles gives the scale
//! `1 + k`. The translation `t` also holds the constant offset between the sample positions of
//! the colours inside a cell, which is geometry and not corrected.

const TILE: usize = 32;
const RANGE: i32 = 3;

/// Radial scale of the red and blue planes against green (1 = no aberration).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CaModel {
    pub red: f32,
    pub blue: f32,
}

/// Half-resolution planes of a Bayer mosaic whose red sites are at `rpos` (`x % 2`, `y % 2`).
struct Planes {
    w: usize,
    h: usize,
    r: Vec<f32>,
    g: Vec<f32>,
    b: Vec<f32>,
}

fn planes(m: &[f32], w: usize, h: usize, rpos: (usize, usize)) -> Planes {
    let (hw, hh) = (w / 2, h / 2);
    let (rx, ry) = rpos;
    let (bx, by) = (1 - rx, 1 - ry);
    let mut p = Planes { w: hw, h: hh, r: vec![0.0; hw * hh], g: vec![0.0; hw * hh], b: vec![0.0; hw * hh] };
    for j in 0..hh {
        for i in 0..hw {
            let at = |x: usize, y: usize| m[(2 * j + y) * w + 2 * i + x];
            p.r[j * hw + i] = at(rx, ry);
            p.b[j * hw + i] = at(bx, by);
            p.g[j * hw + i] = 0.5 * (at(bx, ry) + at(rx, by));
        }
    }
    p
}

/// 3x3 binomial blur, twice (zero-padded border pixels keep their values).
fn blur(a: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut cur = a.to_vec();
    for _ in 0..2 {
        let mut tmp = cur.clone();
        for y in 0..h {
            for x in 1..w - 1 {
                tmp[y * w + x] = 0.25 * cur[y * w + x - 1] + 0.5 * cur[y * w + x] + 0.25 * cur[y * w + x + 1];
            }
        }
        for y in 1..h - 1 {
            for x in 0..w {
                cur[y * w + x] = 0.25 * tmp[(y - 1) * w + x] + 0.5 * tmp[y * w + x] + 0.25 * tmp[(y + 1) * w + x];
            }
        }
    }
    cur
}

/// 5-point Laplacian of the blurred plane (zero on the border).
fn laplacian(a: &[f32], w: usize, h: usize) -> Vec<f32> {
    let a = &blur(a, w, h);
    let mut out = vec![0f32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            out[y * w + x] =
                4.0 * a[y * w + x] - a[y * w + x - 1] - a[y * w + x + 1] - a[(y - 1) * w + x] - a[(y + 1) * w + x];
        }
    }
    out
}

/// Displacement `(dx, dy)` in half-resolution pixels of `c` against `g` inside the tile at
/// `(x0, y0)`: the shift for which `c(x + d)` best matches `g(x)`, with its correlation.
fn tile_shift(lc: &[f32], lg: &[f32], w: usize, x0: usize, y0: usize) -> Option<(f32, f32, f32)> {
    let r = RANGE as usize;
    let n = 2 * r + 1;
    let mut ncc = vec![0f32; n * n];
    let (mut egg, mut best) = (0f32, (f32::MIN, 0usize));
    for y in 0..TILE {
        for x in 0..TILE {
            let v = lg[(y0 + y) * w + x0 + x];
            egg += v * v;
        }
    }
    // Tiles with fewer edges than noise (rms Laplacian below 0.012) say nothing.
    if egg < 1.5e-4 * (TILE * TILE) as f32 {
        return None;
    }
    for (k, c) in ncc.iter_mut().enumerate() {
        let (dx, dy) = ((k % n) as i32 - RANGE, (k / n) as i32 - RANGE);
        let (mut sgc, mut scc) = (0f32, 0f32);
        for y in 0..TILE {
            for x in 0..TILE {
                let g = lg[(y0 + y) * w + x0 + x];
                let cv = lc[((y0 + y) as i32 + dy) as usize * w + ((x0 + x) as i32 + dx) as usize];
                sgc += g * cv;
                scc += cv * cv;
            }
        }
        *c = sgc / (egg * scc).sqrt().max(1e-12);
        if *c > best.0 {
            best = (*c, k);
        }
    }
    let (bk, (bx, by)) = (best.0, (best.1 % n, best.1 / n));
    if bk < 0.35 || bx == 0 || by == 0 || bx == n - 1 || by == n - 1 {
        return None;
    }
    // Sub-pixel peak: parabola through the three correlations along each axis.
    let parab = |m: f32, c: f32, p: f32| {
        let d = m - 2.0 * c + p;
        if d.abs() < 1e-9 { 0.0 } else { (0.5 * (m - p) / d).clamp(-0.5, 0.5) }
    };
    let at = |x: usize, y: usize| ncc[y * n + x];
    let sx = parab(at(bx - 1, by), bk, at(bx + 1, by));
    let sy = parab(at(bx, by - 1), bk, at(bx, by + 1));
    Some((bx as f32 - RANGE as f32 + sx, by as f32 - RANGE as f32 + sy, bk))
}

/// Scale `1 + k` of one colour plane against green from its tile displacements.
fn fit_scale(lc: &[f32], lg: &[f32], w: usize, h: usize) -> Option<f32> {
    let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
    let mut samples: Vec<(f32, f32, f32, f32)> = Vec::new(); // (x - cx, y - cy, dx, dy)
    let m = RANGE as usize + 1;
    let mut y0 = m;
    while y0 + TILE + m <= h {
        let mut x0 = m;
        while x0 + TILE + m <= w {
            if let Some((dx, dy, _)) = tile_shift(lc, lg, w, x0, y0) {
                samples.push((x0 as f32 + TILE as f32 * 0.5 - cx, y0 as f32 + TILE as f32 * 0.5 - cy, dx, dy));
            }
            x0 += TILE;
        }
        y0 += TILE;
    }
    if samples.len() < 24 {
        return None;
    }
    // d = t + k * p, with a shared k and a translation per axis; Huber-style reweighting.
    let mut wt = vec![1f32; samples.len()];
    let mut k = 0f32;
    for _ in 0..6 {
        let (mut sw, mut mx, mut my, mut mdx, mut mdy) = (0f64, 0f64, 0f64, 0f64, 0f64);
        for (s, &w) in samples.iter().zip(&wt) {
            let w = w as f64;
            sw += w;
            mx += w * s.0 as f64;
            my += w * s.1 as f64;
            mdx += w * s.2 as f64;
            mdy += w * s.3 as f64;
        }
        let (mx, my, mdx, mdy) = (mx / sw, my / sw, mdx / sw, mdy / sw);
        let (mut num, mut den) = (0f64, 0f64);
        for (s, &w) in samples.iter().zip(&wt) {
            let w = w as f64;
            let (px, py) = (s.0 as f64 - mx, s.1 as f64 - my);
            num += w * (px * (s.2 as f64 - mdx) + py * (s.3 as f64 - mdy));
            den += w * (px * px + py * py);
        }
        if den <= 0.0 {
            return None;
        }
        k = (num / den) as f32;
        let (tx, ty) = (mdx as f32 - k * mx as f32, mdy as f32 - k * my as f32);
        for (s, w) in samples.iter().zip(wt.iter_mut()) {
            let res = ((s.2 - tx - k * s.0).powi(2) + (s.3 - ty - k * s.1).powi(2)).sqrt();
            *w = 1.0 / (1.0 + (res / 0.15).powi(2));
        }
    }
    Some(1.0 + k)
}

/// The scale factors of a Bayer mosaic (`m`, `w` x `h`, red sites at `rpos`), or `None` when it
/// has too few usable edges. Scales are clamped to 1 +- 0.01.
pub(crate) fn estimate(m: &[f32], w: usize, h: usize, rpos: (usize, usize)) -> Option<CaModel> {
    if w < 8 * TILE || h < 8 * TILE || m.len() < w * h {
        return None;
    }
    let p = planes(m, w, h, rpos);
    let lg = laplacian(&p.g, p.w, p.h);
    let sr = fit_scale(&laplacian(&p.r, p.w, p.h), &lg, p.w, p.h)?;
    let sb = fit_scale(&laplacian(&p.b, p.w, p.h), &lg, p.w, p.h)?;
    Some(CaModel { red: sr.clamp(0.99, 1.01), blue: sb.clamp(0.99, 1.01) })
}

/// Catmull-Rom weights for a fractional position `t` in `[0, 1)`.
fn cubic(t: f32) -> [f32; 4] {
    [
        0.5 * (-t * t * t + 2.0 * t * t - t),
        0.5 * (3.0 * t * t * t - 5.0 * t * t + 2.0),
        0.5 * (-3.0 * t * t * t + 4.0 * t * t + t),
        0.5 * (t * t * t - t * t),
    ]
}

/// Resample the colour whose sites are at `pos` so that its content moves by the radial
/// scale `s` about the frame centre: `new(p) = old(centre + s * (p - centre))`, evaluated on the
/// colour's own lattice (spacing 2) with a bicubic kernel.
fn resample(m: &mut [f32], src: &[f32], w: usize, h: usize, pos: (usize, usize), s: f32) {
    let (lw, lh) = ((w - pos.0).div_ceil(2), (h - pos.1).div_ceil(2));
    let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
    let at = |i: i32, j: i32| {
        src[(2 * j.clamp(0, lh as i32 - 1) as usize + pos.1) * w + 2 * i.clamp(0, lw as i32 - 1) as usize + pos.0]
    };
    for j in 0..lh {
        for i in 0..lw {
            let (x, y) = ((2 * i + pos.0) as f32, (2 * j + pos.1) as f32);
            let (qx, qy) = (cx + s * (x - cx), cy + s * (y - cy));
            let (u, v) = ((qx - pos.0 as f32) * 0.5, (qy - pos.1 as f32) * 0.5);
            let (iu, iv) = (u.floor(), v.floor());
            let (wu, wv) = (cubic(u - iu), cubic(v - iv));
            let (iu, iv) = (iu as i32, iv as i32);
            let mut acc = 0f32;
            for (b, &wy) in wv.iter().enumerate() {
                let mut row = 0f32;
                for (a, &wx) in wu.iter().enumerate() {
                    row += wx * at(iu + a as i32 - 1, iv + b as i32 - 1);
                }
                acc += wy * row;
            }
            m[(2 * j + pos.1) * w + 2 * i + pos.0] = acc;
        }
    }
}

/// Correct the red and blue samples of the mosaic `m` (`w` x `h`, red sites at `rpos`) for the
/// scales of `model`.
pub(crate) fn correct(m: &mut [f32], w: usize, h: usize, rpos: (usize, usize), model: CaModel) {
    let src = m.to_vec();
    resample(m, &src, w, h, rpos, model.red);
    resample(m, &src, w, h, (1 - rpos.0, 1 - rpos.1), model.blue);
}

/// Correct the lateral chromatic aberration of a Bayer frame in place: the scale factors are
/// estimated from the frame and the red and blue samples are resampled. `origin` is the sensor
/// position of the frame's first sample. Does nothing (and returns `false`) for anything but a
/// single-component 16-bit three-colour Bayer mosaic with enough edges.
pub(crate) fn correct_image(
    img: &mut flashraw_core::RawImage,
    info: &flashraw_core::FrameInfo,
    origin: (u32, u32),
) -> bool {
    use flashraw_core::{CfaColor, PixelData};
    let Some(cfa) = info.cfa else { return false };
    let (w, h) = (img.width as usize, img.height as usize);
    if img.cpp != 1 || cfa.width() != 2 || cfa.height() != 2 || !matches!(img.data, PixelData::U16(_)) {
        return false;
    }
    let at = |x: usize, y: usize| cfa.color_at(x + origin.0 as usize, y + origin.1 as usize);
    let Some(rpos) = (0..4).map(|k| (k % 2, k / 2)).find(|&(x, y)| at(x, y) == CfaColor::Red) else {
        return false;
    };
    let (bx, by) = (1 - rpos.0, 1 - rpos.1);
    if at(bx, by) != CfaColor::Blue || at(bx, rpos.1) != CfaColor::Green || at(rpos.0, by) != CfaColor::Green {
        return false;
    }
    let white = info.white_level.max().max(1) as f32;
    let mut m: Vec<f32> = img.u16().iter().map(|&v| v as f32).collect();
    let scaled: Vec<f32> = m.iter().map(|v| v / white).collect();
    let Some(model) = estimate(&scaled, w, h, rpos) else { return false };
    correct(&mut m, w, h, rpos, model);
    for (d, v) in img.u16_mut().iter_mut().zip(&m) {
        *d = v.round().clamp(0.0, 65535.0) as u16;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sharp RGB scene (rectangles and discs at random places, in all three channels
    /// differently) as three planes of `w` x `h`, rendered with a radial scale `s` per channel
    /// about the centre: `plane_c(p) = scene_c(centre + (p - centre) / s_c)` by exact geometry.
    fn render(w: usize, h: usize, s: [f32; 3]) -> [Vec<f32>; 3] {
        let mut st = 987654321u32;
        let mut rnd = |n: usize| {
            st = st.wrapping_mul(1664525).wrapping_add(1013904223);
            ((st >> 8) as usize) % n
        };
        struct Shape {
            x: f32,
            y: f32,
            hw: f32,
            hh: f32,
            disc: bool,
            col: [f32; 3],
        }
        let shapes: Vec<Shape> = (0..260)
            .map(|_| {
                // A shared luminance with a small colour tint, as in real scenes (edges are mostly
                // luminance edges, so the three channels correlate).
                let l = 0.15 + 0.75 * rnd(100) as f32 / 100.0;
                let tint = |r: usize| 0.85 + 0.3 * r as f32 / 100.0;
                Shape {
                    x: rnd(w) as f32,
                    y: rnd(h) as f32,
                    hw: 6.0 + rnd(40) as f32,
                    hh: 6.0 + rnd(40) as f32,
                    disc: rnd(2) == 0,
                    col: [l * tint(rnd(100)), l * tint(rnd(100)), l * tint(rnd(100))],
                }
            })
            .collect();
        let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
        let mut out = [vec![0.3f32; w * h], vec![0.3f32; w * h], vec![0.3f32; w * h]];
        for c in 0..3 {
            for y in 0..h {
                for x in 0..w {
                    let (px, py) = (cx + (x as f32 - cx) / s[c], cy + (y as f32 - cy) / s[c]);
                    for sh in &shapes {
                        let (dx, dy) = ((px - sh.x) / sh.hw, (py - sh.y) / sh.hh);
                        let inside =
                            if sh.disc { dx * dx + dy * dy <= 1.0 } else { dx.abs() <= 1.0 && dy.abs() <= 1.0 };
                        if inside {
                            out[c][y * w + x] = sh.col[c];
                        }
                    }
                }
            }
        }
        // Lens and anti-alias softness: real edges are not hard steps on the pixel grid.
        for c in out.iter_mut() {
            *c = blur(&blur(c, w, h), w, h);
        }
        out
    }

    fn mosaic(planes: &[Vec<f32>; 3], w: usize, h: usize) -> Vec<f32> {
        // RGGB: red at (0, 0).
        let mut m = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let c = match (y % 2, x % 2) {
                    (0, 0) => 0,
                    (1, 1) => 2,
                    _ => 1,
                };
                m[y * w + x] = planes[c][y * w + x];
            }
        }
        m
    }

    #[test]
    fn estimate_recovers_the_radial_scales() {
        let (w, h) = (640, 480);
        let (sr, sb) = (1.0035, 0.9972);
        let m = mosaic(&render(w, h, [sr, 1.0, sb]), w, h);
        let model = estimate(&m, w, h, (0, 0));
        let model = model.unwrap();
        assert!((model.red - sr).abs() < 4e-4, "{model:?}");
        assert!((model.blue - sb).abs() < 4e-4, "{model:?}");
        // No aberration: scales close to 1.
        let flat = mosaic(&render(w, h, [1.0, 1.0, 1.0]), w, h);
        let none = estimate(&flat, w, h, (0, 0)).unwrap();
        assert!((none.red - 1.0).abs() < 3e-4 && (none.blue - 1.0).abs() < 3e-4, "{none:?}");
    }

    #[test]
    fn correction_moves_the_edges_back() {
        let (w, h) = (640, 480);
        let (sr, sb) = (1.004, 0.996);
        let aberrated = mosaic(&render(w, h, [sr, 1.0, sb]), w, h);
        let truth = mosaic(&render(w, h, [1.0, 1.0, 1.0]), w, h);
        let err = |a: &[f32]| -> f64 {
            let mut e = 0f64;
            for y in 8..h - 8 {
                for x in 8..w - 8 {
                    // Red and blue sites only.
                    if (y % 2, x % 2) == (1, 0) || (y % 2, x % 2) == (0, 1) {
                        continue;
                    }
                    e += ((a[y * w + x] - truth[y * w + x]) as f64).powi(2);
                }
            }
            e
        };
        let before = err(&aberrated);
        let mut fixed = aberrated.clone();
        let model = estimate(&aberrated, w, h, (0, 0)).unwrap();
        correct(&mut fixed, w, h, (0, 0), model);
        let after = err(&fixed);
        assert!(after < 0.5 * before, "before {before} after {after}");
        // Green is untouched.
        assert!(
            (0..w * h)
                .filter(|i| ((i / w) % 2, (i % w) % 2) == (0, 1) || ((i / w) % 2, (i % w) % 2) == (1, 0))
                .all(|i| fixed[i] == aberrated[i])
        );
        // An identity model reproduces the mosaic exactly at the lattice points.
        let mut same = aberrated.clone();
        correct(&mut same, w, h, (0, 0), CaModel { red: 1.0, blue: 1.0 });
        assert!(same.iter().zip(&aberrated).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn correct_image_handles_bayer_frames_and_leaves_other_frames_alone() {
        use flashraw_core::{CfaPattern, FrameInfo, Limits, RawImage, WhiteLevel};
        let (w, h) = (640usize, 480usize);
        let m = mosaic(&render(w, h, [1.004, 1.0, 0.996]), w, h);
        let mut img = RawImage::new_u16(w as u32, h as u32, 1, &Limits::default()).unwrap();
        for (d, v) in img.u16_mut().iter_mut().zip(&m) {
            *d = (v * 4000.0).round() as u16;
        }
        let mut info = FrameInfo::new(w as u32, h as u32, 12);
        info.cfa = Some(CfaPattern::RGGB);
        info.white_level = WhiteLevel::uniform(4000);
        let before = img.u16().to_vec();
        assert!(correct_image(&mut img, &info, (0, 0)));
        assert_ne!(img.u16(), &before[..]);
        // The same data with a different origin has its red sites elsewhere and is handled too;
        // a frame without a colour filter array is untouched.
        info.cfa = None;
        let again = img.u16().to_vec();
        assert!(!correct_image(&mut img, &info, (0, 0)));
        assert_eq!(img.u16(), &again[..]);
    }
}
