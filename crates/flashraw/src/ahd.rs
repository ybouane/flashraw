//! Homogeneity-directed demosaic ([`crate::Demosaic::Ahd`]) for Bayer sensors.
//!
//! The method of Hirakawa and Parks (adaptive homogeneity-directed demosaicing, 2005), written
//! from the paper's description:
//!
//! 1. **Two candidates.** Green is interpolated along rows (candidate H) and along columns
//!    (candidate V) with the second-difference corrected average of the two nearest greens,
//!    limited to the range of those greens. Red and blue come from colour differences: the
//!    missing colour of a pixel is its green plus the mean of the differences `R - G` (or
//!    `B - G`) measured in the 3x3 window, where each candidate uses its own green plane.
//! 2. **Perceptual space.** Each candidate is mapped to a lightness `L` and two chroma
//!    coordinates `a`, `b` (square-root encoded red, green and blue: `L` is their weighted mean,
//!    `a = R' - G'`, `b = B' - G'`, a cheap stand-in for CIELab that gives the same results on
//!    the benchmarks).
//! 3. **Homogeneity.** For a pixel, the thresholds are the smaller of the largest lightness
//!    (chroma) steps to its two neighbours along the row in candidate H and along the column in
//!    candidate V. A candidate's homogeneity there is the number of the 8 neighbours whose
//!    lightness and chroma are both within the thresholds of the pixel; it is summed over the
//!    3x3 window.
//! 4. **Blend.** The two candidates are mixed in proportion to their summed homogeneity,
//!    `w_H = h_H / (h_H + h_V)` (equal halves when both are zero). The paper picks the candidate
//!    with the larger sum; on the ground-truth tiles the proportional blend is closer to the
//!    original at every noise level (clean +0.25 dB, noisy +0.7 to +0.8 dB) and real files show
//!    fewer false-colour dots, so it is the one used.
//!
//! The support of a pixel is 6 samples (green 2, colour differences 1, steps 1, neighbours 1,
//! window 1), so results do not depend on the band, the column chunk, the thread count or the
//! crop. Pixels closer than 6 samples to the frame border use bilinear.

use flashraw_core::CfaPattern;

/// Mosaic samples needed around a pixel.
pub(crate) const HALO: usize = 6;
/// Output rows per band: bounds the working memory.
pub(crate) const BAND_ROWS: usize = 64;
/// Output columns developed together inside a band.
const CHUNK: usize = 192;

/// The homogeneity-directed demosaic of one frame: the colour (0 R, 1 G, 2 B) of each of the
/// four positions of the 2x2 pattern, indexed by `(y & 1) * 2 + (x & 1)` of the frame.
#[derive(Clone)]
pub(crate) struct Plan {
    col: [u8; 4],
}

impl Plan {
    /// The plan for a frame whose sample `(0, 0)` is at sensor position `origin`, or `None`
    /// when the pattern is not a Bayer arrangement. `chan` maps `CfaColor` values to channels.
    pub(crate) fn new(cfa: &CfaPattern, chan: &[u8; 8], origin: (usize, usize)) -> Option<Plan> {
        if cfa.width() != 2 || cfa.height() != 2 {
            return None;
        }
        let mut col = [0u8; 4];
        for (i, c) in col.iter_mut().enumerate() {
            *c = chan[cfa.color_at(origin.0 + (i & 1), origin.1 + (i >> 1)) as usize];
        }
        let greens = (col[0] == 1 && col[3] == 1) || (col[1] == 1 && col[2] == 1);
        let others = |a: u8, b: u8| (a == 0 && b == 2) || (a == 2 && b == 0);
        let ok = greens && if col[0] == 1 { others(col[1], col[2]) } else { others(col[0], col[3]) };
        ok.then_some(Plan { col })
    }

    /// The colour (0 R, 1 G, 2 B) of each position of the 2x2 pattern, indexed by `(y & 1) * 2 + (x & 1)`.
    pub(crate) fn colours(&self) -> [u8; 4] {
        self.col
    }

