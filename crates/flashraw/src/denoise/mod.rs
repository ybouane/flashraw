//! Noise reduction of developed images, with separate luma and chroma strengths
//! (`DecodeOptions::luma_noise_reduction`, `DecodeOptions::chroma_noise_reduction`).
//!
//! The image is the linear RGB of the develop pass (after demosaicing, white balance and colour
//! conversion), so the same filter serves every sensor layout: Bayer, X-Trans, four-colour and
//! Foveon. The steps (details and measurements in `docs/design/noise-reduction.md`):
//!
//! 1. **Planes.** RGB is split into luminance (Rec. 709 weights) and two colour differences
//!    (`B - Y`, `R - Y`): the luma strength acts on brightness noise only, the chroma strength on
//!    colour noise only.
//! 2. **Scales.** Each plane is split into a pyramid of 2x2 averages (four levels for luma, six
//!    for chroma). Each level is denoised on its own, then the low frequencies of a level are
//!    replaced by the denoised level below it (Laplacian recomposition), so noise that the
//!    demosaic spread over several pixels, and the blotchy low-frequency colour noise of high ISO
//!    files, are removed at the scale where they live.
//! 3. **Noise model, measured on the image.** On every level, the noise of each of the 64
//!    frequencies of an 8x8 DCT is measured as a function of brightness: the blocks are sorted by
//!    the mean of the luma plane into bins, and in each bin the flatter half of the blocks gives a
//!    robust (median) estimate per frequency. This captures the signal dependence of photon noise,
//!    the extra noise of channels the white balance amplified and the spectral shape the demosaic
//!    gives it, without a per-camera profile. A coarse level has few flat blocks, so its estimate
//!    is capped by what the finer level's noise predicts for 2x2 averages.
//! 4. **DCT denoising.** Overlapping 8x8 blocks are transformed; a first pass keeps the
//!    coefficients above 2.7 noise deviations (hard thresholding) and averages the blocks weighted
//!    by sparsity; a second pass uses that estimate as the pilot of an empirical Wiener filter on
//!    the same blocks (the two stages of BM3D, without the block matching). Every threshold uses
//!    the deviation of that frequency at the block's brightness.
//! 5. **Colour edges.** The denoised colour planes go through a guided filter with the luma plane
//!    as the guide: colour is smoothed over a window except where it changes with brightness, so
//!    colour edges stay where the edges in the image are.
//!
//! Strengths: 1 removes all the noise the model measures; below 1 part of the original noise is
//! kept (luma: `(1 - s)^1.2` of it; chroma: `1 - 2s` up to 0.5), and chroma above 0.5 also raises
//! the thresholds and widens the guided filter. 0.5 is calibrated against the default colour noise
//! reduction and the luminance setting 25 of a common raw developer.

use crate::process::Developed;
use flashraw_core::{Result, cancel, par};

mod lanes;
use lanes::{Lanes, Scalar, arr};

/// Block size of the DCT.
const N: usize = 8;
const NN: usize = N * N;
/// Hard threshold of the first pass, in noise deviations.
const HARD: f32 = 2.7;
/// Median of a chi-square variable with one degree of freedom: the median of `c^2` over
/// coefficients of pure noise with variance 1.
const CHI2_MEDIAN: f32 = 0.454_936_4;
/// The estimate keeps the flatter half of the blocks of each bin, which on pure noise
/// under-estimates the variance slightly; this undoes it (measured, see the tests).
const SELECTION_BIAS: f32 = 1.10;
/// Smallest side of a pyramid level worth denoising.
const MIN_LEVEL_SIDE: usize = 48;
/// Most blocks the noise estimate looks at per level.
const MAX_BLOCKS: usize = 50_000;

/// Luma strength below 1 keeps `(1 - s)^LUMA_KEEP_EXP` of the original noise.
const LUMA_KEEP_EXP: f32 = 1.2;
/// Block step of the colour planes at full resolution (2 elsewhere): the guided filter decides
/// the colour detail there, and a sparser grid halves the cost of chroma.
const CHROMA_STEP0: usize = 4;
/// Guided filter radius at chroma strength 0.5 (it grows with the strength).
const CHROMA_RADIUS: f32 = 6.0;
/// A coarse level's noise deviations may exceed the prediction from the finer level by this factor.
const LEVEL_CAP: f32 = 1.5;

/// Denoise `d` in place. Strengths are clamped to `[0, 1]`; 0 skips that part.
pub(crate) fn apply(d: &mut Developed, luma: f32, chroma: f32, parallel: bool) -> Result<()> {
    let (luma, chroma) = (luma.clamp(0.0, 1.0), chroma.clamp(0.0, 1.0));
    if luma <= 0.0 && chroma <= 0.0 {
        return Ok(());
    }
    let (w, h) = (d.width as usize, d.height as usize);
    if w < N || h < N {
        return Ok(());
    }
    let c = Dct::new();
    // The RGB buffer is released while the planes are processed and rebuilt at the end.
    let [y, u, v] = split(&std::mem::take(&mut d.rgb), w, h, parallel);
    // The luma pyramid gives the brightness every noise model is indexed by, at every level,
    // and is the pyramid of the luma plane itself.
    let levels = level_count(w, h, if chroma > 0.0 { 6 } else { 4 });
    let coarse = pyramid(&y, levels, parallel);
    let guides: Vec<&Plane> = std::iter::once(&y).chain(&coarse).collect();
    cancel::check()?;
    // The luma model of the full-resolution level serves the luma passes and the guided filter.
    let y_model = estimate(&y, &y, &c, parallel);
    let y2 = if luma > 0.0 {
        let n = level_count(w, h, 4);
        let full = denoise_plane(&guides[..n], &guides[..n], y_model.clone(), 1.0, STEP, &c, parallel)?;
        // Strength below 1 keeps part of the original grain.
        let keep = (1.0 - luma).powf(LUMA_KEEP_EXP);
        Some(blend(full, &y, keep, parallel))
    } else {
        None
    };
    let y2 = y2.as_ref().unwrap_or(&y);
    let (u2, v2) = if chroma > 0.0 {
        let gain = 1.0 + 2.0 * (chroma - 0.5).max(0.0);
        let plane = |p: &Plane| -> Result<Plane> {
            let coarse = pyramid(p, levels, parallel);
            let pyr: Vec<&Plane> = std::iter::once(p).chain(&coarse).collect();
            denoise_plane(&pyr, &guides, None, gain, CHROMA_STEP0, &c, parallel)
        };
        let ud = plane(&u)?;
        let vd = plane(&v)?;
        cancel::check()?;
        // Colour detail finer than the guided filter's window follows the luma plane.
        // The regularisation is the luma noise variance at the window's brightness: windows
        // whose luma varies no more than its noise get the plain mean of the colour.
        let r = (CHROMA_RADIUS * (0.5 + chroma)).round().max(1.0) as usize;
        let noise = y_model.map(|m| m.pixel_variance());
        let gf = Guide::new(y2, r, parallel);
        let eps = |m: f32| noise.as_ref().map_or(1e-6, |n| n.at(m)).max(1e-9);
        let (uq, vq) = gf.filter(&ud, &vd, &eps, parallel);
        let keep = (1.0 - 2.0 * chroma).max(0.0);
        (blend(uq, &u, keep, parallel), blend(vq, &v, keep, parallel))
    } else {
        (u, v)
    };
    d.rgb = vec![0.0; w * h * 3];
    merge(&mut d.rgb, [y2, &u2, &v2], parallel);
    Ok(())
}

