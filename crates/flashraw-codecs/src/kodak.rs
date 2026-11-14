//! Kodak codecs: the "65000" block coder (DCR/DCS Pro), the RADC coder (DC40/DC50), the DC120
//! row rotation, and the YCbCr to RGB transforms of the C330/C603 compact cameras.
//!
//! Every function is pure and thread-safe. Malformed input yields an error or a partial result,
//! never a panic or an out-of-bounds access.

use flashraw_core::{Error, Result};

/// Samples per 65000 block (the last block of a row may be shorter).
pub const BLOCK_65000: usize = 256;

// ---------------------------------------------------------------------------------------------
// 65000 blocks
// ---------------------------------------------------------------------------------------------

/// Number of 4-sample groups of a block of `len` samples.
#[inline]
fn groups(len: usize) -> usize {
    len / 4 + usize::from(len % 4 != 0)
}

/// Sum of the header nibbles and whether any nibble exceeds 12.
#[inline]
fn scan_header(hdr: &[u8]) -> (usize, bool) {
    const LOW: u64 = 0x0F0F_0F0F_0F0F_0F0F;
    const THREE: u64 = 0x0303_0303_0303_0303;
    const BIT4: u64 = 0x1010_1010_1010_1010;
    let mut sum = 0usize;
    let mut raw = false;
    let mut chunks = hdr.chunks_exact(8);
    for c in &mut chunks {
        let x = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        let lo = x & LOW;
        let hi = (x >> 4) & LOW;
        raw |= ((lo + THREE) | (hi + THREE)) & BIT4 != 0;
        sum += ((lo + hi).wrapping_mul(0x0101_0101_0101_0101) >> 56) as usize;
    }
    for &b in chunks.remainder() {
        let (lo, hi) = (b & 15, b >> 4);
        raw |= lo > 12 || hi > 12;
        sum += usize::from(lo) + usize::from(hi);
    }
    (sum, raw)
}

/// Layout of one block: header bytes, total byte length, raw flag.
struct BlockInfo {
    hdr: usize,
    total: usize,
    raw: bool,
}

fn block_info(data: &[u8], pos: usize, len: usize) -> Option<BlockInfo> {
    let q = groups(len);
    let hdr = q.checked_mul(2)?;
    let end = pos.checked_add(hdr)?;
    let header = data.get(pos..end)?;
    let (sum, raw) = scan_header(header);
    let total = if raw {
        6 * (q + (q & 1))
    } else {
        let lead = if q & 1 == 1 { 16 } else { 0 };
        let refills = if sum <= lead { 0 } else { (sum - lead).div_ceil(32) };
        hdr + lead / 8 + 4 * refills
    };
    Some(BlockInfo { hdr, total, raw })
}

/// Byte length of the 65000 block of `len` samples that starts at `data[pos..]`, and whether it is
/// a raw (12-bit packed) block. `None` if the block header does not fit in `data`.
pub fn block_len_65000(data: &[u8], pos: usize, len: usize) -> Option<(usize, bool)> {
    block_info(data, pos, len).map(|b| (b.total, b.raw))
}

/// Big-endian 16-bit word at byte `i`; bytes past the end read as 0.
#[inline(always)]
fn word_be(data: &[u8], i: usize) -> u64 {
    if let Some(w) = i.checked_add(2).and_then(|e| data.get(i..e)) {
        return u64::from(u16::from_be_bytes([w[0], w[1]]));
    }
    match data.get(i) {
        Some(&b) => u64::from(b) << 8,
        None => 0,
    }
}

/// Eight absolute 12-bit samples of a raw block group starting at byte `base`.
fn raw_group(data: &[u8], base: usize) -> [u16; 8] {
    let mut w = [0u16; 6];
    for (j, v) in w.iter_mut().enumerate() {
        *v = word_be(data, base.saturating_add(2 * j)) as u16;
    }
    [
        ((w[0] >> 12) << 8) | ((w[2] >> 12) << 4) | (w[4] >> 12),
        ((w[1] >> 12) << 8) | ((w[3] >> 12) << 4) | (w[5] >> 12),
        w[0] & 0xFFF,
        w[1] & 0xFFF,
        w[2] & 0xFFF,
        w[3] & 0xFFF,
        w[4] & 0xFFF,
        w[5] & 0xFFF,
    ]
}