    /// Develop rows `y0..y1` (frame coordinates) and columns `xa..xb` into `out`
    /// (`(y1 - y0) * (xb - xa)` camera RGB pixels, white balanced by `wb`, indexed by channel).
    /// Needs `HALO <= y0`, `y1 + HALO <= height`, `HALO <= xa`, `xb + HALO <= w`.
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
    /// columns `xa + c0..xa + c0 + cw` of every row (`(y1 - y0) * cw` pixels, row by row), so
    /// the caller can finish them while they are in cache.
    pub(crate) fn develop_chunks(
        &self,
        w: usize,
        rows: (usize, usize),
        cols: (usize, usize),
        wb: &[f32; 4],
        fetch: &mut dyn FnMut(usize, &mut [f32]),
        out: &mut crate::process::PixelSink<'_>,
    ) {
        // The same source compiled for AVX2 (no FMA, so every lane performs the same operations
        // in the same order: bit-identical to the baseline build) vectorises the plane loops.
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
        let (nr, nc) = (y1 - y0, xb - xa);
        debug_assert!(y0 >= HALO && xa >= HALO && xb + HALO <= w);
        let er = nr + 2 * HALO;
        let bw = nc + 2 * HALO;
        // The balanced mosaic of the band, clipped at the white level like the output.
        let mut m = vec![0f32; er * bw];
        let mut row = vec![0f32; w];
        for i in 0..er {
            let y = y0 + i - HALO;
            fetch(y, &mut row);
            for j in 0..bw {
                let x = xa + j - HALO;
                let c = self.col[(y & 1) * 2 + (x & 1)] as usize;
                m[i * bw + j] = (row[x] * wb[c]).min(1.0);
            }
        }
        let mut work = Work::new(er, CHUNK + 2 * HALO);
        let mut buf = vec![[0f32; 3]; nr * CHUNK.min(nc)];
        let mut c0 = 0;
        while c0 < nc {
            let c1 = (c0 + CHUNK).min(nc);
            let cw = c1 - c0;
            self.chunk(&m, bw, (y0, xa, c0, c1), er, &mut work, &mut buf[..nr * cw]);
            out(c0, cw, &buf[..nr * cw]);
            c0 = c1;
        }
    }

    /// The pair steps of row `i` of both candidates (columns 4 to `ec - 5` start a pair).
    #[inline(always)]
    fn pair_steps(lab: &[[Vec<f32>; 3]; 2], ec: usize, i: usize, out: &mut [PairSteps; 2]) {
        let (x0, n) = (4, ec - 8);
        let base = i * ec + x0;
        for (d, o) in out.iter_mut().enumerate() {
            let [pl, pa, pb] = &lab[d];
            let (l, a, b) = (&pl[base..base + n], &pa[base..base + n], &pb[base..base + n]);
            let step = |off: usize, ol: &mut [f32], oc: &mut [f32]| {
                let (ql, qa, qb) =
                    (&pl[base + off..base + off + n], &pa[base + off..base + off + n], &pb[base + off..base + off + n]);
                let (ol, oc) = (&mut ol[x0..x0 + n], &mut oc[x0..x0 + n]);
                for x in 0..n {
                    ol[x] = (ql[x] - l[x]).abs();
                    oc[x] = (qa[x] - a[x]) * (qa[x] - a[x]) + (qb[x] - b[x]) * (qb[x] - b[x]);
                }
            };
            step(1, &mut o.l, &mut o.c);
            step(ec, &mut o.dl, &mut o.dc);
            step(ec + 1, &mut o.xl, &mut o.xc);
            step(ec - 1, &mut o.yl, &mut o.yc);
        }
    }