/// `d + keep * (orig - d)`.
fn blend(mut d: Plane, orig: &Plane, keep: f32, parallel: bool) -> Plane {
    if keep > 0.0 {
        update(&mut d, parallel, |k, o| *o += keep * (orig.v[k] - *o));
    }
    d
}

/// A plane of `w` x `h` whose sample `k` (row-major) is `f(k)`, computed in parallel.
fn generate(w: usize, h: usize, parallel: bool, f: impl Fn(usize) -> f32 + Sync) -> Plane {
    let mut p = Plane::new(w, h);
    update(&mut p, parallel, |k, o| *o = f(k));
    p
}

/// Run `f(k, &mut sample)` over every sample of `p`, in parallel.
fn update(p: &mut Plane, parallel: bool, f: impl Fn(usize, &mut f32) + Sync) {
    let w = p.w;
    par::for_each_row_chunk(&mut p.v, w, 64, parallel, |first, rows| {
        for (i, o) in rows.iter_mut().enumerate() {
            f(first * w + i, o);
        }
    });
}

/// Rows `first..` of several planes of the same size, for one pass that writes all of them.
fn for_each_rows<const K: usize>(
    planes: [&mut Plane; K],
    rows_per_chunk: usize,
    parallel: bool,
    f: impl Fn(usize, [&mut [f32]; K]) + Sync + Send,
) {
    let w = planes[0].w;
    let chunk = w * rows_per_chunk;
    let mut iters = planes.map(|p| p.v.chunks_mut(chunk));
    let mut items = Vec::new();
    for i in 0.. {
        let next: [Option<&mut [f32]>; K] = std::array::from_fn(|k| iters[k].next());
        if next.iter().any(|n| n.is_none()) {
            break;
        }
        items.push((i * rows_per_chunk, next.map(|n| n.unwrap())));
    }
    par::for_each_item(items, parallel, |(first, rows)| f(first, rows));
}

/// Per-pixel noise variance as a function of the luma mean (from a [`Model`]).
struct Variance {
    m: Vec<f32>,
    v: Vec<f32>,
}

impl Variance {
    fn at(&self, m: f32) -> f32 {
        let i = self.m.partition_point(|&b| b < m);
        if i == 0 {
            return self.v[0];
        }
        if i >= self.m.len() {
            return self.v[self.m.len() - 1];
        }
        let (a, b) = (self.m[i - 1], self.m[i]);
        let t = if b > a { (m - a) / (b - a) } else { 0.0 };
        self.v[i - 1] + (self.v[i] - self.v[i - 1]) * t
    }
}

/// Mean over a `(2r + 1)^2` window (edges repeated), of `f(k)` for sample `k` (row-major) of a
/// `w` x `h` plane.
fn box_mean(w: usize, h: usize, r: usize, parallel: bool, f: impl Fn(usize) -> f32 + Sync) -> Plane {
    let n = (2 * r + 1) as f32;
    // Horizontal sums, four rows at a time (independent running sums).
    let mut tmp = Plane::new(w, h);
    par::for_each_row_chunk(&mut tmp.v, w, 32, parallel, |first, rows| {
        let mut src = vec![0f32; 4 * w];
        let mut dst = vec![0f32; 4 * w];
        for (q, quad) in rows.chunks_mut(4 * w).enumerate() {
            let nr = quad.len() / w;
            for i in 0..nr {
                let k0 = (first + 4 * q + i) * w;
                for (x, s) in src[i * w..(i + 1) * w].iter_mut().enumerate() {
                    *s = f(k0 + x);
                }
            }
            let at = |i: usize, x: isize| src[i * w + x.clamp(0, w as isize - 1) as usize];
            let mut s = [0f32; 4];
            for (i, s) in s.iter_mut().enumerate() {
                *s = (-(r as isize)..=r as isize).map(|x| at(i, x)).sum();
            }
            for x in 0..w {
                for i in 0..4 {
                    dst[i * w + x] = s[i] / n;
                    s[i] += at(i, x as isize + r as isize + 1) - at(i, x as isize - r as isize);
                }
            }
            quad.copy_from_slice(&dst[..nr * w]);
        }
    });
    let mut out = Plane::new(w, h);
    par::for_each_row_chunk(&mut out.v, w, 32, parallel, |first, rows| {
        let row_at = |y: isize| &tmp.v[y.clamp(0, h as isize - 1) as usize * w..][..w];
        let mut acc = vec![0f32; w];
        for dy in -(r as isize)..=r as isize {
            for (a, v) in acc.iter_mut().zip(row_at(first as isize + dy)) {
                *a += v;
            }
        }
        for (i, row) in rows.chunks_exact_mut(w).enumerate() {
            let y = (first + i) as isize;
            for (o, a) in row.iter_mut().zip(&acc) {
                *o = a / n;
            }
            let (add, sub) = (row_at(y + r as isize + 1), row_at(y - r as isize));
            for ((a, p), m) in acc.iter_mut().zip(add).zip(sub) {
                *a += p - m;
            }
        }
    });
    out
}

/// A guided filter (He, Sun and Tang) with the luma plane as the guide: within each window the
/// output is a linear function of the guide, so colour edges that coincide with brightness
/// edges stay sharp while colour elsewhere is smoothed over the window. The linear coefficients
/// are computed at half resolution and applied at full resolution (the "fast guided filter").
struct Guide<'a> {
    /// The full-resolution guide.
    full: &'a Plane,
    /// The guide at half resolution, its window means and variances.
    i: Plane,
    r: usize,
    mean: Plane,
    var: Plane,
}