/// Bit stream of a difference block: 16-bit big-endian words, least significant bit first.
struct DiffBits<'a> {
    data: &'a [u8],
    wp: usize,
    acc: u64,
    nb: u32,
}

impl<'a> DiffBits<'a> {
    #[inline(always)]
    fn new(data: &'a [u8], start: usize, lead: bool) -> Self {
        let mut s = DiffBits { data, wp: start, acc: 0, nb: 0 };
        if lead {
            s.acc = word_be(data, s.wp);
            s.wp = s.wp.saturating_add(2);
            s.nb = 16;
        }
        s
    }

    /// Next signed difference of `l` bits (`l <= 15`).
    #[inline(always)]
    fn diff(&mut self, l: u32) -> i32 {
        if self.nb < l {
            self.acc |= word_be(self.data, self.wp) << self.nb;
            self.wp = self.wp.saturating_add(2);
            self.nb += 16;
        }
        let d = (self.acc & ((1u64 << l) - 1)) as i32;
        self.acc >>= l;
        self.nb -= l;
        let top = (1i32 << l) >> 1;
        if d & top == 0 { d - ((1i32 << l) - 1) } else { d }
    }
}

/// Fail when a block extends more than 3 bytes beyond the data.
#[inline]
fn check_fit(data: &[u8], pos: usize, total: usize) -> Result<()> {
    match pos.checked_add(total) {
        Some(end) if end <= data.len().saturating_add(3) => Ok(()),
        _ => Err(Error::corrupt("Kodak 65000 block is cut off")),
    }
}

fn header_cut() -> Error {
    Error::corrupt("Kodak 65000 block header is cut off")
}

/// Decode one 65000 block of `len` samples at `data[pos..]` into `out`; returns `true` for a raw
/// block (absolute samples) and `false` for a difference block.
pub fn decode_block_65000(data: &[u8], pos: usize, len: usize, out: &mut [i16]) -> Result<bool> {
    let info = block_info(data, pos, len).ok_or_else(header_cut)?;
    let q = groups(len);
    let need = 4 * (q + (q & 1));
    if out.len() < need {
        return Err(Error::InvalidArgument("Kodak 65000 block output buffer is too small".into()));
    }
    check_fit(data, pos, info.total)?;
    if info.raw {
        for (g, o) in out[..need].chunks_exact_mut(8).enumerate() {
            let v = raw_group(data, pos + 12 * g);
            for (d, s) in o.iter_mut().zip(v) {
                *d = s as i16;
            }
        }
        return Ok(true);
    }
    let hdr = &data[pos..pos + info.hdr];
    let mut bits = DiffBits::new(data, pos + info.hdr, q & 1 == 1);
    for (pair, &h) in out[..4 * q].chunks_exact_mut(2).zip(hdr) {
        pair[0] = bits.diff(u32::from(h & 15)) as i16;
        pair[1] = bits.diff(u32::from(h >> 4)) as i16;
    }
    Ok(false)
}

/// Start offset of every row of a 65000 CFA payload of `width x height` samples at `start`.
pub fn scan_65000_rows(data: &[u8], start: usize, width: usize, height: usize) -> Result<Vec<usize>> {
    let mut rows = Vec::with_capacity(height.min(1 << 16));
    let mut pos = start;
    for _ in 0..height {
        rows.push(pos);
        let mut col = 0;
        while col < width {
            let len = (width - col).min(BLOCK_65000);
            let info = block_info(data, pos, len).ok_or_else(header_cut)?;
            pos = pos.checked_add(info.total).ok_or_else(|| Error::corrupt("Kodak 65000 offset overflow"))?;
            col += len;
        }
    }
    Ok(rows)
}

