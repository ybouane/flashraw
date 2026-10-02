//! Canon's integer 5/3 inverse wavelet (lifting), separable: each level first
//! inverts horizontally (rows of the low/high subband pairs), then vertically.
//!
//! Tiles are transformed independently but their subbands overlap neighbouring
//! tiles by a few coefficients. On a side with a neighbour, the high-pass band
//! carries one extra *leading* coefficient (left/top) or extra *trailing*
//! coefficients (right/bottom), and the lifting uses them instead of mirroring.
//! The same 1-D rules apply to columns (top/bottom) as to rows (left/right).

/// Neighbour flags of a tile (which sides have overlap coefficients).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Sides {
    /// High-pass band has a leading extra coefficient (neighbour on the left/top).
    pub before: bool,
    /// Bands have trailing extra coefficients (neighbour on the right/bottom).
    pub after: bool,
}

/// Number of `(low, high)` coefficients the inverse transform of `n` outputs reads.
pub(crate) fn coeffs_needed(n: usize, s: Sides) -> (usize, usize) {
    if n <= 1 {
        return (1, 0);
    }
    let iters = (n - 2) / 2; // pairs produced by the inner loop
    let base = s.before as usize;
    let lo = 1 + iters + usize::from(s.after || n & 1 == 1);
    let hi = base + iters + if s.after { 2 } else { 1 };
    (lo, hi)
}

/// Inverse 5/3 lifting of one line: `lo`/`hi` coefficients → `out` (`out.len()` samples).
/// Callers guarantee `lo`/`hi` hold at least [`coeffs_needed`] values.
pub(crate) fn inverse_line(lo: &[i32], hi: &[i32], out: &mut [i32], s: Sides) {
    let n = out.len();
    if n == 0 {
        return;
    }
    if n <= 1 {
        out[0] = lo[0];
        return;
    }
    let hi = if s.before {
        out[0] = lo[0].wrapping_sub(hi[0].wrapping_add(hi[1]).wrapping_add(2) >> 2);
        &hi[1..]
    } else {
        out[0] = lo[0].wrapping_sub(hi[0].wrapping_add(1) >> 1);
        hi
    };
    let lo = &lo[1..];
    let iters = (n - 2) / 2;
    // Even samples: lo - (h[j] + h[j+1] + 2) / 4; odd: h + (even_l + even_r) / 2.
    for j in 0..iters {
        let even = lo[j].wrapping_sub(hi[j].wrapping_add(hi[j + 1]).wrapping_add(2) >> 2);
        out[2 * j + 1] = hi[j].wrapping_add(even.wrapping_add(out[2 * j]) >> 1);
        out[2 * j + 2] = even;
    }
    let j = iters;
    let last = 2 * j;
    if s.after {
        let even = lo[j].wrapping_sub(hi[j].wrapping_add(hi[j + 1]).wrapping_add(2) >> 2);
        out[last + 1] = hi[j].wrapping_add(even.wrapping_add(out[last]) >> 1);
        if n & 1 == 1 {
            out[last + 2] = even;
        }
    } else if n & 1 == 1 {
        let even = lo[j].wrapping_sub(hi[j].wrapping_add(1) >> 1);
        out[last + 1] = hi[j].wrapping_add(out[last].wrapping_add(even) >> 1);
        out[last + 2] = even;
    } else {
        out[last + 1] = out[last].wrapping_add(hi[j]);
    }
}

/// First even row: `low - (h + 1) / 2` (mirrored) or `low - (h_prev + h + 2) / 4`.
pub(crate) fn first_even(low: &mut [i32], h_prev: Option<&[i32]>, h: &[i32]) {
    match h_prev {
        Some(hp) => {
            for ((l, &a), &b) in low.iter_mut().zip(hp).zip(h) {
                *l = l.wrapping_sub(a.wrapping_add(b).wrapping_add(2) >> 2);
            }
        }
        None => {
            for (l, &b) in low.iter_mut().zip(h) {
                *l = l.wrapping_sub(b.wrapping_add(1) >> 1);
            }
        }
    }
}

/// General vertical step: `low` becomes the next even row from the high rows around
/// it, and `h` (the high row between the previous even row and the new one) becomes
/// the odd row.
pub(crate) fn step(prev_even: &[i32], h: &mut [i32], low: &mut [i32], h_next: &[i32]) {
    for (((e, o), &pe), &hn) in low.iter_mut().zip(h.iter_mut()).zip(prev_even).zip(h_next) {
        let even = e.wrapping_sub(o.wrapping_add(hn).wrapping_add(2) >> 2);
        *o = o.wrapping_add(even.wrapping_add(pe) >> 1);
        *e = even;
    }
}

/// Final step of an odd-height transform without a neighbour below: the last even
/// row mirrors its high neighbour.
pub(crate) fn step_last_odd(prev_even: &[i32], h: &mut [i32], low: &mut [i32]) {
    for ((e, o), &pe) in low.iter_mut().zip(h.iter_mut()).zip(prev_even) {
        let even = e.wrapping_sub(o.wrapping_add(1) >> 1);
        *o = o.wrapping_add(pe.wrapping_add(even) >> 1);
        *e = even;
    }
}

