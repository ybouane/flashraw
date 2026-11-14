//! Fujifilm compressed RAF codec (lossless and lossy; X-Trans and Bayer).
//!
//! The payload is a 16-byte header, a table of stripe sizes, (lossy only) one table of
//! quantisation bases per stripe, and the entropy-coded stripes. A stripe is a vertical band
//! of up to 768 columns that is an independent bit stream. Inside a stripe the sensor is cut
//! into lines of 6 rows; the samples of a line are regrouped into colour rows (3 red, 6 green,
//! 3 blue), which are coded in six passes of two rows each with an adaptive Golomb-Rice coder
//! whose context is a quantised gradient of the neighbouring samples.
//!
//! Layout used here: one small arena per stripe holds, for each colour, the two rows that
//! precede the current line followed by the rows of the line itself (so the neighbours of
//! a row are always the two rows just before it). Every row carries one guard sample on each
//! side. After a line the last two rows of each colour move to the front of its region.

use flashraw_core::{Error, Rect, Result, par};

/// Size of the stream header in bytes.
pub const HEADER_SIZE: usize = 16;

/// Parsed stream header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FujiHeader {
    pub lossless: bool,
    /// X-Trans (raw type 16) or Bayer (raw type 0).
    pub xtrans: bool,
    pub bits: u32,
    pub height: u32,
    pub rounded_width: u32,
    pub width: u32,
    pub block_size: u32,
    pub blocks: u32,
    /// Number of 6-row lines.
    pub lines: u32,
}

const STRIPE_W: u32 = 768;
/// Most stripes accepted in one image (our own bound: 12,288 columns).
const MAX_STRIPES: u32 = 16;

fn be16(d: &[u8], at: usize) -> u32 {
    u32::from(u16::from_be_bytes([d[at], d[at + 1]]))
}

impl FujiHeader {
    /// Parse and validate the 16-byte header.
    pub fn parse(d: &[u8]) -> Result<Self> {
        if d.len() < HEADER_SIZE {
            return Err(Error::corrupt("truncated Fuji compressed header"));
        }
        let h = FujiHeader {
            lossless: d[2] == 1,
            xtrans: d[3] == 16,
            bits: u32::from(d[4]),
            height: be16(d, 5),
            rounded_width: be16(d, 7),
            width: be16(d, 9),
            block_size: be16(d, 11),
            blocks: u32::from(d[13]),
            lines: be16(d, 14),
        };
        // Extents are bounded by the 16-bit header fields themselves and by the stripe count
        // (at most `MAX_STRIPES` stripes of `STRIPE_W` columns, which also bounds `width`);
        // the size of the output buffer is limited by the caller before decoding.
        let ok = be16(d, 0) == 0x4953
            && d[2] <= 1
            && (d[3] == 0 || d[3] == 16)
            && matches!(h.bits, 12 | 14 | 16)
            && h.height >= 6
            && h.height % 6 == 0
            && h.block_size == STRIPE_W
            && h.width >= 0x300
            && h.width % 24 == 0
            && h.rounded_width >= STRIPE_W
            && h.rounded_width % STRIPE_W == 0
            && h.rounded_width.checked_sub(h.width).is_some_and(|d| d < STRIPE_W)
            && (1..=MAX_STRIPES).contains(&h.blocks)
            && h.blocks == h.rounded_width / STRIPE_W
            && h.lines >= 1
            && h.lines == h.height / 6;
        if ok { Ok(h) } else { Err(Error::corrupt("invalid Fuji compressed header")) }
    }

    /// Width in pixels of block `i` (the last block may be narrower).
    pub fn block_width(&self, i: u32) -> u32 {
        if i + 1 >= self.blocks { self.width.saturating_sub(self.block_size * i) } else { self.block_size }
    }
}

/// Colour (0 = red, 1 = green, 2 = blue) of every position of a 6x6 tile anchored at
/// the buffer origin.
pub type ColorTile = [[u8; 6]; 6];

/// A parsed compressed payload, ready to decode.
pub struct FujiCompressed<'a> {
    pub header: FujiHeader,
    data: &'a [u8],
    /// Entropy data of each stripe as (offset from the start of `data`, length), clamped to `data`.
    spans: Vec<(usize, usize)>,
    /// Quantisation base per line, one slice per stripe (lossy streams only).
    qbase: Vec<&'a [u8]>,
}

const fn ceil16(n: usize) -> usize {
    (n + 15) & !15
}

impl<'a> FujiCompressed<'a> {
    /// `data` starts at the stream header.
    pub fn new(data: &'a [u8]) -> Result<Self> {
        let header = FujiHeader::parse(data)?;
        let body = &data[HEADER_SIZE..];
        let n = header.blocks as usize;
        if body.len() < 4 * n {
            return Err(Error::corrupt("truncated Fuji block size table"));
        }
        let sizes: Vec<u64> =
            body[..4 * n].chunks_exact(4).map(|c| u64::from(u32::from_be_bytes([c[0], c[1], c[2], c[3]]))).collect();
        let table_end = ceil16(4 * n);
        let lines = header.lines as usize;
        let mut qbase = Vec::new();
        let mut start = table_end as u64;
        if !header.lossless {
            let pitch = ceil16(lines);
            for b in 0..n {
                let from = table_end + b * pitch;
                match body.get(from..from + lines) {
                    Some(t) => qbase.push(t),
                    None => return Err(Error::corrupt("truncated Fuji lossy quantisation table")),
                }
            }
            start += (n * pitch) as u64;
        }
        let len = body.len() as u64;
        let mut spans = Vec::with_capacity(n);
        for size in sizes {
            let s = start.min(len);
            let e = start.saturating_add(size).min(len);
            spans.push((HEADER_SIZE + s as usize, (e - s) as usize));
            start = start.saturating_add(size);
        }
        Ok(FujiCompressed { header, data, spans, qbase })
    }

    /// Byte ranges (offset from `base`, length) of each block's entropy data.
    pub fn block_ranges(&self, base: &'a [u8]) -> Vec<(usize, usize)> {
        // `base` is the buffer given to `new`; offsets are relative to its first byte.
        let _ = base;
        self.spans.clone()
    }