/// Decode one CFA row (`out.len()` samples) of 65000 data at `data[pos..]` through `curve`;
/// returns the offset where the next row starts.
pub fn decode_65000_row(data: &[u8], pos: usize, out: &mut [u16], curve: &[u16]) -> Result<usize> {
    let mut pos = pos;
    let table = curve.get(..65535).unwrap_or(curve);
    for block in out.chunks_mut(BLOCK_65000) {
        let len = block.len();
        let info = block_info(data, pos, len).ok_or_else(header_cut)?;
        check_fit(data, pos, info.total)?;
        let q = groups(len);
        if info.raw {
            for (g, o) in block.chunks_mut(8).enumerate() {
                let v = raw_group(data, pos + 12 * g);
                for (d, &s) in o.iter_mut().zip(&v) {
                    if let Some(&c) = table.get(usize::from(s)) {
                        *d = c;
                    }
                }
            }
        } else {
            let hdr = &data[pos..pos + info.hdr];
            let mut bits = DiffBits::new(data, pos + info.hdr, q & 1 == 1);
            let mut pred = [0i32; 2];
            for (pair, &h) in block.chunks_mut(2).zip(hdr) {
                pred[0] += bits.diff(u32::from(h & 15));
                if let Some(&c) = usize::try_from(pred[0]).ok().and_then(|i| table.get(i)) {
                    pair[0] = c;
                }
                if pair.len() > 1 {
                    pred[1] += bits.diff(u32::from(h >> 4));
                    if let Some(&c) = usize::try_from(pred[1]).ok().and_then(|i| table.get(i)) {
                        pair[1] = c;
                    }
                }
            }
        }
        pos += info.total;
    }
    Ok(pos)
}

// ---------------------------------------------------------------------------------------------
// DC120
// ---------------------------------------------------------------------------------------------

/// DC120: undo the per-row rotation of one stored row.
pub fn dc120_row(src: &[u8], row: usize, out: &mut [u16]) {
    const MUL: [u128; 4] = [162, 192, 187, 92];
    const ADD: [u128; 4] = [0, 636, 424, 212];
    let n = src.len();
    if n == 0 {
        return;
    }
    let k = row % 4;
    let mut start = ((row as u128 * MUL[k] + ADD[k]) % n as u128) as usize;
    for chunk in out.chunks_mut(n) {
        let first = (n - start).min(chunk.len());
        for (d, &s) in chunk[..first].iter_mut().zip(&src[start..]) {
            *d = u16::from(s);
        }
        for (d, &s) in chunk[first..].iter_mut().zip(src) {
            *d = u16::from(s);
        }
        start = (start + chunk.len()) % n;
    }
}

// ---------------------------------------------------------------------------------------------
// YCbCr rows
// ---------------------------------------------------------------------------------------------

/// First 256 entries of `curve`, padded with the last entry (or 0 for an empty curve).
fn curve256(curve: &[u16]) -> [u16; 256] {
    let mut t = [curve.last().copied().unwrap_or(0); 256];
    let n = curve.len().min(256);
    t[..n].copy_from_slice(&curve[..n]);
    t
}

#[inline(always)]
fn ycc(t: &[u16; 256], y: u8, cb: u8, cr: u8, o: &mut [u16]) {
    let cb = i32::from(cb) - 128;
    let cr = i32::from(cr) - 128;
    let g = i32::from(y) - ((cb + cr + 2) >> 2);
    let idx = |v: i32| usize::from(v.clamp(0, 255) as u8);
    o[0] = t[idx(g + cr)];
    o[1] = t[idx(g)];
    o[2] = t[idx(g + cb)];
}

