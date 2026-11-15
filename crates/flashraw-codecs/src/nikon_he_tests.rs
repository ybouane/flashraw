//! Tests of the High Efficiency decoder against a model written from the format description:
//! plain loops for the transforms, and a stream writer that mirrors the packet syntax and
//! knows the coefficients it encoded.

use super::*;
use proptest::prelude::*;

// ---------------------------------------------------------------------------------------------
// Random numbers and bit writer
// ---------------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        ((self.next() >> 11) % n as u64) as usize
    }
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[self.below(v.len())]
    }
}

#[derive(Default)]
struct BitWriter {
    bits: Vec<bool>,
}

impl BitWriter {
    fn put(&mut self, v: u64, n: usize) {
        for i in (0..n).rev() {
            self.bits.push((v >> i) & 1 == 1);
        }
    }
    fn unary(&mut self, x: usize) {
        self.bits.extend(std::iter::repeat_n(true, x));
        self.bits.push(false);
    }
    fn align(&mut self) {
        while self.bits.len() % 8 != 0 {
            self.bits.push(false);
        }
    }
    fn bytes(&self) -> Vec<u8> {
        self.bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |a, (i, &b)| a | (u8::from(b) << (7 - i)))).collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Stream writer
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
struct Params {
    width: usize,
    height: usize,
    hsl: usize,
    ss: usize,
    levels: [usize; 4],
    black: usize,
    gain: usize,
    knee: usize,
    /// Probability of a raw packet, of an insignificant group flag, and the bit density.
    p_raw: f64,
    p_insig: f64,
    density: f64,
    /// `None`: random modes; otherwise every band uses this mode.
    mode: Option<u8>,
    qmax: usize,
    /// Encode no coefficients at all.
    zero: bool,
    /// Slack bytes and trailing padding.
    slack: bool,
}

impl Params {
    fn random(rng: &mut Rng) -> Params {
        let levels = rng.pick(&[
            [5, 5, 0, 5],
            [5, 5, 0, 5],
            [3, 3, 0, 3],
            [1, 1, 0, 1],
            [2, 4, 1, 0],
            [8, 8, 8, 8],
            [0, 0, 0, 0],
            [4, 0, 5, 2],
            [6, 6, 0, 6],
        ]);
        let (rb, rg, rk) = (rng.below(4096), rng.range(1, (1 << 18) - 1), rng.below(65536));
        Params {
            width: 2 * rng.range(4, 90),
            height: 4 * rng.range(2, 14),
            hsl: rng.pick(&[1, 2, 3, 5, 16]),
            ss: rng.pick(&[1, 2, 3, 8, 8, 255]),
            levels,
            black: rng.pick(&[1008, 0, rb]),
            gain: rng.pick(&[84429, 65536, rg]),
            knee: rng.pick(&[16255, 32768, rk]),
            p_raw: rng.pick(&[0.0, 0.0, 0.1, 0.5]),
            p_insig: rng.pick(&[0.0, 0.1, 0.5, 0.9]),
            density: rng.pick(&[0.2, 0.5, 0.9]),
            mode: rng.pick(&[Some(3), Some(3), None]),
            qmax: rng.pick(&[6, 12, 20, 255]),
            zero: false,
            slack: rng.chance(0.5),
        }
    }
}

/// The wavelet bands in band order: `(offset, width)`, and the band indices of the eight packets.
fn model_bands(w2: usize, levels: [usize; 4]) -> (Vec<(usize, usize)>, Vec<Vec<usize>>) {
    let mut bands = Vec::new();
    let mut packets = Vec::new();
    let mut shared = [usize::MAX; 4];
    for line in 0..2 {
        for (c, &lv) in levels.iter().enumerate() {
            if lv == 0 {
                if line == 0 {
                    shared[c] = bands.len();
                    bands.push((0, w2));
                }
                packets.push(vec![shared[c]]);
            } else if line == 0 {
                let mut lens = vec![w2];
                for _ in 0..lv {
                    let n = *lens.last().unwrap();
                    lens.push(n - n / 2);
                }
                let mut widths = vec![lens[lv]];
                widths.extend((0..lv).rev().map(|i| lens[i] / 2));
                let start = bands.len();
                let mut off = 0;
                for w in widths {
                    bands.push((off, w));
                    off += w;
                }
                packets.push((start..bands.len()).collect());
            } else {
                let start = bands.len();
                bands.push((0, w2 - w2 / 2));
                bands.push((w2 - w2 / 2, w2 / 2));
                packets.push((start..bands.len()).collect());
            }
        }
    }
    (bands, packets)
}

