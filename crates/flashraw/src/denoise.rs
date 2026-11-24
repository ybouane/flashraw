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
    // The luma plane gives the brightness every noise model is indexed by, at every level.
    let levels = level_count(w, h, if chroma > 0.0 { 6 } else { 4 });
    let mut guides = vec![y.clone()];
    for l in 1..levels {
        let g = down(&guides[l - 1], parallel);
        guides.push(g);
    }
    cancel::check()?;
    let y2 = if luma > 0.0 {
        let n = level_count(w, h, 4);
        let full = denoise_plane(&y, &guides[..n], 1.0, STEP, &c, parallel)?;
        // Strength below 1 keeps part of the original grain.
        let keep = (1.0 - luma).powf(LUMA_KEEP_EXP);
        blend(full, &y, keep, parallel)
    } else {
        y.clone()
    };
    let (u2, v2) = if chroma > 0.0 {
        let gain = 1.0 + 2.0 * (chroma - 0.5).max(0.0);
        let ud = denoise_plane(&u, &guides, gain, CHROMA_STEP0, &c, parallel)?;
        let vd = denoise_plane(&v, &guides, gain, CHROMA_STEP0, &c, parallel)?;
        cancel::check()?;
        // Colour detail finer than the guided filter's window follows the luma plane.
        // The regularisation is the luma noise variance at the window's brightness: windows
        // whose luma varies no more than its noise get the plain mean of the colour.
        let r = (CHROMA_RADIUS * (0.5 + chroma)).round().max(1.0) as usize;
        let noise = estimate(&y, &y, &c, parallel).map(|m| m.pixel_variance());
        let gf = Guide::new(&y2, r, parallel);
        let eps = |m: f32| noise.as_ref().map_or(1e-6, |n| n.at(m)).max(1e-9);
        let (uq, vq) = (gf.filter(&ud, &eps, parallel), gf.filter(&vd, &eps, parallel));
        let keep = (1.0 - 2.0 * chroma).max(0.0);
        (blend(uq, &u, keep, parallel), blend(vq, &v, keep, parallel))
    } else {
        (u, v)
    };
    d.rgb = vec![0.0; w * h * 3];
    merge(&mut d.rgb, [&y2, &u2, &v2], parallel);
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