    /// Decode the whole image into `out` (`width * height`, row-major).
    pub fn decode(&self, out: &mut [u16], width: u32, height: u32, colors: &ColorTile, parallel: bool) -> Result<()> {
        if width != self.header.width || height != self.header.height {
            return Err(Error::InvalidArgument("Fuji compressed output size does not match the stream".into()));
        }
        self.decode_region(out, Rect { x: 0, y: 0, width, height }, colors, parallel)
    }

    /// Decode only `region` (image coordinates) into `out` (`region.width * region.height`).
    pub fn decode_region(&self, out: &mut [u16], region: Rect, colors: &ColorTile, parallel: bool) -> Result<()> {
        let hd = &self.header;
        let bad = || Error::InvalidArgument("Fuji compressed region outside the image or buffer too small".into());
        let (rw, rh) = (u64::from(region.width), u64::from(region.height));
        if (out.len() as u64) < rw * rh
            || u64::from(region.x) + rw > u64::from(hd.width)
            || u64::from(region.y) + rh > u64::from(hd.height)
        {
            return Err(bad());
        }
        if rw == 0 || rh == 0 {
            return Ok(());
        }
        let lw = line_width(hd.xtrans);
        let map = GatherMap::new(colors, hd.xtrans, lw);
        let need_lines = (hd.lines as usize).min((region.y as usize + region.height as usize).div_ceil(6));
        let grid = par::GridWriter::new(
            out,
            region.width,
            region.height,
            1,
            (region.x, region.y),
            hd.block_size,
            hd.height,
            hd.width,
            hd.height,
        )
        .ok_or_else(bad)?;
        let failed = std::sync::atomic::AtomicBool::new(false);
        par::for_each_index(hd.blocks as usize, parallel, |b| {
            let Some(mut tile) = grid.claim(b as u32, 0) else { return };
            let (off, len) = self.spans[b];
            let job = Stripe { hdr: hd, data: &self.data[off..off + len], q: self.qbase.get(b).copied() };
            if !job.run(need_lines, region.y as usize, &map, &mut tile) {
                failed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
        if failed.into_inner() { Err(Error::corrupt("truncated Fuji compressed block")) } else { Ok(()) }
    }
}

// ---------------------------------------------------------------------------------------------
// Geometry

const fn line_width(xtrans: bool) -> usize {
    if xtrans { 512 } else { 384 }
}

/// Rows kept per colour: two history rows plus the rows of one line.
const SLOTS_R: usize = 5;
const SLOTS_G: usize = 8;
const SLOTS_B: usize = 5;

/// Where each sample of a 6 x 768 band of the image lives in the arena.
struct GatherMap {
    at: Vec<u16>,
    /// For image row `r` of a line and column phase `c` (column mod 6): the arena index of the sample
    /// at column `c`; columns `c + 6g` follow at steps of `step * g`.
    phase: [[usize; 6]; 6],
    step: usize,
}

impl GatherMap {
    fn new(tile: &ColorTile, xtrans: bool, lw: usize) -> Self {
        const FOLD: [usize; 6] = [0, 1, 1, 2, 3, 3];
        let rs = lw + 2;
        let base = [0usize, SLOTS_R * rs, (SLOTS_R + SLOTS_G) * rs];
        let mut at = Vec::with_capacity(6 * STRIPE_W as usize);
        for (r, trow) in tile.iter().enumerate() {
            for p in 0..STRIPE_W as usize {
                let (class, row) = match trow[p % 6] {
                    0 => (0, r / 2),
                    2 => (2, r / 2),
                    _ => (1, r),
                };
                let i = if xtrans { 4 * (p / 6) + FOLD[p % 6] } else { p / 2 };
                at.push((base[class] + (row + 2) * rs + 1 + i) as u16);
            }
        }
        let mut phase = [[0usize; 6]; 6];
        for r in 0..6 {
            for (c, ph) in phase[r].iter_mut().enumerate() {
                *ph = usize::from(at[r * STRIPE_W as usize + c]);
            }
        }
        GatherMap { at, phase, step: if xtrans { 4 } else { 3 } }
    }
}

// ---------------------------------------------------------------------------------------------
// Bit input

/// MSB-first reader over one stripe; bits past the end read as zero.
#[derive(Clone, Copy)]
struct Bits<'a> {
    data: &'a [u8],
    /// Next byte to load (may run past the end: virtual zero bytes).
    pos: usize,
    /// Left-aligned bit cache.
    cache: u64,
    /// Whole bits of `cache` that count as loaded.
    have: u32,
    /// Set when a run of zeros left the data far behind.
    lost: bool,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0, cache: 0, have: 0, lost: false }
    }

    #[inline(always)]
    fn refill(&mut self) {
        if let Some(c) = self.data.get(self.pos..).and_then(<[u8]>::first_chunk::<8>) {
            let w = u64::from_be_bytes(*c);
            self.cache |= w >> self.have;
            self.pos += ((63 - self.have) >> 3) as usize;
            self.have |= 56;
        } else {
            *self = self.refilled_tail();
        }
    }

    /// End-of-data refill. Takes and returns the reader by value so that a reader held in
    /// locals stays in registers in the hot loops.
    #[cold]
    #[inline(never)]
    fn refilled_tail(mut self) -> Self {
        while self.have <= 56 {
            let b = self.data.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.cache |= u64::from(b) << (56 - self.have);
            self.have += 8;
        }
        self
    }

    /// Bits consumed since the start of the data.
    #[inline(always)]
    fn consumed(&self) -> u64 {
        self.pos as u64 * 8 - u64::from(self.have)
    }

    #[inline(always)]
    fn take(&mut self, n: u32) -> u32 {
        if self.have < 32 {
            self.refill();
        }
        let v = (self.cache >> 1 >> (63 - n)) as u32;
        self.cache <<= n;
        self.have -= n;
        v
    }

    /// Number of zero bits before the next one bit; the one bit is consumed.
    fn zeros(&mut self) -> u32 {
        let mut z = 0u32;
        loop {
            if self.have < 32 {
                self.refill();
            }
            let lz = self.cache.leading_zeros();
            if lz < self.have {
                self.cache <<= lz + 1;
                self.have -= lz + 1;
                return z + lz;
            }
            z += self.have;
            self.cache = 0;
            self.have = 0;
            if self.pos > self.data.len() + 8 {
                self.lost = true;
                return 1 << 20;
            }
        }
    }