struct Written {
    stream: Vec<u8>,
    /// Dequantised coefficients: `[precinct][row 0..8][w2]`, row = line * 4 + component.
    coef: Vec<i32>,
    /// Offset of the first precinct.
    first_precinct: usize,
}

/// Expected value of a magnitude after the uniform quantiser's reconstruction.
fn dequant(m: u32, gcli: u32, t: u32) -> i32 {
    let mut v = m as u64;
    if t > 0 && gcli > t && m != 0 {
        let s = gcli - t + 1;
        let mut k = 1;
        while (m as u64) >> (s * k) > 0 {
            v += (m as u64) >> (s * k);
            k += 1;
        }
    }
    (v << 4) as i32
}

fn write_stream(p: &Params, rng: &mut Rng) -> Written {
    let w2 = p.width / 2;
    let (bands, packets) = model_bands(w2, p.levels);
    let nb = bands.len();
    let mut prios: Vec<u8> = (0..nb as u8).collect();
    for i in (1..nb).rev() {
        prios.swap(i, rng.below(i + 1));
    }
    let weights: Vec<(u8, u8)> =
        (0..nb).map(|i| (if p.qmax == 0 { 0 } else { rng.pick(&[0, 0, 1, 2, 3]) }, prios[i])).collect();
    let mut s = vec![0xFF, 0x10, 0xFF, 0x50, 0x00, 0x22];
    s.extend_from_slice(b"CONTACT_INTOPIX_");
    s.extend_from_slice(&[0xEF, 0xC0]);
    s.extend(std::iter::repeat_n(0, 14));
    s.extend_from_slice(&[0xFF, 0x12, 0x00, 0x27]);
    s.extend_from_slice(&[0; 8]);
    s.extend_from_slice(&(p.width as u16).to_be_bytes());
    s.extend_from_slice(&(p.height as u16).to_be_bytes());
    s.extend_from_slice(&[0, 0]);
    s.extend_from_slice(&(p.hsl as u16).to_be_bytes());
    s.extend_from_slice(&[4, 4, p.ss as u8, 18, 0x44, 0x10, 0x51, 0x14]);
    let mut vendor = [0u8; 13];
    for v in &mut vendor {
        *v = rng.below(256) as u8;
    }
    vendor[3] = (vendor[3] & 0xF0) | (p.black >> 8) as u8;
    vendor[4] = p.black as u8;
    vendor[7] = (vendor[7] & 0xFC) | (p.gain >> 16) as u8;
    vendor[8] = (p.gain >> 8) as u8;
    vendor[9] = p.gain as u8;
    vendor[10] = (p.knee >> 8) as u8;
    vendor[11] = p.knee as u8;
    s.extend_from_slice(&vendor);
    s.extend_from_slice(&[0xFF, 0x13, 0x00, 0x0E]);
    for &lv in &p.levels {
        s.extend_from_slice(&[14, 0x11, if lv == 0 { 0 } else { (lv << 4 | 1) as u8 }]);
    }
    s.extend_from_slice(&[0xFF, 0x14]);
    s.extend_from_slice(&((2 + 2 * nb) as u16).to_be_bytes());
    for &(g, pr) in &weights {
        s.extend_from_slice(&[g, pr]);
    }

    let precincts = p.height / 4;
    let first_precinct = s.len();
    let mut coef = vec![0i32; precincts * 8 * w2];
    let mut prev: Vec<Option<(Vec<u8>, u8)>> = vec![None; nb];
    for pi in 0..precincts {
        if pi % p.hsl == 0 {
            s.extend_from_slice(&[0xFF, 0x20, 0x00, 0x04]);
            s.extend_from_slice(&((pi / p.hsl) as u16).to_be_bytes());
            prev = vec![None; nb];
        }
        let q = rng.below(p.qmax + 1);
        let r = rng.below(nb + 2);
        let modes: Vec<u8> = (0..nb).map(|_| p.mode.unwrap_or_else(|| rng.below(4) as u8)).collect();
        let gtli = |b: usize| -> u32 {
            let v = q as i32 - i32::from(weights[b].0) - i32::from(usize::from(weights[b].1) < r);
            v.clamp(0, 15) as u32
        };
        let mut payload = Vec::new();
        for (pk, bidx) in packets.iter().enumerate() {
            let (line, comp) = (pk / 4, pk % 4);
            let raw = rng.chance(p.p_raw);
            let (mut sigw, mut gw, mut dw, mut sw) =
                (BitWriter::default(), BitWriter::default(), BitWriter::default(), BitWriter::default());
            let mut sig: Vec<Option<Vec<bool>>> = vec![None; nb];
            if !raw {
                for &b in bidx {
                    if modes[b] & 2 != 0 {
                        let n = bands[b].1.div_ceil(4).div_ceil(p.ss);
                        let fl: Vec<bool> = (0..n).map(|_| rng.chance(p.p_insig)).collect();
                        for &f in &fl {
                            sigw.put(u64::from(f), 1);
                        }
                        sig[b] = Some(fl);
                    }
                }
                sigw.align();
            }
            let mut lines: Vec<Vec<u8>> = vec![Vec::new(); nb];
            for &b in bidx {
                let t = gtli(b);
                let groups = bands[b].1.div_ceil(4);
                let mut line_g = Vec::with_capacity(groups);
                let insig = |g: usize| sig[b].as_ref().is_some_and(|f| f[g / p.ss]);
                for g in 0..groups {
                    let want = |rng: &mut Rng| -> u32 {
                        if p.zero {
                            t
                        } else if rng.chance(0.7) {
                            (t + (rng.below(8) as u32) / (1 + rng.below(3) as u32)).min(15)
                        } else {
                            rng.range(t as usize, 15) as u32
                        }
                    };
                    if raw {
                        let v = if p.zero { 0 } else { rng.below(16) as u32 };
                        gw.put(u64::from(v), 4);
                        line_g.push(v as u8);
                    } else if let Some((top, top_t)) = prev[b].as_ref().filter(|_| modes[b] & 1 != 0) {
                        let floor = u32::from(*top_t).max(t);
                        let m = u32::from(top[g]).max(floor);
                        if insig(g) {
                            line_g.push(m as u8);
                        } else {
                            let room = m.saturating_sub(t) as i32;
                            let tgt = loop {
                                let c = want(rng);
                                let delta = c as i32 - m as i32;
                                let x = if delta > room {
                                    delta + room
                                } else if delta >= 0 {
                                    2 * delta
                                } else {
                                    -2 * delta - 1
                                };
                                if x < 32 {
                                    gw.unary(x as usize);
                                    break c;
                                }
                            };
                            line_g.push(tgt as u8);
                        }
                    } else if insig(g) {
                        line_g.push(t as u8);
                    } else {
                        let tgt = want(rng);
                        gw.unary((tgt - t) as usize);
                        line_g.push(tgt as u8);
                    }
                }
                lines[b] = line_g;
            }
            gw.align();
            // Bit planes, then signs of every non-zero slot (padding slots included).
            let mut slots: Vec<(usize, Vec<u32>)> = Vec::new();
            for &b in bidx {
                let t = gtli(b);
                let groups = bands[b].1.div_ceil(4);
                let mut mags = vec![0u32; groups * 4];
                for g in 0..groups {
                    let gc = u32::from(lines[b][g]);
                    if gc <= t {
                        continue;
                    }
                    let mut v = [0u32; 4];
                    for _ in 0..gc - t {
                        for x in v.iter_mut() {
                            let bit = u32::from(rng.chance(p.density));
                            dw.put(u64::from(bit), 1);
                            *x = (*x << 1) | bit;
                        }
                    }
                    for k in 0..4 {
                        mags[4 * g + k] = v[k] << t;
                    }
                }
                slots.push((b, mags));
            }
            dw.align();
            for (b, mags) in &slots {
                let t = gtli(*b);
                let (off, width) = bands[*b];
                for (i, &m) in mags.iter().enumerate() {
                    if m == 0 {
                        continue;
                    }
                    let neg = rng.chance(0.5);
                    sw.put(u64::from(neg), 1);
                    if i < width {
                        let g = i / 4;
                        let v = dequant(m, u32::from(lines[*b][g]), t);
                        coef[(pi * 8 + line * 4 + comp) * w2 + off + i] = if neg { -v } else { v };
                    }
                }
            }
            sw.align();
            let (mut gb, mut db, mut sb) = (gw.bytes(), dw.bytes(), sw.bytes());
            if p.slack {
                for b in [&mut gb, &mut db, &mut sb] {
                    if rng.chance(0.15) {
                        b.extend(std::iter::repeat_n(0, rng.range(1, 3)));
                    }
                }
            }
            let a = (u32::from(raw) << 31) | ((db.len() as u32) << 11) | (gb.len() as u32 >> 9);
            let bw = ((gb.len() as u32 & 0x1FF) << 15) | sb.len() as u32;
            payload.extend_from_slice(&a.to_be_bytes());
            payload.extend_from_slice(&bw.to_be_bytes()[1..]);
            payload.extend(sigw.bytes());
            payload.extend(gb);
            payload.extend(db);
            payload.extend(sb);
            for &b in bidx {
                prev[b] = Some((std::mem::take(&mut lines[b]), gtli(b) as u8));
            }
        }
        if p.slack {
            payload.extend(std::iter::repeat_n(0, rng.pick(&[0, 0, 1, 5, 17])));
        }
        s.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
        s.extend_from_slice(&[q as u8, r as u8]);
        let mut mw = BitWriter::default();
        for &m in &modes {
            mw.put(u64::from(m), 2);
        }
        s.extend(mw.bytes());
        s.extend(payload);
    }
    s.extend_from_slice(&[0xFF, 0x11]);
    Written { stream: s, coef, first_precinct }
}

