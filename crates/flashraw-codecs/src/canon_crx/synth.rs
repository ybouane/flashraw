//! Inverse 5/3 wavelet of one plane tile, pulled one output row at a time.
//!
//! A [`Pyramid`] holds the coarsest band reader and one [`Level`] per wavelet level. Asking the finest level for a
//! row makes it ask the next coarser level for the low-pass rows it needs, and so on down to the coarsest band, so
//! only a few lines per level are alive at any time.

use super::lines::BandReader;
use flashraw_core::{Error, Result};

#[inline(always)]
fn lift_even(l: i32, a: i32, b: i32) -> i32 {
    l.wrapping_sub(a.wrapping_add(b).wrapping_add(2) >> 2)
}

#[inline(always)]
fn lift_odd(h: i32, e0: i32, e1: i32) -> i32 {
    h.wrapping_add(e0.wrapping_add(e1) >> 1)
}

/// Number of low-pass and high-pass coefficients a 1-D inverse of length `n` reads.
pub(super) fn coefficients_needed(n: usize, before: bool, after: bool) -> (usize, usize) {
    if n <= 1 {
        return (1, 0);
    }
    let iters = (n - 2) / 2;
    (1 + iters + usize::from(after || n & 1 == 1), usize::from(before) + iters + if after { 2 } else { 1 })
}

/// One-dimensional inverse transform: `out.len()` samples from the low-pass `lo` and high-pass `hi` coefficients.
/// `before` / `after` tell that a neighbouring tile supplies real coefficients beyond that end (otherwise the
/// signal is mirrored there).
pub(super) fn inverse_1d(lo: &[i32], hi: &[i32], out: &mut [i32], before: bool, after: bool) {
    let n = out.len();
    if n == 0 {
        return;
    }
    if n == 1 {
        out[0] = lo[0];
        return;
    }
    let (first, h) = if before {
        (lo[0].wrapping_sub(hi[0].wrapping_add(hi[1]).wrapping_add(2) >> 2), &hi[1..])
    } else {
        (lo[0].wrapping_sub(hi[0].wrapping_add(1) >> 1), hi)
    };
    let l = &lo[1..];
    let iters = (n - 2) / 2;
    out[0] = first;
    let mut even = first;
    for (((pair, &lj), &h0), &h1) in out[1..1 + 2 * iters].chunks_exact_mut(2).zip(l).zip(h).zip(&h[1..]) {
        let e = lift_even(lj, h0, h1);
        pair[0] = lift_odd(h0, e, even);
        pair[1] = e;
        even = e;
    }
    let last = 2 * iters;
    let h0 = h[iters];
    if after {
        let e = lift_even(l[iters], h0, h[iters + 1]);
        out[last + 1] = lift_odd(h0, e, even);
        if n & 1 == 1 {
            out[last + 2] = e;
        }
    } else if n & 1 == 1 {
        let e = l[iters].wrapping_sub(h0.wrapping_add(1) >> 1);
        out[last + 1] = lift_odd(h0, even, e);
        out[last + 2] = e;
    } else {
        out[last + 1] = even.wrapping_add(h0);
    }
}

/// Which neighbours a tile has, as the wavelet sees them.
#[derive(Clone, Copy)]
pub(super) struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

/// One wavelet level: combines the rows of the next coarser result (or the coarsest band) with its three detail
/// bands into rows `width` wide.
pub(super) struct Level<'a> {
    edges: Edges,
    height: usize,
    lo_n: usize,
    hi_n: usize,
    hl: BandReader<'a>,
    lh: BandReader<'a>,
    hh: BandReader<'a>,
    // line buffers
    src: Vec<i32>,
    hl_line: Vec<i32>,
    lh_line: Vec<i32>,
    hh_line: Vec<i32>,
    /// Last even output row, current high row, scratch low and high rows.
    even: Vec<i32>,
    high: Vec<i32>,
    low_new: Vec<i32>,
    high_new: Vec<i32>,
    emitted: usize,
    /// `even` holds a finished row that has not been handed out yet.
    owed: bool,
}

impl<'a> Level<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        edges: Edges,
        width: usize,
        height: usize,
        source_width: usize,
        hl: BandReader<'a>,
        lh: BandReader<'a>,
        hh: BandReader<'a>,
        widths: [usize; 3],
    ) -> Result<Level<'a>> {
        let (lo_n, hi_n) = coefficients_needed(width, edges.left, edges.right);
        let [hl_w, lh_w, hh_w] = widths;
        if width == 0 || height == 0 || source_width < lo_n || lh_w < lo_n || hl_w < hi_n || hh_w < hi_n {
            return Err(Error::corrupt("CRX: inconsistent subband sizes"));
        }
        Ok(Level {
            edges,
            height,
            lo_n,
            hi_n,
            hl,
            lh,
            hh,
            src: vec![0; source_width],
            hl_line: vec![0; hl_w],
            lh_line: vec![0; lh_w],
            hh_line: vec![0; hh_w],
            even: vec![0; width],
            high: vec![0; width],
            low_new: vec![0; width],
            high_new: vec![0; width],
            emitted: 0,
            owed: false,
        })
    }
}

/// Fetch and horizontally synthesise the next low row of `lv` into `lv.low_new`.
fn fetch_low(ll: &mut BandReader, below: &mut [Level], lv: &mut Level) -> Result<()> {
    if below.is_empty() {
        ll.next_scaled(&mut lv.src)?;
    } else {
        next_row(ll, below, &mut lv.src)?;
    }
    lv.hl.next_scaled(&mut lv.hl_line)?;
    inverse_1d(&lv.src[..lv.lo_n], &lv.hl_line[..lv.hi_n], &mut lv.low_new, lv.edges.left, lv.edges.right);
    Ok(())
}