/// Final odd row of an even-height transform without a neighbour below.
pub(crate) fn step_last_even(prev_even: &[i32], h: &mut [i32]) {
    for (o, &pe) in h.iter_mut().zip(prev_even) {
        *o = pe.wrapping_add(*o);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forward 5/3 lifting with mirrored boundaries (no neighbours), the encoder-side
    /// counterpart of [`inverse_line`].
    fn forward(x: &[i32]) -> (Vec<i32>, Vec<i32>) {
        let n = x.len();
        let at = |i: isize| -> i32 {
            let i = if i < 0 {
                -i
            } else if i as usize >= n {
                2 * (n as isize - 1) - i
            } else {
                i
            };
            x[i as usize]
        };
        let nh = n / 2;
        let nl = n.div_ceil(2);
        let hi: Vec<i32> =
            (0..nh).map(|j| at(2 * j as isize + 1) - ((at(2 * j as isize) + at(2 * j as isize + 2)) >> 1)).collect();
        let hat = |j: isize| -> i32 {
            let j = if j < 0 {
                -j - 1
            } else if j as usize >= nh {
                2 * nh as isize - 1 - j
            } else {
                j
            };
            hi[j.clamp(0, nh as isize - 1) as usize]
        };
        let lo: Vec<i32> =
            (0..nl).map(|j| at(2 * j as isize) + ((hat(j as isize - 1) + hat(j as isize) + 2) >> 2)).collect();
        (lo, hi)
    }

    #[test]
    fn needs() {
        let none = Sides::default();
        assert_eq!(coeffs_needed(8, none), (4, 4));
        assert_eq!(coeffs_needed(9, none), (5, 4));
        assert_eq!(coeffs_needed(8, Sides { before: true, after: false }), (4, 5));
        assert_eq!(coeffs_needed(8, Sides { before: false, after: true }), (5, 5));
        assert_eq!(coeffs_needed(1, none), (1, 0));
    }

    #[test]
    fn round_trip_mirrored() {
        // Interior samples reconstruct exactly; the decoder's boundary handling at the
        // first sample uses (h0 + 1) / 2, i.e. a mirrored h[-1] = h[0].
        let mut seed = 12345u32;
        for n in 2..40usize {
            let x: Vec<i32> = (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                    ((seed >> 16) & 0x3fff) as i32 - 8192
                })
                .collect();
            let (lo, hi) = forward(&x);
            let mut out = vec![0; n];
            inverse_line(&lo, &hi, &mut out, Sides::default());
            assert_eq!(out, x, "n = {n}");
        }
    }

    #[test]
    fn vertical_matches_horizontal() {
        // The row-wise vertical helpers implement the same 1-D transform as `inverse_line`.
        for n in 2..20usize {
            for &s in &[
                Sides::default(),
                Sides { before: true, after: false },
                Sides { before: false, after: true },
                Sides { before: true, after: true },
            ] {
                let (nl, nh) = coeffs_needed(n, s);
                let lo: Vec<i32> = (0..nl as i32).map(|v| v * 37 % 101 - 50).collect();
                let hi: Vec<i32> = (0..nh as i32).map(|v| v * 53 % 89 - 44).collect();
                let mut want = vec![0; n];
                inverse_line(&lo, &hi, &mut want, s);
                // Column version on 1-wide rows.
                let mut got = Vec::new();
                let mut hi_it = hi.iter().copied();
                let mut lo_it = lo.iter().copied();
                let mut e = [lo_it.next().unwrap()];
                let mut h;
                if s.before {
                    let a = [hi_it.next().unwrap()];
                    h = [hi_it.next().unwrap()];
                    first_even(&mut e, Some(&a), &h);
                } else {
                    h = [hi_it.next().unwrap()];
                    first_even(&mut e, None, &h);
                }
                got.push(e[0]);
                while got.len() < n {
                    let left = n - got.len();
                    if !s.after && left == 1 {
                        step_last_even(&e, &mut h);
                        got.push(h[0]);
                    } else if !s.after && left == 2 {
                        let mut l = [lo_it.next().unwrap()];
                        step_last_odd(&e, &mut h, &mut l);
                        got.push(h[0]);
                        got.push(l[0]);
                    } else {
                        let mut l = [lo_it.next().unwrap()];
                        let hn = [hi_it.next().unwrap()];
                        step(&e, &mut h, &mut l, &hn);
                        got.push(h[0]);
                        if got.len() < n {
                            got.push(l[0]);
                        }
                        e = l;
                        h = hn;
                    }
                }
                assert_eq!(got, want, "n = {n} {s:?}");
                assert!(lo_it.next().is_none() && hi_it.next().is_none(), "all coefficients consumed n={n} {s:?}");
            }
        }
    }
}
