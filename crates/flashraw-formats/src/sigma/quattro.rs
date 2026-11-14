//! Post-processing of the Quattro layers: the repair of the phase-detect autofocus sites and
//! the replication of the half-resolution layers.
//!
//! A Quattro frame is a buffer of `W x H` pixels with three 16-bit layers each (bottom, middle,
//! top, stored as samples 0, 1, 2). The bottom and middle layers are captured at half the
//! resolution of the top layer and arrive on the even-row/even-column sites only.
//!
//! The per-size data (where the autofocus sites are) is kept apart from the arithmetic, in
//! the two tables below. Each row says whether it was verified against a real file (the
//! sites whose samples differ before and after the repair are exactly the table's sites) or
//! is carried over unverified, in which case no sample file exists for that size.

use flashraw_core::{Rect, par};

/// Raw buffer sizes whose autofocus sites form a regular lattice (the dp bodies).
///
/// The sites are the points `(x, y)` with `x` a multiple of `xs` and `y` a multiple of `ys`
/// (counted from the buffer origin) inside the active area and at least `reach` away from
/// every buffer edge.
pub(super) struct Lattice {
    pub raw: (usize, usize),
    pub xs: usize,
    pub ys: usize,
    /// Distance of the neighbours the repair reads.
    pub reach: usize,
}

/// Lattice sites per raw size.
pub(super) const LATTICES: [Lattice; 2] = [
    // Verified on the dp0 and dp2 Quattro samples: 170 x 453 = 77,010 sites.
    Lattice { raw: (5888, 3672), xs: 32, ys: 8, reach: 2 },
    // UNVERIFIED (half-resolution mode of the dp bodies, no sample file).
    Lattice { raw: (2944, 1836), xs: 16, ys: 4, reach: 1 },
];

/// Raw buffer sizes whose autofocus sites form a window of a lattice (the sd bodies): the
/// sites start at `(x0, y0)`, step by `(xs, ys)`, and stop at row `y1` and at the last column.
pub(super) struct Window {
    pub raw: (usize, usize),
    pub x0: usize,
    pub y0: usize,
    pub y1: usize,
    pub xs: usize,
    pub ys: usize,
    pub reach: usize,
}

/// Window sites per raw size.
pub(super) const WINDOWS: [Window; 6] = [
    // Verified on the sd Quattro sample.
    Window { raw: (5888, 3776), x0: 216, y0: 464, y1: 3312, xs: 16, ys: 32, reach: 2 },
    // Verified on the sd Quattro H sample.
    Window { raw: (6656, 4480), x0: 232, y0: 592, y1: 3920, xs: 16, ys: 32, reach: 2 },
    // UNVERIFIED below (no sample files). Their y1 do not scale with the verified sizes; the
    // values are kept as they were measured to keep the output unchanged.
    Window { raw: (3328, 2240), x0: 116, y0: 296, y1: 2200, xs: 8, ys: 16, reach: 1 },
    Window { raw: (5504, 3680), x0: 8, y0: 192, y1: 3185, xs: 16, ys: 32, reach: 2 },
    Window { raw: (2752, 1840), x0: 4, y0: 96, y1: 1800, xs: 8, ys: 16, reach: 1 },
    Window { raw: (2944, 1888), x0: 108, y0: 232, y1: 1656, xs: 8, ys: 16, reach: 1 },
];

/// Highest value of the 14-bit dp sensors; repaired samples never exceed it.
const DP_CLIP: u32 = 16383;
/// A top-layer value this close to black carries no usable ratio.
const NEAR_BLACK_MARGIN: u32 = 16;
/// Response of an autofocus site in the lower layers when only the site's own sample is
/// known, relative to a regular pixel (empirical).
const SITE_RESPONSE_RATIO: f64 = 3.75;
/// The same scale used when the neighbourhood gives no ratio.
const SITE_RESPONSE_FALLBACK: u32 = 4;

/// Repair the autofocus sites of a `w x h` Quattro buffer in place. Sizes without a table
/// entry are left alone. `active` is the frame's active area, `black` the black level the
/// ratios are measured from.
pub(super) fn repair_af_sites(buf: &mut [u16], w: usize, h: usize, active: Rect, black: u32) {
    if buf.len() < w.saturating_mul(h).saturating_mul(3) {
        return;
    }
    if let Some(l) = LATTICES.iter().find(|l| l.raw == (w, h)) {
        repair_lattice(buf, w, h, active, black, l);
    } else if let Some(win) = WINDOWS.iter().find(|s| s.raw == (w, h)) {
        repair_window(buf, w, h, active, win);
    }
}