/// Fetch and horizontally synthesise the next high row of `lv` into `lv.high_new`.
fn fetch_high(lv: &mut Level) -> Result<()> {
    lv.lh.next_scaled(&mut lv.lh_line)?;
    lv.hh.next_scaled(&mut lv.hh_line)?;
    inverse_1d(&lv.lh_line[..lv.lo_n], &lv.hh_line[..lv.hi_n], &mut lv.high_new, lv.edges.left, lv.edges.right);
    Ok(())
}

/// Produce the next row of the last level of `levels` into `dst`.
pub(super) fn next_row(ll: &mut BandReader, levels: &mut [Level], dst: &mut [i32]) -> Result<()> {
    let (below, rest) = levels.split_at_mut(levels.len() - 1);
    let lv = &mut rest[0];
    if lv.emitted >= lv.height {
        return Err(Error::corrupt("CRX: wavelet level exhausted"));
    }
    let remaining = lv.height - lv.emitted;
    let first = lv.emitted == 0;
    lv.emitted += 1;
    let Edges { top, bottom, .. } = lv.edges;

    if first {
        fetch_low(ll, below, lv)?;
        std::mem::swap(&mut lv.even, &mut lv.low_new);
        if lv.height > 1 {
            fetch_high(lv)?;
            std::mem::swap(&mut lv.high, &mut lv.high_new);
            if top {
                fetch_high(lv)?;
                for ((e, &a), &b) in lv.even.iter_mut().zip(&lv.high).zip(&lv.high_new) {
                    *e = lift_even(*e, a, b);
                }
                std::mem::swap(&mut lv.high, &mut lv.high_new);
            } else {
                for (e, &a) in lv.even.iter_mut().zip(&lv.high) {
                    *e = e.wrapping_sub(a.wrapping_add(1) >> 1);
                }
            }
        }
        dst.copy_from_slice(&lv.even);
        return Ok(());
    }
    if lv.owed {
        lv.owed = false;
        dst.copy_from_slice(&lv.even);
        return Ok(());
    }
    if !bottom && remaining == 1 {
        for ((d, &h), &e) in dst.iter_mut().zip(&lv.high).zip(&lv.even) {
            *d = h.wrapping_add(e);
        }
        return Ok(());
    }
    if !bottom && remaining == 2 {
        fetch_low(ll, below, lv)?;
        for (f, &h) in lv.low_new.iter_mut().zip(&lv.high) {
            *f = f.wrapping_sub(h.wrapping_add(1) >> 1);
        }
        for (((d, &h), &e), &f) in dst.iter_mut().zip(&lv.high).zip(&lv.even).zip(&lv.low_new) {
            *d = lift_odd(h, e, f);
        }
        std::mem::swap(&mut lv.even, &mut lv.low_new);
        lv.owed = true;
        return Ok(());
    }
    fetch_low(ll, below, lv)?;
    fetch_high(lv)?;
    for ((f, &h), &g) in lv.low_new.iter_mut().zip(&lv.high).zip(&lv.high_new) {
        *f = lift_even(*f, h, g);
    }
    for (((d, &h), &e), &f) in dst.iter_mut().zip(&lv.high).zip(&lv.even).zip(&lv.low_new) {
        *d = lift_odd(h, f, e);
    }
    std::mem::swap(&mut lv.even, &mut lv.low_new);
    std::mem::swap(&mut lv.high, &mut lv.high_new);
    lv.owed = remaining >= 2;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forward reversible 5/3 transform of a whole signal (mirrored ends), for round trips.
    fn forward(x: &[i32]) -> (Vec<i32>, Vec<i32>) {
        let n = x.len();
        if n == 1 {
            return (x.to_vec(), vec![]);
        }
        let at = |i: isize| -> i32 {
            let m = (n as isize - 1) * 2;
            let mut i = i.rem_euclid(m.max(1));
            if i >= n as isize {
                i = m - i;
            }
            x[i as usize]
        };
        let nh = n / 2;
        let nl = n - nh;
        let hi: Vec<i32> =
            (0..nh).map(|j| at(2 * j as isize + 1) - ((at(2 * j as isize) + at(2 * j as isize + 2)) >> 1)).collect();
        let h = |j: isize| -> i32 {
            // mirrored high-pass sequence
            let j = if j < 0 { -j - 1 } else { j };
            let j = if j >= nh as isize { 2 * nh as isize - 1 - j } else { j };
            hi[j as usize]
        };
        let lo: Vec<i32> =
            (0..nl).map(|j| at(2 * j as isize) + ((h(j as isize - 1) + h(j as isize) + 2) >> 2)).collect();
        (lo, hi)
    }

    #[test]
    fn inverse_undoes_forward_for_all_lengths() {
        let mut seed = 5u32;
        for n in 1..40usize {
            let x: Vec<i32> = (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (seed >> 20) as i32 - 2048
                })
                .collect();
            let (lo, hi) = forward(&x);
            let mut out = vec![0; n];
            inverse_1d(&lo, &hi, &mut out, false, false);
            assert_eq!(out, x, "n = {n}");
            let (lo_n, hi_n) = coefficients_needed(n, false, false);
            assert!(
                lo_n <= lo.len().max(1) && hi_n <= hi.len(),
                "n = {n}: needs {lo_n},{hi_n} has {},{}",
                lo.len(),
                hi.len()
            );
        }
    }
}