/// C330: one output row of `width` RGB pixels from a YUYV-like stored row.
pub fn c330_row(pixel: &[u8], width: usize, curve: &[u16], out: &mut [u16]) {
    let w = width.min(out.len() / 3);
    let t = curve256(curve);
    let at = |i: usize| pixel.get(i).copied().unwrap_or(0);
    // Pairs of pixels whose four bytes (Y0 Cb Y1 Cr) are all stored.
    let pairs = (w / 2).min(pixel.len() / 4);
    for (o, p) in out[..6 * pairs].chunks_exact_mut(6).zip(pixel.chunks_exact(4)) {
        let (o0, o1) = o.split_at_mut(3);
        ycc(&t, p[0], p[1], p[3], o0);
        ycc(&t, p[2], p[1], p[3], o1);
    }
    for (col, o) in out.chunks_exact_mut(3).take(w).enumerate().skip(2 * pairs) {
        let b = 4 * (col / 2);
        ycc(&t, at(2 * col), at(b + 1), at(b + 3), o);
    }
}

/// C603: one output row of `width` RGB pixels from a `3 * width`-byte record shared by two rows.
pub fn c603_row(pixel: &[u8], width: usize, odd: bool, curve: &[u16], out: &mut [u16]) {
    let w = width.min(out.len() / 3);
    let t = curve256(curve);
    let at = |i: usize| pixel.get(i).copied().unwrap_or(0);
    let ybase = if odd { width.saturating_mul(2) } else { 0 };
    // Pairs of pixels whose luma bytes and shared Cb Cr bytes are all stored.
    let pairs = match (pixel.get(ybase..), pixel.get(width..)) {
        (Some(ys), Some(cs)) => {
            let pairs = (w / 2).min(ys.len() / 2).min(cs.len() / 2);
            for ((o, y), c) in out[..6 * pairs].chunks_exact_mut(6).zip(ys.chunks_exact(2)).zip(cs.chunks_exact(2)) {
                let (o0, o1) = o.split_at_mut(3);
                ycc(&t, y[0], c[0], c[1], o0);
                ycc(&t, y[1], c[0], c[1], o1);
            }
            pairs
        }
        _ => 0,
    };
    for (col, o) in out.chunks_exact_mut(3).take(w).enumerate().skip(2 * pairs) {
        let c = width.saturating_add(2 * (col / 2));
        ycc(&t, at(ybase.saturating_add(col)), at(c), at(c.saturating_add(1)), o);
    }
}

// ---------------------------------------------------------------------------------------------
// Transfer curve
// ---------------------------------------------------------------------------------------------

/// 65536-entry transfer table with power `pwr` and linear-toe slope `ts` over `0..=imax`.
pub fn gamma_curve(pwr: f64, ts: f64, imax: u32) -> Vec<u16> {
    let (p, t) = (pwr, ts);
    let mut knee = 0.0f64;
    let mut e = 0.0f64;
    if t != 0.0 && (t - 1.0) * (p - 1.0) <= 0.0 {
        let (mut b0, mut b1) = if t < 1.0 { (1.0f64, 0.0f64) } else { (0.0, 1.0) };
        for _ in 0..48 {
            let m = (b0 + b1) / 2.0;
            knee = m;
            let hit =
                if p != 0.0 { ((m / t).powf(-p) - 1.0) / p - 1.0 / m > -1.0 } else { m / (1.0 - 1.0 / m).exp() < t };
            if hit {
                b1 = m;
            } else {
                b0 = m;
            }
        }
        if p != 0.0 {
            e = knee * (1.0 / p - 1.0);
        }
    }
    let mut out = vec![65535u16; 65536];
    let max = f64::from(imax);
    for (i, o) in out.iter_mut().enumerate() {
        let r = i as f64 / max;
        // Also stops on NaN (imax = 0): the remaining entries stay 65535.
        if r.partial_cmp(&1.0) != Some(std::cmp::Ordering::Less) {
            break;
        }
        let v = if r < knee {
            r / t
        } else if p != 0.0 {
            ((r + e) / (1.0 + e)).powf(1.0 / p)
        } else {
            ((r - 1.0) / knee).exp()
        };
        *o = (65536.0 * v) as u16;
    }
    out
}

// ---------------------------------------------------------------------------------------------
// RADC
// ---------------------------------------------------------------------------------------------

