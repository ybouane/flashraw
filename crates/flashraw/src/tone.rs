//! The tone of [`crate::Tone::Standard`]: the DNG `BaselineExposure` of the file, then one
//! S-curve shared by every camera, applied to the display outputs just before their encoding.
//!
//! The curve acts on linear ProPhoto RGB without shifting hues: the largest and the smallest
//! channel of a pixel go through the curve and the middle one keeps its relative place between
//! them, so `(r, g, b)` with `r >= g >= b` becomes `(f(r), f(b) + (g - b) (f(r) - f(b)) / (r - b),
//! f(b))`. Neutral pixels get `f` on every channel; a colour keeps its hue, and its saturation
//! follows the contrast of the curve (as a per-channel curve would), without the hue shifts of a
//! per-channel curve (orange skin, cyan skies). The method is the hue-preserving RGB tone curve of
//! the DNG specification's reference rendering; the control points of the curve were measured on
//! renderings of a raw developer's default settings (design note: `docs/design/tone.md`).
//!
//! The curve is a table of `f(u^2)` at evenly spaced `u` (finer steps in the shadows than a table
//! over the linear values), read with linear interpolation; the exposure only changes the scale of
//! the index, so the table is built once per process.

use crate::camera_profile::Curve;
use crate::color::{ColorSpace, Mat, inv, mul};
use crate::process::{Developed, Encode};
use std::sync::OnceLock;

/// Control points of the curve, `(input, output)`, both in gamma 2.2 encoded linear values
/// (`v^(1 / 2.2)`), through `(0, 0)` and `(1, 1)`; monotone cubic interpolation between them. They
/// are the linear inputs 0.001, 0.0025, 0.005, 0.01, 0.02, 0.035, 0.06, 0.1, 0.16, 0.25, 0.38,
/// 0.55 and 0.75 and the medians of what they became in the measured renderings (mid grey, 0.18,
/// goes to 0.39, +1.1 stops; 0.005 to 0.0018; 0.55 to 0.86), except 0.001, which few files reach:
/// it continues the slope of the shadows (to 0.000088, -3.5 stops).
const POINTS: [[f32; 2]; 15] = [
    [0.0, 0.0],
    [0.0433, 0.0144],
    [0.0657, 0.0312],
    [0.0900, 0.0566],
    [0.1233, 0.0837],
    [0.1689, 0.1434],
    [0.2179, 0.2269],
    [0.2784, 0.3356],
    [0.3511, 0.4669],
    [0.4347, 0.6136],
    [0.5325, 0.7515],
    [0.6442, 0.8626],
    [0.7620, 0.9324],
    [0.8774, 0.9663],
    [1.0, 1.0],
];

/// Gamma of the domain of [`POINTS`].
const GAMMA: f32 = 2.2;

/// Steps of the table (in `u = sqrt(x)`).
const SEGMENTS: usize = 1024;

/// Pixels per block of [`Toned::run`].
const BLOCK: usize = 64;

/// `[f(u_i^2), f(u_{i+1}^2) - f(u_i^2)]` for `u_i = i / SEGMENTS`: one read per lookup.
fn table() -> &'static [[f32; 2]; SEGMENTS] {
    static TABLE: OnceLock<Box<[[f32; 2]; SEGMENTS]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let c = Curve::new(&POINTS);
        let f = |i: usize| {
            let x = (i as f64 / SEGMENTS as f64).powi(2) as f32;
            c.eval(x.powf(1.0 / GAMMA)).clamp(0.0, 1.0).powf(GAMMA)
        };
        let mut t = Box::new([[0f32; 2]; SEGMENTS]);
        let mut prev = f(0);
        for (i, e) in t.iter_mut().enumerate() {
            let next = f(i + 1);
            *e = [prev, next - prev];
            prev = next;
        }
        t
    })
}

/// The tone of one decode: the curve after an exposure gain, applied in linear ProPhoto RGB to
/// pixels in the linear output colour space.
pub struct ToneMap {
    table: &'static [[f32; 2]; SEGMENTS],
    /// Input at which the curve reaches 1 (`1 / gain`).
    xmax: f32,
    /// `SEGMENTS / sqrt(xmax)`: index of `x` is `sqrt(x) * scale`.
    scale: f32,
    /// Linear output RGB to linear ProPhoto (the identity when the output is ProPhoto).
    to_pp: [[f32; 3]; 3],
    /// ProPhoto white in the output space: `(1, 1, 1)` up to rounding.
    white: [f32; 3],
}