impl<'a> Guide<'a> {
    fn new(full: &'a Plane, r: usize, parallel: bool) -> Self {
        let i = down(full, parallel);
        let r = r.div_ceil(2).max(1);
        let mean = box_mean(i.w, i.h, r, parallel, |k| i.v[k]);
        let mut var = box_mean(i.w, i.h, r, parallel, |k| i.v[k] * i.v[k]);
        update(&mut var, parallel, |k, v| *v = (*v - mean.v[k] * mean.v[k]).max(0.0));
        Guide { full, i, r, mean, var }
    }

    /// Filter the two colour planes.
    fn filter(&self, u: &Plane, v: &Plane, eps: &(dyn Fn(f32) -> f32 + Sync), parallel: bool) -> (Plane, Plane) {
        let (w, h, r, i) = (self.i.w, self.i.h, self.r, &self.i);
        // The denominator of the slopes, shared by both planes.
        let den = generate(w, h, parallel, |k| self.var.v[k] + eps(self.mean.v[k]));
        let one = |p: &Plane| -> Plane {
            let ps = down(p, parallel);
            let mp = box_mean(w, h, r, parallel, |k| ps.v[k]);
            let cov = box_mean(w, h, r, parallel, |k| i.v[k] * ps.v[k]);
            let mut a = cov;
            update(&mut a, parallel, |k, a| *a = (*a - self.mean.v[k] * mp.v[k]) / den.v[k]);
            let mb = box_mean(w, h, r, parallel, |k| mp.v[k] - a.v[k] * self.mean.v[k]);
            let ma = box_mean(w, h, r, parallel, |k| a.v[k]);
            up_affine(&ma, &mb, self.full, parallel)
        };
        (one(u), one(v))
    }
}

/// Pyramid levels (the full image counts as one) with no side below [`MIN_LEVEL_SIDE`].
fn level_count(w: usize, h: usize, max: usize) -> usize {
    let mut n = 1;
    let (mut w, mut h) = (w, h);
    while n < max && w.div_ceil(2) >= MIN_LEVEL_SIDE && h.div_ceil(2) >= MIN_LEVEL_SIDE {
        w = w.div_ceil(2);
        h = h.div_ceil(2);
        n += 1;
    }
    n
}

/// Levels `1..levels` of the pyramid of 2x2 averages of `p`.
fn pyramid(p: &Plane, levels: usize, parallel: bool) -> Vec<Plane> {
    let mut v: Vec<Plane> = Vec::with_capacity(levels.saturating_sub(1));
    for _ in 1..levels {
        let d = down(v.last().unwrap_or(p), parallel);
        v.push(d);
    }
    v
}

#[derive(Clone)]
struct Plane {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Plane {
    fn new(w: usize, h: usize) -> Self {
        Plane { w, h, v: vec![0.0; w * h] }
    }

