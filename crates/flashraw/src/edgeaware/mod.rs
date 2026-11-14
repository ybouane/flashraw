//! Edge-aware demosaic ([`crate::Demosaic::EdgeAware`]) for Bayer and X-Trans sensors.
//!
//! For every pixel:
//!
//! 1. a first estimate of the green plane `G0` comes from one non-directional 7x7 filter per
//!    CFA position,
//! 2. the squared gradients of `sqrt(G0)` along four directions (horizontal, vertical and
//!    both diagonals), summed over 3x3, give four weights `w_d = 1 / (E_d + EPS)^2`
//!    (normalised to sum to one): the lower the variation along a direction, the more that
//!    direction is trusted,
//! 3. each missing colour is `sum_d w_d * (filter_d * mosaic)`, with one 7x7 filter per CFA
//!    position, colour and direction. The measured colour is kept as is,
//!
//! The filters are least-squares fits to re-mosaicked training images and sum to one, so a
//! flat field stays flat; they are trained by `tools/demosaic/edgeaware_train.py` (see
//! `docs/design/edge-aware-demosaic.md` for the method and its provenance) and stored in
//! `bayer.bin` / `xtrans.bin`. The X-Trans filters are trained with a 5x5 support inside the
//! 7x7 layout (the outer taps are zero and skipped): on held-out tiles they match the 7x7 fit
//! and cost fewer multiplications; the Bayer filters use the full 7x7.
//!
//! 4. the colour differences R-G and B-G are cleaned in two passes, so that isolated
//!    coloured dots (the main artefact of sparse red and blue sites) disappear and green
//!    (luminance) detail is untouched. Pass 1 for a missing red or blue: Bayer, the green plus
//!    the weighted mean of the differences measured at the red or blue sites of the 3x3
//!    window, a site counting `1 / (1 + ((G_site - G) / s)^2)` with `s = 0.2 G + 0.015`, so
//!    that colour does not leak across a luminance edge; X-Trans, the green plus the 3x3
//!    median of the differences of the filtered planes. Pass 2, both: the same weighted mean
//!    over the pass-1 differences of all pixels of the 3x3 window, with `s = 0.1 G + 0.007`.
//!    X-Trans, also at a red or blue site: green moves towards the measured sample minus the
//!    pass-2 difference, fully on clean frames and less as `beta` (step 5) grows (`1 - 2 beta`).
//!    The filtered green at those sites is softer than the measured green next to them, which
//!    left a texture tied to the 6x6 pattern ("pixelated" lines and edges); from the measured
//!    sample it carries that pixel's own detail, as the green sites do.
//! 5. On noisy frames: every channel of the pixel moves by `beta` towards the
//!    mean of its 3x3 window, weighted as in step 4 with `s = 0.2 G + 0.015`. `beta` is 0 for
//!    clean frames and up to 0.5 for noisy ones, from a noise estimate over 48 rows of the
//!    whole frame ([`Plan::set_noise_from`]): the lower quintile of
//!    `|a - b| / sqrt((a + b) / 2)` over same-colour green pairs two samples apart.
//!
//! The mosaic is white balanced before the filters see it, so the colour channels have
//! comparable levels (as in the training data).
//!
//! Every output pixel depends only on mosaic samples within [`HALO`] pixels, and is computed
//! by the same operations in the same order whatever the band, thread or block it falls in:
//! results are bit-identical for any strip height, thread count or crop. On aarch64 the filter
//! taps use fused multiply-adds, so results there differ from x86-64 by rounding.

use flashraw_core::CfaPattern;
use std::f32::consts::FRAC_1_SQRT_2;
use std::sync::OnceLock;

const K: usize = 7;
const R: usize = K / 2;
const TAPS: usize = K * K;
const NDIR: usize = 4;
const EPS: f32 = 1e-3;
/// Mosaic samples needed around a pixel by the filters: the 7x7 filter reaches 3, and the
/// weights need `G0` two pixels further (gradient, then 3x3 sum) which itself reaches 3.
const CORE_HALO: usize = R + 2;
/// Mosaic samples needed around a pixel: the filters, plus three (two chroma passes and the
/// noise smoothing).
pub(crate) const HALO: usize = CORE_HALO + 3;
/// Output rows per band: bounds the working memory (a few MB at 8000 columns).
pub(crate) const BAND_ROWS: usize = 64;

struct Tables {
    period: usize,
    /// Colour (0 R, 1 G, 2 B) of each position of the canonical pattern.
    canon: Vec<u8>,
    /// The taps of the `G0` filter (`[period*period][49]`) that are not zero, per position.
    g0_taps: Vec<Vec<Tap<1>>>,
    /// The taps of the directional filters (`[period*period][3][NDIR][49]`) where some direction is
    /// not zero, per position and colour.
    dir_taps: Vec<[Vec<Tap<NDIR>>; 3]>,
    /// The dense tables the GPU kernels read: `[period*period][49]` and `[period*period][3][NDIR][49]`.
    #[cfg(feature = "gpu")]
    g0: Vec<f32>,
    #[cfg(feature = "gpu")]
    dir: Vec<f32>,
}

/// One filter tap: its index in the 7x7 window and its coefficient in each of `N` filters.
/// Taps that are zero in every filter are left out (adding `0 * x` changes nothing).
#[derive(Clone, Copy)]
struct Tap<const N: usize> {
    idx: usize,
    c: [f32; N],
}

/// Lanes of output columns computed together by [`apply_taps`].
const LANES: usize = 16;

/// `acc[f][l] = sum over taps of tap.c[f] * planes[start(tap) + k + l]` for the columns
/// `k..k + n` of a run (`LANES` at a time, the accumulators in registers, the taps added in
/// list order), handed to `emit(k, count, acc)`. Reads up to `LANES - 1` samples past the end
/// of a run (the planes carry that padding).
#[inline(always)]
fn apply_taps<const N: usize>(
    taps: &[Tap<N>],
    starts: &[usize; TAPS],
    planes: &[f32],
    n: usize,
    mut emit: impl FnMut(usize, usize, &[[f32; LANES]; N]),
) {
    let mut k = 0;
    while k < n {
        let mut acc = [[0f32; LANES]; N];
        for t in taps {
            let s = starts[t.idx] + k;
            let v: &[f32; LANES] = planes[s..s + LANES].try_into().unwrap();
            for f in 0..N {
                let c = t.c[f];
                for l in 0..LANES {
                    // aarch64 always has a fused multiply-add (one instruction, one rounding).
                    #[cfg(target_arch = "aarch64")]
                    {
                        acc[f][l] = c.mul_add(v[l], acc[f][l]);
                    }
                    #[cfg(not(target_arch = "aarch64"))]
                    {
                        acc[f][l] += c * v[l];
                    }
                }
            }
        }
        emit(k, (n - k).min(LANES), &acc);
        k += LANES;
    }
}