impl std::fmt::Debug for ToneMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToneMap").field("xmax", &self.xmax).field("to_pp", &self.to_pp).finish_non_exhaustive()
    }
}

/// `a.max(b)` for values that are not NaN (a plain comparison, which vectorises to one
/// instruction).
#[inline(always)]
fn max(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

/// `a.min(b)` for values that are not NaN.
#[inline(always)]
fn min(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}

impl ToneMap {
    /// The tone of a frame with `baseline_exposure` stops (the file's DNG `BaselineExposure`,
    /// or 0) rendered to the output colour space reached from linear sRGB by `out_from_srgb`.
    pub fn new(baseline_exposure: f64, out_from_srgb: &Mat) -> ToneMap {
        let ev = if baseline_exposure.is_finite() { baseline_exposure.clamp(-8.0, 8.0) } else { 0.0 };
        let xmax = 2f64.powf(-ev);
        let pp: Mat = mul(&ColorSpace::ProPhoto.matrix_from_srgb(), &inv(out_from_srgb));
        let identity = (0..3).all(|i| (0..3).all(|j| (pp[i][j] - if i == j { 1.0 } else { 0.0 }).abs() < 1e-6));
        let one: Mat = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let f32m = |m: Mat| m.map(|r| r.map(|v| v as f32));
        ToneMap {
            table: table(),
            xmax: xmax as f32,
            scale: (SEGMENTS as f64 / xmax.sqrt()) as f32,
            to_pp: f32m(if identity { one } else { pp }),
            white: if identity { [1.0; 3] } else { inv(&pp).map(|r| (r[0] + r[1] + r[2]) as f32) },
        }
    }

    /// The same tone for the GPU develop kernel.
    #[cfg(feature = "gpu")]
    pub fn gpu(&self) -> flashraw_gpu::GpuTone {
        flashraw_gpu::GpuTone {
            table: &self.table[..],
            xmax: self.xmax,
            scale: self.scale,
            to_prophoto: self.to_pp,
            white: self.white,
        }
    }

    /// Table step and position within it of linear `x >= 0` (after the exposure gain; not NaN).
    #[inline(always)]
    fn position(&self, x: f32) -> (u32, f32) {
        let t = min(x, self.xmax).sqrt() * self.scale;
        // `t` is in `[0, SEGMENTS]` and the last step also takes `SEGMENTS`.
        let t1 = min(t, (SEGMENTS - 1) as f32);
        // SAFETY: `t1` is finite and in `[0, SEGMENTS - 1]` (`x` is not NaN), so it fits an i32; a
        // plain truncation, which vectorises where the saturating `as` does not.
        let i = unsafe { t1.to_int_unchecked::<i32>() };
        (i as u32, t - i as f32)
    }

    /// The curve at step `i`, fraction `r`.
    #[inline(always)]
    fn read(&self, i: u32, r: f32) -> f32 {
        let [a, d] = self.table[i as usize];
        a + d * r
    }

    /// The curve at linear `x >= 0` (after the exposure gain).
    #[cfg(test)]
    fn curve(&self, x: f32) -> f32 {
        let (i, r) = self.position(x);
        self.read(i, r)
    }

    /// The largest and smallest channel of a pixel of linear output RGB, in linear ProPhoto,
    /// clamped at 0 (NaN becomes 0 too).
    #[inline(always)]
    fn extremes(&self, p: [f32; 3]) -> (f32, f32) {
        let m = &self.to_pp;
        let c = [
            max(m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2], 0.0),
            max(m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2], 0.0),
            max(m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2], 0.0),
        ];
        (max(max(c[0], c[1]), c[2]), min(min(c[0], c[1]), c[2]))
    }

    /// Last stage. In ProPhoto every channel `c` becomes `fl + (c - lo) k` with
    /// `k = (fh - fl) / (hi - lo)`: the largest and smallest go to their toned values `fh` and
    /// `fl`, the middle one keeps its place between them. Back in the output space that is
    /// `(fl - k lo) w + k p`, with `w` the output colour of ProPhoto white (the row sums of the
    /// matrix back, 1 up to rounding) and `p` the input pixel: one multiply-add per channel instead
    /// of two matrix products. It is exact for pixels inside the ProPhoto gamut; a pixel with a
    /// negative ProPhoto channel (far outside the gamut of any camera) has `hi` and `lo` measured
    /// with that channel at 0.
    #[inline(always)]
    fn leave(&self, p: [f32; 3], hi: f32, lo: f32, fh: f32, fl: f32) -> [f32; 3] {
        let d = hi - lo;
        let k = if d > 0.0 { (fh - fl) / d } else { 0.0 };
        let b = fl - k * lo;
        [b * self.white[0] + k * p[0], b * self.white[1] + k * p[1], b * self.white[2] + k * p[2]]
    }

    /// One pixel of linear output RGB.
    #[inline(always)]
    pub fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        let (hi, lo) = self.extremes(p);
        let ((ih, rh), (il, rl)) = (self.position(hi), self.position(lo));
        self.leave(p, hi, lo, self.read(ih, rh), self.read(il, rl))
    }

    /// `src` into `dst` (the same length), in blocks: the arithmetic of the first and last stage
    /// (loops that vectorise) around the table reads. The same operations as [`ToneMap::apply`],
    /// so the same result. Compiled a second time for AVX2 on x86-64 (no FMA, for the same reason).
    #[inline(always)]
    fn run(&self, src: &[[f32; 3]], dst: &mut [[f32; 3]]) {
        #[cfg(all(feature = "simd", target_arch = "x86_64"))]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: the CPU supports AVX2 (checked above).
            return unsafe { self.run_avx2(src, dst) };
        }
        self.run_impl(src, dst)
    }

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[target_feature(enable = "avx2")]
    unsafe fn run_avx2(&self, src: &[[f32; 3]], dst: &mut [[f32; 3]]) {
        self.run_impl(src, dst)
    }

    #[inline(always)]
    fn run_impl(&self, src: &[[f32; 3]], dst: &mut [[f32; 3]]) {
        // One array per quantity, so each stage is a straight loop over plain floats; every array
        // is cut to the block's length first, so the loops carry no bounds checks.
        let mut buf = [[0f32; BLOCK]; 9];
        let mut idx = [[0u32; BLOCK]; 2];
        for (s, d) in src.chunks(BLOCK).zip(dst.chunks_mut(BLOCK)) {
            let n = s.len();
            let d = &mut d[..n];
            let [p0, p1, p2, hi, lo, rh, rl, fh, fl] = &mut buf;
            let (p0, p1, p2, hi, lo) = (&mut p0[..n], &mut p1[..n], &mut p2[..n], &mut hi[..n], &mut lo[..n]);
            let (rh, rl, fh, fl) = (&mut rh[..n], &mut rl[..n], &mut fh[..n], &mut fl[..n]);
            let [ih, il] = &mut idx;
            let (ih, il) = (&mut ih[..n], &mut il[..n]);
            // The pixels to three planes first: a plain copy, which vectorises on its own.
            for i in 0..n {
                (p0[i], p1[i], p2[i]) = (s[i][0], s[i][1], s[i][2]);
            }
            for i in 0..n {
                (hi[i], lo[i]) = self.extremes([p0[i], p1[i], p2[i]]);
            }
            for i in 0..n {
                (ih[i], rh[i]) = self.position(hi[i]);
                (il[i], rl[i]) = self.position(lo[i]);
            }
            for i in 0..n {
                fh[i] = self.read(ih[i], rh[i]);
                fl[i] = self.read(il[i], rl[i]);
            }
            for i in 0..n {
                let o = self.leave([p0[i], p1[i], p2[i]], hi[i], lo[i], fh[i], fl[i]);
                (p0[i], p1[i], p2[i]) = (o[0], o[1], o[2]);
            }
            for i in 0..n {
                d[i] = [p0[i], p1[i], p2[i]];
            }
        }
    }

    /// A developed image in place.
    pub fn apply_image(&self, d: &mut Developed, parallel: bool) {
        const PIXELS: usize = 4096;
        flashraw_core::par::for_each_row_chunk(&mut d.rgb, PIXELS * 3, 1, parallel, |_, chunk| {
            let mut src = [[0f32; 3]; BLOCK];
            let mut out = [[0f32; 3]; BLOCK];
            for c in chunk.chunks_mut(BLOCK * 3) {
                let n = c.len() / 3;
                for (s, v) in src.iter_mut().zip(c.chunks_exact(3)) {
                    *s = [v[0], v[1], v[2]];
                }
                self.run(&src[..n], &mut out[..n]);
                c.copy_from_slice(out[..n].as_flattened());
            }
        });
    }
}