// ---------------------------------------------------------------------------------------------
// Model of the transforms (plain loops)
// ---------------------------------------------------------------------------------------------

/// Inverse 5/3 of one deinterleaved row, written as in the format description.
fn model_inverse_53(x: &[i32]) -> Vec<i32> {
    let n = x.len();
    if n < 2 {
        return x.to_vec();
    }
    let lows = n.div_ceil(2);
    let highs = n / 2;
    let lo = &x[..lows];
    let hi = &x[lows..];
    let mut e = vec![0i32; n];
    for k in 0..lows {
        let left = if k > 0 { hi[k - 1] } else { hi[0] };
        let right = if k < highs { hi[k] } else { hi[highs - 1] };
        e[2 * k] = lo[k].wrapping_sub(left.wrapping_add(right).wrapping_add(2) >> 2);
    }
    for k in 0..highs {
        let right = if 2 * k + 2 < n { e[2 * k + 2] } else { e[2 * k] };
        e[2 * k + 1] = hi[k].wrapping_add(e[2 * k].wrapping_add(right) >> 1);
    }
    e
}

/// Everything after the entropy decoder, per the format description, on whole planes.
fn model_reconstruct(
    coef: &[i32],
    w2: usize,
    precincts: usize,
    levels: [usize; 4],
    (black, gain, knee): (i64, i64, i64),
) -> Vec<u16> {
    let h2 = 2 * precincts;
    // Horizontal stage.
    let mut hor = coef.to_vec();
    for p in 0..precincts {
        for c in 0..4 {
            if levels[c] == 0 {
                continue;
            }
            let low = &mut hor[(p * 8 + c) * w2..(p * 8 + c + 1) * w2];
            let mut lens = vec![w2];
            for _ in 0..levels[c] {
                let n = *lens.last().unwrap();
                lens.push(n - n / 2);
            }
            for i in (0..levels[c]).rev() {
                let r = model_inverse_53(&low[..lens[i]]);
                low[..lens[i]].copy_from_slice(&r);
            }
            let high = &mut hor[(p * 8 + 4 + c) * w2..(p * 8 + 5 + c) * w2];
            let r = model_inverse_53(high);
            high.copy_from_slice(&r);
        }
    }
    // Vertical stage into four planes.
    let mut planes = vec![vec![0i32; h2 * w2]; 4];
    let row = |p: usize, line: usize, c: usize, x: usize| hor[(p * 8 + line * 4 + c) * w2 + x];
    for c in 0..4 {
        for p in 0..precincts {
            for x in 0..w2 {
                if levels[c] == 0 {
                    planes[c][2 * p * w2 + x] = row(p, 0, c, x);
                    planes[c][(2 * p + 1) * w2 + x] = row(p, 1, c, x);
                } else {
                    let e = |q: usize| {
                        let before = if q == 0 { row(0, 1, c, x) } else { row(q - 1, 1, c, x) };
                        row(q, 0, c, x).wrapping_sub(before.wrapping_add(row(q, 1, c, x)).wrapping_add(2) >> 2)
                    };
                    let ep = e(p);
                    let next = if p + 1 < precincts { e(p + 1) } else { ep };
                    planes[c][2 * p * w2 + x] = ep;
                    planes[c][(2 * p + 1) * w2 + x] = row(p, 1, c, x).wrapping_add(ep.wrapping_add(next) >> 1);
                }
            }
        }
    }
    // Colour transform.
    let [a, b, d, e] = &mut planes[..] else { unreachable!() };
    let idx = |r: usize, x: usize| r * w2 + x;
    let up = |r: usize| r.saturating_sub(1);
    let dn = |r: usize| (r + 1).min(h2 - 1);
    let lx = |x: usize| x.saturating_sub(1);
    let rx = |x: usize| (x + 1).min(w2 - 1);
    for r in 0..h2 {
        for x in 0..w2 {
            let s = d[idx(up(r), x)]
                .wrapping_add(d[idx(up(r), rx(x))])
                .wrapping_add(d[idx(r, x)])
                .wrapping_add(d[idx(r, rx(x))]);
            a[idx(r, x)] = a[idx(r, x)].wrapping_sub(s >> 3);
        }
    }
    for r in 0..h2 {
        for x in 0..w2 {
            let s1 =
                b[idx(r, x)].wrapping_add(b[idx(dn(r), x)]).wrapping_add(e[idx(r, lx(x))]).wrapping_add(e[idx(r, x)]);
            let s2 = a[idx(r, lx(x))]
                .wrapping_add(a[idx(r, x)])
                .wrapping_add(a[idx(dn(r), lx(x))])
                .wrapping_add(a[idx(dn(r), x)]);
            d[idx(r, x)] = d[idx(r, x)].wrapping_sub(s1 >> 3).wrapping_add(s2 >> 2);
        }
    }
    for r in 0..h2 {
        for x in 0..w2 {
            let s =
                b[idx(r, x)].wrapping_add(b[idx(r, rx(x))]).wrapping_add(e[idx(up(r), x)]).wrapping_add(e[idx(r, x)]);
            a[idx(r, x)] = a[idx(r, x)].wrapping_sub(s >> 3);
        }
    }
    for r in 0..h2 {
        for x in 0..w2 {
            let s =
                a[idx(r, lx(x))].wrapping_add(a[idx(r, x)]).wrapping_add(d[idx(up(r), x)]).wrapping_add(d[idx(r, x)]);
            b[idx(r, x)] = b[idx(r, x)].wrapping_add(s >> 2);
        }
    }
    for r in 0..h2 {
        for x in 0..w2 {
            let s =
                a[idx(r, x)].wrapping_add(a[idx(dn(r), x)]).wrapping_add(d[idx(r, x)]).wrapping_add(d[idx(r, rx(x))]);
            e[idx(r, x)] = e[idx(r, x)].wrapping_add(s >> 2);
        }
    }
    // Curve and mosaic.
    let lut = |v: i32| -> u16 {
        let i = i64::from(v.wrapping_add(32768).clamp(0, 65535));
        let dd = i - knee;
        let q = if dd < 0 {
            -(dd * dd)
        } else {
            let t = (dd * gain + 32768) >> 16;
            t * t
        };
        (black + ((q + 163_840) >> 18)).clamp(0, 16383) as u16
    };
    let mut out = vec![0u16; 4 * h2 * w2];
    for y in 0..h2 {
        for x in 0..w2 {
            out[2 * y * 2 * w2 + 2 * x] = lut(b[idx(y, x)]);
            out[2 * y * 2 * w2 + 2 * x + 1] = lut(a[idx(y, x)]);
            out[(2 * y + 1) * 2 * w2 + 2 * x] = lut(d[idx(y, x)]);
            out[(2 * y + 1) * 2 * w2 + 2 * x + 1] = lut(e[idx(y, x)]);
        }
    }
    out
}