/// The 18 prefix codes as (length, symbol); each tree's entries tile the 256 eight-bit patterns.
const RADC_CODES: [&[(u8, i8)]; 18] = [
    &[(1, 1), (2, 3), (3, 4), (4, 2), (5, 7), (6, 5), (7, 6), (7, 8)],
    &[(1, 0), (2, 1), (3, 3), (4, 4), (5, 2), (6, 7), (7, 6), (8, 5), (8, 8)],
    &[(2, 1), (2, 3), (3, 0), (3, 2), (3, 4), (4, 6), (5, 5), (6, 7), (6, 8)],
    &[(2, 0), (2, 1), (2, 3), (3, 2), (4, 4), (5, 6), (6, 7), (7, 5), (7, 8)],
    &[(2, 1), (2, 4), (3, 0), (3, 2), (3, 3), (4, 7), (5, 5), (6, 6), (6, 8)],
    &[(2, 3), (3, 1), (3, 2), (3, 4), (3, 5), (3, 6), (4, 7), (5, 0), (5, 8)],
    &[(2, 3), (2, 6), (3, 0), (3, 1), (4, 4), (4, 5), (4, 7), (5, 2), (5, 8)],
    &[(2, 4), (2, 7), (3, 3), (3, 6), (4, 1), (4, 2), (4, 5), (5, 0), (5, 8)],
    &[(2, 6), (3, 1), (3, 3), (3, 5), (3, 7), (3, 8), (4, 0), (5, 2), (5, 4)],
    &[(2, 0), (2, 1), (3, 2), (3, 3), (4, 4), (4, 5), (5, 6), (5, 7), (4, 8)],
    &[(1, 0), (2, 2), (2, -2)],
    &[(1, -3), (1, 3)],
    &[(2, -17), (2, -5), (2, 5), (2, 17)],
    &[(2, -7), (2, 2), (2, 9), (2, 18)],
    &[(2, -18), (2, -9), (2, -2), (2, 7)],
    &[(2, -28), (2, 28), (3, -49), (3, -9), (3, 9), (4, 49), (5, -79), (5, 79)],
    &[(2, -1), (2, 13), (2, 26), (3, 39), (4, -16), (5, 55), (6, -37), (6, 76)],
    &[(2, -26), (2, -13), (2, 1), (3, -39), (4, 16), (5, -55), (6, -76), (6, 37)],
];

/// Peek-8 lookup: entry = length << 8 | symbol (as u8).
static RADC_LUT: [[u16; 256]; 18] = {
    let mut lut = [[0u16; 256]; 18];
    let mut t = 0;
    while t < 18 {
        let mut at = 0usize;
        let mut k = 0;
        while k < RADC_CODES[t].len() {
            let (l, s) = RADC_CODES[t][k];
            let span = 1usize << (8 - l);
            let mut j = 0;
            while j < span {
                lut[t][at + j] = ((l as u16) << 8) | (s as u8 as u16);
                j += 1;
            }
            at += span;
            k += 1;
        }
        t += 1;
    }
    lut
};

/// Output tone curve for inputs 0..=4095 (everything above maps to 16383).
static RADC_CURVE: [u16; 4096] = {
    const PTS: [(u32, u32); 5] = [(0, 0), (1280, 1344), (2320, 3616), (3328, 8000), (4095, 16383)];
    let mut t = [0u16; 4096];
    let mut seg = 0;
    while seg < 4 {
        let (x0, y0) = PTS[seg];
        let (x1, y1) = PTS[seg + 1];
        let mut c = x0;
        while c <= x1 {
            let v = (c - x0) as f32 / (x1 - x0) as f32 * (y1 - y0) as f32 + y0 as f32 + 0.5;
            t[c as usize] = v as u16;
            c += 1;
        }
        seg += 1;
    }
    t
};