/// An encoder that applies a [`ToneMap`] before `enc`. Runs are toned in blocks into a buffer
/// that `enc` then encodes with its own (vectorised) run.
pub(crate) struct Toned<'a, E> {
    pub tone: &'a ToneMap,
    pub enc: E,
}

impl<T: Copy, const N: usize, E: Encode<T, N>> Encode<T, N> for Toned<'_, E> {
    #[inline(always)]
    fn px(&self, p: [f32; 3]) -> [T; N] {
        self.enc.px(self.tone.apply(p))
    }

    #[inline(always)]
    fn run(&self, src: &[[f32; 3]], dst: &mut [T]) {
        let mut buf = [[0f32; 3]; BLOCK];
        for (s, d) in src.chunks(BLOCK).zip(dst.chunks_mut(BLOCK * N)) {
            let b = &mut buf[..s.len()];
            self.tone.run(s, b);
            self.enc.run(b, d);
        }
    }

    #[inline(always)]
    fn prepare<'s>(&self, src: &'s [[f32; 3]], buf: &'s mut Vec<[f32; 3]>) -> &'s [[f32; 3]] {
        buf.resize(src.len(), [0.0; 3]);
        self.tone.run(src, buf);
        buf
    }

    #[inline(always)]
    fn px_prepared(&self, p: [f32; 3]) -> [T; N] {
        self.enc.px(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srgb() -> ToneMap {
        ToneMap::new(0.0, &ColorSpace::Srgb.matrix_from_srgb())
    }

    #[test]
    fn the_curve_is_monotonic_and_maps_black_and_white_to_themselves() {
        let t = srgb();
        assert_eq!(t.curve(0.0), 0.0);
        assert!((t.curve(1.0) - 1.0).abs() < 1e-6);
        assert_eq!(t.curve(4.0), t.curve(1.0));
        let mut prev = 0.0;
        for i in 1..=10_000 {
            let v = t.curve(i as f32 / 10_000.0);
            assert!(v >= prev, "not monotonic at {i}");
            prev = v;
        }
        // An S-curve: mid grey up, deep shadows down.
        assert!(t.curve(0.18) > 0.18 * 2.0);
        assert!(t.curve(0.003) < 0.003);
    }

    #[test]
    fn the_table_matches_the_curve() {
        let t = srgb();
        let c = Curve::new(&POINTS);
        for i in 0..=1000 {
            let x = (i as f32 / 1000.0).powi(3);
            let exact = c.eval(x.powf(1.0 / GAMMA)).clamp(0.0, 1.0).powf(GAMMA);
            assert!((t.curve(x) - exact).abs() < 2e-4, "{x}: {} vs {exact}", t.curve(x));
        }
    }

    #[test]
    fn neutrals_stay_neutral_and_hues_keep_their_order() {
        for cs in [ColorSpace::Srgb, ColorSpace::DisplayP3, ColorSpace::ProPhoto] {
            let t = ToneMap::new(0.0, &cs.matrix_from_srgb());
            for v in [0.001f32, 0.05, 0.18, 0.5, 0.9] {
                let o = t.apply([v; 3]);
                assert!((o[0] - o[1]).abs() < 1e-4 && (o[1] - o[2]).abs() < 1e-4, "{cs:?} {v}: {o:?}");
                assert!((o[1] - t.curve(v)).abs() < 1e-4);
            }
        }
        let t = srgb();
        let o = t.apply([0.3, 0.12, 0.05]);
        assert!(o[0] > o[1] && o[1] > o[2], "{o:?}");
        assert!(o[0] > 0.3);
    }

    #[test]
    fn baseline_exposure_scales_the_input() {
        let a = srgb();
        let b = ToneMap::new(1.0, &ColorSpace::Srgb.matrix_from_srgb());
        for v in [0.01f32, 0.1, 0.3] {
            assert!((b.curve(v) - a.curve(2.0 * v)).abs() < 1e-3, "{v}");
        }
        assert!((b.curve(0.5) - a.curve(1.0)).abs() < 1e-6);
    }

    #[test]
    fn blocks_match_pixels() {
        struct Id;
        impl Encode<f32, 3> for Id {
            fn px(&self, p: [f32; 3]) -> [f32; 3] {
                p
            }
        }
        let t = srgb();
        let src: Vec<[f32; 3]> = (0..200).map(|i| [i as f32 / 200.0, (i % 7) as f32 / 7.0, 0.2]).collect();
        let enc = Toned { tone: &t, enc: Id };
        let mut run = vec![0f32; 600];
        enc.run(&src, &mut run);
        for (i, p) in src.iter().enumerate() {
            assert_eq!(&run[i * 3..i * 3 + 3], &enc.px(*p));
        }
    }
}