fn load(bytes: &[u8], period: usize, canon: &[u8]) -> Tables {
    let n = period * period;
    let f: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    assert_eq!(f.len(), n * TAPS + n * 3 * NDIR * TAPS, "edge-aware table size");
    let (g0, dir) = (&f[..n * TAPS], &f[n * TAPS..]);
    let g0_taps = (0..n)
        .map(|ph| {
            (0..TAPS).filter(|&i| g0[ph * TAPS + i] != 0.0).map(|idx| Tap { idx, c: [g0[ph * TAPS + idx]] }).collect()
        })
        .collect();
    let dir_taps = (0..n)
        .map(|ph| {
            std::array::from_fn(|c| {
                (0..TAPS)
                    .map(|idx| Tap { idx, c: std::array::from_fn(|d| dir[((ph * 3 + c) * NDIR + d) * TAPS + idx]) })
                    .filter(|t| t.c.iter().any(|&v| v != 0.0))
                    .collect()
            })
        })
        .collect();
    Tables {
        period,
        canon: canon.to_vec(),
        g0_taps,
        dir_taps,
        #[cfg(feature = "gpu")]
        g0: g0.to_vec(),
        #[cfg(feature = "gpu")]
        dir: dir.to_vec(),
    }
}

fn bayer() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| load(include_bytes!("bayer.bin"), 2, &[0, 1, 1, 2]))
}

fn xtrans() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        // GGRGGB / GGBGGR / BRGRBG / GGBGGR / GGRGGB / RBGBRG
        let rows = ["GGRGGB", "GGBGGR", "BRGRBG", "GGBGGR", "GGRGGB", "RBGBRG"];
        let canon: Vec<u8> = rows
            .iter()
            .flat_map(|r| {
                r.bytes().map(|c| match c {
                    b'R' => 0,
                    b'G' => 1,
                    _ => 2,
                })
            })
            .collect();
        load(include_bytes!("xtrans.bin"), 6, &canon)
    })
}

/// The edge-aware demosaic of one frame: which table applies and where the CFA sits in it.
#[derive(Clone)]
pub(crate) struct Plan {
    t: &'static Tables,
    /// Canonical column / row of frame column / row 0.
    cx: usize,
    cy: usize,
    /// Weight of the gated smoothing of the output (Bayer), from the noise of the frame.
    beta: f32,
    /// `shifts[q][tx]` = (phase of the tap's column, offset of its index in that phase's plane).
    shifts: Vec<[(usize, isize); K]>,
}

impl Plan {
    /// The plan for a frame whose sample `(0, 0)` is at sensor position `origin`, or `None`
    /// when the pattern is neither a Bayer nor an X-Trans arrangement (a shifted copy of the
    /// canonical one is fine). `chan` maps `CfaColor` values to channels 0..3.
    pub(crate) fn new(cfa: &CfaPattern, chan: &[u8; 8], origin: (usize, usize)) -> Option<Plan> {
        for t in [bayer(), xtrans()] {
            let p = t.period;
            if cfa.width() != p || cfa.height() != p {
                continue;
            }
            for sy in 0..p {
                for sx in 0..p {
                    let ok = (0..p).all(|y| {
                        (0..p).all(|x| chan[cfa.color_at(x, y) as usize] == t.canon[((y + sy) % p) * p + (x + sx) % p])
                    });
                    if ok {
                        let (cx, cy) = ((sx + origin.0) % p, (sy + origin.1) % p);
                        let off = |q: usize| (q + p - cx) % p; // frame column offset of phase q
                        let shifts = (0..p)
                            .map(|q| {
                                std::array::from_fn(|tx| {
                                    let dx = tx as isize - R as isize;
                                    let q2 = (q as isize + dx).rem_euclid(p as isize) as usize;
                                    (q2, (off(q) as isize + dx - off(q2) as isize) / p as isize)
                                })
                            })
                            .collect();
                        return Some(Plan { t, cx, cy, beta: 0.0, shifts });
                    }
                }
            }
        }
        None
    }

    /// Develop rows `y0..y1` (frame coordinates) and columns `xa..xb` into `out`
    /// (`(y1 - y0) * (xb - xa)` camera RGB pixels, white balanced by `wb`, indexed by
    /// channel). Needs `HALO <= y0`, `y1 + HALO <= height`, `HALO <= xa`, `xb + HALO <= w`.
    /// `fetch(y, row)` fills `row` (`w` samples) with the normalised mosaic row `y`.
    #[cfg(test)]
    pub(crate) fn develop(
        &self,
        w: usize,
        rows: (usize, usize),
        cols: (usize, usize),
        wb: &[f32; 4],
        fetch: &mut dyn FnMut(usize, &mut [f32]),
        out: &mut [[f32; 3]],
    ) {
        let nc = cols.1 - cols.0;
        self.develop_chunks(w, rows, cols, wb, fetch, &mut |c0, cw, px| {
            for (i, r) in px.chunks_exact(cw).enumerate() {
                out[i * nc + c0..i * nc + c0 + cw].copy_from_slice(r);
            }
        })
    }