fn decode_with(cs: &[u8], w: usize, h: usize, parallel: bool) -> Result<Vec<u16>> {
    let mut out = vec![0xA5A5u16; w * h];
    let mut run = || decode(cs, &mut out, w, h, w, &Limits::default(), parallel);
    let r = if parallel { run() } else { par::sequential(run) };
    r.map(|()| out)
}

fn expected_output(p: &Params, wr: &Written) -> Vec<u16> {
    model_reconstruct(&wr.coef, p.width / 2, p.height / 4, p.levels, (p.black as i64, p.gain as i64, p.knee as i64))
}

// ---------------------------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------------------------

#[test]
fn model_bands_match_the_real_layout() {
    // 25 bands for the layout of the camera files, and the widths of the worked example.
    let (bands, packets) = model_bands(2800, [5, 5, 0, 5]);
    assert_eq!(bands.len(), 25);
    assert_eq!(&bands[..6], &[(0, 88), (88, 87), (175, 175), (350, 350), (700, 700), (1400, 1400)]);
    assert_eq!(bands[19], (0, 1400));
    assert_eq!(bands[20], (1400, 1400));
    assert_eq!(packets[2], vec![12]);
    assert_eq!(packets[6], vec![12]);
    let l = Layout::new(&Header {
        width: 5600,
        height: 3728,
        slice_height: 16,
        ss: 8,
        levels: [5, 5, 0, 5],
        weights: vec![(0, 0); 25],
        black: 0,
        gain: 0,
        knee: 0,
        first_slice: 0,
    })
    .unwrap();
    assert_eq!(l.header_len, 12);
    for (b, &(off, w)) in bands.iter().enumerate() {
        assert_eq!((l.bands[b].offset, l.bands[b].width), (off, w));
    }
    assert_eq!(l.packets.to_vec(), packets);
}