    /// One coded residual: unary prefix, then `k` low bits or, after a prefix of at least `zmax`
    /// zeros, a raw field of `rb` bits plus one.
    #[inline(always)]
    fn code(&mut self, k: u32, zfast: u32, zmax: u32, rb: u32) -> u32 {
        // Always topped up to 56+ bits, so any prefix below FAST_ZEROS plus up to 15 more bits is present.
        self.refill();
        let lz = self.cache.leading_zeros();
        if lz < zfast {
            let used = lz + 1 + k;
            let low = (self.cache << (lz + 1) >> 1 >> (63 - k)) as u32;
            self.cache <<= used;
            self.have -= used;
            (lz << k) | low
        } else {
            let (next, v) = self.code_slow(k, zmax, rb);
            *self = next;
            v
        }
    }

    #[cold]
    #[inline(never)]
    fn code_slow(mut self, k: u32, zmax: u32, rb: u32) -> (Self, u32) {
        let z = self.zeros();
        let v = if z < zmax { (z << k) | self.take(k) } else { self.take(rb) + 1 };
        (self, v)
    }
}

// ---------------------------------------------------------------------------------------------
// Quantisation tables and statistics

/// Level thresholds and escape geometry for one quantisation base.
/// Longest unary prefix handled without checking the number of buffered bits (40 + 1 + 15 = 56).
const FAST_ZEROS: u32 = 41;

/// Size of a level table; the index is masked to it.
const LEVELS: usize = 8192;

struct Quant {
    /// Level of a difference `d` clamped to `[-a3, a3]`, stored at `d + a3`.
    level: Box<[i8; LEVELS]>,
    a3: i32,
    /// Base `q`, its step `2q + 1`, `V * step`, and the legal range edges `-q` and `q + M`.
    q: i32,
    step: i32,
    wrap: i32,
    lo: i32,
    hi: i32,
    /// Raw escape field width and the unary length that triggers it.
    rb: u32,
    zmax: u32,
    /// `zmax` capped so that the prefix and the low bits are always buffered.
    zfast: u32,
    /// Initial statistic sum.
    d0: u32,
}

impl Quant {
    fn new(q: u32, bits: u32) -> Self {
        let m = (1i32 << bits) - 1;
        let q = q as i32;
        let (a0, a1, a2, a3) = (q, 3 * q + 18, 5 * q + 67, 7 * q + 276);
        let mut level = Box::new([0i8; LEVELS]);
        for d in -a3..=a3 {
            let pos = (d > a0) as i32 + (d >= a1) as i32 + (d >= a2) as i32 + (d >= a3) as i32;
            let neg = (d < -a0) as i32 + (d <= -a1) as i32 + (d <= -a2) as i32 + (d <= -a3) as i32;
            level[(d + a3) as usize] = (pos - neg) as i8;
        }
        let step = 2 * q + 1;
        let v = (m + 2 * q) / step + 1;
        let rb = if v <= 1 { 0 } else { 32 - (v as u32 - 1).leading_zeros() };
        Quant {
            level,
            a3,
            q,
            step,
            wrap: v * step,
            lo: -q,
            hi: q + m,
            rb,
            zmax: 4 * bits - rb - 1,
            zfast: (4 * bits - rb - 1).min(FAST_ZEROS),
            d0: ((v as u32 + 32) >> 6).max(2),
        }
    }

    #[inline(always)]
    fn lev(&self, d: i32) -> i32 {
        i32::from(self.level[(d.clamp(-self.a3, self.a3) + self.a3) as usize & (LEVELS - 1)])
    }
}

/// Adaptive Rice statistic: running sum of magnitudes over `n` samples, with the parameter cached.
#[derive(Clone, Copy)]
struct Stat {
    sum: u32,
    n: u16,
    k: u16,
}

/// Per window length `n` (1..=64): `ceil(2^32 / n)`, so that dividing by `n` is a multiplication, and
/// the largest numerator worth dividing, `32767 * n + n - 1` (anything above gives a quotient of
/// 2^15 or more, which is the Rice cap).
const WINDOW: [(u64, u32); 65] = {
    let mut t = [(0u64, 0u32); 65];
    let mut n = 1;
    while n <= 64 {
        t[n] = ((1u64 << 32).div_ceil(n as u64), 32767 * n as u32 + n as u32 - 1);
        n += 1;
    }
    t
};

/// Rice parameter of a window: the smallest `k` with `(n << k) >= max(sum, 1)`, capped at 15.
///
/// That `k` is the bit length of `(sum - 1) / n` (rounded down), and the reciprocal product is
/// exact for numerators below 2^26.
#[inline(always)]
fn rice_param(sum: u32, n: u32) -> u32 {
    let (recip, cap) = WINDOW[n as usize];
    let x = (sum - u32::from(sum != 0)).min(cap);
    let q = (u64::from(x) * recip) >> 32;
    32 - (q as u32).leading_zeros()
}

impl Stat {
    fn fresh(d0: u32) -> Self {
        Stat { sum: d0, n: 1, k: rice_param(d0, 1) as u16 }
    }

    /// Account for a residual of magnitude `mag`.
    #[inline(always)]
    fn add(&mut self, mag: u32) {
        let mut sum = self.sum.wrapping_add(mag);
        let mut n = u32::from(self.n);
        if n == 64 {
            // The window is full: halve it.
            sum >>= 1;
            n = 32;
        }
        n += 1;
        self.sum = sum;
        self.n = n as u16;
        self.k = rice_param(sum, n) as u16;
    }
}

/// Statistics of one (parity, pass set) group: 41 main contexts then 3 x 5 flat-area contexts.
const MAIN_CTX: usize = 41;
const FLAT_BASE: [usize; 4] = [41, 46, 51, 0];
const FLAT_CAP: [u32; 4] = [4, 4, 4, 40];
const FLAT_W: [i32; 4] = [3, 3, 3, 9];
type Group = [Stat; MAIN_CTX + 15];