    /// [`Plan::develop`] handing the result over in column chunks: `emit(c0, cw, pixels)` gets
    /// columns `xa + c0..xa + c0 + cw` of every row (`(y1 - y0) * cw` pixels, row by row).
    pub(crate) fn develop_chunks(
        &self,
        w: usize,
        rows: (usize, usize),
        cols: (usize, usize),
        wb: &[f32; 4],
        fetch: &mut dyn FnMut(usize, &mut [f32]),
        out: &mut crate::process::PixelSink<'_>,
    ) {
        // The same source compiled for AVX2 (without FMA: every lane performs the same
        // multiplies and adds in the same order, so results are bit-identical to the baseline
        // build on any CPU) gives 8-wide vectors in the filter loops.
        #[cfg(all(feature = "simd", target_arch = "x86_64"))]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: the CPU supports AVX2 (checked above).
            return unsafe { self.develop_avx2(w, rows, cols, wb, fetch, out) };
        }
        self.develop_impl(w, rows, cols, wb, fetch, out)
    }

    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[target_feature(enable = "avx2")]
    unsafe fn develop_avx2(
        &self,
        w: usize,
        rows: (usize, usize),
        cols: (usize, usize),
        wb: &[f32; 4],
        fetch: &mut dyn FnMut(usize, &mut [f32]),
        out: &mut crate::process::PixelSink<'_>,
    ) {
        self.develop_impl(w, rows, cols, wb, fetch, out)
    }

    #[inline(always)]
    fn develop_impl(
        &self,
        w: usize,
        (y0, y1): (usize, usize),
        (xa, xb): (usize, usize),
        wb: &[f32; 4],
        fetch: &mut dyn FnMut(usize, &mut [f32]),
        out: &mut crate::process::PixelSink<'_>,
    ) {
        debug_assert!(y0 >= HALO && xa >= HALO && xb + HALO <= w);
        // Columns are developed in chunks of `CHUNK`, so the working planes of a chunk stay in
        // cache (a band across a whole frame is tens of megabytes). The rows of the band are
        // normalised once; a chunk sees a frame that starts at column `x0` (a multiple of the
        // CFA period, so the pattern phase does not change) and is as wide as it needs. A
        // pixel depends only on its neighbourhood, so the result is that of the whole band.
        const CHUNK: usize = 240;
        let p = self.t.period;
        let side = 2 + usize::from(self.beta > 0.0) + CORE_HALO;
        let (ry0, ry1) = (y0 - side, y1 + side);
        let mut band = vec![0f32; (ry1 - ry0) * w];
        for (i, row) in band.chunks_exact_mut(w).enumerate() {
            fetch(ry0 + i, row);
        }
        let nr = y1 - y0;
        let mut tmp = Vec::new();
        let mut ca = xa;
        while ca < xb {
            let cb = (ca + CHUNK).min(xb);
            let x0 = (ca - side) / p * p;
            let wl = (cb + side).min(w) - x0;
            tmp.clear();
            tmp.resize(nr * (cb - ca), [0f32; 3]);
            let mut local = |y: usize, row: &mut [f32]| {
                let o = (y - ry0) * w + x0;
                row.copy_from_slice(&band[o..o + wl]);
            };
            self.develop_chroma(wl, (y0, y1), (ca - x0, cb - x0), wb, &mut local, &mut tmp);
            out(ca - xa, cb - ca, &tmp);
            ca = cb;
        }
    }

    /// The plan in the form the GPU kernels take (`flashraw_gpu::EdgeSpec`): the tables, where
    /// the CFA sits in them and the smoothing weight.
    #[cfg(feature = "gpu")]
    pub(crate) fn gpu_spec(&self) -> flashraw_gpu::EdgeSpec {
        let t: &'static Tables = self.t;
        flashraw_gpu::EdgeSpec {
            period: t.period as u32,
            cx: self.cx as u32,
            cy: self.cy as u32,
            beta: self.beta,
            halo: HALO as u32,
            canon: &t.canon,
            g0: &t.g0,
            dir: &t.dir,
            ahd: false,
        }
    }

    /// The AHD demosaic of the same frame for the GPU kernels: the pattern of this plan (a
    /// Bayer one), no smoothing and the border of [`crate::ahd::HALO`] pixels.
    #[cfg(feature = "gpu")]
    pub(crate) fn gpu_ahd_spec(&self) -> Option<flashraw_gpu::EdgeSpec> {
        (self.t.period == 2).then(|| flashraw_gpu::EdgeSpec {
            beta: 0.0,
            halo: crate::ahd::HALO as u32,
            ahd: true,
            ..self.gpu_spec()
        })
    }

    /// Estimate the noise of a frame of `w` x `h` samples and set the weight of the
    /// smoothing pass (module docs, step 5) from it. Deterministic and independent of any
    /// crop: 48 rows spread over the whole frame, so call it with the full frame's `fetch`.
    pub(crate) fn set_noise_from(&mut self, w: usize, h: usize, g_gain: f32, fetch: &mut dyn FnMut(usize, &mut [f32])) {
        self.beta = 0.0;
        if h < 16 || w < 16 {
            return;
        }
        const ROWS: usize = 48;
        let mut row = vec![0f32; w];
        let mut v: Vec<f32> = Vec::with_capacity(ROWS * w / 2);
        for n in 0..ROWS {
            let y = 4 + n * (h - 9) / (ROWS - 1);
            fetch(y, &mut row);
            let p = self.t.period;
            let green = |x: usize| self.t.canon[((y + self.cy) % p) * p + (x + self.cx) % p] == 1;
            for x in 0..w - 2 {
                if !green(x) {
                    continue;
                }
                // Same-colour green pairs one or two samples apart (two apart on Bayer).
                for d in 1..=2 {
                    if !green(x + d) {
                        continue;
                    }
                    let (a, b) = (row[x] * g_gain, row[x + d] * g_gain);
                    let mid = 0.5 * (a + b);
                    if mid > 0.03 && mid < 0.6 {
                        v.push((a - b).abs() / mid.sqrt());
                    }
                }
            }
        }
        if v.len() < 1000 {
            return;
        }
        // The lower quintile of |a - b| / sqrt(mean) comes from the flattest areas, where the
        // difference of two samples of variance nu * mean has a half-normal distribution.
        let k = v.len() / 5;
        let q = *v.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1;
        let sigma = q / (std::f32::consts::SQRT_2 * 0.2533);
        let nu = sigma * sigma;
        self.beta = 0.5 * ((nu - 2e-4) / 6e-4).clamp(0.0, 1.0);
    }

    /// Apply a user level to the estimated smoothing weight: `Off` clears it, the other levels
    /// are floors (`Auto` keeps the estimate).
    pub(crate) fn apply_level(&mut self, level: crate::NoiseReduction) {
        match level.floor() {
            None => {}
            Some(0.0) => self.beta = 0.0,
            Some(f) => self.beta = self.beta.max(f),
        }
    }

    /// The filters, the two-pass chroma stage and, on a noisy frame, a gated smoothing (module
    /// docs, steps 4 and 5). Same contract as [`Plan::develop`].
    #[inline(always)]
    fn develop_chroma(
        &self,
        w: usize,
        (y0, y1): (usize, usize),
        (xa, xb): (usize, usize),
        wb: &[f32; 4],
        fetch: &mut dyn FnMut(usize, &mut [f32]),
        out: &mut [[f32; 3]],
    ) {
        let t = self.t;
        let p = t.period;
        let (nr, nc) = (y1 - y0, xb - xa);
        // `h` extra pixels around the core for the smoothing pass; two for the chroma passes.
        let h = usize::from(self.beta > 0.0);
        let m = 2 + h;
        let (er, ec) = (nr + 2 * m, nc + 2 * m);
        let mut rgb: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0f32; er * ec]);
        self.demosaic(w, (y0 - m, y1 + m), (xa - m, xb + m), wb, fetch, &mut rgb);
        let [r, g, bl] = &rgb;
        // Channel at plane position (iy, ix): the plane starts `m` pixels before (y0, xa).
        let (py, px) = ((y0 - m + self.cy) % p, (xa - m + self.cx) % p);
        // Masks (1.0 or 0.0) per row kind and plane column: the sample is red / blue. They let
        // the loops below handle every pixel alike, over whole rows.
        let mask = |ch: usize| -> Vec<Vec<f32>> {
            (0..p)
                .map(|rk| {
                    (0..ec).map(|x| if t.canon[rk * p + (x + px) % p] as usize == ch { 1.0 } else { 0.0 }).collect()
                })
                .collect()
        };
        let masks = [mask(0), mask(2)];
        // Pass 1, over the core plus `1 + h` pixels: green plus the gated mean of the measured
        // differences of the 3x3 window (Bayer) or the median of the filtered differences
        // (X-Trans). `d1[ch / 2]` holds the colour difference (R-G, B-G).
        let (e1r, e1c) = (nr + 2 + 2 * h, nc + 2 + 2 * h);
        let mut d1 = [vec![0f32; e1r * e1c], vec![0f32; e1r * e1c]];
        let planes = [r, bl];
        if p != 2 {
            let mut med = vec![0f32; e1c];
            for (cc, pl) in planes.iter().enumerate() {
                let df: Vec<f32> = pl.iter().zip(g).map(|(a, b)| a - b).collect();
                for i in 0..e1r {
                    median_row(&df, ec, i, &mut med);
                    let own = &masks[cc][(i + 1 + py) % p][1..1 + e1c];
                    let dc = &df[(i + 1) * ec + 1..(i + 1) * ec + 1 + e1c];
                    for (j, o) in d1[cc][i * e1c..(i + 1) * e1c].iter_mut().enumerate() {
                        *o = if own[j] != 0.0 { dc[j] } else { med[j] };
                    }
                }
            }
        }
        // Both passes work on blocks of `CB` columns with their sums in registers, a block
        // `run!` once with the constant `CB` (full blocks) and once with the remainder; every
        // sum adds the same terms in the same order as a per-column loop would.
        const CB: usize = 32;
        macro_rules! blocks {
            ($n:expr, |$j0:ident, $len:ident| $body:block) => {{
                let n = $n;
                let mut $j0 = 0;
                while $j0 + CB <= n {
                    let $len = CB;
                    $body
                    $j0 += CB;
                }
                if $j0 < n {
                    let $len = n - $j0;
                    $body
                }
            }};
        }
        let mut inv = vec![0f32; e1c];
        for i in (0..e1r).filter(|_| p == 2) {
            let gc = &g[(i + 1) * ec + 1..(i + 1) * ec + 1 + e1c];
            for (v, &x) in inv.iter_mut().zip(gc) {
                *v = 1.0 / (0.2 * x.max(0.0) + 0.015);
            }
            let rk1 = (i + 1 + py) % p;
            blocks!(e1c, |j0, len| {
                // Per colour, the sums over its sites: weights, weighted differences,
                // differences, count.
                let mut a = [[[0f32; CB]; 4]; 2];
                for dy in 0..3 {
                    let rk = (i + dy + py) % p;
                    for dx in 0..3 {
                        let k0 = (i + dy) * ec + dx + j0;
                        let (gk, gb, ib) = (&g[k0..k0 + len], &gc[j0..j0 + len], &inv[j0..j0 + len]);
                        let ms = [&masks[0][rk][dx + j0..dx + j0 + len], &masks[1][rk][dx + j0..dx + j0 + len]];
                        let pk = [&planes[0][k0..k0 + len], &planes[1][k0..k0 + len]];
                        let [[sw0, sd0, su0, n0], [sw1, sd1, su1, n1]] = &mut a;
                        let (m0, m1, p0, p1) = (ms[0], ms[1], pk[0], pk[1]);
                        for l in 0..len {
                            // The gate weight does not depend on the colour: once for both.
                            let e = (gk[l] - gb[l]) * ib[l];
                            let wt = 1.0 / (1.0 + e * e);
                            let (m, d) = (m0[l], p0[l] - gk[l]);
                            sw0[l] += m * wt;
                            sd0[l] += (m * wt) * d;
                            su0[l] += m * d;
                            n0[l] += m;
                            let (m, d) = (m1[l], p1[l] - gk[l]);
                            sw1[l] += m * wt;
                            sd1[l] += (m * wt) * d;
                            su1[l] += m * d;
                            n1[l] += m;
                        }
                    }
                }
                for (cc, [sw, sd, su, n]) in a.iter().enumerate() {
                    let own = &masks[cc][rk1][1 + j0..1 + j0 + len];
                    let pc = &planes[cc][(i + 1) * ec + 1 + j0..(i + 1) * ec + 1 + j0 + len];
                    let o = &mut d1[cc][i * e1c + j0..i * e1c + j0 + len];
                    for l in 0..len {
                        // Every 3x3 window holds at least two sites of each colour, and every
                        // weight is at least 1 / (1 + e^2) > 0; the mean is a safeguard.
                        let mixed = if sw[l] > 1e-12 { sd[l] / sw[l] } else { su[l] / n[l] };
                        o[l] = if own[l] != 0.0 { pc[l] - gc[j0 + l] } else { mixed };
                    }
                }
            });
        }
        // Pass 2, over the core plus `h` pixels: the same gate over the differences of every
        // pixel (measured and pass-1 values), for the two colours the pixel does not measure.
        let (e2r, e2c) = (nr + 2 * h, nc + 2 * h);
        let mut v2 = vec![[0f32; 3]; e2r * e2c];
        // Weight of the green re-estimate at red and blue sites (X-Trans only): full on clean
        // frames, none on the noisiest (the measured red and blue samples carry more noise).
        let xg = if p == 6 { (1.0 - 2.0 * self.beta).clamp(0.0, 1.0) } else { 0.0 };
        let mut inv = vec![0f32; e2c];
        for i in 0..e2r {
            let c0 = (i + 2) * ec + 2;
            let gc = &g[c0..c0 + e2c];
            for (v, &x) in inv.iter_mut().zip(gc) {
                *v = 1.0 / (0.1 * x.max(0.0) + 0.007);
            }
            let rk = (i + 2 + py) % p;
            blocks!(e2c, |j0, len| {
                let [mut sw, mut sdr, mut sdb] = [[0f32; CB]; 3];
                for dy in 0..3 {
                    for dx in 0..3 {
                        let k0 = (i + dy + 1) * ec + dx + 1 + j0;
                        let (gk, gb, ib) = (&g[k0..k0 + len], &gc[j0..j0 + len], &inv[j0..j0 + len]);
                        let q0 = (i + dy) * e1c + dx + j0;
                        let (dr, db) = (&d1[0][q0..q0 + len], &d1[1][q0..q0 + len]);
                        for l in 0..len {
                            let e = (gk[l] - gb[l]) * ib[l];
                            let wt = 1.0 / (1.0 + e * e);
                            sw[l] += wt;
                            sdr[l] += wt * dr[l];
                            sdb[l] += wt * db[l];
                        }
                    }
                }
                let (own_r, own_b) = (&masks[0][rk][2 + j0..2 + j0 + len], &masks[1][rk][2 + j0..2 + j0 + len]);
                let (rc, bc) = (&r[c0 + j0..c0 + j0 + len], &bl[c0 + j0..c0 + j0 + len]);
                let o = &mut v2[i * e2c + j0..i * e2c + j0 + len];
                for l in 0..len {
                    let (dr, db) = (sdr[l] / sw[l], sdb[l] / sw[l]);
                    // X-Trans: green at a red or blue site moves towards the measured sample minus
                    // the cleaned colour difference (module docs, step 4).
                    let g0 = gc[j0 + l];
                    let gv = if xg == 0.0 {
                        g0
                    } else if own_r[l] != 0.0 {
                        g0 + xg * ((rc[l] - dr) - g0)
                    } else if own_b[l] != 0.0 {
                        g0 + xg * ((bc[l] - db) - g0)
                    } else {
                        g0
                    };
                    o[l] = [
                        if own_r[l] != 0.0 { rc[l] } else { gv + dr },
                        gv,
                        if own_b[l] != 0.0 { bc[l] } else { gv + db },
                    ];
                }
            });
        }
        if h == 0 {
            out.copy_from_slice(&v2);
            return;
        }
        smooth(&v2, e2c, self.beta, nr, nc, out);
    }

    /// The filters over `y0..y1` x `xa..xb`, into three planes of `(y1-y0) * (xb-xa)`; needs
    /// `CORE_HALO` samples around.
    #[inline(always)]
    fn demosaic(
        &self,
        w: usize,
        (y0, y1): (usize, usize),
        (xa, xb): (usize, usize),
        wb: &[f32; 4],
        fetch: &mut dyn FnMut(usize, &mut [f32]),
        out: &mut [Vec<f32>; 3],
    ) {
        let t = self.t;
        let p = t.period;
        let (nr, nc) = (y1 - y0, xb - xa);
        debug_assert!(y0 >= CORE_HALO && xa >= CORE_HALO && xb + CORE_HALO <= w && out[0].len() == nr * nc);
        let by0 = y0 - CORE_HALO;
        let nrb = nr + 2 * CORE_HALO;
        let pw = w.div_ceil(p) + 1;
        let off = |q: usize| (q + p - self.cx) % p;

        // 1. White balanced mosaic, split by column phase so every filter tap is a
        //    contiguous run.
        let mut planes = vec![0f32; p * nrb * pw + LANES];
        let mut row = vec![0f32; w];
        for lr in 0..nrb {
            fetch(by0 + lr, &mut row);
            let cr = (by0 + lr + self.cy) % p;
            for q in 0..p {
                // Clipped samples end up at the white level in every channel (the output
                // clamps there too).
                let scale = wb[t.canon[cr * p + q] as usize];
                let dst = &mut planes[(q * nrb + lr) * pw..(q * nrb + lr + 1) * pw];
                for (d, &v) in dst.iter_mut().zip(row[off(q)..].iter().step_by(p)) {
                    *d = (v * scale).min(1.0);
                }
            }
        }

        // Columns of phase q within [lo, hi): indices j with x = p*j + off(q).
        let jrange = |q: usize, lo: usize, hi: usize| {
            let o = off(q);
            let j0 = lo.saturating_sub(o).div_ceil(p);
            let j1 = hi.saturating_sub(o).div_ceil(p);
            (j0, j1.max(j0))
        };
        // Start of the run read by tap column `tx` for outputs `j0..` of phase `q` at local row `r`.
        let src = |r: usize, q: usize, tx: usize, j0: usize| {
            let (q2, js) = self.shifts[q][tx];
            (r + q2 * nrb) * pw + (j0 as isize + js) as usize
        };

        // 2. sqrt(G0) on rows y0-2..y1+2, columns xa-2..xb+2.
        let (gr, gc) = (nr + 4, nc + 4);
        let mut g = vec![0f32; gr * gc];
        for i in 0..gr {
            let y = y0 + i - 2;
            let cr = (y + self.cy) % p;
            let lr = y - by0;
            for q in 0..p {
                let (j0, j1) = jrange(q, xa - 2, xb + 2);
                if j1 == j0 {
                    continue;
                }
                let n = j1 - j0;
                let mut starts = [0usize; TAPS];
                for ty in 0..K {
                    for tx in 0..K {
                        starts[ty * K + tx] = src(lr + ty - R, q, tx, j0);
                    }
                }
                apply_taps(&t.g0_taps[cr * p + q], &starts, &planes, n, |k0, len, acc| {
                    for l in 0..len {
                        let x = p * (j0 + k0 + l) + off(q);
                        g[i * gc + x - (xa - 2)] = acc[0][l].max(0.0).sqrt();
                    }
                });
            }
        }

        // 3. Direction weights from the squared gradients of sqrt(G0): per direction the
        //    sum over 3x3 of the squared derivative, then w = 1/(E+eps)^2 normalised.
        let (dr, dc) = (nr + 2, nc + 2);
        let mut dsq: [Vec<f32>; NDIR] = std::array::from_fn(|_| vec![0f32; dr * dc]);
        for a in 0..dr {
            let rm = &g[a * gc..a * gc + gc];
            let r0 = &g[(a + 1) * gc..(a + 2) * gc];
            let rp = &g[(a + 2) * gc..(a + 3) * gc];
            // Pre-cut slices of length `dc`, so the loop has no bounds checks and vectorises.
            let (h0, h2) = (&r0[..dc], &r0[2..2 + dc]);
            let (vm, vp) = (&rm[1..1 + dc], &rp[1..1 + dc]);
            let (m0, m2, p0, p2) = (&rm[..dc], &rm[2..2 + dc], &rp[..dc], &rp[2..2 + dc]);
            let [q0, q1, q2, q3] = &mut dsq;
            let r = a * dc..(a + 1) * dc;
            let (q0, q1, q2, q3) = (&mut q0[r.clone()], &mut q1[r.clone()], &mut q2[r.clone()], &mut q3[r]);
            for b in 0..dc {
                let dh = h2[b] - h0[b];
                let dv = vp[b] - vm[b];
                let d1 = (p2[b] - m0[b]) * FRAC_1_SQRT_2;
                let d2 = (p0[b] - m2[b]) * FRAC_1_SQRT_2;
                q0[b] = dh * dh;
                q1[b] = dv * dv;
                q2[b] = d1 * d1;
                q3[b] = d2 * d2;
            }
        }
        let mut hsum: [Vec<f32>; NDIR] = std::array::from_fn(|_| vec![0f32; dr * nc]);
        for d in 0..NDIR {
            for a in 0..dr {
                let s = &dsq[d][a * dc..(a + 1) * dc];
                let (s0, s1, s2) = (&s[..nc], &s[1..1 + nc], &s[2..2 + nc]);
                let o = &mut hsum[d][a * nc..(a + 1) * nc];
                for j in 0..nc {
                    o[j] = s0[j] + s1[j] + s2[j];
                }
            }
        }
        let mut wts: [Vec<f32>; NDIR] = std::array::from_fn(|_| vec![0f32; nr * nc]);
        let mut inv = vec![0f32; nc];
        for i in 0..nr {
            for d in 0..NDIR {
                let h = &hsum[d];
                let (a, b, c) =
                    (&h[i * nc..(i + 1) * nc], &h[(i + 1) * nc..(i + 2) * nc], &h[(i + 2) * nc..(i + 3) * nc]);
                let o = &mut wts[d][i * nc..(i + 1) * nc];
                for j in 0..nc {
                    let u = a[j] + b[j] + c[j] + EPS;
                    o[j] = 1.0 / (u * u);
                }
            }
            let [w0, w1, w2, w3] = &mut wts;
            let r = i * nc..(i + 1) * nc;
            let (w0, w1, w2, w3) = (&mut w0[r.clone()], &mut w1[r.clone()], &mut w2[r.clone()], &mut w3[r]);
            for j in 0..nc {
                inv[j] = 1.0 / (w0[j] + w1[j] + w2[j] + w3[j]);
            }
            for j in 0..nc {
                w0[j] *= inv[j];
                w1[j] *= inv[j];
                w2[j] *= inv[j];
                w3[j] *= inv[j];
            }
        }

        // 4. The filters, `LANES` columns at a time with the accumulators in registers.
        let mut res = [vec![[0f32; NDIR]; pw + LANES], vec![[0f32; NDIR]; pw + LANES]];
        for i in 0..nr {
            let y = y0 + i;
            let lr = y - by0;
            let cr = (y + self.cy) % p;
            for q in 0..p {
                let (j0, j1) = jrange(q, xa, xb);
                if j1 == j0 {
                    continue;
                }
                let n = j1 - j0;
                let own = t.canon[cr * p + q] as usize;
                let miss: [usize; 2] = match own {
                    0 => [1, 2],
                    1 => [0, 2],
                    _ => [0, 1],
                };
                let mut starts = [0usize; TAPS];
                for ty in 0..K {
                    for tx in 0..K {
                        starts[ty * K + tx] = src(lr + ty - R, q, tx, j0);
                    }
                }
                for (m, &c) in miss.iter().enumerate() {
                    let r = &mut res[m];
                    apply_taps(&t.dir_taps[cr * p + q][c], &starts, &planes, n, |k0, len, acc| {
                        for l in 0..len {
                            r[k0 + l] = std::array::from_fn(|d| acc[d][l]);
                        }
                    });
                }
                for k in 0..n {
                    let x = p * (j0 + k) + off(q) - xa;
                    let o = i * nc + x;
                    let wd = [wts[0][o], wts[1][o], wts[2][o], wts[3][o]];
                    for (m, &c) in miss.iter().enumerate() {
                        let a = &res[m][k];
                        out[c][o] = wd[0] * a[0] + wd[1] * a[1] + wd[2] * a[2] + wd[3] * a[3];
                    }
                    out[own][o] = planes[(lr + q * nrb) * pw + j0 + k];
                }
            }
        }
    }
}