proptest! {
    #[test]
    fn inverse_53_matches_the_model(x in proptest::collection::vec(-100_000i32..100_000, 0..70)) {
        let mut a = x.clone();
        let mut tmp = vec![0; a.len()];
        inverse_53(&mut a, &mut tmp);
        prop_assert_eq!(a, model_inverse_53(&x));
    }
}

#[test]
fn inverse_53_of_every_length_and_wrapping() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for n in 0..70 {
        for wide in [false, true] {
            let x: Vec<i32> =
                (0..n).map(|_| if wide { rng.next() as i32 } else { (rng.next() % 5000) as i32 - 2500 }).collect();
            let mut a = x.clone();
            let mut tmp = vec![0; n];
            inverse_53(&mut a, &mut tmp);
            assert_eq!(a, model_inverse_53(&x), "n = {n}");
        }
    }
    // Lengths below two are left alone.
    let mut one = [7];
    inverse_53(&mut one, &mut [0]);
    assert_eq!(one, [7]);
}

#[test]
fn output_curve_of_the_camera_parameters() {
    let lut = build_curve(1008, 84429, 16255);
    for (i, v) in [
        (0, 0),
        (8192, 760),
        (16254, 1008),
        (16255, 1008),
        (20000, 1097),
        (32768, 2734),
        (40000, 4578),
        (49023, 7806),
        (60000, 13124),
        (65535, 16383),
    ] {
        assert_eq!(lut[i], v, "index {i}");
    }
}