fn repair_lattice(buf: &mut [u16], w: usize, h: usize, active: Rect, black: u32, l: &Lattice) {
    let d = l.reach;
    let x_end = (active.right() as usize).min(w.saturating_sub(d));
    let y_end = (active.bottom() as usize).min(h.saturating_sub(d));
    let x_first = (active.x as usize).max(d).next_multiple_of(l.xs);
    let y_first = (active.y as usize).max(d).next_multiple_of(l.ys);
    for y in (y_first..y_end).step_by(l.ys) {
        for x in (x_first..x_end).step_by(l.xs) {
            repair_lattice_site(buf, w, x, y, d, black);
        }
    }
}

/// Replace the bottom and middle samples at one lattice site, using the top layer's
/// nearest neighbour as the guide.
fn repair_lattice_site(buf: &mut [u16], w: usize, x: usize, y: usize, d: usize, black: u32) {
    let at = |x: usize, y: usize, k: usize| (y * w + x) * 3 + k;
    let top = |x: usize, y: usize| u32::from(buf[at(x, y, 2)]);
    let b0 = top(x, y);
    // The neighbour whose top-layer value is closest to the site's own; the first on ties.
    let around = [(x, y - d), (x, y + d), (x - d, y), (x + d, y)];
    let mut near = around[0];
    let mut best = top(near.0, near.1).abs_diff(b0);
    for &c in &around[1..] {
        let diff = top(c.0, c.1).abs_diff(b0);
        if diff < best {
            best = diff;
            near = c;
        }
    }
    let bn = top(near.0, near.1);
    let floor = black.saturating_add(NEAR_BLACK_MARGIN);
    if b0 < floor || bn < floor {
        for k in 0..2 {
            let v = u32::from(buf[at(x, y, k)]).max(black);
            let scaled =
                (u64::from(v - black) * u64::from(SITE_RESPONSE_FALLBACK) + u64::from(black)).min(u64::from(DP_CLIP));
            buf[at(x, y, k)] = scaled as u16;
        }
        return;
    }
    let ratio = (bn - black) as f32 / (b0 - black) as f32;
    let black_f = black as f32;
    for k in 0..2 {
        let v = u32::from(buf[at(x, y, k)]).max(black);
        let neighbour = f32::from(buf[at(near.0, near.1, k)]).max(black_f);
        // Two estimates of the true level: scaled from the neighbour, and from the site itself.
        let from_neighbour = (neighbour - black_f) * ratio + black_f;
        let from_site = f64::from(v - black) * SITE_RESPONSE_RATIO + f64::from(black);
        let r = (f64::from(from_neighbour) + from_site) / 2.0;
        buf[at(x, y, k)] = if r > f64::from(DP_CLIP) { DP_CLIP as u16 } else { r as u16 };
    }
}

fn repair_window(buf: &mut [u16], w: usize, h: usize, active: Rect, s: &Window) {
    let d = s.reach;
    let last_col = w.saturating_sub(1);
    let (x_lim, y_lim) = (active.right() as usize, active.bottom() as usize);
    let mut y = s.y0;
    while y <= s.y1 && y < y_lim {
        if !(y >= d && y + d < h && y + 1 < h) {
            break;
        }
        let mut x = s.x0;
        while x < last_col && x < x_lim {
            if !(x >= d && x + d + 1 < w) {
                break;
            }
            repair_window_site(buf, w, x, y, d);
            x += s.xs;
        }
        y += s.ys;
    }
}