    #[inline]
    fn at(&self, x: usize, y: usize) -> f32 {
        self.v[y * self.w + x]
    }
}

// Luminance and two colour differences (Rec. 709 weights, like YCbCr without the scaling):
// the luma strength then acts on brightness noise only and the chroma strength on colour noise
// only, whatever the colour of the noise. Each plane gets its own measured noise model, so the
// planes need not be orthogonal.
const KR: f32 = 0.2126;
const KG: f32 = 0.7152;
const KB: f32 = 0.0722;

fn split(rgb: &[f32], w: usize, h: usize, parallel: bool) -> [Plane; 3] {
    let mut out = [Plane::new(w, h), Plane::new(w, h), Plane::new(w, h)];
    let [y, u, v] = &mut out;
    for_each_rows([y, u, v], 32, parallel, |first, [y, u, v]| {
        let px = &rgb[first * w * 3..][..y.len() * 3];
        for (((p, y), u), v) in px.chunks_exact(3).zip(y.iter_mut()).zip(u.iter_mut()).zip(v.iter_mut()) {
            let l = KR * p[0] + KG * p[1] + KB * p[2];
            *y = l;
            *u = p[2] - l;
            *v = p[0] - l;
        }
    });
    out
}

fn merge(rgb: &mut [f32], [y, u, v]: [&Plane; 3], parallel: bool) {
    let w = y.w;
    par::for_each_row_chunk(rgb, w * 3, 64, parallel, |first, rows| {
        for (i, p) in rows.chunks_exact_mut(3).enumerate() {
            let k = first * w + i;
            let (r, b) = (v.v[k] + y.v[k], u.v[k] + y.v[k]);
            p[0] = r;
            p[1] = (y.v[k] - KR * r - KB * b) / KG;
            p[2] = b;
        }
    });
}

/// 2x2 average (the last row or column is repeated on odd sides).
fn down(p: &Plane, parallel: bool) -> Plane {
    let (w, h) = (p.w.div_ceil(2), p.h.div_ceil(2));
    let mut o = Plane::new(w, h);
    par::for_each_row_chunk(&mut o.v, w, 32, parallel, |first, rows| {
        for (i, row) in rows.chunks_exact_mut(w).enumerate() {
            let y = first + i;
            let (y0, y1) = (2 * y, (2 * y + 1).min(p.h - 1));
            let (r0, r1) = (&p.v[y0 * p.w..][..p.w], &p.v[y1 * p.w..][..p.w]);
            let pairs = p.w / 2;
            for ((o, a), b) in row.iter_mut().zip(r0.chunks_exact(2)).zip(r1.chunks_exact(2)) {
                *o = 0.25 * (a[0] + a[1] + b[0] + b[1]);
            }
            if pairs < w {
                let x = p.w - 1;
                row[w - 1] = 0.25 * (r0[x] + r0[x] + r1[x] + r1[x]);
            }
        }
    });
    o
}

/// The two taps and the weight of the second one of bilinear upsampling of a [`down`] level of
/// side `n` at position `i` (pixel centres aligned).
#[inline]
fn tap(i: usize, n: usize) -> (usize, usize, f32) {
    let s = ((i as f32 + 0.5) * 0.5 - 0.5).max(0.0);
    let a = (s as usize).min(n - 1);
    (a, (a + 1).min(n - 1), s - a as f32)
}

/// Bilinear upsampling of row `y` of the [`down`] level `p` into `out` (`xs`: the column taps).
#[inline]
fn up_row(p: &Plane, xs: &[(usize, usize, f32)], y: usize, h: usize, out: &mut [f32]) {
    let _ = h;
    let (y0, y1, fy) = tap(y, p.h);
    let (r0, r1) = (&p.v[y0 * p.w..][..p.w], &p.v[y1 * p.w..][..p.w]);
    for (o, &(x0, x1, fx)) in out.iter_mut().zip(xs) {
        let a = r0[x0] + (r0[x1] - r0[x0]) * fx;
        let b = r1[x0] + (r1[x1] - r1[x0]) * fx;
        *o = a + (b - a) * fy;
    }
}

/// `d += up(p)`, `p` the [`down`] level of `d`'s size.
fn add_up(d: &mut Plane, p: &Plane, parallel: bool) {
    let (w, h) = (d.w, d.h);
    let xs: Vec<(usize, usize, f32)> = (0..w).map(|x| tap(x, p.w)).collect();
    par::for_each_row_chunk(&mut d.v, w, 32, parallel, |first, rows| {
        let mut u = vec![0f32; w];
        for (i, row) in rows.chunks_exact_mut(w).enumerate() {
            up_row(p, &xs, first + i, h, &mut u);
            for (o, u) in row.iter_mut().zip(&u) {
                *o += u;
            }
        }
    });
}

/// `up(a) * g + up(b)`: the guided filter's linear functions, upsampled, applied to the guide.
fn up_affine(a: &Plane, b: &Plane, g: &Plane, parallel: bool) -> Plane {
    let (w, h) = (g.w, g.h);
    let xs: Vec<(usize, usize, f32)> = (0..w).map(|x| tap(x, a.w)).collect();
    let mut o = Plane::new(w, h);
    par::for_each_row_chunk(&mut o.v, w, 32, parallel, |first, rows| {
        let mut ua = vec![0f32; w];
        for (i, row) in rows.chunks_exact_mut(w).enumerate() {
            let y = first + i;
            up_row(a, &xs, y, h, &mut ua);
            up_row(b, &xs, y, h, row);
            for ((o, a), g) in row.iter_mut().zip(&ua).zip(&g.v[y * w..][..w]) {
                *o += a * g;
            }
        }
    });
    o
}

/// Denoise one opponent plane over its pyramid `levels` (`levels[0]` the plane), the noise of
/// every level binned by the brightness of the same level of `guides` (the luma pyramid).
/// `first` is the noise model of the full-resolution level when the caller already measured it;
/// `step0` is the distance between blocks on that level (2 on the others).
#[allow(clippy::too_many_arguments)]
fn denoise_plane(
    levels: &[&Plane],
    guides: &[&Plane],
    first: Option<Model>,
    gain: f32,
    step0: usize,
    c: &Dct,
    parallel: bool,
) -> Result<Plane> {
    // Noise models, finest first. A coarse level has few flat blocks, so texture can pass for
    // noise there: its estimate is capped by what the finer level's noise predicts for it.
    let mut models: Vec<Option<Model>> = Vec::with_capacity(levels.len());
    for l in 0..levels.len() {
        let mut m = match (l, &first) {
            (0, Some(m)) => Some(m.clone()),
            _ => estimate(levels[l], guides[l], c, parallel),
        };
        if l > 0 {
            match (&mut m, &models[l - 1]) {
                (Some(m), Some(prev)) => m.cap(&prev.next_level(), LEVEL_CAP),
                _ => m = None,
            }
        }
        models.push(m);
    }
    let mut result: Option<Plane> = None;
    for l in (0..levels.len()).rev() {
        let (src, guide) = (levels[l], guides[l]);
        let d = match models[l].take() {
            Some(model) => {
                let step = if l == 0 { step0 } else { STEP };
                let pilot = dct_pass(src, guide, None, &model, gain, step, c, parallel);
                cancel::check()?;
                dct_pass(src, guide, Some(&pilot), &model, gain, step, c, parallel)
            }
            None => src.clone(),
        };
        cancel::check()?;
        result = Some(match result {
            None => d,
            Some(coarse) => {
                // The detail of this level over the denoised coarser level: `d` with its low
                // frequencies (its 2x2 average) replaced by `coarse`, both upsampled.
                let mut d = d;
                let mut diff = down(&d, parallel);
                update(&mut diff, parallel, |k, o| *o = coarse.v[k] - *o);
                add_up(&mut d, &diff, parallel);
                d
            }
        });
    }
    Ok(result.expect("at least one level"))
}

/// Eight samples or coefficients.
type V = [f32; N];

/// The orthonormal 8-point DCT-II matrix, `m[k][i]`, and its transforms. The 1-D transforms
/// work on eight vectors at once, lane by lane ([`Lanes`]), with the even/odd symmetries of the
/// matrix rows.
struct Dct {
    m: [[f32; N]; N],
}

impl Dct {
    fn new() -> Self {
        let mut m = [[0f32; N]; N];
        for (k, row) in m.iter_mut().enumerate() {
            let a = if k == 0 { (1.0 / N as f64).sqrt() } else { (2.0 / N as f64).sqrt() };
            for (i, v) in row.iter_mut().enumerate() {
                *v = (a * (std::f64::consts::PI * (2 * i + 1) as f64 * k as f64 / (2 * N) as f64).cos()) as f32;
            }
        }
        Dct { m }
    }

    /// `out[k] = sum_i m[k][i] x[i]`, lane by lane.
    #[inline(always)]
    fn fwd1<L: Lanes>(&self, x: &[L; N]) -> [L; N] {
        let m = &self.m;
        let s = arr!(4, |i| x[i].add(x[7 - i]));
        let d = arr!(4, |i| x[i].sub(x[7 - i]));
        // Even part: a 4-point transform of `s`.
        let (e0, e1) = (s[0].add(s[3]), s[1].add(s[2]));
        let (e2, e3) = (s[0].sub(s[3]), s[1].sub(s[2]));
        [
            L::splat(m[0][0]).mul(e0.add(e1)),
            dot4(&m[1], &d),
            L::splat(m[2][0]).mul(e2).add(L::splat(m[2][1]).mul(e3)),
            dot4(&m[3], &d),
            L::splat(m[4][0]).mul(e0.sub(e1)),
            dot4(&m[5], &d),
            L::splat(m[6][0]).mul(e2).add(L::splat(m[6][1]).mul(e3)),
            dot4(&m[7], &d),
        ]
    }

