//! Alternative Bayer demosaics ([`crate::Demosaic::Ppg`], [`crate::Demosaic::Rcd`]). Both reconstruct green first and then
//! red and blue from colour differences; they differ in how green is estimated and how the
//! diagonal differences are weighted. Each pixel's result depends only on the samples within [`HALO`], so
//! bands, crops and thread counts give identical output. Pixels closer than [`HALO`] to the frame
//! border use bilinear.
//!
//! * **Ppg** (patterned pixel grouping): green at a red or blue sample is interpolated along the
//!   row or the column, whichever has the smaller gradient, with a second-difference correction
//!   limited to the range of the two greens. Red at a blue sample (and the reverse) is the green
//!   plus the mean colour difference along the diagonal of smaller gradient; red and blue at a
//!   green sample are the green plus the mean colour difference of the two neighbours in the row
//!   or in the column.
//! * **Rcd** (ratio corrected, written from the multiplicative colour model: where the image
//!   scales by a factor from one pixel to the next, the green at a pixel is the neighbouring
//!   green times the ratio of the local intensities, estimated with a low-pass of the mosaic):
//!   four directional estimates `g_n * 2 L_c / (L_c + L_2n)` combined with inverse-gradient
//!   weights, then blended between the row and column results by how much the image varies across
//!   them. Red and blue as in Ppg with soft diagonal weights.
//!
//! Residual interpolation (a guided-filter prediction of red and blue from green, corrected by the
//! interpolated residuals at the samples) was implemented and benchmarked on the same tiles; it
//! scored below the colour-difference method of [`Method::Rcd`] on every measure at every noise
//! level, so it was not kept.

use flashraw_core::CfaPattern;

/// Mosaic samples needed around a pixel (even, so the colour of a pixel follows from the parity of
/// its frame coordinates).
pub(crate) const HALO: usize = 4;
/// Output rows per band.
pub(crate) const BAND_ROWS: usize = 64;
/// Output columns developed together inside a band.
const CHUNK: usize = 256;

/// The algorithm of a plan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Method {
    Ppg,
    Rcd,
}

/// One frame's plan: the colour of each of the four positions of the 2x2 pattern, indexed by
/// `(y & 1) * 2 + (x & 1)` of the frame.
#[derive(Clone)]
pub(crate) struct Plan {
    col: [u8; 4],
    method: Method,
}

impl Plan {
    /// The plan for a frame whose sample `(0, 0)` is at sensor position `origin`, or `None` when
    /// the pattern is not a Bayer arrangement.
    pub(crate) fn new(method: Method, cfa: &CfaPattern, chan: &[u8; 8], origin: (usize, usize)) -> Option<Plan> {
        crate::ahd::Plan::new(cfa, chan, origin).map(|p| Plan { col: p.colours(), method })
    }

    /// Develop rows `y0..y1` and columns `xa..xb` (frame coordinates) into `out`
    /// (`(y1 - y0) * (xb - xa)` RGB pixels, white balanced by `wb`). Needs `HALO <= y0`,
    /// `y1 + HALO <= height`, `HALO <= xa`, `xb + HALO <= w`. `fetch(y, row)` fills `row` with
    /// the normalised mosaic row `y`.
    /// [`Plan::develop`] handing the band over as one chunk (`emit(0, xb - xa, pixels)`), the
    /// interface of the demosaics that work in column chunks.
    pub(crate) fn develop_chunks(
        &self,
        w: usize,
        rows: (usize, usize),
        cols: (usize, usize),
        wb: &[f32; 4],
        fetch: &mut dyn FnMut(usize, &mut [f32]),
        out: &mut crate::process::PixelSink<'_>,
    ) {
        let mut buf = vec![[0f32; 3]; (rows.1 - rows.0) * (cols.1 - cols.0)];
        self.develop(w, rows, cols, wb, fetch, &mut buf);
        out(0, cols.1 - cols.0, &buf);
    }