/// Replace the bottom and middle samples at one site by the mean of the eight samples
/// around it; where the reach is two, also rebuild two top-layer samples beside it.
fn repair_window_site(buf: &mut [u16], w: usize, x: usize, y: usize, d: usize) {
    let at = |x: usize, y: usize, k: usize| (y * w + x) * 3 + k;
    for k in 0..2 {
        let ring = [
            (x - d, y - d),
            (x, y - d),
            (x + d, y - d),
            (x - d, y),
            (x + d, y),
            (x - d, y + d),
            (x, y + d),
            (x + d, y + d),
        ];
        let sum: u32 = ring.iter().map(|&(px, py)| u32::from(buf[at(px, py, k)])).sum();
        buf[at(x, y, k)] = (sum / 8) as u16;
    }
    if d == 2 {
        let top = |x: usize, y: usize| u32::from(buf[at(x, y, 2)]);
        let upper = top(x - 2, y - 1) + top(x, y - 1) + top(x + 2, y - 1) + top(x - 2, y) + top(x + 2, y);
        let lower = top(x - 2, y + 2) + top(x, y + 2) + top(x + 2, y + 2) + top(x - 2, y + 1) + top(x + 2, y + 1);
        buf[at(x + 1, y, 2)] = (upper / 5) as u16;
        buf[at(x + 1, y + 1, 2)] = (lower / 5) as u16;
    }
}