    /// `out[i] = sum_k m[k][i] x[k]`, lane by lane (the inverse of [`Dct::fwd1`]).
    #[inline(always)]
    fn inv1<L: Lanes>(&self, x: &[L; N]) -> [L; N] {
        let m = &self.m;
        let p = L::splat(m[0][0]).mul(x[0].add(x[4]));
        let q = L::splat(m[0][0]).mul(x[0].sub(x[4]));
        let u = L::splat(m[2][0]).mul(x[2]).add(L::splat(m[6][0]).mul(x[6]));
        let v = L::splat(m[2][1]).mul(x[2]).add(L::splat(m[6][1]).mul(x[6]));
        let e = [p.add(u), q.add(v), q.sub(v), p.sub(u)];
        let o = arr!(4, |i| dot4(&[m[1][i], m[3][i], m[5][i], m[7][i]], &[x[1], x[3], x[5], x[7]]));
        arr!(8, |i| if i < 4 { e[i].add(o[i]) } else { e[7 - i].sub(o[7 - i]) })
    }

    /// Full 2-D transform of the block at `(x0, y0)` of `p` (for the noise estimate): `out[l][k]`
    /// is the coefficient of horizontal frequency `l` and vertical frequency `k`.
    fn block(&self, p: &Plane, x0: usize, y0: usize) -> [V; N] {
        let rows: [Scalar; N] = std::array::from_fn(|i| Scalar::load(&p.v[(y0 + i) * p.w + x0..]));
        let v = self.fwd1(&rows); // v[k] lane j
        let c = self.fwd1(&Scalar::transpose(&v));
        std::array::from_fn(|l| {
            let mut o = [0f32; N];
            c[l].store(&mut o);
            o
        })
    }

    /// Inverse of [`Dct::block`]: `out[i][j]` is the sample of row `i`, column `j`.
    #[cfg(test)]
    fn inverse(&self, x: &[V; N]) -> [V; N] {
        let x: [Scalar; N] = std::array::from_fn(|l| Scalar::load(&x[l]));
        let t = self.inv1(&x); // t[j] lane k
        let px = self.inv1(&Scalar::transpose(&t));
        std::array::from_fn(|i| {
            let mut o = [0f32; N];
            px[i].store(&mut o);
            o
        })
    }
}

/// `(c0 x0 + c1 x1) + (c2 x2 + c3 x3)`.
#[inline(always)]
fn dot4<L: Lanes>(c: &[f32], x: &[L; 4]) -> L {
    (L::splat(c[0]).mul(x[0]).add(L::splat(c[1]).mul(x[1]))).add(L::splat(c[2]).mul(x[2]).add(L::splat(c[3]).mul(x[3])))
}

/// Noise deviation of each DCT frequency as a function of the luma mean of the block.
#[derive(Clone)]
struct Model {
    /// Luma means of the bins, increasing.
    m: Vec<f32>,
    /// Deviation per frequency, per bin (the DC entry is unused).
    s: Vec<[f32; NN]>,
}

impl Model {
    /// Predicted noise variance per pixel of the next coarser level (2x2 averages): a quarter of
    /// the mean variance of the 15 lowest AC frequencies (exact for white noise, larger for noise
    /// the demosaic correlated over neighbouring pixels).
    fn next_level(&self) -> Variance {
        let v = self
            .s
            .iter()
            .map(|s| {
                let mut t = 0.0;
                for l in 0..4 {
                    for k in 0..4 {
                        t += s[l * N + k] * s[l * N + k];
                    }
                }
                0.25 * t / 15.0
            })
            .collect();
        Variance { m: self.m.clone(), v }
    }

    /// Limit every frequency's variance to `f^2` times the predicted per-pixel variance.
    fn cap(&mut self, pred: &Variance, f: f32) {
        for (m, s) in self.m.iter().zip(self.s.iter_mut()) {
            let cap = f * pred.at(*m).max(0.0).sqrt();
            for v in s.iter_mut() {
                *v = v.min(cap);
            }
        }
    }

    /// Noise variance per pixel (the mean over the AC frequencies) at each bin.
    fn pixel_variance(&self) -> Variance {
        let v = self.s.iter().map(|s| s[1..].iter().map(|x| x * x).sum::<f32>() / (NN - 1) as f32).collect();
        Variance { m: self.m.clone(), v }
    }