    /// One column chunk `c0..c1` (band columns) of the band.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn chunk(
        &self,
        m: &[f32],
        bw: usize,
        (y0, xa, c0, c1): (usize, usize, usize, usize),
        er: usize,
        wk: &mut Work,
        out: &mut [[f32; 3]],
    ) {
        let ec = c1 - c0 + 2 * HALO;
        // Local copy of the chunk's mosaic: local column j is band column c0 + j.
        for i in 0..er {
            wk.m[i * ec..(i + 1) * ec].copy_from_slice(&m[i * bw + c0..i * bw + c0 + ec]);
        }
        // Colour of local (i, j): frame row y0 - HALO + i, column xa - HALO + c0 + j.
        let (py, px) = ((y0 + HALO) & 1, (xa + HALO + c0) & 1); // parity of y0-HALO, xa-HALO+c0 (HALO is even)
        // Masks (1.0 or 0.0) per row kind and column: the sample is green, red, blue, or green
        // with red neighbours along its row. They let the plane loops pick formulas without
        // branches.
        for rk in 0..2 {
            for j in 0..ec {
                let c = self.col[rk * 2 + ((j + px) & 1)];
                let right = self.col[rk * 2 + ((j + 1 + px) & 1)];
                let f = |b: bool| if b { 1.0 } else { 0.0 };
                wk.mask[rk][0][j] = f(c == 1);
                wk.mask[rk][1][j] = f(c == 0);
                wk.mask[rk][2][j] = f(c == 2);
                wk.mask[rk][3][j] = f(c == 1 && right == 0);
            }
        }

        // 1. Green along rows (H) and columns (V), computed for every sample and replaced by the
        //    sample itself where it is green (branch-free so the loop vectorises; `max`/`min`
        //    rather than `clamp`, whose bounds check would keep it scalar).
        for i in 2..er - 2 {
            let n = ec - 4;
            let base = i * ec + 2;
            let gm = &wk.mask[(i + py) & 1][0][2..2 + n];
            let m = &wk.m;
            let (c, l, r) = (&m[base..base + n], &m[base - 1..base - 1 + n], &m[base + 1..base + 1 + n]);
            let (l2, r2) = (&m[base - 2..base - 2 + n], &m[base + 2..base + 2 + n]);
            let (u, d) = (&m[base - ec..base - ec + n], &m[base + ec..base + ec + n]);
            let (u2, d2) = (&m[base - 2 * ec..base - 2 * ec + n], &m[base + 2 * ec..base + 2 * ec + n]);
            let [gh, gv] = &mut wk.g;
            let (gh, gv) = (&mut gh[base..base + n], &mut gv[base..base + n]);
            for x in 0..n {
                let h =
                    ((l[x] + r[x]) * 0.5 + (2.0 * c[x] - l2[x] - r2[x]) * 0.25).max(l[x].min(r[x])).min(l[x].max(r[x]));
                let v =
                    ((u[x] + d[x]) * 0.5 + (2.0 * c[x] - u2[x] - d2[x]) * 0.25).max(u[x].min(d[x])).min(u[x].max(d[x]));
                gh[x] = if gm[x] != 0.0 { c[x] } else { h };
                gv[x] = if gm[x] != 0.0 { c[x] } else { v };
            }
        }

        // 2. Red, green and blue planes by colour differences, then the perceptual coordinates.
        //    Both the formula for a red or blue sample and the one for a green sample are
        //    evaluated everywhere and the masks pick the right one.
        for d in 0..2 {
            for k in 0..er * ec {
                wk.diff[k] = wk.m[k] - wk.g[d][k];
            }
            for i in 3..er - 3 {
                let n = ec - 6;
                let base = i * ec + 3;
                let rk = (i + py) & 1;
                let [gm, rm, bm, hr] = &wk.mask[rk];
                let (gm, rm, bm, hr) = (&gm[3..3 + n], &rm[3..3 + n], &bm[3..3 + n], &hr[3..3 + n]);
                let df = &wk.diff;
                let (c, g) = (&wk.m[base..base + n], &wk.g[d][base..base + n]);
                let (ul, ur) = (&df[base - ec - 1..base - ec - 1 + n], &df[base - ec + 1..base - ec + 1 + n]);
                let (dl, dr) = (&df[base + ec - 1..base + ec - 1 + n], &df[base + ec + 1..base + ec + 1 + n]);
                let (l, r) = (&df[base - 1..base - 1 + n], &df[base + 1..base + 1 + n]);
                let (u, dn) = (&df[base - ec..base - ec + n], &df[base + ec..base + ec + n]);
                let [pr, pg, pb] = &mut wk.rgb[d];
                let (pr, pg, pb) = (&mut pr[base..base + n], &mut pg[base..base + n], &mut pb[base..base + n]);
                let [pl, pa, pbb] = &mut wk.lab[d];
                let (pl, pa, pbb) = (&mut pl[base..base + n], &mut pa[base..base + n], &mut pbb[base..base + n]);
                for x in 0..n {
                    let diag = g[x] + 0.25 * (ul[x] + ur[x] + dl[x] + dr[x]);
                    let row = g[x] + 0.5 * (l[x] + r[x]);
                    let col = g[x] + 0.5 * (u[x] + dn[x]);
                    let on_green_r = if hr[x] != 0.0 { row } else { col };
                    let on_green_b = if hr[x] != 0.0 { col } else { row };
                    let red = if rm[x] != 0.0 {
                        c[x]
                    } else if gm[x] != 0.0 {
                        on_green_r
                    } else {
                        diag
                    };
                    let blue = if bm[x] != 0.0 {
                        c[x]
                    } else if gm[x] != 0.0 {
                        on_green_b
                    } else {
                        diag
                    };
                    pr[x] = red;
                    pg[x] = g[x];
                    pb[x] = blue;
                    let (sr, sg, sb) = (red.max(0.0).sqrt(), g[x].max(0.0).sqrt(), blue.max(0.0).sqrt());
                    pl[x] = (sr + 2.0 * sg + sb) * 0.25;
                    pa[x] = sr - sg;
                    pbb[x] = sb - sg;
                }
            }
        }

        // 3. and 4. Thresholds and homogeneity. The lightness step and the chroma distance of two
        //    neighbouring pixels are computed once per pair, row by row (pairs to the right, down,
        //    down-right and down-left), and read from both ends: the same values as computing
        //    them per pixel, as `|a - b| = |b - a|` and `(a - b)^2 = (b - a)^2` exactly.
        //    Thresholds of a pixel: the smaller of the largest steps along the row in H and along
        //    the column in V (chroma as a squared distance). Homogeneity of a candidate: the
        //    number of the 8 neighbours within both thresholds.
        let rd = &mut wk.rd;
        Self::pair_steps(&wk.lab, ec, 4, &mut rd[0]);
        for i in 5..er - 5 {
            let (prev, cur) = rd.split_at_mut(1);
            let (prev, cur) = (&prev[0], &mut cur[0]);
            Self::pair_steps(&wk.lab, ec, i, cur);
            let (x0, n) = (5, ec - 10);
            let [e0, e1] = &mut wk.eps;
            let (el, ecr) = (&mut e0[..n], &mut e1[..n]);
            {
                let (h, v0, v1) = (&cur[0], &prev[1], &cur[1]);
                let (hl0, hl1, hc0, hc1) =
                    (&h.l[x0 - 1..x0 - 1 + n], &h.l[x0..x0 + n], &h.c[x0 - 1..x0 - 1 + n], &h.c[x0..x0 + n]);
                let (vl0, vl1, vc0, vc1) =
                    (&v0.dl[x0..x0 + n], &v1.dl[x0..x0 + n], &v0.dc[x0..x0 + n], &v1.dc[x0..x0 + n]);
                for x in 0..n {
                    el[x] = hl0[x].max(hl1[x]).min(vl0[x].max(vl1[x]));
                    ecr[x] = hc0[x].max(hc1[x]).min(vc0[x].max(vc1[x]));
                }
            }
            for d in 0..2 {
                let (p, q) = (&prev[d], &cur[d]);
                let cnt = &mut wk.hom[d][i * ec + x0..i * ec + x0 + n];
                cnt.fill(0.0);
                // The eight neighbours: (lightness, chroma) steps of the pair, from the row above
                // (up-left, up, up-right) and this row (left, right, down-left, down, down-right).
                let pairs: [(&[f32], &[f32]); 8] = [
                    (&p.xl[x0 - 1..x0 - 1 + n], &p.xc[x0 - 1..x0 - 1 + n]),
                    (&p.dl[x0..x0 + n], &p.dc[x0..x0 + n]),
                    (&p.yl[x0 + 1..x0 + 1 + n], &p.yc[x0 + 1..x0 + 1 + n]),
                    (&q.l[x0 - 1..x0 - 1 + n], &q.c[x0 - 1..x0 - 1 + n]),
                    (&q.l[x0..x0 + n], &q.c[x0..x0 + n]),
                    (&q.yl[x0..x0 + n], &q.yc[x0..x0 + n]),
                    (&q.dl[x0..x0 + n], &q.dc[x0..x0 + n]),
                    (&q.xl[x0..x0 + n], &q.xc[x0..x0 + n]),
                ];
                for (sl, sc) in pairs {
                    let (sl, sc) = (&sl[..n], &sc[..n]);
                    for x in 0..n {
                        cnt[x] += f32::from(u8::from(sl[x] <= el[x]) & u8::from(sc[x] <= ecr[x]));
                    }
                }
            }
            rd.swap(0, 1);
        }

        // 5. The candidates blended by their summed homogeneity.
        let n = ec - 2 * HALO;
        // Vertical sums of the three rows, then horizontal sums of three samples.
        for d in 0..2 {
            for i in HALO..er - HALO {
                let base = i * ec + HALO - 1;
                let len = n + 2;
                let (u, c, dn) = (
                    &wk.hom[d][base - ec..base - ec + len],
                    &wk.hom[d][base..base + len],
                    &wk.hom[d][base + ec..base + ec + len],
                );
                let v = &mut wk.vsum[d][..len];
                for x in 0..len {
                    v[x] = u[x] + c[x] + dn[x];
                }
                let h = &mut wk.hsum[d][(i - HALO) * n..(i - HALO + 1) * n];
                for x in 0..n {
                    h[x] = v[x] + v[x + 1] + v[x + 2];
                }
            }
        }
        for i in HALO..er - HALO {
            let r = (i - HALO) * n..(i - HALO + 1) * n;
            let (sh, sv) = (&wk.hsum[0][r.clone()], &wk.hsum[1][r]);
            let base = i * ec + HALO;
            let [r0, g0, b0] = &wk.rgb[0];
            let [r1, g1, b1] = &wk.rgb[1];
            let (r0, g0, b0) = (&r0[base..base + n], &g0[base..base + n], &b0[base..base + n]);
            let (r1, g1, b1) = (&r1[base..base + n], &g1[base..base + n], &b1[base..base + n]);
            let o = (i - HALO) * n;
            for (x, px_out) in out[o..o + n].iter_mut().enumerate() {
                // Weighted by the homogeneity of each candidate rather than picking the larger: on
                // ground-truth tiles this is closer to the original at every noise level, and real
                // files show fewer false-colour dots (docs/design/ahd-demosaic.md).
                let (a, b) = (sh[x], sv[x]);
                let w = if a + b > 0.0 { a / (a + b) } else { 0.5 };
                *px_out = [w * r0[x] + (1.0 - w) * r1[x], w * g0[x] + (1.0 - w) * g1[x], w * b0[x] + (1.0 - w) * b1[x]];
            }
        }
    }
}