    pub(crate) fn develop(
        &self,
        w: usize,
        rows: (usize, usize),
        cols: (usize, usize),
        wb: &[f32; 4],
        fetch: &mut dyn FnMut(usize, &mut [f32]),
        out: &mut [[f32; 3]],
    ) {
        #[cfg(all(feature = "simd", target_arch = "x86_64"))]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: the CPU supports AVX2 (checked above). Compiled without FMA, so the
            // results equal the baseline build's bit for bit.
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
        out: &mut [[f32; 3]],
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
        out: &mut [[f32; 3]],
    ) {
        let (nr, nc) = (y1 - y0, xb - xa);
        debug_assert!(y0 >= HALO && xa >= HALO && xb + HALO <= w && out.len() == nr * nc);
        let (er, bw) = (nr + 2 * HALO, nc + 2 * HALO);
        let mut m = vec![0f32; er * bw];
        let mut row = vec![0f32; w];
        for i in 0..er {
            let y = y0 + i - HALO;
            fetch(y, &mut row);
            for j in 0..bw {
                let x = xa + j - HALO;
                m[i * bw + j] = (row[x] * wb[self.col[(y & 1) * 2 + (x & 1)] as usize]).min(1.0);
            }
        }
        // Column chunks keep the working planes in cache (a full-width band of several planes is
        // megabytes per thread). Results do not depend on the chunking: each pixel reads only its halo.
        let mut mc = vec![0f32; er * (CHUNK + 2 * HALO)];
        let mut c0 = 0;
        while c0 < nc {
            let cw = CHUNK.min(nc - c0);
            let ec = cw + 2 * HALO;
            let mc = &mut mc[..er * ec];
            for i in 0..er {
                mc[i * ec..(i + 1) * ec].copy_from_slice(&m[i * bw + c0..i * bw + c0 + ec]);
            }
            let band = Band { m: mc, er, bw: ec, col: &self.col, py: y0 & 1, px: (xa + c0) & 1 };
            let g = band.green(self.method);
            band.diagonal(self.method, &g, out, nr, cw, (nc, c0));
            c0 += cw;
        }
    }
}

/// A band of the balanced mosaic: `er` rows of `bw` samples, starting `HALO` before the first
/// output row and column.
struct Band<'a> {
    m: &'a [f32],
    er: usize,
    bw: usize,
    col: &'a [u8; 4],
    /// Parity of the frame row / column of band row / column `HALO` (HALO is even).
    py: usize,
    px: usize,
}

/// The `bw` samples of band row `i` of `p`.
#[inline(always)]
fn row(p: &[f32], bw: usize, i: usize) -> &[f32] {
    &p[i * bw..(i + 1) * bw]
}