/// Tables in force for a line: flat-area tables 0..=2 and the main table (index 3).
struct Tables<'t> {
    sel: [&'t Quant; 4],
    /// Activity ceilings below which flat table m is used (`-1` when the main base is too small).
    ceil: [i32; 3],
    maxv: i32,
}

// ---------------------------------------------------------------------------------------------
// Sample coding

/// Code one sample given its prediction and context inputs.
#[inline(always)]
fn sample<const LOSSY: bool>(
    br: &mut Bits,
    grp: &mut Group,
    t: &Tables,
    interp: i32,
    v1: i32,
    v2: i32,
    flat: i32,
) -> u16 {
    let mut m = 3;
    if LOSSY {
        m = if flat <= t.ceil[2] { 2 } else { m };
        m = if flat <= t.ceil[1] { 1 } else { m };
        m = if flat <= t.ceil[0] { 0 } else { m };
    }
    let qt = t.sel[m];
    let grad = FLAT_W[m] * qt.lev(v1) + qt.lev(v2);
    let idx = FLAT_BASE[m] + (grad.unsigned_abs().min(FLAT_CAP[m])) as usize;
    let st = &mut grp[idx];
    let code = br.code(u32::from(st.k), qt.zfast, qt.zmax, qt.rb);
    let r = ((code >> 1) as i32) ^ -((code & 1) as i32);
    st.add((code + 1) >> 1);
    let sg = grad >> 31;
    let delta = ((r ^ sg) - sg).wrapping_mul(qt.step);
    let v = interp.wrapping_add(delta);
    if (v as u32) <= t.maxv as u32 {
        return v as u16;
    }
    wrapped(v, qt, t.maxv)
}

/// Fold a reconstructed value outside `0..=maxv` back into range (at most one wrap, then clamp).
#[cold]
#[inline(never)]
fn wrapped(v: i32, qt: &Quant, maxv: i32) -> u16 {
    let v = if v < qt.lo {
        v + qt.wrap
    } else if v > qt.hi {
        v - qt.wrap
    } else {
        v
    };
    v.clamp(0, maxv) as u16
}

/// Prediction and contexts of an even position from the two rows above (storage index `p`).
#[inline(always)]
fn even_inputs<const RS: usize>(n1: &[u16; RS], n2: &[u16; RS], p: usize) -> (i32, i32, i32, i32) {
    let above = i32::from(n1[p]);
    let above_left = i32::from(n1[p - 1]);
    let above_right = i32::from(n1[p + 1]);
    let two_above = i32::from(n2[p]);
    let (dev_left, dev_two, dev_right) =
        ((above_left - above).abs(), (two_above - above).abs(), (above_right - above).abs());
    // Drop the neighbour that deviates most from `above` (ties drop `two_above`); selects, not branches.
    let drop_left = (dev_left > dev_two) & (dev_left > dev_right);
    let drop_right = (dev_right > dev_left) & (dev_right > dev_two);
    let mut dropped = two_above;
    dropped = if drop_right { above_right } else { dropped };
    dropped = if drop_left { above_left } else { dropped };
    (
        (two_above + above_left + above_right - dropped + 2 * above) >> 2,
        above - two_above,
        above_left - above,
        dev_two + dev_left,
    )
}

/// Prediction and contexts of an odd position; `w` is the row being coded.
#[inline(always)]
fn odd_inputs<const RS: usize>(n1: &[u16; RS], w: &[u16; RS], p: usize) -> (i32, i32, i32, i32) {
    let above = i32::from(n1[p]);
    let above_left = i32::from(n1[p - 1]);
    let above_right = i32::from(n1[p + 1]);
    let left = i32::from(w[p - 1]);
    let right = i32::from(w[p + 1]);
    let (x, y) = (above - above_left, above - above_right);
    // `above` is a strict local extremum of its row neighbourhood when x and y are non-zero with equal signs.
    let ext = ((x ^ y) >= 0) & (x != 0) & (y != 0);
    let interp = if ext { (left + right + 2 * above) >> 2 } else { (left + right) >> 1 };
    (interp, x, above_left - left, x.abs() + (above_left - left).abs())
}

/// Skip rules of X-Trans even positions.
const KEEP: u8 = 0;
const SKIP_ALL: u8 = 1;
const SKIP_EVEN_STEP: u8 = 2; // i % 4 == 0
const SKIP_ODD_STEP: u8 = 3; // i % 4 == 2

/// Rows of one colour that a pass works on: the two before, and the one being coded.
struct Rows<'r, const RS: usize> {
    n2: &'r [u16; RS],
    n1: &'r [u16; RS],
    cur: &'r mut [u16; RS],
}

/// Hand out the neighbours and the writable row for row `j` of a colour region.
fn rows_of<const RS: usize>(region: &mut [u16], j: usize) -> Rows<'_, RS> {
    let (head, tail) = region.split_at_mut((j + 2) * RS);
    let cur: &mut [u16; RS] = (&mut tail[..RS]).try_into().unwrap();
    let n2: &[u16; RS] = (&head[j * RS..(j + 1) * RS]).try_into().unwrap();
    let n1: &[u16; RS] = (&head[(j + 1) * RS..]).try_into().unwrap();
    // Guards: the neighbours of the row's ends are the ends of the row above.
    cur[0] = n1[1];
    cur[RS - 1] = n1[RS - 2];
    Rows { n2, n1, cur }
}

#[inline(always)]
fn even_step<const LOSSY: bool, const SKIP: u8, const RS: usize>(
    br: &mut Bits,
    grp: &mut Group,
    t: &Tables,
    row: &mut Rows<RS>,
    s: usize,
) {
    let p = 2 * s + 1;
    let (interp, v1, v2, flat) = even_inputs(row.n1, row.n2, p);
    let skip = match SKIP {
        KEEP => false,
        SKIP_ALL => true,
        SKIP_EVEN_STEP => s & 1 == 0,
        _ => s & 1 == 1,
    };
    row.cur[p] = if skip { interp as u16 } else { sample::<LOSSY>(br, grp, t, interp, v1, v2, flat) };
}