/// Copy the bottom and middle samples of every even site to the other three pixels of its
/// 2x2 block. The first two block rows, the last two and the last four block columns are
/// left as they were (zero at the odd sites).
pub(super) fn replicate_half_layers(buf: &mut [u16], w: usize, h: usize, parallel: bool) {
    let (bw, bh) = (w / 2, h / 2);
    if bw < 5 || bh < 5 || buf.len() < w * h * 3 {
        return;
    }
    let pair = w * 6; // two buffer rows
    let used = &mut buf[..bh * 2 * w * 3];
    par::for_each_row_chunk(used, pair, 16, parallel, |first, chunk| {
        for (i, rows) in chunk.chunks_exact_mut(pair).enumerate() {
            let by = first + i;
            if !(2..bh - 2).contains(&by) {
                continue;
            }
            let (even, odd) = rows.split_at_mut(w * 3);
            for bx in 0..bw - 4 {
                let o = bx * 6;
                let (v0, v1) = (even[o], even[o + 1]);
                for row in [&mut *even, &mut *odd] {
                    for site in [o, o + 3] {
                        row[site] = v0;
                        row[site + 1] = v1;
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Lcg;

    /// A buffer of random samples below 2^14 (so a repaired sample cannot hide in the clip).
    fn random_buffer(w: usize, h: usize, seed: u64) -> Vec<u16> {
        let mut rng = Lcg(seed);
        (0..w * h * 3).map(|_| (rng.next() % 16000) as u16).collect()
    }

    /// Positions (x, y, layer) at which two buffers differ.
    fn changes(before: &[u16], after: &[u16], w: usize) -> Vec<(usize, usize, usize)> {
        before
            .iter()
            .zip(after)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| ((i / 3) % w, i / 3 / w, i % 3))
            .collect()
    }

    #[test]
    fn near_black_sites_are_scaled() {
        // 5 x 5 pixels, site at (2, 2), reach 2 would not fit: use reach 1 on the 3 x 3 core.
        let w = 5;
        let mut buf = vec![0u16; w * w * 3];
        let at = |x: usize, y: usize, k: usize| (y * w + x) * 3 + k;
        buf[at(2, 2, 2)] = 110; // the site's top layer: below black + 16
        buf[at(2, 2, 0)] = 150;
        buf[at(2, 2, 1)] = 50; // under black: raised to it first
        repair_lattice_site(&mut buf, w, 2, 2, 1, 100);
        assert_eq!((buf[at(2, 2, 0)], buf[at(2, 2, 1)]), (300, 100));
        // The clip.
        buf[at(2, 2, 0)] = 9000;
        repair_lattice_site(&mut buf, w, 2, 2, 1, 100);
        assert_eq!(buf[at(2, 2, 0)], 16383);
    }

    #[test]
    fn lattice_sites_average_both_estimates() {
        let w = 5;
        let at = |x: usize, y: usize, k: usize| (y * w + x) * 3 + k;
        let make = |north: u16, south: u16, west: u16, east: u16| {
            let mut buf = vec![0u16; w * w * 3];
            buf[at(2, 2, 2)] = 1100;
            for ((x, y), v) in [((2, 1), north), ((2, 3), south), ((1, 2), west), ((3, 2), east)] {
                buf[at(x, y, 2)] = v;
            }
            buf[at(2, 2, 0)] = 400;
            buf[at(2, 2, 1)] = 150;
            // Lower layers of the south neighbour and of the north neighbour.
            buf[at(2, 3, 0)] = 700;
            buf[at(2, 3, 1)] = 100;
            buf[at(2, 1, 0)] = 3000;
            buf[at(2, 1, 1)] = 3000;
            buf
        };
        // The south neighbour (1600) is the nearest to 1100 (|diff| 500): ratio (1600-100)/(1100-100) = 1.5.
        let mut buf = make(2600, 1600, 5000, 4000);
        repair_lattice_site(&mut buf, w, 2, 2, 1, 100);
        // layer 0: from the neighbour (700-100)*1.5+100 = 1000, from the site (400-100)*3.75+100 = 1225 -> 1112.5
        // layer 1: neighbour 100 (raised to black) -> 100, site (150-100)*3.75+100 = 287.5 -> 193.75
        assert_eq!((buf[at(2, 2, 0)], buf[at(2, 2, 1)]), (1112, 193));
        // On a tie the earlier candidate (north) wins: both 500 away.
        let mut buf = make(1600, 600, 9000, 9000);
        repair_lattice_site(&mut buf, w, 2, 2, 1, 100);
        // north: ratio 1.5, lower layers 3000: (2900*1.5+100 + 1225)/2 = 2837.5
        assert_eq!(buf[at(2, 2, 0)], 2837);
        // The top layer and everything else stay.
        assert_eq!(buf[at(2, 2, 2)], 1100);
    }

    #[test]
    fn window_sites_are_ring_means() {
        let w = 9;
        let at = |x: usize, y: usize, k: usize| (y * w + x) * 3 + k;
        let mut buf = vec![0u16; w * w * 3];
        for (i, v) in buf.iter_mut().enumerate() {
            *v = (i % 97) as u16 * 3;
        }
        let before = buf.clone();
        repair_window_site(&mut buf, w, 4, 4, 2);
        for k in 0..2 {
            let ring: u32 = [(2, 2), (4, 2), (6, 2), (2, 4), (6, 4), (2, 6), (4, 6), (6, 6)]
                .iter()
                .map(|&(x, y)| u32::from(before[at(x, y, k)]))
                .sum();
            assert_eq!(u32::from(buf[at(4, 4, k)]), ring / 8);
        }
        let sum = |pts: [(usize, usize); 5]| pts.iter().map(|&(x, y)| u32::from(before[at(x, y, 2)])).sum::<u32>();
        assert_eq!(u32::from(buf[at(5, 4, 2)]), sum([(2, 3), (4, 3), (6, 3), (2, 4), (6, 4)]) / 5,);
        assert_eq!(u32::from(buf[at(5, 5, 2)]), sum([(2, 6), (4, 6), (6, 6), (2, 5), (6, 5)]) / 5,);
        // Reach one leaves the top layer alone.
        let mut buf = before.clone();
        repair_window_site(&mut buf, w, 4, 4, 1);
        assert_eq!(changes(&before, &buf, w).iter().filter(|c| c.2 == 2).count(), 0);
    }

    /// Where the repair may write, per size: the sites of the table (spec: the sites are the points of the lattice
    /// inside the active area; the window sites step from the start to the last row/column).
    #[test]
    fn only_the_sites_change() {
        for (w, h, active, sites) in [
            // (5888, 3672): 170 columns x 453 rows measured on real files.
            (5888, 3672, Rect::new(204, 24, 5446, 3624), Some(170 * 453)),
            (5888, 3776, Rect::new(204, 24, 5446, 3728), Some(340 * 90)),
            (6656, 4480, Rect::new(200, 32, 6225, 4400), Some(388 * 105)),
            (2944, 1836, Rect::new(100, 12, 2700, 1800), None),
            (3328, 2240, Rect::new(100, 20, 3200, 2200), None),
            (5504, 3680, Rect::new(8, 20, 5400, 3600), None),
            (2752, 1840, Rect::new(8, 20, 2700, 1800), None),
            (2944, 1888, Rect::new(100, 20, 2800, 1860), None),
        ] {
            let before = random_buffer(w, h, (w * h) as u64);
            let mut buf = before.clone();
            repair_af_sites(&mut buf, w, h, active, 2047);
            let changed = changes(&before, &buf, w);
            assert!(!changed.is_empty(), "{w}x{h}");
            let dp = LATTICES.iter().find(|l| l.raw == (w, h));
            let window = WINDOWS.iter().find(|s| s.raw == (w, h));
            let mut pixels = std::collections::BTreeSet::new();
            for &(x, y, k) in &changed {
                match (dp, window) {
                    (Some(l), _) => {
                        // Lattice: bottom and middle layers at multiples of the steps, inside the area.
                        assert!(k < 2 && x % l.xs == 0 && y % l.ys == 0, "{w}x{h} {x},{y},{k}");
                        assert!(x >= active.x as usize && x < active.right() as usize);
                        assert!(y >= active.y as usize && y < active.bottom() as usize);
                        pixels.insert((x, y));
                    }
                    (_, Some(s)) => {
                        // Window: sites on the grid; a site's top-layer samples sit to the right/below.
                        let (gx, gy) = if k == 2 { (x - 1, y - (y - s.y0) % s.ys) } else { (x, y) };
                        assert!(gx >= s.x0 && (gx - s.x0) % s.xs == 0, "{w}x{h} {x},{y},{k}");
                        assert!(gy >= s.y0 && (gy - s.y0) % s.ys == 0 && gy <= s.y1, "{w}x{h} {x},{y},{k}");
                        assert!(k < 2 || s.reach == 2);
                        if k < 2 {
                            pixels.insert((x, y));
                        }
                    }
                    _ => unreachable!(),
                }
            }
            if let Some(n) = sites {
                assert_eq!(pixels.len(), n, "{w}x{h}");
            }
        }
    }

    #[test]
    fn other_sizes_are_left_alone() {
        let before = random_buffer(640, 480, 5);
        let mut buf = before.clone();
        repair_af_sites(&mut buf, 640, 480, Rect::full(640, 480), 100);
        assert_eq!(buf, before);
        // A buffer too small for its size is not touched either.
        let mut short = vec![1u16; 100];
        repair_af_sites(&mut short, 5888, 3672, Rect::full(5888, 3672), 0);
        assert_eq!(short, vec![1u16; 100]);
    }

    #[test]
    fn replication_fills_the_blocks_inside_the_borders() {
        let (w, h) = (24, 20);
        let mut rng = Lcg(3);
        let mut buf = vec![0u16; w * h * 3];
        for y in (0..h).step_by(2) {
            for x in (0..w).step_by(2) {
                for k in 0..2 {
                    buf[(y * w + x) * 3 + k] = 1 + rng.below(60000) as u16;
                }
            }
        }
        for px in buf.chunks_exact_mut(3) {
            px[2] = rng.below(60000) as u16;
        }
        let before = buf.clone();
        let mut serial = before.clone();
        replicate_half_layers(&mut serial, w, h, false);
        let mut parallel = before.clone();
        replicate_half_layers(&mut parallel, w, h, true);
        assert_eq!(serial, parallel);
        let (bw, bh) = (w / 2, h / 2);
        for y in 0..h {
            for x in 0..w {
                let (bx, by) = (x / 2, y / 2);
                let inside = bx < bw - 4 && (2..bh - 2).contains(&by);
                for k in 0..3 {
                    let i = (y * w + x) * 3 + k;
                    let expected = if k < 2 && inside { before[((y & !1) * w + (x & !1)) * 3 + k] } else { before[i] };
                    assert_eq!(serial[i], expected, "({x},{y},{k})");
                }
            }
        }
        // Too small to have any interior: nothing happens.
        let mut small = vec![7u16; 8 * 8 * 3];
        small[0] = 9;
        let copy = small.clone();
        replicate_half_layers(&mut small, 8, 8, false);
        assert_eq!(small, copy);
    }

    /// The traversal order does not matter: sites never read what another site wrote.
    #[test]
    fn repair_is_independent_of_the_traversal_order() {
        let (w, h) = (5888, 3672);
        let before = random_buffer(w, h, 99);
        let active = Rect::new(204, 24, 5446, 3624);
        let mut forward = before.clone();
        repair_af_sites(&mut forward, w, h, active, 2047);
        // Reverse order by hand with the single-site function.
        let mut backward = before.clone();
        let l = &LATTICES[0];
        for y in (l.reach..h - l.reach).rev().filter(|y| y % l.ys == 0 && (24..3648).contains(y)) {
            for x in (l.reach..w - l.reach).rev().filter(|x| x % l.xs == 0 && (204..5650).contains(x)) {
                repair_lattice_site(&mut backward, w, x, y, l.reach, 2047);
            }
        }
        assert_eq!(forward, backward);
    }
}