/// MSB-first bit reader with zero padding past the end; counts the bits it supplied.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    nb: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0, acc: 0, nb: 0 }
    }

    #[inline(always)]
    fn fill(&mut self) {
        if self.nb < 16 {
            let w = match self.data.get(self.pos..self.pos + 4) {
                Some(b) => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
                None => {
                    let mut v = 0u32;
                    for i in 0..4 {
                        v = (v << 8) | u32::from(self.data.get(self.pos + i).copied().unwrap_or(0));
                    }
                    v
                }
            };
            self.acc = (self.acc << 32) | u64::from(w);
            self.nb += 32;
            self.pos += 4;
        }
    }

    #[inline(always)]
    fn bits(&mut self, n: u32) -> u32 {
        self.fill();
        self.nb -= n;
        ((self.acc >> self.nb) & ((1u64 << n) - 1)) as u32
    }

    #[inline(always)]
    fn sym(&mut self, tree: usize) -> i32 {
        self.fill();
        let b = ((self.acc >> (self.nb - 8)) & 0xFF) as usize;
        let e = RADC_LUT[tree][b];
        self.nb -= u32::from(e >> 8);
        i32::from(e as u8 as i8)
    }

    /// True when more than 64 bits beyond the end of the data have been supplied.
    fn overrun(&self) -> bool {
        let used = (self.pos as u64) * 8 - u64::from(self.nb);
        used > (self.data.len() as u64) * 8 + 64
    }
}

const COLS: usize = 386;
type Plane = [[i16; COLS]; 3];

#[inline(always)]
fn radc_pred<const LUMA: bool>(a: &Plane, y: usize, x: usize) -> i32 {
    if LUMA {
        (i32::from(a[y - 1][x + 1]) + 2 * i32::from(a[y - 1][x]) + i32::from(a[y][x + 1])) / 4
    } else {
        (i32::from(a[y - 1][x]) + i32::from(a[y][x + 1])) / 2
    }
}

/// Decode one pass of a plane: fills rows 1 and 2 of `a` over columns `0..h`.
#[inline(always)]
fn radc_pass<const LUMA: bool>(rd: &mut Bits, a: &mut Plane, h: usize, m: i32, lit_shift: u32) {
    const CELLS: [(usize, usize); 4] = [(1, 1), (1, 0), (2, 1), (2, 0)];
    let mut col = h as isize;
    let mut s = 1usize;
    while col > 0 {
        s = (rd.sym(s) as usize).min(8);
        match s {
            1..=7 => {
                if col < 2 {
                    break;
                }
                col -= 2;
                let q = col as usize;
                for (y, dx) in CELLS {
                    let x = q + dx;
                    let r = rd.sym(10 + s);
                    a[y][x] = (r * 16 + radc_pred::<LUMA>(a, y, x)) as i16;
                }
            }
            8 => {
                if col < 2 {
                    break;
                }
                col -= 2;
                let q = col as usize;
                for (y, dx) in CELLS {
                    let v = rd.bits(8 - lit_shift);
                    let lit = ((v << lit_shift) | (1 << (lit_shift - 1))) as i32;
                    a[y][q + dx] = (lit * m) as i16;
                }
            }
            _ => loop {
                let n = if col > 2 { rd.sym(9) + 1 } else { 1 };
                let mut k = 0;
                while k < 8 && k < n && col > 0 {
                    if col < 2 {
                        col = 0;
                        break;
                    }
                    col -= 2;
                    let q = col as usize;
                    for (y, dx) in CELLS {
                        let x = q + dx;
                        a[y][x] = radc_pred::<LUMA>(a, y, x) as i16;
                    }
                    if k & 1 == 1 {
                        let d = (rd.sym(10) * 16) as i16;
                        for (y, dx) in CELLS {
                            let c = &mut a[y][q + dx];
                            *c = c.wrapping_add(d);
                        }
                    }
                    k += 1;
                }
                if n != 9 {
                    break;
                }
            },
        }
    }
}

/// Scale the three working rows from multiplier `last` to `m`.
fn radc_rescale(a: &mut Plane, h: usize, last: i32, m: i32) {
    let v = (((1i64 << 24) / i64::from(last) + 2047) >> 12) * i64::from(m);
    let s = if v > 65564 { 10 } else { 12 };
    let v = v << (12 - s);
    let add = (1i64 << (s - 1)) - 1;
    for row in a.iter_mut() {
        for e in &mut row[..=h] {
            let t = (i64::from(*e) * v + add).min(i64::from(i32::MAX));
            *e = (t >> s) as i16;
        }
    }
}