    /// The deviations at luma mean `m` are `a + (b - a) t` (linear between the bins, constant
    /// beyond them); entry `l * 8 + k` is horizontal frequency `l`, vertical frequency `k`.
    #[inline(always)]
    fn sigmas(&self, m: f32) -> (&[f32; NN], &[f32; NN], f32) {
        let n = self.m.len();
        let i = self.m.partition_point(|&b| b < m);
        if i == 0 || n == 1 {
            return (&self.s[0], &self.s[0], 0.0);
        }
        if i >= n {
            return (&self.s[n - 1], &self.s[n - 1], 0.0);
        }
        let (a, b) = (self.m[i - 1], self.m[i]);
        (&self.s[i - 1], &self.s[i], if b > a { (m - a) / (b - a) } else { 0.0 })
    }
}

/// Measure the noise model of `p`, binned by the luma mean of `guide` (same size).
fn estimate(p: &Plane, guide: &Plane, c: &Dct, parallel: bool) -> Option<Model> {
    let (bx, by) = (p.w / N, p.h / N);
    let step = estimate_step(p.w, p.h)?;
    let rows: Vec<usize> = (0..by).step_by(step).collect();
    let stats: Vec<[f32; NN]> = par::map(rows, parallel, |r| {
        let mut out = Vec::with_capacity(bx.div_ceil(step));
        for b in (0..bx).step_by(step) {
            let (x0, y0) = (b * N, r * N);
            let mut mean = 0.0;
            for i in 0..N {
                for j in 0..N {
                    mean += guide.at(x0 + j, y0 + i);
                }
            }
            let coef = c.block(p, x0, y0);
            let mut st = [0f32; NN];
            for (s, v) in st.iter_mut().zip(coef.as_flattened()) {
                *s = v * v;
            }
            st[0] = mean / NN as f32;
            out.push(st);
        }
        out
    })
    .into_iter()
    .flatten()
    .collect();
    Model::from_stats(&stats)
}

/// Distance in blocks between the blocks of the noise estimate of a `w` x `h` level (`None`:
/// too small to measure). Blocks `(bx, by)` with both multiples of it are measured, in rows.
pub(crate) fn estimate_step(w: usize, h: usize) -> Option<usize> {
    let (bx, by) = (w / N, h / N);
    if bx * by < 32 {
        return None;
    }
    Some((((bx * by) as f64 / MAX_BLOCKS as f64).sqrt().ceil() as usize).max(1))
}

impl Model {
    /// The model from the measured blocks: per block the luma mean (entry 0) and the squared DCT
    /// coefficients (entry `l * 8 + k`: horizontal frequency `l`, vertical frequency `k`).
    pub(crate) fn from_stats(stats: &[[f32; NN]]) -> Option<Model> {
        // Blocks by luma mean (stable: equal means keep their order), with their AC energy.
        let mut order: Vec<(f32, f32, u32)> = stats
            .iter()
            .enumerate()
            .map(|(i, s)| (s[0], s[1..].iter().fold(0.0f32, |a, v| a + v), i as u32))
            .filter(|(m, a, _)| m.is_finite() && a.is_finite())
            .collect();
        if order.len() < 32 {
            return None;
        }
        order.sort_by(|a, b| a.0.total_cmp(&b.0));
        let bins = (order.len() / 96).clamp(1, 12);
        let per = order.len().div_ceil(bins);
        let mut m = Vec::with_capacity(bins);
        let mut s: Vec<[f32; NN]> = Vec::with_capacity(bins);
        let mut vals = Vec::new();
        for bin in order.chunks(per) {
            let mut act: Vec<f32> = bin.iter().map(|b| b.1).collect();
            let half = act.len() / 2;
            let cut = *act.select_nth_unstable_by(half, f32::total_cmp).1;
            let flat: Vec<&[f32; NN]> = bin.iter().filter(|b| b.1 <= cut).map(|b| &stats[b.2 as usize]).collect();
            let mut sig = [0f32; NN];
            for (k, out) in sig.iter_mut().enumerate().skip(1) {
                vals.clear();
                vals.extend(flat.iter().map(|b| b[k]));
                let mid = vals.len() / 2;
                let med = *vals.select_nth_unstable_by(mid, f32::total_cmp).1;
                *out = (med / CHI2_MEDIAN * SELECTION_BIAS).sqrt();
            }
            m.push(bin[bin.len() / 2].0);
            s.push(sig);
        }
        // Light smoothing across bins: each estimate is a median of a few hundred blocks.
        if s.len() > 2 {
            let orig = s.clone();
            for i in 0..s.len() {
                let (a, b) = (i.saturating_sub(1), (i + 1).min(s.len() - 1));
                for k in 0..NN {
                    s[i][k] = 0.25 * orig[a][k] + 0.5 * orig[i][k] + 0.25 * orig[b][k];
                }
            }
        }
        Some(Model { m, s })
    }
}

/// Source index of each position of a side of `n` samples padded symmetrically by `pad` on both
/// ends, plus `extra` more at the far end.
fn pad_map(n: usize, pad: usize, extra: usize) -> Vec<usize> {
    (0..n + 2 * pad + extra)
        .map(|i| {
            let (i, n) = (i as isize - pad as isize, n as isize);
            let r = if i < 0 {
                -i - 1
            } else if i >= n {
                2 * n - i - 1
            } else {
                i
            };
            r.clamp(0, n - 1) as usize
        })
        .collect()
}

/// Block positions `0, step, 2 step, ...` up to `n - N`, always including `n - N`.
fn positions(n: usize, step: usize) -> Vec<usize> {
    let last = n - N;
    let mut v: Vec<usize> = (0..=last).step_by(step).collect();
    if *v.last().unwrap() != last {
        v.push(last);
    }
    v
}

/// Padding around a plane for the DCT passes.
const PAD: usize = N;
/// Distance between neighbouring blocks.
const STEP: usize = 2;
/// Block rows per unit of parallel work.
const GROUP: usize = 16;

/// The planes and settings of one pass. Blocks are placed on the plane padded symmetrically by
/// [`PAD`] (and on the right up to a whole number of vectors); each group of block rows builds
/// the padded rows it reads, so no padded copy of a whole plane is made.
struct Pass<'a> {
    src: &'a Plane,
    guide: &'a Plane,
    pilot: Option<&'a Plane>,
    /// Source column of each padded column (`fw` of them, a multiple of [`N`]).
    xmap: Vec<usize>,
    /// Source row of each padded row.
    ymap: Vec<usize>,
    xs: &'a [usize],
    model: &'a Model,
    gain: f32,
    c: &'a Dct,
}

/// What one group of block rows accumulated: rows `first..first + rows` of the padded plane.
struct Part {
    first: usize,
    rows: usize,
    acc: Vec<f32>,
    wsum: Vec<f32>,
}

/// One DCT pass over `src`: hard thresholding when `pilot` is `None`, the empirical Wiener
/// filter of `pilot` otherwise.
#[allow(clippy::too_many_arguments)]
fn dct_pass(
    src: &Plane,
    guide: &Plane,
    pilot: Option<&Plane>,
    model: &Model,
    gain: f32,
    step: usize,
    c: &Dct,
    parallel: bool,
) -> Plane {
    let pw = src.w + 2 * PAD;
    let extra = pw.next_multiple_of(N) - pw;
    let ys = positions(src.h + 2 * PAD, step);
    let xs = positions(pw, step);
    let pass = Pass {
        src,
        guide,
        pilot,
        xmap: pad_map(src.w, PAD, extra),
        ymap: pad_map(src.h, PAD, 0),
        xs: &xs,
        model,
        gain,
        c,
    };
    let groups: Vec<&[usize]> = ys.chunks(GROUP).collect();
    let parts = par::map(groups, parallel, |g| run_group(&pass, g));
    // Every padded row lies in at most two groups (a group spans more rows than the overlap).
    let fw = pass.xmap.len();
    let mut out = Plane::new(src.w, src.h);
    let (w, parts) = (src.w, &parts);
    par::for_each_row_chunk(&mut out.v, w, 32, parallel, |first, rows| {
        let mut a = vec![0f32; w];
        let mut ws = vec![0f32; w];
        for (i, row) in rows.chunks_exact_mut(w).enumerate() {
            let y = first + i + PAD;
            a.iter_mut().for_each(|v| *v = 0.0);
            ws.iter_mut().for_each(|v| *v = 0.0);
            let k = parts.partition_point(|p| p.first + p.rows <= y);
            for p in parts[k..].iter().take_while(|p| p.first <= y) {
                let o = (y - p.first) * fw + PAD;
                for ((d, s), (dw, sw)) in a.iter_mut().zip(&p.acc[o..o + w]).zip(ws.iter_mut().zip(&p.wsum[o..o + w])) {
                    *d += s;
                    *dw += sw;
                }
            }
            let srow = &src.v[(first + i) * w..][..w];
            for (((o, a), ws), s) in row.iter_mut().zip(&a).zip(&ws).zip(srow) {
                *o = if *ws > 0.0 { a / ws } else { *s };
            }
        }
    });
    out
}