impl Band<'_> {
    /// Colour of the sample at band row `i`, column `j`.
    #[inline(always)]
    fn colour(&self, i: usize, j: usize) -> u8 {
        self.col[((i + self.py) & 1) * 2 + ((j + self.px) & 1)]
    }

    /// Put the measured greens of row `i`, columns `from..to`, back into `g` (the row loops
    /// estimate every pixel; greens alternate with the other colours).
    #[inline(always)]
    fn keep_greens(&self, g: &mut [f32], i: usize, from: usize, to: usize) {
        let bw = self.bw;
        let mut j = if self.colour(i, from) == 1 { from } else { from + 1 };
        while j < to {
            g[i * bw + j] = self.m[i * bw + j];
            j += 2;
        }
    }

    /// The green plane: measured at green samples, estimated elsewhere, valid for rows and
    /// columns `3..` from the edges (the support of the estimates is 3 samples).
    fn green(&self, method: Method) -> Vec<f32> {
        let mut g = self.m.to_vec();
        match method {
            Method::Ppg => self.green_ppg(&mut g),
            Method::Rcd => self.green_ratio(&mut g),
        }
        g
    }

    /// Green by the directional estimate of smaller gradient (module docs). Every pixel of a row
    /// is estimated in one branch-free loop, then the measured greens are put back.
    fn green_ppg(&self, g: &mut [f32]) {
        let (er, bw, m) = (self.er, self.bw, self.m);
        let n = bw - 4;
        for i in 2..er - 2 {
            let c = row(m, bw, i);
            let (n1, s1) = (&row(m, bw, i - 1)[2..2 + n], &row(m, bw, i + 1)[2..2 + n]);
            let (n2, s2) = (&row(m, bw, i - 2)[2..2 + n], &row(m, bw, i + 2)[2..2 + n]);
            let o = &mut g[i * bw + 2..i * bw + 2 + n];
            for t in 0..n {
                let (cc, gw, ge, w2, e2) = (c[t + 2], c[t + 1], c[t + 3], c[t], c[t + 4]);
                let (gn, gs) = (n1[t], s1[t]);
                let lv = 2.0 * cc - n2[t] - s2[t];
                let lh = 2.0 * cc - w2 - e2;
                let dv = (gn - gs).abs() + lv.abs();
                let dh = (gw - ge).abs() + lh.abs();
                let ev = ((gn + gs) * 0.5 + lv * 0.25).max(gn.min(gs)).min(gn.max(gs));
                let eh = ((gw + ge) * 0.5 + lh * 0.25).max(gw.min(ge)).min(gw.max(ge));
                let both = (ev + eh) * 0.5;
                o[t] = if dv < dh {
                    ev
                } else if dh < dv {
                    eh
                } else {
                    both
                };
            }
            self.keep_greens(g, i, 2, bw - 2);
        }
    }

    /// Green by ratio-corrected directional estimates (module docs), in branch-free row loops.
    fn green_ratio(&self, g: &mut [f32]) {
        const EPS: f32 = 1e-4;
        let (er, bw, m) = (self.er, self.bw, self.m);
        // Low pass of the mosaic, `[1 2 1; 2 4 2; 1 2 1] / 16`, for rows and columns 1.. from the edges.
        let mut l = vec![0f32; er * bw];
        let n = bw - 2;
        for i in 1..er - 1 {
            let (a, b, c) = (row(m, bw, i - 1), row(m, bw, i), row(m, bw, i + 1));
            let o = &mut l[i * bw + 1..i * bw + 1 + n];
            for t in 0..n {
                o[t] = (4.0 * b[t + 1]
                    + 2.0 * (b[t] + b[t + 2] + a[t + 1] + c[t + 1])
                    + (a[t] + a[t + 2] + c[t] + c[t + 2]))
                    * (1.0 / 16.0);
            }
        }
        let n = bw - 6;
        for i in 3..er - 3 {
            let lc = row(&l, bw, i);
            let (ln, ls) = (&row(&l, bw, i - 1)[3..3 + n], &row(&l, bw, i + 1)[3..3 + n]);
            let (ln2, ls2) = (&row(&l, bw, i - 2)[3..3 + n], &row(&l, bw, i + 2)[3..3 + n]);
            let mc = row(m, bw, i);
            let (mn, ms) = (&row(m, bw, i - 1)[3..3 + n], &row(m, bw, i + 1)[3..3 + n]);
            let o = &mut g[i * bw + 3..i * bw + 3 + n];
            for t in 0..n {
                let (c, lw, le, lw2, le2) = (lc[t + 3], lc[t + 2], lc[t + 4], lc[t + 1], lc[t + 5]);
                let (gn, gs, gw, ge) = (mn[t], ms[t], mc[t + 2], mc[t + 4]);
                // The ratio-corrected estimate from each neighbouring green: the intensity at the
                // neighbour is taken as the mean of the low pass at the centre and two away.
                let en = gn * 2.0 * c / (EPS + c + ln2[t]);
                let es = gs * 2.0 * c / (EPS + c + ls2[t]);
                let ew = gw * 2.0 * c / (EPS + c + lw2);
                let ee = ge * 2.0 * c / (EPS + c + le2);
                // Weights from the low pass: the closer a green's intensity is to the centre's,
                // the more it counts.
                let wgt = |a: f32| {
                    let d = EPS + (a - c).abs();
                    1.0 / (d * d)
                };
                let (wn, ws, ww, we) = (wgt(ln[t]), wgt(ls[t]), wgt(lw), wgt(le));
                let gv = (wn * en + ws * es) / (wn + ws);
                let gh = (ww * ew + we * ee) / (ww + we);
                // Trust the direction across which the image varies less.
                let ev = (ln[t] - ls[t]).abs() + (ln2[t] - c).abs() + (ls2[t] - c).abs();
                let eh = (lw - le).abs() + (lw2 - c).abs() + (le2 - c).abs();
                let a = (eh * eh + EPS) / (ev * ev + eh * eh + 2.0 * EPS);
                let est = a * gv + (1.0 - a) * gh;
                let lo = gn.min(gs).min(gw).min(ge);
                let hi = gn.max(gs).max(gw).max(ge);
                o[t] = est.max(lo).min(hi);
            }
            self.keep_greens(g, i, 3, bw - 3);
        }
    }

    /// Red and blue from colour differences, written to `out` (rows of `stride` pixels, from column
    /// `off`) for the `nr` x `nc` core. The
    /// row and column means and the diagonal estimate are computed for every pixel of a row in
    /// branch-free loops; each pixel then takes the ones its colour needs.
    fn diagonal(
        &self,
        method: Method,
        g: &[f32],
        out: &mut [[f32; 3]],
        nr: usize,
        nc: usize,
        (stride, off): (usize, usize),
    ) {
        const EPS: f32 = 1e-4;
        let (bw, m) = (self.bw, self.m);
        let e: Vec<f32> = m.iter().zip(g).map(|(a, b)| a - b).collect();
        let (mut hm, mut vm, mut dg) = (vec![0f32; nc], vec![0f32; nc], vec![0f32; nc]);
        let j0 = HALO;
        for oi in 0..nr {
            let i = oi + HALO;
            let (en, ec, es) = (&row(&e, bw, i - 1)[j0 - 1..], row(&e, bw, i), &row(&e, bw, i + 1)[j0 - 1..]);
            let (mn, ms) = (&row(m, bw, i - 1)[j0 - 1..], &row(m, bw, i + 1)[j0 - 1..]);
            let (gn, gc, gs) =
                (&row(g, bw, i - 1)[j0 - 1..], &row(g, bw, i)[j0..j0 + nc], &row(g, bw, i + 1)[j0 - 1..]);
            let (en, es, mn, ms, gn, gs) =
                (&en[..nc + 2], &es[..nc + 2], &mn[..nc + 2], &ms[..nc + 2], &gn[..nc + 2], &gs[..nc + 2]);
            let ec = &ec[j0 - 1..j0 + nc + 1];
            for t in 0..nc {
                hm[t] = 0.5 * (ec[t] + ec[t + 2]);
                vm[t] = 0.5 * (en[t + 1] + es[t + 1]);
                // ne = (i - 1, j + 1), sw = (i + 1, j - 1), nw = (i - 1, j - 1), se = (i + 1, j + 1).
                let d1 = (mn[t + 2] - ms[t]).abs() + (2.0 * gc[t] - gn[t + 2] - gs[t]).abs();
                let d2 = (mn[t] - ms[t + 2]).abs() + (2.0 * gc[t] - gn[t] - gs[t + 2]).abs();
                let (s1, s2) = (en[t + 2] + es[t], en[t] + es[t + 2]);
                dg[t] = gc[t]
                    + match method {
                        Method::Rcd => {
                            let (w1, w2) = (1.0 / ((EPS + d1) * (EPS + d1)), 1.0 / ((EPS + d2) * (EPS + d2)));
                            (w1 * 0.5 * s1 + w2 * 0.5 * s2) / (w1 + w2)
                        }
                        Method::Ppg => {
                            let both = 0.25 * (en[t + 2] + es[t] + en[t] + es[t + 2]);
                            if d1 < d2 {
                                0.5 * s1
                            } else if d2 < d1 {
                                0.5 * s2
                            } else {
                                both
                            }
                        }
                    };
            }
            let mc = &row(m, bw, i)[j0..j0 + nc];
            let o = &mut out[oi * stride + off..oi * stride + off + nc];
            // Green samples: the colour of their row neighbours from the row, the other from the column.
            let tg = if self.colour(i, j0) == 1 { 0 } else { 1 };
            let row_is_red = self.colour(i, j0 + tg + 1) == 0;
            let mut t = tg;
            while t < nc {
                let gk = gc[t];
                o[t] = if row_is_red { [gk + hm[t], gk, gk + vm[t]] } else { [gk + vm[t], gk, gk + hm[t]] };
                t += 2;
            }
            // Red and blue samples: the measured value, the green, and the diagonal estimate.
            let red = self.colour(i, j0 + 1 - tg) == 0;
            let mut t = 1 - tg;
            while t < nc {
                o[t] = if red { [mc[t], gc[t], dg[t]] } else { [dg[t], gc[t], mc[t]] };
                t += 2;
            }
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

    fn scene(w: usize, h: usize) -> (Vec<f32>, Vec<[f32; 3]>) {
        let mut rgb = vec![[0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / w as f32;
                let edge = if x > w / 2 { 0.4 } else { 0.0 };
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

    const METHODS: [Method; 2] = [Method::Ppg, Method::Rcd];

    #[test]
    fn smooth_fields_are_reproduced_and_measured_samples_kept() {
        let (w, h) = (90, 80);
        let (m, rgb) = scene(w, h);
        for method in METHODS {
            let plan = Plan::new(method, &CfaPattern::RGGB, &chan(), (0, 0)).unwrap();
            let got = run(&plan, w, &m, (HALO, h - HALO), (HALO, w - HALO));
            let nc = w - 2 * HALO;
            let mut worst = 0f32;
            for y in HALO..h - HALO {
                for x in HALO..w / 2 - 6 {
                    let p = got[(y - HALO) * nc + x - HALO];
                    for c in 0..3 {
                        worst = worst.max((p[c] - rgb[y * w + x][c]).abs());
                    }
                }
            }
            assert!(worst < 0.03, "{method:?} {worst}");
            for y in HALO..h - HALO {
                for x in HALO..w - HALO {
                    let c = [0usize, 1, 1, 2][(y & 1) * 2 + (x & 1)];
                    assert_eq!(got[(y - HALO) * nc + x - HALO][c], m[y * w + x], "{method:?} ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn bands_and_crops_agree_with_the_whole_frame() {
        let (w, h) = (130, 70);
        let mut s = 11u32;
        let m: Vec<f32> = (0..w * h)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 8) as f32 / (1u32 << 24) as f32
            })
            .collect();
        for method in METHODS {
            for origin in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let plan = Plan::new(method, &CfaPattern::RGGB, &chan(), origin).unwrap();
                let all = run(&plan, w, &m, (HALO, h - HALO), (HALO, w - HALO));
                let nc = w - 2 * HALO;
                let part = run(&plan, w, &m, (11, 29), (13, 77));
                for (i, y) in (11..29).enumerate() {
                    for (j, x) in (13..77).enumerate() {
                        assert_eq!(
                            part[i * 64 + j],
                            all[(y - HALO) * nc + x - HALO],
                            "{method:?} {origin:?} ({x},{y})"
                        );
                    }
                }
            }
        }
    }
}