#[inline(always)]
fn odd_step<const LOSSY: bool, const RS: usize>(
    br: &mut Bits,
    grp: &mut Group,
    t: &Tables,
    row: &mut Rows<RS>,
    s: usize,
) {
    let p = 2 * s - 6;
    let (interp, v1, v2, flat) = odd_inputs(row.n1, row.cur, p);
    row.cur[p] = sample::<LOSSY>(br, grp, t, interp, v1, v2, flat);
}

/// One pass: rows `a` then `b` at every position, odd positions trailing the even ones by 4 steps.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn pass<const LOSSY: bool, const SA: u8, const SB: u8, const RS: usize>(
    br: &mut Bits,
    ge: &mut Group,
    go: &mut Group,
    t: &Tables,
    a: &mut Rows<RS>,
    b: &mut Rows<RS>,
) {
    let half = RS / 2 - 1;
    for s in 0..4 {
        even_step::<LOSSY, SA, RS>(br, ge, t, a, s);
        even_step::<LOSSY, SB, RS>(br, ge, t, b, s);
    }
    for s in 4..half {
        even_step::<LOSSY, SA, RS>(br, ge, t, a, s);
        even_step::<LOSSY, SB, RS>(br, ge, t, b, s);
        odd_step::<LOSSY, RS>(br, go, t, a, s);
        odd_step::<LOSSY, RS>(br, go, t, b, s);
    }
    for s in half..half + 4 {
        odd_step::<LOSSY, RS>(br, go, t, a, s);
        odd_step::<LOSSY, RS>(br, go, t, b, s);
    }
}

// ---------------------------------------------------------------------------------------------
// Stripe decoding

struct Stripe<'s> {
    hdr: &'s FujiHeader,
    data: &'s [u8],
    q: Option<&'s [u8]>,
}

/// `[parity][pass set]` statistics, parity 0 = even positions.
type Groups = [[Group; 3]; 2];

impl Stripe<'_> {
    /// Decode the first `need` lines, writing the rows `row0..` of the tile; false if the data ran out.
    fn run(&self, need: usize, row0: usize, map: &GatherMap, tile: &mut par::TileMut<u16>) -> bool {
        match (self.hdr.xtrans, !self.hdr.lossless) {
            (false, false) => self.run_as::<false, false, 386>(need, row0, map, tile),
            (false, true) => self.run_as::<false, true, 386>(need, row0, map, tile),
            (true, false) => self.run_as::<true, false, 514>(need, row0, map, tile),
            (true, true) => self.run_as::<true, true, 514>(need, row0, map, tile),
        }
    }

    /// Picks the build of the decoding loop that suits the CPU.
    fn run_as<const XT: bool, const LOSSY: bool, const RS: usize>(
        &self,
        need: usize,
        row0: usize,
        map: &GatherMap,
        tile: &mut par::TileMut<u16>,
    ) -> bool {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("bmi1")
            && std::arch::is_x86_feature_detected!("bmi2")
            && std::arch::is_x86_feature_detected!("lzcnt")
        {
            // SAFETY: the CPU features the function is compiled for were just detected.
            return unsafe { self.run_bmi::<XT, LOSSY, RS>(need, row0, map, tile) };
        }
        self.run_body::<XT, LOSSY, RS>(need, row0, map, tile)
    }

    /// The decoding loop compiled with single-instruction variable shifts and leading-zero count.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "bmi1,bmi2,lzcnt")]
    unsafe fn run_bmi<const XT: bool, const LOSSY: bool, const RS: usize>(
        &self,
        need: usize,
        row0: usize,
        map: &GatherMap,
        tile: &mut par::TileMut<u16>,
    ) -> bool {
        self.run_body::<XT, LOSSY, RS>(need, row0, map, tile)
    }

    #[inline(always)]
    fn run_body<const XT: bool, const LOSSY: bool, const RS: usize>(
        &self,
        need: usize,
        row0: usize,
        map: &GatherMap,
        tile: &mut par::TileMut<u16>,
    ) -> bool {
        let bits = self.hdr.bits;
        let rs = RS;
        let maxv = (1i32 << bits) - 1;
        let limit = self.data.len() as u64 * 8 + 7;
        let (width, nrows) = tile.size();
        let skip_x = tile.skip_x as usize;

        let flat_q = [Quant::new(0, bits), Quant::new(1, bits), Quant::new(2, bits)];
        // Main tables for every base used by the lines to decode.
        let mut cache: Vec<Option<Quant>> = Vec::new();
        let qline = |line: usize| usize::from(self.q.and_then(|t| t.get(line).copied()).unwrap_or(0));
        if LOSSY {
            cache.resize_with(256, || None);
            for line in 0..need {
                let q = qline(line);
                if cache[q].is_none() {
                    cache[q] = Some(Quant::new(q as u32, bits));
                }
            }
        }
        let mut br = Bits::new(self.data);
        let mut groups: Groups = [[[Stat::fresh(2); MAIN_CTX + 15]; 3]; 2];
        let mut arena = vec![0u16; (SLOTS_R + SLOTS_G + SLOTS_B) * rs];
        let init_main = |g: &mut Group, d0: u32| {
            for s in &mut g[..MAIN_CTX] {
                *s = Stat::fresh(d0);
            }
        };
        for g in groups.iter_mut().flatten() {
            init_main(g, flat_q[0].d0);
            if LOSSY {
                for (m, fq) in flat_q.iter().enumerate() {
                    for s in &mut g[FLAT_BASE[m]..FLAT_BASE[m] + 5] {
                        *s = Stat::fresh(fq.d0);
                    }
                }
            }
        }

        let mut prev_q = usize::MAX;
        let mut main_ref: &Quant = &flat_q[0];
        for line in 0..need {
            if LOSSY {
                let q = qline(line);
                if prev_q != q {
                    let e = cache[q].as_ref().unwrap();
                    main_ref = e;
                    for g in groups.iter_mut().flatten() {
                        init_main(g, e.d0);
                    }
                    prev_q = q;
                }
            }
            let tabs = Tables {
                sel: [&flat_q[0], &flat_q[1], &flat_q[2], main_ref],
                ceil: if LOSSY {
                    let q = main_ref.q;
                    [if q >= 1 { 5 } else { -1 }, if q >= 2 { 6 } else { -1 }, if q >= 3 { 7 } else { -1 }]
                } else {
                    [-1; 3]
                },
                maxv,
            };
            code_line::<XT, LOSSY, RS>(&mut br, &mut groups, &mut arena, &tabs);
            if br.lost || br.consumed() > limit {
                return false;
            }
            // Gather the six image rows of this line.
            for r in 0..6 {
                let y = line * 6 + r;
                if y < row0 || y - row0 >= nrows {
                    continue;
                }
                let dst = tile.row(y - row0);
                if skip_x == 0 {
                    // Whole stripe: each column phase reads one colour row at a fixed stride.
                    for (c, &first) in map.phase[r].iter().enumerate() {
                        let src = &arena[first..];
                        for (g, o) in dst.iter_mut().skip(c).step_by(6).enumerate() {
                            *o = src[map.step * g];
                        }
                    }
                } else {
                    let src = &map.at[r * STRIPE_W as usize + skip_x..r * STRIPE_W as usize + skip_x + width];
                    for (o, &i) in dst.iter_mut().zip(src) {
                        *o = arena[usize::from(i)];
                    }
                }
            }
            // Carry the last two rows of each colour to the front.
            let (r, rest) = arena.split_at_mut(SLOTS_R * rs);
            let (g, b) = rest.split_at_mut(SLOTS_G * rs);
            r.copy_within((SLOTS_R - 2) * rs.., 0);
            g.copy_within((SLOTS_G - 2) * rs.., 0);
            b.copy_within((SLOTS_B - 2) * rs.., 0);
        }
        true
    }
}