/// Mean over a `(2r + 1)^2` window (edges repeated).
fn box_mean(p: &Plane, r: usize, parallel: bool) -> Plane {
    let (w, h) = (p.w, p.h);
    let n = (2 * r + 1) as f32;
    let mut tmp = Plane::new(w, h);
    par::for_each_row_chunk(&mut tmp.v, w, 32, parallel, |first, rows| {
        for (i, row) in rows.chunks_exact_mut(w).enumerate() {
            let src = &p.v[(first + i) * w..][..w];
            let at = |x: isize| src[x.clamp(0, w as isize - 1) as usize];
            let mut s: f32 = (-(r as isize)..=r as isize).map(at).sum();
            for (x, o) in row.iter_mut().enumerate() {
                *o = s / n;
                s += at(x as isize + r as isize + 1) - at(x as isize - r as isize);
            }
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
        let mean = box_mean(&i, r, parallel);
        let sq = generate(i.w, i.h, parallel, |k| i.v[k] * i.v[k]);
        let mut var = box_mean(&sq, r, parallel);
        update(&mut var, parallel, |k, v| *v = (*v - mean.v[k] * mean.v[k]).max(0.0));
        Guide { full, i, r, mean, var }
    }

    fn filter(&self, p: &Plane, eps: &(dyn Fn(f32) -> f32 + Sync), parallel: bool) -> Plane {
        let ps = down(p, parallel);
        let (w, h, i) = (ps.w, ps.h, &self.i);
        let mp = box_mean(&ps, self.r, parallel);
        let cov = box_mean(&generate(w, h, parallel, |k| i.v[k] * ps.v[k]), self.r, parallel);
        let a = generate(w, h, parallel, |k| {
            let mi = self.mean.v[k];
            (cov.v[k] - mi * mp.v[k]) / (self.var.v[k] + eps(mi))
        });
        let b = generate(w, h, parallel, |k| mp.v[k] - a.v[k] * self.mean.v[k]);
        let (fw, fh) = (self.full.w, self.full.h);
        let ma = up(&box_mean(&a, self.r, parallel), fw, fh, parallel);
        let mb = up(&box_mean(&b, self.r, parallel), fw, fh, parallel);
        generate(fw, fh, parallel, |k| ma.v[k] * self.full.v[k] + mb.v[k])
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
    par::for_each_row_chunk(&mut y.v, w, 64, parallel, |first, rows| {
        for (i, o) in rows.iter_mut().enumerate() {
            let p = &rgb[(first * w + i) * 3..][..3];
            *o = KR * p[0] + KG * p[1] + KB * p[2];
        }
    });
    par::for_each_row_chunk(&mut u.v, w, 64, parallel, |first, rows| {
        for (i, o) in rows.iter_mut().enumerate() {
            let p = &rgb[(first * w + i) * 3..][..3];
            *o = p[2] - (KR * p[0] + KG * p[1] + KB * p[2]);
        }
    });
    par::for_each_row_chunk(&mut v.v, w, 64, parallel, |first, rows| {
        for (i, o) in rows.iter_mut().enumerate() {
            let p = &rgb[(first * w + i) * 3..][..3];
            *o = p[0] - (KR * p[0] + KG * p[1] + KB * p[2]);
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
            for (x, o) in row.iter_mut().enumerate() {
                let (x0, x1) = (2 * x, (2 * x + 1).min(p.w - 1));
                *o = 0.25 * (p.at(x0, y0) + p.at(x1, y0) + p.at(x0, y1) + p.at(x1, y1));
            }
        }
    });
    o
}

/// Bilinear upsampling of a [`down`] level to `w` x `h` (pixel centres aligned).
fn up(p: &Plane, w: usize, h: usize, parallel: bool) -> Plane {
    let mut o = Plane::new(w, h);
    let tap = |i: usize, n: usize| -> (usize, usize, f32) {
        let s = ((i as f32 + 0.5) * 0.5 - 0.5).max(0.0);
        let a = (s as usize).min(n - 1);
        (a, (a + 1).min(n - 1), s - a as f32)
    };
    let xs: Vec<(usize, usize, f32)> = (0..w).map(|x| tap(x, p.w)).collect();
    par::for_each_row_chunk(&mut o.v, w, 32, parallel, |first, rows| {
        for (i, row) in rows.chunks_exact_mut(w).enumerate() {
            let (y0, y1, fy) = tap(first + i, p.h);
            let (r0, r1) = (&p.v[y0 * p.w..][..p.w], &p.v[y1 * p.w..][..p.w]);
            for (o, &(x0, x1, fx)) in row.iter_mut().zip(&xs) {
                let a = r0[x0] + (r0[x1] - r0[x0]) * fx;
                let b = r1[x0] + (r1[x1] - r1[x0]) * fx;
                *o = a + (b - a) * fy;
            }
        }
    });
    o
}

/// Denoise one opponent plane over all levels of `guides` (the luma pyramid).
/// `step0` is the distance between blocks on the full-resolution level (2 on the others).
fn denoise_plane(p: &Plane, guides: &[Plane], gain: f32, step0: usize, c: &Dct, parallel: bool) -> Result<Plane> {
    let mut pyr = vec![p.clone()];
    for l in 1..guides.len() {
        let d = down(&pyr[l - 1], parallel);
        pyr.push(d);
    }
    // Noise models, finest first. A coarse level has few flat blocks, so texture can pass for
    // noise there: its estimate is capped by what the finer level's noise predicts for it.
    let mut models: Vec<Option<Model>> = Vec::with_capacity(pyr.len());
    for l in 0..pyr.len() {
        let mut m = estimate(&pyr[l], &guides[l], c, parallel);
        if l > 0 {
            match (&mut m, &models[l - 1]) {
                (Some(m), Some(prev)) => m.cap(&prev.next_level(), LEVEL_CAP),
                _ => m = None,
            }
        }
        models.push(m);
    }
    let mut result: Option<Plane> = None;
    for l in (0..pyr.len()).rev() {
        let (src, guide) = (&pyr[l], &guides[l]);
        let est = models[l].take();
        let d = match est {
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
                // The detail of this level over the denoised coarser level.
                let low = up(&down(&d, parallel), d.w, d.h, parallel);
                let base = up(&coarse, d.w, d.h, parallel);
                let mut d = d;
                update(&mut d, parallel, |k, o| *o += base.v[k] - low.v[k]);
                d
            }
        });
    }
    Ok(result.expect("at least one level"))
}

/// Eight lanes of samples or coefficients.
type V = [f32; N];

/// The orthonormal 8-point DCT-II matrix, `m[k][i]`, and its transforms. The 1-D transforms
/// work on eight vectors at once, lane by lane (the even/odd split halves the multiplications),
/// so they vectorise.
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
    fn fwd1(&self, x: &[V; N]) -> [V; N] {
        let mut s = [[0f32; N]; 4];
        let mut d = [[0f32; N]; 4];
        for i in 0..4 {
            for t in 0..N {
                s[i][t] = x[i][t] + x[7 - i][t];
                d[i][t] = x[i][t] - x[7 - i][t];
            }
        }
        let mut out = [[0f32; N]; N];
        for r in 0..4 {
            for i in 0..4 {
                let (ce, co) = (self.m[2 * r][i], self.m[2 * r + 1][i]);
                for t in 0..N {
                    out[2 * r][t] += ce * s[i][t];
                    out[2 * r + 1][t] += co * d[i][t];
                }
            }
        }
        out
    }

    /// `out[i] = sum_k m[k][i] x[k]`, lane by lane (the inverse of [`Dct::fwd1`]).
    #[inline(always)]
    fn inv1(&self, x: &[V; N]) -> [V; N] {
        let mut e = [[0f32; N]; 4];
        let mut o = [[0f32; N]; 4];
        for i in 0..4 {
            for r in 0..4 {
                let (ce, co) = (self.m[2 * r][i], self.m[2 * r + 1][i]);
                for t in 0..N {
                    e[i][t] += ce * x[2 * r][t];
                    o[i][t] += co * x[2 * r + 1][t];
                }
            }
        }
        let mut out = [[0f32; N]; N];
        for i in 0..4 {
            for t in 0..N {
                out[i][t] = e[i][t] + o[i][t];
                out[7 - i][t] = e[i][t] - o[i][t];
            }
        }
        out
    }

    /// 2-D transform of a block given its columns already transformed vertically
    /// (`cols[j][k]`): `out[l][k]` is the coefficient of horizontal frequency `l` and vertical
    /// frequency `k`.
    #[inline(always)]
    fn horizontal(&self, cols: &[V; N]) -> [V; N] {
        self.fwd1(cols)
    }

    /// Inverse 2-D transform: `out[i][j]` is the sample of row `i`, column `j`.
    #[inline(always)]
    fn inverse(&self, x: &[V; N]) -> [V; N] {
        let t = self.inv1(x); // t[j][k]
        self.inv1(&transpose(&t)) // over k, lanes j
    }

    /// Full 2-D transform of the block at `(x0, y0)` of `p` (for the noise estimate).
    fn block(&self, p: &Plane, x0: usize, y0: usize) -> [V; N] {
        let rows: [V; N] = std::array::from_fn(|i| std::array::from_fn(|j| p.at(x0 + j, y0 + i)));
        let v = self.fwd1(&rows); // v[k][j]
        self.horizontal(&transpose(&v))
    }
}