#[test]
fn dequantisation_of_the_uniform_quantiser() {
    // Reconstruction m + (m >> s) + (m >> 2s) + ... with s = gcli - t + 1, then << 4.
    assert_eq!(dequant(0, 9, 3), 0);
    assert_eq!(dequant(1024, 12, 3), (1024 + 1) << 4);
    assert_eq!(dequant(0x7FF8, 15, 3), (0x7FF8 + (0x7FF8 >> 13)) << 4);
    // No expansion without truncation.
    assert_eq!(dequant(5, 4, 0), 5 << 4);
}

fn run_model_case(seed: u64) {
    let mut rng = Rng(seed | 1);
    let p = Params::random(&mut rng);
    let wr = write_stream(&p, &mut rng);
    let want = expected_output(&p, &wr);
    let got = decode_with(&wr.stream, p.width, p.height, false).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
    assert!(got == want, "seed {seed}: output differs");
    let par = decode_with(&wr.stream, p.width, p.height, true).unwrap();
    assert!(got == par, "seed {seed}: threads differ");
}

#[test]
fn random_streams_match_the_model() {
    let cases = if cfg!(debug_assertions) { 120 } else { 600 };
    for seed in 0..cases {
        run_model_case(0x9E37_79B9_7F4A_7C15u64.wrapping_mul(seed + 1));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn any_seed_matches_the_model(seed in any::<u64>()) {
        run_model_case(seed);
    }
}

#[test]
fn an_empty_stream_is_the_curve_at_zero() {
    let mut rng = Rng(77);
    let mut p = Params::random(&mut rng);
    (p.black, p.gain, p.knee) = (1008, 84429, 16255);
    p.zero = true;
    p.levels = [5, 5, 0, 5];
    p.p_raw = 0.0;
    let wr = write_stream(&p, &mut rng);
    let out = decode_with(&wr.stream, p.width, p.height, false).unwrap();
    assert!(out.iter().all(|&v| v == 2734));
}

#[test]
fn big_pictures_decode_the_same_on_one_and_many_threads() {
    let mut rng = Rng(4242);
    let mut p = Params::random(&mut rng);
    (p.width, p.height, p.hsl, p.levels) = (264, 200, 3, [5, 5, 0, 5]);
    let wr = write_stream(&p, &mut rng);
    let seq = decode_with(&wr.stream, p.width, p.height, false).unwrap();
    assert!(seq == expected_output(&p, &wr));
    assert!(seq == decode_with(&wr.stream, p.width, p.height, true).unwrap());
}

#[test]
fn extreme_coefficients_wrap_instead_of_panicking() {
    let mut rng = Rng(99);
    let mut p = Params::random(&mut rng);
    p.levels = [8, 8, 8, 8];
    p.qmax = 0;
    p.density = 1.0;
    p.p_insig = 0.0;
    p.p_raw = 0.0;
    p.mode = Some(3);
    let wr = write_stream(&p, &mut rng);
    let a = decode_with(&wr.stream, p.width, p.height, false).unwrap();
    assert!(a == expected_output(&p, &wr));
}

// ---------------------------------------------------------------------------------------------
// Malformed input
// ---------------------------------------------------------------------------------------------

fn small_stream(seed: u64) -> (Vec<u8>, Params, usize) {
    let mut rng = Rng(seed);
    let mut p = Params::random(&mut rng);
    (p.width, p.height) = (rng.range(4, 12) * 2, rng.range(2, 5) * 4);
    let w = write_stream(&p, &mut rng);
    (w.stream, p, w.first_precinct)
}

fn kind(r: Result<Vec<u16>>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(e) => e.kind(),
    }
}