fn run_group(p: &Pass<'_>, g: &[usize]) -> Part {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports AVX2 (checked above).
        return unsafe { run_group_avx2(p, g) };
    }
    #[cfg(all(feature = "simd", target_arch = "aarch64"))]
    return run_group_impl::<lanes::Neon>(p, g);
    #[cfg(all(feature = "simd", target_arch = "wasm32", target_feature = "simd128"))]
    return run_group_impl::<lanes::Wasm>(p, g);
    #[allow(unreachable_code)]
    run_group_impl::<Scalar>(p, g)
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn run_group_avx2(p: &Pass<'_>, g: &[usize]) -> Part {
    run_group_impl::<lanes::Avx>(p, g)
}

/// Vertical transform of the eight rows of `plane` (`fw` wide) from `row` for every column,
/// eight columns at a time: `out[x][k]` is frequency `k` of column `x`.
#[inline(always)]
fn vertical<L: Lanes>(c: &Dct, plane: &[f32], fw: usize, row: usize, out: &mut [V]) {
    for (x, o) in (0..fw).step_by(N).zip(out.chunks_exact_mut(N)) {
        let rows = arr!(8, |i| L::load(&plane[(row + i) * fw + x..]));
        let v = L::transpose(&c.fwd1(&rows)); // v[t] lane k: column x + t
        for (o, v) in o.iter_mut().zip(v) {
            v.store(o);
        }
    }
}