/// Smoothing (module docs, step 5): each pixel of the `nr` x `nc` core of `v` (rows of `ec`
/// pixels, one pixel of margin on every side) moves by `beta` towards the mean of its 3x3
/// window, a neighbour weighing `1 / (1 + e^2)` with `e = (G_neighbour - G) / (0.2 G + 0.015)`.
#[inline(always)]
fn smooth(v: &[[f32; 3]], ec: usize, beta: f32, nr: usize, nc: usize, out: &mut [[f32; 3]]) {
    for i in 0..nr {
        for j in 0..nc {
            let p = v[(i + 1) * ec + j + 1];
            let inv = 1.0 / (0.2 * p[1].max(0.0) + 0.015);
            let (mut sw, mut s) = (0f32, [0f32; 3]);
            for dy in 0..3 {
                for dx in 0..3 {
                    let q = v[(i + dy) * ec + j + dx];
                    let e = (q[1] - p[1]) * inv;
                    let wt = 1.0 / (1.0 + e * e);
                    sw += wt;
                    for c in 0..3 {
                        s[c] += wt * q[c];
                    }
                }
            }
            out[i * nc + j] = [0, 1, 2].map(|c| p[c] + beta * (s[c] / sw - p[c]));
        }
    }
}

/// Medians of the 3x3 neighbourhoods of row `i + 1` of `a` (`ec` wide), for the columns
/// `1..out.len() + 1`, into `out`.
fn median_row(a: &[f32], ec: usize, i: usize, out: &mut [f32]) {
    // Each column of three is sorted once and shared by the three windows that contain it;
    // the median of the window is then the median of (largest of the column minima, median
    // of the column medians, smallest of the column maxima), the same value as sorting all
    // nine (a median is one of the inputs, so no rounding is involved).
    let n = out.len();
    let (r0, r1, r2) =
        (&a[i * ec..i * ec + n + 2], &a[(i + 1) * ec..(i + 1) * ec + n + 2], &a[(i + 2) * ec..(i + 2) * ec + n + 2]);
    let m = n + 2;
    let (mut lo, mut mi, mut hi) = (vec![0f32; m], vec![0f32; m], vec![0f32; m]);
    for j in 0..m {
        let (a, b, c) = (r0[j], r1[j], r2[j]);
        let (x, y) = (a.min(b), a.max(b));
        let (y, z) = (y.min(c), y.max(c));
        let (x, y) = (x.min(y), x.max(y));
        lo[j] = x;
        mi[j] = y;
        hi[j] = z;
    }
    let (l0, l1, l2) = (&lo[..n], &lo[1..n + 1], &lo[2..n + 2]);
    let (m0, m1, m2) = (&mi[..n], &mi[1..n + 1], &mi[2..n + 2]);
    let (h0, h1, h2) = (&hi[..n], &hi[1..n + 1], &hi[2..n + 2]);
    for j in 0..n {
        let a = l0[j].max(l1[j]).max(l2[j]);
        let c = h0[j].min(h1[j]).min(h2[j]);
        let b = med3(m0[j], m1[j], m2[j]);
        out[j] = med3(a, b, c);
    }
}