/// Write the two lines of a decoded pass into the mosaic. `rows[y]`/`offs[y]` give the mosaic row
/// and first column of line `y`; values are `A * 16 / m`, floored at zero.
fn radc_emit(a: &Plane, h: usize, m: i32, out: &mut [u16], width: usize, rows: [usize; 2], offs: [usize; 2]) {
    let recip = (1u64 << 40) / m as u64 + 1;
    for y in 0..2 {
        let row = &mut out[rows[y] * width..(rows[y] + 1) * width];
        for (d, &v) in row[offs[y]..].iter_mut().step_by(2).zip(&a[y + 1][..h]) {
            *d = if v < 0 { 0 } else { ((v as u64 * 16 * recip) >> 40) as u16 };
        }
    }
}

/// Replace the chroma positions of rows `r0..r0+4` by their interpolation and apply the curve.
fn radc_finish_band(out: &mut [u16], width: usize, r0: usize) {
    for y in r0..r0 + 4 {
        let row = &mut out[y * width..(y + 1) * width];
        for x in ((y + 1) & 1..width).step_by(2) {
            let a = if x > 0 { x - 1 } else { x + 1 };
            let b = if x + 1 < width { x + 1 } else { x - 1 };
            let v = (i32::from(row[x]) - 2048) * 2 + ((i32::from(row[a]) + i32::from(row[b])) >> 1);
            row[x] = v.max(0) as u16;
        }
        for v in row.iter_mut() {
            *v = RADC_CURVE[usize::from(*v).min(4095)];
        }
    }
}

/// RADC (DC40/DC50): decode `height x width` samples into `out` (row stride `width`).
pub fn decode_radc(data: &[u8], width: usize, height: usize, cbpp: u32, out: &mut [u16]) -> Result<()> {
    if !(4..=768).contains(&width) || height > 512 || height % 4 != 0 || out.len() < width * height {
        return Err(Error::corrupt("Kodak RADC: unsupported geometry"));
    }
    let h = width / 2;
    let lit_shift = if cbpp == 243 { 2 } else { 3 };
    let mut rd = Bits::new(data);
    let mut planes: [Plane; 3] = [[[2048; COLS]; 3]; 3];
    let mut last = [16i32; 3];
    for r0 in (0..height).step_by(4) {
        let mut m = [0i32; 3];
        for v in &mut m {
            *v = rd.bits(6) as i32;
        }
        if m.contains(&0) {
            return Err(Error::corrupt("Kodak RADC: zero multiplier"));
        }
        for c in 0..3 {
            let a = &mut planes[c];
            radc_rescale(a, h, last[c], m[c]);
            last[c] = m[c];
            let passes = if c == 0 { 2 } else { 1 };
            for r in 0..passes {
                a[1][h] = (m[c] << 7) as i16;
                a[2][h] = (m[c] << 7) as i16;
                if c == 0 {
                    radc_pass::<true>(&mut rd, a, h, m[c], lit_shift);
                    radc_emit(a, h, m[c], out, width, [r0 + 2 * r, r0 + 2 * r + 1], [0, 1]);
                    for j in (1..=h).rev() {
                        a[0][j] = a[2][j - 1];
                    }
                } else {
                    radc_pass::<false>(&mut rd, a, h, m[c], lit_shift);
                    let (rows, offs) = if c == 1 { ([r0, r0 + 2], [1, 1]) } else { ([r0 + 1, r0 + 3], [0, 0]) };
                    radc_emit(a, h, m[c], out, width, rows, offs);
                    let (top, rest) = a.split_at_mut(1);
                    top[0][..=h].copy_from_slice(&rest[1][..=h]);
                }
                if rd.overrun() {
                    return Err(Error::corrupt("Kodak RADC: data ends early"));
                }
            }
        }
        radc_finish_band(out, width, r0);
    }
    Ok(())
}