#[test]
fn every_prefix_of_a_valid_stream_fails() {
    for seed in [3, 11, 29] {
        let (s, p, _) = small_stream(seed);
        assert_eq!(kind(decode_with(&s, p.width, p.height, false)), "ok");
        for n in 0..s.len() {
            let k = kind(decode_with(&s[..n], p.width, p.height, false));
            assert!(k == "CorruptFile" || k == "UnsupportedCompression", "seed {seed} prefix {n}: {k}");
        }
    }
}

#[test]
fn replaced_bytes_never_panic() {
    for seed in [5, 17] {
        let (s, p, first) = small_stream(seed);
        let limit = (first + 200).min(s.len());
        for at in 0..limit {
            for v in [0x00, 0x7F, 0xFF, s[at] ^ 0x01, s[at].wrapping_add(1)] {
                let mut m = s.clone();
                m[at] = v;
                let _ = decode_with(&m, p.width, p.height, false);
                let _ = decode_with(&m, p.width, p.height, true);
            }
        }
    }
}

#[test]
fn extreme_header_fields_are_rejected_cleanly() {
    let (s, p, first) = small_stream(21);
    // Offsets inside the picture header (marker at 0x24 in streams of this writer).
    let pih = s.windows(4).position(|w| w == [0xFF, 0x12, 0x00, 0x27]).unwrap() + 4;
    let set = |at: usize, v: &[u8]| {
        let mut m = s.clone();
        m[at..at + v.len()].copy_from_slice(v);
        m
    };
    // Slice height, significance group size, quantiser and refinement of the first precinct,
    // and an enormous payload length: none may panic or allocate in proportion to the field.
    for m in [
        set(pih + 14, &[0xFF, 0xFF]),
        set(pih + 18, &[0xFF]),
        set(first + 6, &[0xFF, 0xFF, 0xFF]),
        set(first + 9, &[0xFF, 0xFF]),
        set(first + 6, &[0x00, 0x00, 0x00]),
    ] {
        let k = kind(decode_with(&m, p.width, p.height, false));
        assert!(k == "CorruptFile" || k == "ok", "{k}");
    }
    // A header that claims a large picture with almost no data: rejected before allocating.
    let mut big = s.clone();
    big[pih + 8..pih + 10].copy_from_slice(&4000u16.to_be_bytes());
    big[pih + 10..pih + 12].copy_from_slice(&4000u16.to_be_bytes());
    assert_eq!(kind(decode_with(&big, 4000, 4000, false)), "CorruptFile");
}