/// Median of three values.
#[inline(always)]
fn med3(a: f32, b: f32, c: f32) -> f32 {
    a.min(b).max(a.max(b).min(c))
}

/// Median of nine values (a fixed comparison network, branch free): the reference for
/// [`median_row`] in the tests.
#[cfg(test)]
fn median9(mut p: [f32; 9]) -> f32 {
    #[inline(always)]
    fn sort(p: &mut [f32; 9], a: usize, b: usize) {
        let (lo, hi) = (p[a].min(p[b]), p[a].max(p[b]));
        p[a] = lo;
        p[b] = hi;
    }
    sort(&mut p, 1, 2);
    sort(&mut p, 4, 5);
    sort(&mut p, 7, 8);
    sort(&mut p, 0, 1);
    sort(&mut p, 3, 4);
    sort(&mut p, 6, 7);
    sort(&mut p, 1, 2);
    sort(&mut p, 4, 5);
    sort(&mut p, 7, 8);
    sort(&mut p, 0, 3);
    sort(&mut p, 5, 8);
    sort(&mut p, 4, 7);
    sort(&mut p, 3, 6);
    sort(&mut p, 1, 4);
    sort(&mut p, 2, 5);
    sort(&mut p, 4, 7);
    sort(&mut p, 4, 2);
    sort(&mut p, 6, 4);
    sort(&mut p, 4, 2);
    p[4]
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashraw_core::CfaPattern;

    fn chan() -> [u8; 8] {
        [0, 1, 2, 3, 3, 3, 3, 3]
    }

    fn run(plan: &Plan, w: usize, h: usize, mosaic: &[f32], y: (usize, usize), x: (usize, usize)) -> Vec<[f32; 3]> {
        let mut out = vec![[0f32; 3]; (y.1 - y.0) * (x.1 - x.0)];
        let mut fetch = |yy: usize, row: &mut [f32]| row.copy_from_slice(&mosaic[yy * w..(yy + 1) * w]);
        let _ = h;
        plan.develop(w, y, x, &[1.0; 4], &mut fetch, &mut out);
        out
    }

    fn patterns() -> Vec<(&'static str, CfaPattern)> {
        vec![
            ("RGGB", CfaPattern::RGGB),
            ("GBRG", CfaPattern::GBRG),
            ("BGGR", CfaPattern::BGGR),
            ("xtrans", CfaPattern::parse("GGRGGB/GGBGGR/BRGRBG/GGBGGR/GGRGGB/RBGBRG").unwrap()),
            ("xtrans shifted", CfaPattern::parse("GGRGGB/GGBGGR/BRGRBG/GGBGGR/GGRGGB/RBGBRG").unwrap().shifted(2, 1)),
        ]
    }

    #[test]
    fn flat_field_stays_flat() {
        let (w, h) = (64, 48);
        for (name, cfa) in patterns() {
            let plan = Plan::new(&cfa, &chan(), (0, 0)).unwrap_or_else(|| panic!("{name}: no plan"));
            let m = vec![0.4f32; w * h];
            let out = run(&plan, w, h, &m, (HALO, h - HALO), (HALO, w - HALO));
            for px in out {
                for v in px {
                    assert!((v - 0.4).abs() < 1e-4, "{name}: {v}");
                }
            }
        }
    }

    #[test]
    fn keeps_measured_samples_and_interpolates_a_ramp() {
        let (w, h) = (64, 48);
        for (name, cfa) in patterns() {
            let plan = Plan::new(&cfa, &chan(), (0, 0)).unwrap();
            // Smooth grey ramp: every colour has the same value at a given position.
            let m: Vec<f32> = (0..w * h).map(|i| 0.2 + 0.004 * (i % w) as f32 + 0.002 * (i / w) as f32).collect();
            let out = run(&plan, w, h, &m, (HALO, h - HALO), (HALO, w - HALO));
            for (n, px) in out.iter().enumerate() {
                let (x, y) = (HALO + n % (w - 2 * HALO), HALO + n / (w - 2 * HALO));
                let want = m[y * w + x];
                let own = chan()[cfa.color_at(x, y) as usize] as usize;
                assert_eq!(px[own], want, "{name}: measured sample at ({x},{y})");
                for v in px {
                    assert!((v - want).abs() < 0.02, "{name}: ({x},{y}) {v} vs {want}");
                }
            }
        }
    }

    #[test]
    fn smoothing_keeps_bands_and_crops_identical_and_flat_fields_flat() {
        let (w, h) = (90, 60);
        let mut s = 777u32;
        let m: Vec<f32> = (0..w * h)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 8) as f32 / (1u32 << 24) as f32
            })
            .collect();
        for (name, cfa) in patterns() {
            let mut plan = Plan::new(&cfa, &chan(), (0, 0)).unwrap();
            plan.beta = 0.4;
            let all = run(&plan, w, h, &m, (HALO, h - HALO), (HALO, w - HALO));
            let nc = w - 2 * HALO;
            let part = run(&plan, w, h, &m, (9, 20), (13, 41));
            for (i, y) in (9..20).enumerate() {
                for (j, x) in (13..41).enumerate() {
                    assert_eq!(part[i * 28 + j], all[(y - HALO) * nc + x - HALO], "{name}: ({x},{y})");
                }
            }
            let flat = vec![0.3f32; w * h];
            for px in run(&plan, w, h, &flat, (HALO, h - HALO), (HALO, w - HALO)) {
                for c in px {
                    assert!((c - 0.3).abs() < 1e-5, "{name}: {px:?}");
                }
            }
            // Smoothing changes the result of noise, not of nothing.
            plan.beta = 0.0;
            assert_ne!(run(&plan, w, h, &m, (HALO, h - HALO), (HALO, w - HALO)), all, "{name}");
        }
    }

    /// The AVX2 build of the develop must give exactly the baseline's bits.
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    #[test]
    fn avx2_develop_is_bit_identical_to_the_baseline() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let (w, h) = (90, 60);
        let mut s = 4242u32;
        let m: Vec<f32> = (0..w * h)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 8) as f32 / (1u32 << 24) as f32
            })
            .collect();
        for (name, cfa) in patterns() {
            let mut plan = Plan::new(&cfa, &chan(), (0, 0)).unwrap();
            for beta in [0.0, 0.4] {
                plan.beta = beta;
                let (y, x) = ((HALO, h - HALO), (HALO, w - HALO));
                let mut a = vec![[0f32; 3]; (y.1 - y.0) * (x.1 - x.0)];
                let mut b = a.clone();
                let mut fetch = |yy: usize, row: &mut [f32]| row.copy_from_slice(&m[yy * w..(yy + 1) * w]);
                let nc = x.1 - x.0;
                fn into(out: &mut [[f32; 3]], nc: usize) -> impl FnMut(usize, usize, &[[f32; 3]]) + '_ {
                    move |c0, cw, px| {
                        for (i, r) in px.chunks_exact(cw).enumerate() {
                            out[i * nc + c0..i * nc + c0 + cw].copy_from_slice(r);
                        }
                    }
                }
                plan.develop_impl(w, y, x, &[1.1, 1.0, 0.9, 1.0], &mut fetch, &mut into(&mut a, nc));
                // SAFETY: AVX2 was detected above.
                unsafe { plan.develop_avx2(w, y, x, &[1.1, 1.0, 0.9, 1.0], &mut fetch, &mut into(&mut b, nc)) };
                assert!(
                    a.iter().zip(&b).all(|(p, q)| p.iter().zip(q).all(|(u, v)| u.to_bits() == v.to_bits())),
                    "{name} {beta}"
                );
            }
        }
    }

    #[test]
    fn noise_estimate_separates_clean_from_noisy_frames() {
        let (w, h) = (256, 256);
        let (_, cfa) = patterns().remove(0);
        let mut s = 99u32;
        let mut gauss = || {
            let mut u = || {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                ((s >> 8) as f32 + 0.5) / (1u32 << 24) as f32
            };
            (-2.0 * u().ln()).sqrt() * (std::f32::consts::TAU * u()).cos()
        };
        let beta_for = |sigma_rel: f32, gauss: &mut dyn FnMut() -> f32| {
            // A smooth ramp with noise of variance sigma_rel^2 * value.
            let m: Vec<f32> = (0..w * h)
                .map(|i| {
                    let v = 0.1 + 0.3 * ((i % w) as f32 / w as f32);
                    v + gauss() * (sigma_rel * sigma_rel * v).sqrt()
                })
                .collect();
            let mut plan = Plan::new(&cfa, &chan(), (0, 0)).unwrap();
            let mut fetch = |yy: usize, row: &mut [f32]| row.copy_from_slice(&m[yy * w..(yy + 1) * w]);
            plan.set_noise_from(w, h, 1.0, &mut fetch);
            plan.beta
        };
        assert_eq!(beta_for(0.005, &mut gauss), 0.0);
        let noisy = beta_for(0.03, &mut gauss);
        assert!(noisy > 0.45 && noisy <= 0.5, "{noisy}");
        let mid = beta_for(0.017, &mut gauss);
        assert!(mid > 0.0 && mid < 0.5, "{mid}");
    }

    #[test]
    fn noise_levels_are_floors_and_off_clears() {
        use crate::NoiseReduction as N;
        let mut plan = Plan::new(&CfaPattern::RGGB, &chan(), (0, 0)).unwrap();
        for (level, est, want) in [
            (N::Auto, 0.3, 0.3),
            (N::Off, 0.4, 0.0),
            (N::Low, 0.0, 0.25),
            (N::Low, 0.4, 0.4),
            (N::Medium, 0.1, 0.5),
            (N::High, 0.5, 0.8),
        ] {
            plan.beta = est;
            plan.apply_level(level);
            assert_eq!(plan.beta, want, "{level:?} on {est}");
        }
    }

    #[test]
    fn bands_and_crops_agree_with_the_whole_frame() {
        let (w, h) = (70, 53);
        let mut s = 12345u32;
        let m: Vec<f32> = (0..w * h)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 8) as f32 / (1u32 << 24) as f32
            })
            .collect();
        for (name, cfa) in patterns() {
            let plan = Plan::new(&cfa, &chan(), (0, 0)).unwrap();
            let all = run(&plan, w, h, &m, (HALO, h - HALO), (HALO, w - HALO));
            let nc = w - 2 * HALO;
            // Rows 9..20, columns 13..41.
            let part = run(&plan, w, h, &m, (9, 20), (13, 41));
            for (i, y) in (9..20).enumerate() {
                for (j, x) in (13..41).enumerate() {
                    assert_eq!(part[i * 28 + j], all[(y - HALO) * nc + x - HALO], "{name}: ({x},{y})");
                }
            }
        }
    }

    /// Sharp coloured shapes (edges at all angles, thin lines), mosaicked and developed:
    /// the edge-aware demosaic must be clearly closer to the truth than the 3x3 average.
    #[test]
    fn beats_the_3x3_average_on_sharp_edges() {
        let (w, h) = (96, 96);
        let truth: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                let disc = ((x - 40.0).powi(2) + (y - 50.0).powi(2)).sqrt() < 26.0;
                let diag = (x * 0.8 + y * 0.6) as i32 % 18 < 6;
                let line = (y - 20.0).abs() < 0.8 || (x - 70.0).abs() < 0.8;
                match (disc, diag, line) {
                    (_, _, true) => [0.9, 0.1, 0.1],
                    (true, true, _) => [0.1, 0.8, 0.3],
                    (true, false, _) => [0.2, 0.3, 0.9],
                    (false, true, _) => [0.8, 0.7, 0.2],
                    _ => [0.3, 0.3, 0.3],
                }
            })
            .collect();
        for (name, cfa) in patterns() {
            let plan = Plan::new(&cfa, &chan(), (0, 0)).unwrap();
            let mosaic: Vec<f32> =
                (0..w * h).map(|i| truth[i][chan()[cfa.color_at(i % w, i / w) as usize] as usize]).collect();
            let out = run(&plan, w, h, &mosaic, (HALO, h - HALO), (HALO, w - HALO));
            let nc = w - 2 * HALO;
            // Reference: average of the same-colour samples in the 3x3 window.
            let bil = |x: usize, y: usize, c: usize| {
                let (mut s, mut n) = (0.0, 0.0);
                for (dy, dx) in (0..3).flat_map(|a| (0..3).map(move |b| (a, b))) {
                    let (xx, yy) = (x + dx - 1, y + dy - 1);
                    if chan()[cfa.color_at(xx, yy) as usize] as usize == c {
                        s += mosaic[yy * w + xx];
                        n += 1.0;
                    }
                }
                if n > 0.0 { s / n } else { f32::NAN }
            };
            let (mut e_edge, mut e_bil, mut count) = (0.0f64, 0.0f64, 0.0f64);
            for y in HALO..h - HALO {
                for x in HALO..w - HALO {
                    let px = out[(y - HALO) * nc + x - HALO];
                    for c in 0..3 {
                        let b = bil(x, y, c);
                        if b.is_nan() {
                            continue;
                        }
                        e_edge += ((px[c] - truth[y * w + x][c]) as f64).powi(2);
                        e_bil += ((b - truth[y * w + x][c]) as f64).powi(2);
                        count += 1.0;
                    }
                }
            }
            let (m_edge, m_bil) = (e_edge / count, e_bil / count);
            eprintln!("{name}: edge-aware MSE {m_edge:.5} vs 3x3 average {m_bil:.5}");
            assert!(m_edge < m_bil, "{name}: edge-aware MSE {m_edge:.5} vs 3x3 average {m_bil:.5}");
        }
    }

    #[test]
    fn median9_is_the_median() {
        let mut s = 99u32;
        for _ in 0..2000 {
            let v: [f32; 9] = std::array::from_fn(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                ((s >> 16) % 7) as f32
            });
            let mut sorted = v;
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            assert_eq!(median9(v), sorted[4], "{v:?}");
        }
    }

    #[test]
    fn median_row_is_the_median_of_each_window() {
        let (ec, rows) = (37, 4);
        let mut s = 7u32;
        let a: Vec<f32> = (0..ec * rows)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                ((s >> 16) % 11) as f32 * 0.25 - 1.0
            })
            .collect();
        for i in 0..rows - 2 {
            let mut out = vec![0f32; ec - 2];
            median_row(&a, ec, i, &mut out);
            for (j, &o) in out.iter().enumerate() {
                let w: [f32; 9] = std::array::from_fn(|k| a[(i + k / 3) * ec + j + k % 3]);
                assert_eq!(o, median9(w), "row {i} column {j}");
            }
        }
    }

    #[test]
    fn unknown_patterns_have_no_plan() {
        // Greens in a column instead of on a diagonal: not a shifted Bayer pattern.
        let rgbg = CfaPattern::parse("RGBG").unwrap();
        assert!(Plan::new(&rgbg, &chan(), (0, 0)).is_none());
    }
}