/// The six passes of one line.
#[inline(always)]
fn code_line<const XT: bool, const LOSSY: bool, const RS: usize>(
    br: &mut Bits,
    groups: &mut Groups,
    arena: &mut [u16],
    t: &Tables,
) {
    let rs = RS;
    let (reg_r, rest) = arena.split_at_mut(SLOTS_R * rs);
    let (reg_g, reg_b) = rest.split_at_mut(SLOTS_G * rs);
    let [ge, go] = groups;

    macro_rules! one {
        ($g:expr, $sa:expr, $sb:expr, $a:expr, $b:expr) => {{
            let (mut a, mut b) = ($a, $b);
            let (ge, go) = (&mut ge[$g], &mut go[$g]);
            if XT {
                pass::<LOSSY, $sa, $sb, RS>(br, ge, go, t, &mut a, &mut b);
            } else {
                pass::<LOSSY, KEEP, KEEP, RS>(br, ge, go, t, &mut a, &mut b);
            }
        }};
    }
    one!(0, SKIP_ALL, KEEP, rows_of::<RS>(reg_r, 0), rows_of::<RS>(reg_g, 0));
    one!(1, KEEP, SKIP_ALL, rows_of::<RS>(reg_g, 1), rows_of::<RS>(reg_b, 0));
    one!(2, SKIP_EVEN_STEP, SKIP_ALL, rows_of::<RS>(reg_r, 1), rows_of::<RS>(reg_g, 2));
    one!(0, KEEP, SKIP_ODD_STEP, rows_of::<RS>(reg_g, 3), rows_of::<RS>(reg_b, 1));
    one!(1, SKIP_ODD_STEP, KEEP, rows_of::<RS>(reg_r, 2), rows_of::<RS>(reg_g, 4));
    one!(2, SKIP_ALL, SKIP_EVEN_STEP, rows_of::<RS>(reg_g, 5), rows_of::<RS>(reg_b, 2));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes() -> Vec<u8> {
        // Lossless X-Trans, 14 bit, 792 x 12, two stripes, two lines.
        let mut h = vec![0x49, 0x53, 1, 16, 14];
        h.extend_from_slice(&12u16.to_be_bytes());
        h.extend_from_slice(&1536u16.to_be_bytes());
        h.extend_from_slice(&792u16.to_be_bytes());
        h.extend_from_slice(&0x300u16.to_be_bytes());
        h.push(2);
        h.extend_from_slice(&2u16.to_be_bytes());
        h
    }

    fn with(h: &[u8], at: usize, bytes: &[u8]) -> Vec<u8> {
        let mut v = h.to_vec();
        v[at..at + bytes.len()].copy_from_slice(bytes);
        v
    }

    #[test]
    fn header_accepts_a_valid_header() {
        let h = FujiHeader::parse(&header_bytes()).unwrap();
        assert!(h.lossless && h.xtrans);
        assert_eq!((h.bits, h.height, h.rounded_width, h.width), (14, 12, 1536, 792));
        assert_eq!((h.block_size, h.blocks, h.lines), (768, 2, 2));
        assert_eq!((h.block_width(0), h.block_width(1)), (768, 24));
    }

    #[test]
    fn header_rejects_each_violated_condition() {
        let good = header_bytes();
        let bad: Vec<(&str, Vec<u8>)> = vec![
            ("signature", with(&good, 0, &[0x49, 0x54])),
            ("lossless flag", with(&good, 2, &[2])),
            ("raw type", with(&good, 3, &[8])),
            ("bits", with(&good, 4, &[13])),
            ("height not a multiple of 6", with(&good, 5, &13u16.to_be_bytes())),
            ("height zero", with(&good, 5, &0u16.to_be_bytes())),
            ("height disagrees with the line count", with(&good, 5, &0x4008u16.to_be_bytes())),
            ("stripe width", with(&good, 11, &0x200u16.to_be_bytes())),
            ("width not a multiple of 24", with(&good, 9, &793u16.to_be_bytes())),
            ("width too small", with(&good, 9, &744u16.to_be_bytes())),
            ("width too large", with(&good, 9, &0x4218u16.to_be_bytes())),
            ("rounded width not a multiple of 768", with(&good, 7, &1500u16.to_be_bytes())),
            ("rounded width far above width", with(&good, 9, &768u16.to_be_bytes())),
            ("stripe count", with(&good, 13, &[3])),
            ("stripe count zero", with(&good, 13, &[0])),
            ("line count", with(&good, 14, &3u16.to_be_bytes())),
        ];
        for (what, d) in bad {
            let e = FujiHeader::parse(&d).expect_err(what);
            assert!(e.to_string().contains("invalid Fuji compressed header"), "{what}: {e}");
        }
        let e = FujiHeader::parse(&good[..15]).unwrap_err();
        assert!(e.to_string().contains("truncated Fuji compressed header"));
    }

    #[test]
    fn container_tables_and_clamped_ranges() {
        // Lossless, two stripes: sizes 5 and 100; only 8 payload bytes follow the 16-byte table area.
        let mut d = header_bytes();
        d.extend_from_slice(&5u32.to_be_bytes());
        d.extend_from_slice(&100u32.to_be_bytes());
        d.extend_from_slice(&[0; 8]); // table padding up to 16
        d.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let f = FujiCompressed::new(&d).unwrap();
        assert_eq!(f.block_ranges(&d), vec![(32, 5), (37, 3)]);
        // A stripe starting past the end is empty, at the end of the data.
        let mut d2 = header_bytes();
        d2.extend_from_slice(&40u32.to_be_bytes());
        d2.extend_from_slice(&7u32.to_be_bytes());
        d2.extend_from_slice(&[0; 8]);
        d2.extend_from_slice(&[9; 10]);
        let f = FujiCompressed::new(&d2).unwrap();
        assert_eq!(f.block_ranges(&d2), vec![(32, 10), (42, 0)]);
        // Too short for the size table.
        let e = FujiCompressed::new(&header_bytes()).err().unwrap();
        assert!(e.to_string().contains("truncated Fuji block size table"));
    }

    #[test]
    fn lossy_container_needs_every_quantisation_table() {
        let mut h = header_bytes();
        h[2] = 0;
        let mut d = h.clone();
        d.extend_from_slice(&[0; 8]); // sizes
        d.extend_from_slice(&[0; 8]); // pad to 16
        d.extend_from_slice(&[3; 16]); // table of stripe 0 (2 lines, padded to 16)
        let e = FujiCompressed::new(&d).err().unwrap();
        assert!(e.to_string().contains("truncated Fuji lossy quantisation table"));
        d.extend_from_slice(&[4; 2]); // table of stripe 1 (2 bytes are enough)
        let f = FujiCompressed::new(&d).unwrap();
        assert_eq!(f.qbase, vec![&[3u8, 3][..], &[4u8, 4][..]]);
        // Entropy data would start after both padded tables, i.e. past the end.
        assert_eq!(f.block_ranges(&d), vec![(d.len(), 0), (d.len(), 0)]);
    }

    #[test]
    fn quantisation_tables_follow_the_format() {
        for bits in [12u32, 14, 16] {
            let m = (1i64 << bits) - 1;
            for q in [0u32, 1, 3, 255] {
                let t = Quant::new(q, bits);
                let (qi, a1, a2, a3) = (q as i64, 3 * q as i64 + 18, 5 * q as i64 + 67, 7 * q as i64 + 276);
                // Thresholds strictly increase and stay below M + 1.
                assert!(qi < a1 && a1 < a2 && a2 < a3 && a3 < m + 1);
                let lev = |d: i64| -> i32 {
                    if d <= -a3 {
                        -4
                    } else if d <= -a2 {
                        -3
                    } else if d <= -a1 {
                        -2
                    } else if d < -qi {
                        -1
                    } else if d <= qi {
                        0
                    } else if d < a1 {
                        1
                    } else if d < a2 {
                        2
                    } else if d < a3 {
                        3
                    } else {
                        4
                    }
                };
                for d in (-m..=m).step_by(7).chain([
                    -a3,
                    -a3 + 1,
                    -a2,
                    -a1,
                    -qi,
                    -qi - 1,
                    0,
                    qi,
                    qi + 1,
                    a1 - 1,
                    a1,
                    a2 - 1,
                    a2,
                    a3 - 1,
                    a3,
                    m,
                ]) {
                    assert_eq!(t.lev(d as i32), lev(d), "bits {bits} q {q} d {d}");
                }
                let v = (m + 2 * qi) / (2 * qi + 1) + 1;
                assert_eq!(t.wrap as i64, v * (2 * qi + 1));
                assert!((1u64 << t.rb) >= v as u64 && (t.rb == 0 || (1u64 << (t.rb - 1)) < v as u64));
                assert_eq!(t.zmax, 4 * bits - t.rb - 1);
                assert_eq!(t.d0 as i64, ((v + 32) >> 6).max(2));
            }
        }
        // The worked example: lossless 14 bit.
        let t = Quant::new(0, 14);
        assert_eq!((t.wrap, t.rb, t.zmax, t.d0), (16384, 14, 41, 256));
        assert_eq!((t.lev(18), t.lev(-67), t.lev(276), t.lev(1), t.lev(0)), (2, -3, 4, 1, 0));
    }

    #[test]
    fn rice_parameter_matches_the_definition() {
        for n in 1u32..=64 {
            for sum in (0u32..5000).chain([1 << 15, (1 << 20) + 17, 1 << 26, (1 << 26) + 5, 1 << 28, u32::MAX >> 1]) {
                let mut k = 0;
                while k < 15 && (u64::from(n) << k) < u64::from(sum.max(1)) {
                    k += 1;
                }
                assert_eq!(rice_param(sum, n), k, "sum {sum} n {n}");
            }
        }
    }

    #[test]
    fn statistics_halve_at_64() {
        let mut s = Stat::fresh(256);
        for _ in 0..63 {
            s.add(10);
        }
        assert_eq!((s.sum, s.n), (256 + 630, 64));
        s.add(4);
        assert_eq!((s.sum, s.n), ((256 + 630 + 4) >> 1, 33));
        assert_eq!(u32::from(s.k), rice_param(s.sum, 33));
    }

    fn bits_of(bits: &[u8]) -> Vec<u8> {
        // '0'/'1' characters to bytes, zero padded.
        let mut out = vec![0u8; bits.len().div_ceil(8)];
        for (i, &b) in bits.iter().enumerate() {
            if b == b'1' {
                out[i / 8] |= 0x80 >> (i % 8);
            }
        }
        out
    }

    #[test]
    fn residual_code_worked_example_and_escape() {
        // 0001 | 10100101: three zeros, then eight low bits, k = 8 -> 3 << 8 | 0xA5.
        let data = bits_of(b"000110100101");
        let mut br = Bits::new(&data);
        assert_eq!(br.code(8, 41, 41, 14), 933);
        assert_eq!(933 >> 1, 466); // odd code: r = -(466) - 1 = -467
        // Escape: 41 zeros, a one, then 14 raw bits -> value + 1.
        let mut s = "0".repeat(41);
        s.push('1');
        s.push_str("00100100110100");
        let data = bits_of(s.as_bytes());
        let mut br = Bits::new(&data);
        assert_eq!(br.code(8, 41, 41, 14), 0b00100100110100 + 1);
        assert_eq!(br.consumed(), 56);
        // The same prefix of 40 zeros is an ordinary code (low bits follow).
        let mut s = "0".repeat(40);
        s.push('1');
        s.push_str("11111111");
        let data = bits_of(s.as_bytes());
        let mut br = Bits::new(&data);
        assert_eq!(br.code(8, 41, 41, 14), (40 << 8) | 0xFF);
    }

    #[test]
    fn long_zero_runs_terminate() {
        let data = [0u8; 10];
        let mut br = Bits::new(&data);
        let _ = br.code(3, 41, 41, 14);
        assert!(br.lost || br.consumed() > 8 * data.len() as u64);
        let mut br = Bits::new(&[]);
        let _ = br.code(0, 41, 41, 14);
        assert!(br.lost || br.consumed() > 7);
    }

    fn lcg_bytes(seed: u64, n: usize) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 33) as u8
            })
            .collect()
    }

    fn stream(lossless: bool, xtrans: bool, bits: u8, data_len: usize, seed: u64) -> Vec<u8> {
        let mut h = vec![0x49, 0x53, lossless as u8, if xtrans { 16 } else { 0 }, bits];
        h.extend_from_slice(&6u16.to_be_bytes());
        h.extend_from_slice(&768u16.to_be_bytes());
        h.extend_from_slice(&768u16.to_be_bytes());
        h.extend_from_slice(&0x300u16.to_be_bytes());
        h.push(1);
        h.extend_from_slice(&1u16.to_be_bytes());
        h.extend_from_slice(&(data_len as u32).to_be_bytes());
        h.extend_from_slice(&[0; 12]);
        if !lossless {
            h.push(5);
            h.extend_from_slice(&[0; 15]);
        }
        h.extend_from_slice(&lcg_bytes(seed, data_len));
        h
    }

    fn rggb() -> ColorTile {
        let mut t = [[1u8; 6]; 6];
        for (y, row) in t.iter_mut().enumerate() {
            for (x, v) in row.iter_mut().enumerate() {
                *v = [[0, 1], [1, 2]][y % 2][x % 2];
            }
        }
        t
    }

    #[test]
    fn decode_checks_sizes_and_regions() {
        let d = stream(true, false, 14, 20000, 5);
        let f = FujiCompressed::new(&d).unwrap();
        let mut out = vec![0u16; 768 * 6];
        let t = rggb();
        assert!(matches!(f.decode(&mut out, 768, 12, &t, false), Err(Error::InvalidArgument(_))));
        assert!(matches!(f.decode(&mut out, 767, 6, &t, false), Err(Error::InvalidArgument(_))));
        let bad = |r: Rect, len: usize| {
            let mut o = vec![0u16; len];
            matches!(f.decode_region(&mut o, r, &t, false), Err(Error::InvalidArgument(_)))
        };
        assert!(bad(Rect { x: 700, y: 0, width: 100, height: 1 }, 100));
        assert!(bad(Rect { x: 0, y: 5, width: 1, height: 2 }, 2));
        assert!(bad(Rect { x: 0, y: 0, width: 10, height: 10 }, 99));
        // An empty rectangle succeeds and leaves the output alone.
        let mut o = vec![9u16; 4];
        f.decode_region(&mut o, Rect { x: 3, y: 1, width: 0, height: 4 }, &t, false).unwrap();
        assert_eq!(o, vec![9; 4]);
    }

    #[test]
    fn empty_stripe_fails_and_writes_nothing() {
        let d = stream(true, true, 14, 0, 1);
        let f = FujiCompressed::new(&d).unwrap();
        let mut out = vec![0x1234u16; 768 * 6];
        let e = f.decode(&mut out, 768, 6, &XTRANS_TILE, false).unwrap_err();
        assert!(e.to_string().contains("truncated Fuji compressed block"));
        assert!(out.iter().all(|&v| v == 0x1234));
    }

    const XTRANS_TILE: ColorTile = [
        [1, 1, 0, 1, 1, 2],
        [1, 1, 2, 1, 1, 0],
        [2, 0, 1, 0, 2, 1],
        [1, 1, 2, 1, 1, 0],
        [1, 1, 0, 1, 1, 2],
        [0, 2, 1, 2, 0, 1],
    ];

    #[test]
    fn arbitrary_bytes_never_panic_and_are_deterministic() {
        for (i, &(ll, xt, bits)) in
            [(true, true, 14u8), (false, true, 12), (true, false, 16), (false, false, 14)].iter().enumerate()
        {
            for len in [0usize, 7, 300, 9000, 40000] {
                let d = stream(ll, xt, bits, len, 40 + len as u64 + i as u64);
                let f = FujiCompressed::new(&d).unwrap();
                let tile = if xt { XTRANS_TILE } else { rggb() };
                let mut a = vec![0u16; 768 * 6];
                let mut b = vec![0u16; 768 * 6];
                let ra = f.decode(&mut a, 768, 6, &tile, false).is_ok();
                let rb = f.decode(&mut b, 768, 6, &tile, true).is_ok();
                assert_eq!(ra, rb);
                assert_eq!(a, b);
                if ra {
                    let max = (1u32 << bits) - 1;
                    assert!(a.iter().all(|&v| u32::from(v) <= max));
                }
            }
        }
    }

    #[test]
    fn header_garbage_never_panics() {
        let base = lcg_bytes(99, 4096);
        for i in 0..2000usize {
            let mut d = base[i..i + 64].to_vec();
            if i % 2 == 0 {
                d[0] = 0x49;
                d[1] = 0x53;
            }
            let _ = FujiHeader::parse(&d);
            let _ = FujiCompressed::new(&d);
        }
    }
}