#[inline(always)]
fn run_group_impl<L: Lanes>(p: &Pass<'_>, g: &[usize]) -> Part {
    let fw = p.xmap.len(); // a multiple of N
    let first = g[0];
    let rows = g[g.len() - 1] + N - first;
    // The padded rows this group reads.
    let slab = |plane: &Plane| -> Vec<f32> {
        let w = plane.w;
        let mut v = vec![0f32; rows * fw];
        for (&sy, o) in p.ymap[first..first + rows].iter().zip(v.chunks_exact_mut(fw)) {
            let row = &plane.v[sy * w..][..w];
            o[PAD..PAD + w].copy_from_slice(row);
            for (o, &x) in o[..PAD].iter_mut().zip(&p.xmap[..PAD]) {
                *o = row[x];
            }
            for (o, &x) in o[PAD + w..].iter_mut().zip(&p.xmap[PAD + w..]) {
                *o = row[x];
            }
        }
        v
    };
    let sp = slab(p.src);
    let gp = slab(p.guide);
    let pp = p.pilot.map(slab);
    let mut acc = vec![0f32; rows * fw];
    let mut wsum = vec![0f32; rows * fw];
    let mut vert = vec![[0f32; N]; fw];
    let mut vpil = vec![[0f32; N]; if pp.is_some() { fw } else { 0 }];
    // The blocks of one block row, summed per column before the vertical inverse transform:
    // `racc[x][k]` is vertical frequency `k` of column `x`, `rw[x]` the sum of the weights.
    let mut racc = vec![[0f32; N]; fw];
    let mut rw = vec![0f32; fw];
    let mut gcol = vec![0f32; fw];
    let thr = L::splat(HARD * p.gain);
    let n2scale = L::splat(p.gain * p.gain);
    let tiny = L::splat(1e-30);
    for &y0 in g {
        if cancel::is_cancelled() {
            break;
        }
        let r0 = y0 - first;
        vertical::<L>(p.c, &sp, fw, r0, &mut vert);
        if let Some(pp) = &pp {
            vertical::<L>(p.c, pp, fw, r0, &mut vpil);
        }
        gcol.iter_mut().for_each(|v| *v = 0.0);
        for i in 0..N {
            for (gs, &gv) in gcol.iter_mut().zip(&gp[(r0 + i) * fw..][..fw]) {
                *gs += gv;
            }
        }
        racc.iter_mut().for_each(|v| *v = [0.0; N]);
        rw.iter_mut().for_each(|v| *v = 0.0);
        for &x0 in p.xs {
            let cols = arr!(8, |j| L::load(&vert[x0 + j]));
            let mut coef = p.c.fwd1(&cols); // coef[l] lane k
            let mean = gcol[x0..x0 + N].iter().sum::<f32>() / NN as f32;
            let (sa, sb, t) = p.model.sigmas(mean);
            let t = L::splat(t);
            let sigma = |l: usize| {
                let (a, b) = (L::load(&sa[l * N..]), L::load(&sb[l * N..]));
                a.add(b.sub(a).mul(t))
            };
            // The DC deviation is 0: the hard threshold keeps it, the Wiener factor is
            // restored below.
            let weight = if pp.is_some() {
                let pcols = arr!(8, |j| L::load(&vpil[x0 + j]));
                let pcoef = p.c.fwd1(&pcols);
                let mut c0 = [0f32; N];
                coef[0].store(&mut c0);
                let dc = c0[0];
                let mut e = L::splat(0.0);
                for (l, (c, pc)) in coef.iter_mut().zip(&pcoef).enumerate() {
                    let sg = sigma(l);
                    let n2 = sg.mul(sg).mul(n2scale);
                    let p2 = pc.mul(*pc);
                    let s = p2.div(p2.add(n2).add(tiny));
                    *c = c.mul(s);
                    e = e.add(s.mul(s).mul(n2));
                }
                coef[0].store(&mut c0);
                c0[0] = dc;
                coef[0] = L::load(&c0);
                // Blocks whose estimate is more certain count more (inverse variance).
                1.0 / (e.sum() + 1e-20)
            } else {
                let mut kept = 0;
                for (l, c) in coef.iter_mut().enumerate() {
                    let (k, n) = c.keep_ge(sigma(l).mul(thr));
                    *c = k;
                    kept += n;
                }
                1.0 / kept as f32
            };
            let wv = L::splat(weight);
            for c in coef.iter_mut() {
                *c = c.mul(wv);
            }
            // Horizontal inverse: `t[j]` lane `k` is column `x0 + j`, vertical frequency `k`.
            let t = p.c.inv1(&coef);
            for (a, c) in racc[x0..x0 + N].iter_mut().zip(t) {
                L::load(a).add(c).store(a);
            }
            let ws = &mut rw[x0..x0 + N];
            L::load(ws).add(wv).store(ws);
        }
        // Vertical inverse of the summed blocks, eight columns at a time.
        for x in (0..fw).step_by(N) {
            let cols = arr!(8, |j| L::load(&racc[x + j]));
            let px = p.c.inv1(&L::transpose(&cols)); // px[i] lane t: row r0 + i, column x + t
            let w = L::load(&rw[x..]);
            for (i, prow) in px.iter().enumerate() {
                let o = (r0 + i) * fw + x;
                let a = &mut acc[o..o + N];
                L::load(a).add(*prow).store(a);
                let ws = &mut wsum[o..o + N];
                L::load(ws).add(w).store(ws);
            }
        }
    }
    Part { first, rows, acc, wsum }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic pseudo-random normal sample (Box-Muller over a 64-bit LCG).
    struct Rng(u64);
    impl Rng {
        fn uniform(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }
        fn normal(&mut self) -> f32 {
            let (a, b) = (self.uniform(), self.uniform());
            ((-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos()) as f32
        }
    }

    #[test]
    fn dct_is_orthonormal_and_inverts() {
        let c = Dct::new();
        let mut rng = Rng(7);
        let p = Plane { w: N, h: N, v: (0..NN).map(|_| rng.normal()).collect() };
        let coef = c.block(&p, 0, 0);
        let e_in: f32 = p.v.iter().map(|v| v * v).sum();
        let e_out: f32 = coef.as_flattened().iter().map(|v| v * v).sum();
        assert!((e_in - e_out).abs() < 1e-3 * e_in, "{e_in} {e_out}");
        // Coefficient [l][k]: horizontal frequency l, vertical k; a horizontal ramp has only l > 0.
        let px = c.inverse(&coef);
        for (a, b) in px.as_flattened().iter().zip(&p.v) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn the_estimate_finds_the_deviation_of_white_noise() {
        let c = Dct::new();
        let (w, h) = (512, 512);
        let mut rng = Rng(11);
        for sigma in [0.01f32, 0.05] {
            let p = Plane { w, h, v: (0..w * h).map(|_| 0.3 + sigma * rng.normal()).collect() };
            let g = Plane { w, h, v: vec![0.3; w * h] };
            let m = estimate(&p, &g, &c, false).unwrap();
            let mean: f32 =
                m.s.iter().map(|s| s[1..].iter().sum::<f32>() / (NN - 1) as f32).sum::<f32>() / m.s.len() as f32;
            assert!((mean / sigma - 1.0).abs() < 0.04, "sigma {sigma}: estimated {mean}");
        }
    }

    #[test]
    fn flat_noise_is_removed_and_the_mean_kept() {
        let (w, h) = (256, 192);
        let mut rng = Rng(3);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for _ in 0..w * h {
            rgb.extend([0.4 + 0.03 * rng.normal(), 0.3 + 0.03 * rng.normal(), 0.2 + 0.03 * rng.normal()]);
        }
        let mut d = Developed { width: w as u32, height: h as u32, rgb: rgb.clone() };
        apply(&mut d, 1.0, 1.0, false).unwrap();
        let dev = |v: &[f32], c: usize, mean: f32| -> f32 {
            (v.chunks(3).map(|p| (p[c] - mean).powi(2)).sum::<f32>() / (w * h) as f32).sqrt()
        };
        for (c, mean) in [(0, 0.4f32), (1, 0.3), (2, 0.2)] {
            let (before, after) = (dev(&rgb, c, mean), dev(&d.rgb, c, mean));
            assert!(after < before * 0.25, "channel {c}: {before} -> {after}");
            let m: f32 = d.rgb.chunks(3).map(|p| p[c]).sum::<f32>() / (w * h) as f32;
            assert!((m - mean).abs() < 2e-3, "channel {c}: mean {m}");
        }
    }

    #[test]
    fn zero_strength_changes_nothing_and_edges_survive() {
        let (w, h) = (128, 128);
        let mut rng = Rng(5);
        let rgb: Vec<f32> = (0..w * h)
            .flat_map(|i| {
                let v = if i % w < w / 2 { 0.1 } else { 0.8 };
                [v + 0.01 * rng.normal(), v + 0.01 * rng.normal(), v + 0.01 * rng.normal()]
            })
            .collect();
        let mut d = Developed { width: w as u32, height: h as u32, rgb: rgb.clone() };
        apply(&mut d, 0.0, 0.0, false).unwrap();
        assert_eq!(d.rgb, rgb);
        apply(&mut d, 1.0, 1.0, false).unwrap();
        // The step between the halves stays sharp: the columns next to it keep their levels.
        let col = |x: usize| (0..h).map(|y| d.rgb[(y * w + x) * 3 + 1]).sum::<f32>() / h as f32;
        assert!((col(w / 2 - 1) - 0.1).abs() < 0.02, "{}", col(w / 2 - 1));
        assert!((col(w / 2) - 0.8).abs() < 0.02, "{}", col(w / 2));
    }

    #[test]
    fn the_vector_units_give_the_portable_result() {
        let c = Dct::new();
        let (w, h) = (203, 77);
        let mut rng = Rng(9);
        let p = Plane { w, h, v: (0..w * h).map(|i| 0.2 + 0.001 * (i % w) as f32 + 0.02 * rng.normal()).collect() };
        let m = estimate(&p, &p, &c, false).unwrap();
        let pilot = dct_pass(&p, &p, None, &m, 1.0, STEP, &c, false);
        for (pil, step) in [(None, 2), (Some(&pilot), 2), (None, 4), (Some(&pilot), 3)] {
            let pw = w + 2 * PAD;
            let extra = pw.next_multiple_of(N) - pw;
            let xs = positions(pw, step);
            let pass = Pass {
                src: &p,
                guide: &p,
                pilot: pil,
                xmap: pad_map(w, PAD, extra),
                ymap: pad_map(h, PAD, 0),
                xs: &xs,
                model: &m,
                gain: 1.3,
                c: &c,
            };
            let ys = positions(h + 2 * PAD, step);
            for g in ys.chunks(GROUP) {
                let (a, b) = (run_group(&pass, g), run_group_impl::<Scalar>(&pass, g));
                assert!(a.acc == b.acc && a.wsum == b.wsum, "step {step}");
            }
        }
    }
}