#[test]
fn container_mismatches_and_small_buffers() {
    let (s, p, _) = small_stream(8);
    assert_eq!(kind(decode_with(&s, p.width + 2, p.height, false)), "CorruptFile");
    assert_eq!(kind(decode_with(&s, p.width, p.height + 4, false)), "CorruptFile");
    let mut out = vec![0u16; p.width * p.height];
    let lim = Limits::default();
    let e = decode(&s, &mut out, p.width, p.height, p.width - 1, &lim, false).unwrap_err();
    assert_eq!(e.kind(), "InvalidArgument");
    let e = decode(&s, &mut out[..p.width * p.height - 1], p.width, p.height, p.width, &lim, false).unwrap_err();
    assert_eq!(e.kind(), "InvalidArgument");
    let tight = Limits::default().with_max_samples(8);
    let e = decode(&s, &mut out, p.width, p.height, p.width, &tight, false).unwrap_err();
    assert_eq!(e.kind(), "LimitExceeded");
    // Garbage is not a codestream.
    assert_eq!(kind(decode_with(&[0, 1, 2, 3], 8, 8, false)), "UnsupportedCompression");
    assert_eq!(kind(decode_with(&[], 8, 8, false)), "UnsupportedCompression");
}

#[test]
fn trailing_bytes_after_the_end_marker_are_ignored() {
    let (mut s, p, _) = small_stream(33);
    let a = decode_with(&s, p.width, p.height, false).unwrap();
    s.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 1]);
    assert!(a == decode_with(&s, p.width, p.height, false).unwrap());
}

#[test]
fn more_than_eight_levels_are_unsupported() {
    let (mut s, p, _) = small_stream(41);
    let cdt = s.windows(4).position(|w| w == [0xFF, 0x13, 0x00, 0x0E]).unwrap() + 4;
    s[cdt + 2] = 0x91;
    assert_eq!(kind(decode_with(&s, p.width, p.height, false)), "UnsupportedCompression");
}