/// Steps between neighbouring pixels of one row of one candidate: to the right (`l`, `c`), down
/// (`dl`, `dc`), down-right (`xl`, `xc`) and down-left (`yl`, `yc`), indexed by the column of the
/// pixel the pair starts from. `l`: absolute lightness step; `c`: squared chroma distance.
#[derive(Clone)]
struct PairSteps {
    l: Vec<f32>,
    c: Vec<f32>,
    dl: Vec<f32>,
    dc: Vec<f32>,
    xl: Vec<f32>,
    xc: Vec<f32>,
    yl: Vec<f32>,
    yc: Vec<f32>,
}

impl PairSteps {
    fn new(ec: usize) -> PairSteps {
        let v = || vec![0f32; ec];
        PairSteps { l: v(), c: v(), dl: v(), dc: v(), xl: v(), xc: v(), yl: v(), yc: v() }
    }
}

/// Working planes of one chunk (`er` rows of up to `CHUNK + 2 * HALO` columns).
struct Work {
    m: Vec<f32>,
    g: [Vec<f32>; 2],
    rgb: [[Vec<f32>; 3]; 2],
    lab: [[Vec<f32>; 3]; 2],
    diff: Vec<f32>,
    vsum: [Vec<f32>; 2],
    hsum: [Vec<f32>; 2],
    mask: [[Vec<f32>; 4]; 2],
    eps: [Vec<f32>; 2],
    hom: [Vec<f32>; 2],
    /// Pair steps of the previous and current row, per candidate.
    rd: [[PairSteps; 2]; 2],
}