#[inline(always)]
fn transpose(x: &[V; N]) -> [V; N] {
    std::array::from_fn(|a| std::array::from_fn(|b| x[b][a]))
}

/// Noise deviation of each DCT frequency as a function of the luma mean of the block.
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

    /// Deviations at luma mean `m` (linear between the bins, constant beyond them).
    #[inline]
    fn sigmas(&self, m: f32, out: &mut [f32; NN]) {
        let n = self.m.len();
        let i = self.m.partition_point(|&b| b < m);
        if i == 0 || n == 1 {
            *out = self.s[0];
            return;
        }
        if i >= n {
            *out = self.s[n - 1];
            return;
        }
        let (a, b) = (self.m[i - 1], self.m[i]);
        let t = if b > a { (m - a) / (b - a) } else { 0.0 };
        let (sa, sb) = (&self.s[i - 1], &self.s[i]);
        for k in 0..NN {
            out[k] = sa[k] + (sb[k] - sa[k]) * t;
        }
    }
}

/// Measure the noise model of `p`, binned by the luma mean of `guide` (same size).
fn estimate(p: &Plane, guide: &Plane, c: &Dct, parallel: bool) -> Option<Model> {
    let (bx, by) = (p.w / N, p.h / N);
    if bx * by < 32 {
        return None;
    }
    let step = (((bx * by) as f64 / MAX_BLOCKS as f64).sqrt().ceil() as usize).max(1);
    let rows: Vec<usize> = (0..by).step_by(step).collect();
    struct Block {
        mean: f32,
        activity: f32,
        sq: [f32; NN],
    }
    let blocks: Vec<Block> = par::map(rows, parallel, |r| {
        let mut out = Vec::new();
        for b in (0..bx).step_by(step) {
            let (x0, y0) = (b * N, r * N);
            let mut mean = 0.0;
            for i in 0..N {
                for j in 0..N {
                    mean += guide.at(x0 + j, y0 + i);
                }
            }
            let coef = c.block(p, x0, y0);
            let coef = coef.as_flattened();
            let mut sq = [0f32; NN];
            let mut activity = 0.0;
            for k in 1..NN {
                sq[k] = coef[k] * coef[k];
                activity += sq[k];
            }
            out.push(Block { mean: mean / NN as f32, activity, sq });
        }
        out
    })
    .into_iter()
    .flatten()
    .collect();
    let mut blocks = blocks;
    blocks.retain(|b| b.mean.is_finite() && b.activity.is_finite());
    if blocks.len() < 32 {
        return None;
    }
    blocks.sort_by(|a, b| a.mean.total_cmp(&b.mean));
    let bins = (blocks.len() / 96).clamp(1, 12);
    let per = blocks.len().div_ceil(bins);
    let mut m = Vec::with_capacity(bins);
    let mut s: Vec<[f32; NN]> = Vec::with_capacity(bins);
    let mut vals = Vec::new();
    for bin in blocks.chunks(per) {
        let mut act: Vec<f32> = bin.iter().map(|b| b.activity).collect();
        let half = act.len() / 2;
        let cut = *act.select_nth_unstable_by(half, f32::total_cmp).1;
        let flat: Vec<&Block> = bin.iter().filter(|b| b.activity <= cut).collect();
        let mut sig = [0f32; NN];
        for (k, out) in sig.iter_mut().enumerate().skip(1) {
            vals.clear();
            vals.extend(flat.iter().map(|b| b.sq[k]));
            let mid = vals.len() / 2;
            let med = *vals.select_nth_unstable_by(mid, f32::total_cmp).1;
            *out = (med / CHI2_MEDIAN * SELECTION_BIAS).sqrt();
        }
        m.push(bin[bin.len() / 2].mean);
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
    if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma") {
        // SAFETY: the CPU supports AVX2 and FMA (checked above).
        return unsafe { run_group_avx2(p, g) };
    }
    run_group_impl(p, g)
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2,fma")]
unsafe fn run_group_avx2(p: &Pass<'_>, g: &[usize]) -> Part {
    run_group_impl(p, g)
}

#[inline(always)]
fn run_group_impl(p: &Pass<'_>, g: &[usize]) -> Part {
    let fw = p.xmap.len(); // a multiple of N
    let first = g[0];
    let rows = g[g.len() - 1] + N - first;
    // The padded rows this group reads.
    let slab = |plane: &Plane| -> Vec<f32> {
        let mut v = Vec::with_capacity(rows * fw);
        for &sy in &p.ymap[first..first + rows] {
            let row = &plane.v[sy * plane.w..][..plane.w];
            v.extend(p.xmap.iter().map(|&x| row[x]));
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
    let mut gcol = vec![0f32; fw];
    let mut sig = [0f32; NN];
    let thr_scale = HARD * p.gain;
    let n2scale = p.gain * p.gain;
    // Vertical transform of 8 rows from y0 for every column, eight columns at a time.
    let vertical = |plane: &[f32], y0: usize, out: &mut [V]| {
        for (x, o) in (0..fw).step_by(N).zip(out.chunks_exact_mut(N)) {
            let rows: [V; N] = std::array::from_fn(|i| {
                let r = &plane[(y0 - first + i) * fw + x..][..N];
                std::array::from_fn(|t| r[t])
            });
            let v = p.c.fwd1(&rows); // v[k][t], column x + t
            for (t, col) in o.iter_mut().enumerate() {
                for k in 0..N {
                    col[k] = v[k][t];
                }
            }
        }
    };
    for &y0 in g {
        if cancel::is_cancelled() {
            break;
        }
        vertical(&sp, y0, &mut vert);
        if let Some(pp) = &pp {
            vertical(pp, y0, &mut vpil);
        }
        gcol.iter_mut().for_each(|v| *v = 0.0);
        for i in 0..N {
            for (gs, &gv) in gcol.iter_mut().zip(&gp[(y0 - first + i) * fw..][..fw]) {
                *gs += gv;
            }
        }
        for &x0 in p.xs {
            let cols: &[V; N] = vert[x0..x0 + N].try_into().unwrap();
            let mut coef = p.c.horizontal(cols);
            let mean = gcol[x0..x0 + N].iter().sum::<f32>() / NN as f32;
            p.model.sigmas(mean, &mut sig);
            let cf = coef.as_flattened_mut();
            let weight = if pp.is_some() {
                let pcols: &[V; N] = vpil[x0..x0 + N].try_into().unwrap();
                let pcoef = p.c.horizontal(pcols);
                let pf = pcoef.as_flattened();
                let mut e = 0.0f32;
                for k in 1..NN {
                    let n2 = sig[k] * sig[k] * n2scale;
                    let p2 = pf[k] * pf[k];
                    let s = p2 / (p2 + n2 + 1e-30);
                    cf[k] *= s;
                    e += s * s * n2;
                }
                // Blocks whose estimate is more certain count more (inverse variance).
                1.0 / (e + 1e-20)
            } else {
                let mut kept = 1.0f32;
                for k in 1..NN {
                    let keep = cf[k].abs() >= thr_scale * sig[k];
                    cf[k] = if keep { cf[k] } else { 0.0 };
                    kept += keep as u32 as f32;
                }
                1.0 / kept
            };
            let px = p.c.inverse(&coef);
            for (i, prow) in px.iter().enumerate() {
                let o = (y0 - first + i) * fw + x0;
                let (a, ws) = (&mut acc[o..o + N], &mut wsum[o..o + N]);
                for j in 0..N {
                    a[j] += weight * prow[j];
                    ws[j] += weight;
                }
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
}