impl Work {
    fn new(er: usize, ec: usize) -> Work {
        let n = er * ec;
        Work {
            m: vec![0.0; n],
            g: [vec![0.0; n], vec![0.0; n]],
            rgb: std::array::from_fn(|_| std::array::from_fn(|_| vec![0.0; n])),
            diff: vec![0.0; n],
            vsum: [vec![0.0; ec], vec![0.0; ec]],
            hsum: [vec![0.0; n], vec![0.0; n]],
            mask: std::array::from_fn(|_| std::array::from_fn(|_| vec![0.0; ec])),
            lab: std::array::from_fn(|_| std::array::from_fn(|_| vec![0.0; n])),
            eps: [vec![0.0; ec], vec![0.0; ec]],
            hom: [vec![0.0; n], vec![0.0; n]],
            rd: std::array::from_fn(|_| std::array::from_fn(|_| PairSteps::new(ec))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashraw_core::CfaPattern;

    fn chan() -> [u8; 8] {
        let mut c = [255u8; 8];
        c[..3].copy_from_slice(&[0, 1, 2]);
        c
    }

    /// A smooth coloured scene with a hard diagonal-free edge, mosaicked as RGGB.
    fn scene(w: usize, h: usize) -> (Vec<f32>, Vec<[f32; 3]>) {
        let mut rgb = vec![[0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / w as f32;
                let edge = if x > w / 2 { 0.5 } else { 0.0 };
                rgb[y * w + x] = [0.2 + 0.3 * t + edge, 0.3 + 0.2 * (y as f32 / h as f32) + edge * 0.5, 0.6 - 0.3 * t];
            }
        }
        let m = (0..w * h)
            .map(|k| {
                let (x, y) = (k % w, k / w);
                rgb[k][[0usize, 1, 1, 2][(y & 1) * 2 + (x & 1)]]
            })
            .collect();
        (m, rgb)
    }

    fn run(plan: &Plan, w: usize, m: &[f32], rows: (usize, usize), cols: (usize, usize)) -> Vec<[f32; 3]> {
        let mut out = vec![[0f32; 3]; (rows.1 - rows.0) * (cols.1 - cols.0)];
        let mut fetch = |y: usize, r: &mut [f32]| r.copy_from_slice(&m[y * w..(y + 1) * w]);
        plan.develop(w, rows, cols, &[1.0; 4], &mut fetch, &mut out);
        out
    }

    #[test]
    fn rejects_other_patterns_and_accepts_shifted_bayer() {
        assert!(Plan::new(&CfaPattern::RGGB, &chan(), (0, 0)).is_some());
        assert!(Plan::new(&CfaPattern::RGGB, &chan(), (1, 1)).is_some());
        assert!(Plan::new(&CfaPattern::RGGB, &chan(), (1, 0)).is_some());
    }

    #[test]
    fn flat_and_smooth_fields_are_reproduced() {
        let (w, h) = (80, 70);
        let (m, rgb) = scene(w, h);
        let plan = Plan::new(&CfaPattern::RGGB, &chan(), (0, 0)).unwrap();
        let got = run(&plan, w, &m, (HALO, h - HALO), (HALO, w - HALO));
        // Away from the edge the smooth scene is recovered closely.
        let mut worst = 0f32;
        for y in HALO..h - HALO {
            for x in HALO..w / 2 - 4 {
                let g = got[(y - HALO) * (w - 2 * HALO) + x - HALO];
                for c in 0..3 {
                    worst = worst.max((g[c] - rgb[y * w + x][c]).abs());
                }
            }
        }
        assert!(worst < 0.02, "{worst}");
    }

    #[test]
    fn bands_chunks_and_crops_agree_with_the_whole_frame() {
        let (w, h) = (430, 61);
        let mut s = 7u32;
        let m: Vec<f32> = (0..w * h)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 8) as f32 / (1u32 << 24) as f32
            })
            .collect();
        for origin in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let plan = Plan::new(&CfaPattern::RGGB, &chan(), origin).unwrap();
            let all = run(&plan, w, &m, (HALO, h - HALO), (HALO, w - HALO));
            let nc = w - 2 * HALO;
            // Rows 9..20 and columns 13..300 cross chunk borders (192 columns).
            let part = run(&plan, w, &m, (9, 20), (13, 300));
            for (i, y) in (9..20).enumerate() {
                for (j, x) in (13..300).enumerate() {
                    assert_eq!(part[i * 287 + j], all[(y - HALO) * nc + x - HALO], "{origin:?} ({x},{y})");
                }
            }
        }
    }
}
