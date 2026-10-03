//! it shares with Pentax PEF and Samsung SRW (compression 32772).
//!
//! # Bitstream model
//!
//! All three formats code a CFA image row by row as a single MSB-first bit stream
//! (no JPEG byte stuffing). Every sample is a Huffman symbol followed by a
//! difference; the predictor is the previous same-colour sample of the row
//! (`hpred[col & 1]`), and the first two samples of a row predict from the first two
//! samples two rows above (`vpred[row & 1][col]`).
//!
//! `len`, high nibble = `shl`, the number of low-order difference bits that are *not*
//! transmitted (Nikon's lossy "after split" tables). For `shl == 0` the difference is
//! the usual JPEG sign-extended value.
//!
//! # Truncated streams
//!
//! rest of the file and, at end of file, keep decoding with a zero-padded bit buffer
//! until the first read that crosses EOF; every later read returns zero (so the
//! predictors freeze). [`ExactBits`] reproduces that so truncated files decode to the
//!
//! # Performance
//!
//! The hot loop uses one unaligned 64-bit big-endian load per sample: a
//! [`FAST_BITS`]-bit lookup either yields the fully decoded difference (code plus
//! difference bits fit in the index) or the code length and symbol, whose difference
//! bits are taken from the same load. Rows that are provably far from the end of the
//! data use this path; the last rows go through the exact slow path.

use flashraw_core::bytes::ByteOrder;
use flashraw_core::{Error, Result};

/// Index width of the combined code+difference lookup table.
pub const FAST_BITS: u32 = 13;
const FAST_SIZE: usize = 1 << FAST_BITS;
const FLAG_FULL: u32 = 1 << 5;
/// Fast-table entry whose code is longer than `FAST_BITS` (resolve via the direct table).
const SLOW_MARK: u32 = 0x8000_0000;

/// `code_len << 8 | symbol` entries (zero for unused codes, which consume no bits and
#[derive(Clone)]
pub struct DirectHuff {
    peek_bits: u32,
    direct: Vec<u16>,
    /// Combined table indexed by the next `FAST_BITS` bits. Entry layout:
    /// bits 0..5 bits consumed; bit 5 FULL (diff in bits 16..32); bits 8..16 symbol.
    fast: Box<[u32; FAST_SIZE]>,
}

impl std::fmt::Debug for DirectHuff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectHuff").field("peek_bits", &self.peek_bits).finish()
    }
}

/// Difference value for a symbol (`len | shl << 4`) given the `len - shl` transmitted bits.
#[inline(always)]
pub fn symbol_diff(bits: u32, sym: u32) -> i32 {
    let len = sym & 15;
    let shl = sym >> 4;
    let mut diff = ((((bits as i32) << 1) + 1) << shl) >> 1;
    if len > 0 && diff & (1 << (len - 1)) == 0 {
        diff -= (1 << len) - (shl == 0) as i32;
    }
    diff
}

impl DirectHuff {
    /// symbols in code order.
    pub fn from_counts(source: &[u8]) -> Result<Self> {
        if source.len() < 16 {
            return Err(Error::corrupt("huffman source shorter than 16 counts"));
        }
        let counts = &source[..16];
        let max = (1..=16).rev().find(|&l| counts[l - 1] != 0).unwrap_or(0) as u32;
        if max == 0 {
            return Err(Error::corrupt("empty huffman table"));
        }
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        let syms = source.get(16..16 + total).ok_or_else(|| Error::corrupt("huffman table symbols missing"))?;
        let mut direct = vec![0u16; 1 << max];
        let mut h = 0usize;
        let mut k = 0usize;
        for len in 1..=max {
            for _ in 0..counts[len as usize - 1] {
                let sym = syms[k];
                k += 1;
                for _ in 0..(1usize << (max - len)) {
                    if h < direct.len() {
                        direct[h] = ((len as u16) << 8) | sym as u16;
                        h += 1;
                    }
                }
            }
        }
        Ok(Self::from_direct(max, direct))
    }

    /// Build from a direct table of `1 << peek_bits` entries (`code_len << 8 | symbol`).
    pub fn from_direct(peek_bits: u32, direct: Vec<u16>) -> Self {
        debug_assert_eq!(direct.len(), 1 << peek_bits);
        let mut fast: Box<[u32; FAST_SIZE]> = vec![0u32; FAST_SIZE].into_boxed_slice().try_into().expect("FAST_SIZE");
        for (idx, e) in fast.iter_mut().enumerate() {
            // Code bits are the top `peek_bits` of the FAST_BITS index. When the table is
            // wider than the fast index, only codes short enough to be determined by
            // FAST_BITS bits are resolved here (canonical tables fill aligned blocks);
            // the rest are marked for the slow path.
            let entry = if peek_bits <= FAST_BITS {
                direct[idx >> (FAST_BITS - peek_bits)]
            } else {
                let d = direct[idx << (peek_bits - FAST_BITS)];
                let clen = (d >> 8) as u32;
                if clen == 0 || clen > FAST_BITS {
                    *e = SLOW_MARK;
                    continue;
                }
                d
            };
            let clen = (entry >> 8) as u32;
            let sym = (entry & 0xFF) as u32;
            let len = sym & 15;
            let shl = sym >> 4;
            let nbits = len.saturating_sub(shl);
            if clen + nbits <= FAST_BITS {
                let b = if nbits == 0 { 0 } else { ((idx as u32) >> (FAST_BITS - clen - nbits)) & ((1 << nbits) - 1) };
                let d = symbol_diff(b, sym);
                *e = (clen + nbits) | FLAG_FULL | (sym << 8) | ((d as i16 as u16 as u32) << 16);
            } else {
                *e = clen | (sym << 8);
            }
        }
        DirectHuff { peek_bits, direct, fast }
    }

    pub fn peek_bits(&self) -> u32 {
        self.peek_bits
    }

    /// Direct-table entry for the next `peek_bits` bits.
    #[inline(always)]
    fn direct(&self, idx: u32) -> u16 {
        self.direct[idx as usize]
    }
}

#[derive(Clone)]
pub struct ExactBits<'a> {
    data: &'a [u8],
    /// Bits consumed so far.
    pos: u64,
    /// Set after the first read that crossed the end of the data.
    dead: bool,
}

impl<'a> ExactBits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        ExactBits { data, pos: 0, dead: false }
    }

    /// Bits consumed so far.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Whether the stream ran past the end of its data.
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    #[inline(always)]
    fn total_bits(&self) -> u64 {
        self.data.len() as u64 * 8
    }

    /// The 57+ bits starting at the current position, left-aligned, zero padded past the end.
    #[inline(always)]
    fn window(&self) -> u64 {
        let byte = (self.pos >> 3) as usize;
        let w = match self.data.get(byte..byte + 8) {
            Some(s) => u64::from_be_bytes(s.try_into().unwrap()),
            None => {
                let mut b = [0u8; 8];
                if let Some(s) = self.data.get(byte..) {
                    b[..s.len()].copy_from_slice(s);
                }
                u64::from_be_bytes(b)
            }
        };
        w << (self.pos & 7)
    }

    /// Consume `n` bits after peeking (marks the stream dead if this crosses the end).
    #[inline(always)]
    fn consume(&mut self, n: u32) {
        self.pos += n as u64;
        if self.pos > self.total_bits() {
            self.dead = true;
        }
    }

    #[inline]
    pub fn get(&mut self, n: u32) -> u32 {
        if n == 0 || self.dead {
            return 0;
        }
        let v = (self.window() >> (64 - n)) as u32;
        self.consume(n);
        v
    }

    #[inline]
    pub fn huff(&mut self, t: &DirectHuff) -> u32 {
        if self.dead {
            return 0;
        }
        let idx = (self.window() >> (64 - t.peek_bits)) as u32;
        let e = t.direct(idx);
        self.consume((e >> 8) as u32);
        (e & 0xFF) as u32
    }

    #[inline]
    pub fn diff(&mut self, t: &DirectHuff) -> i32 {
        let sym = self.huff(t);
        let len = sym & 15;
        let shl = sym >> 4;
        let b = self.get(len.saturating_sub(shl));
        symbol_diff(b, sym)
    }
}

/// Parameters of a predictive Huffman image.
pub struct PredictiveParams<'t> {
    pub width: usize,
    /// Rows to decode (rows beyond are left untouched).
    pub rows: usize,
    /// Tables and the first row each applies to (ascending; first must start at 0).
    pub tables: &'t [(usize, &'t DirectHuff)],
    /// Initial vertical predictors (`vpred[row & 1][col]` for the first two columns).
    pub vpred: [[u16; 2]; 2],
}

/// Decode a predictive Huffman image into `out` (rows of `stride` samples), mapping each
/// reconstructed predictor value through `map` (identity, or a linearisation curve).
///
/// Returns the reader so callers can inspect truncation.
pub fn decode_predictive<'a, M>(
    src: &'a [u8],
    p: &PredictiveParams,
    out: &mut [u16],
    stride: usize,
    map: M,
) -> Result<ExactBits<'a>>
where
    M: Fn(u16) -> u16,
{
    let PredictiveParams { width, rows, tables, vpred } = *p;
    if width < 2 || stride < width {
        return Err(Error::InvalidArgument("predictive huffman: bad width".into()));
    }
    if out.len() < stride * rows.saturating_sub(1) + width && rows > 0 {
        return Err(Error::InvalidArgument("predictive huffman: output too small".into()));
    }
    let first = tables.first().ok_or_else(|| Error::InvalidArgument("no huffman table".into()))?;
    if first.0 != 0 {
        return Err(Error::InvalidArgument("first huffman table must start at row 0".into()));
    }
    let mut vpred = vpred;
    let mut bits = ExactBits::new(src);
    let mut ti = 0usize;
    // Worst case bits per sample is 31 (16-bit code + 15 difference bits); the fast
    // reader also looks up to 16 bytes ahead of its position.
    let row_budget = width as u64 * 32 + 192;
    for row in 0..rows {
        while ti + 1 < tables.len() && tables[ti + 1].0 <= row {
            ti += 1;
        }
        let t = tables[ti].1;
        let line = &mut out[row * stride..row * stride + width];
        let vp = &mut vpred[row & 1];
        let fast = !bits.dead && bits.pos + row_budget <= bits.total_bits();
        if fast {
            let mut rb = RowBits::start(bits.data, bits.pos);
            decode_row_fast(&mut rb, t, vp, line, &map);
            bits.pos = rb.position();
        } else {
            decode_row_exact(&mut bits, t, vp, line, &map);
        }
    }
    Ok(bits)
}

/// [`decode_predictive`] that entropy-decodes large single-table streams on all threads
/// ([`crate::par_entropy`]) when `parallel` is set. Returns whether the stream ran past
/// its data (`is_dead`). Output is identical to [`decode_predictive`]: streams whose
/// speculation cannot be proven equal (table switches, truncation, invalid codes) take
/// the sequential path. `declared` is the payload length the container states (`src`
/// may run on to the end of the file); speculation covers about that much.
pub fn decode_predictive_par<M>(
    src: &[u8],
    p: &PredictiveParams,
    out: &mut [u16],
    stride: usize,
    map: M,
    parallel: bool,
    declared: usize,
) -> Result<bool>
where
    M: Fn(u16) -> u16 + Sync,
{
    let (width, rows) = (p.width, p.rows);
    if parallel
        && p.tables.len() == 1
        && width >= 2
        && stride >= width
        && rows > 0
        && out.len() >= stride * (rows - 1) + width
        && rows * width >= crate::ljpeg_par::PARALLEL_MIN_SAMPLES
        && decode_predictive_parallel(src, p, out, stride, &map, declared, None)
    {
        return Ok(false);
    }
    Ok(decode_predictive(src, p, out, stride, map)?.is_dead())
}

/// The stream as one difference per symbol.
struct DiffScan<'a> {
    src: &'a [u8],
    table: &'a DirectHuff,
    span: u64,
}

impl crate::par_entropy::SymbolStream for DiffScan<'_> {
    type Pump<'p>
        = ExactBits<'p>
    where
        Self: 'p;
    const WIDTH: usize = 1;
    fn cycle(&self) -> usize {
        1
    }
    fn period(&self) -> usize {
        1
    }
    fn total_bits(&self) -> u64 {
        self.src.len() as u64 * 8
    }
    fn span_bits(&self) -> u64 {
        self.span
    }
    fn end_check(&self, pos: u64) -> crate::par_entropy::EndCheck {
        // Past the end the reader goes dead and yields zeros: leave that to the
        // sequential decoder.
        if pos <= self.total_bits() {
            crate::par_entropy::EndCheck::Valid
        } else {
            crate::par_entropy::EndCheck::Unknown
        }
    }
    fn pump_at(&self, bit: u64) -> ExactBits<'_> {
        ExactBits { data: self.src, pos: bit, dead: false }
    }
    fn position(p: &ExactBits<'_>) -> u64 {
        p.pos
    }
    #[inline(always)]
    fn decode(&self, p: &mut ExactBits<'_>, _phase: usize, out: &mut [u16]) -> bool {
        let before = p.pos;
        out[0] = p.diff(self.table) as u16;
        if p.pos == before {
            // A code of length zero (unused table slot): the sequential decoder would not
            // advance either; treat it as invalid so it decodes this stream itself.
            p.pos += 1;
            return false;
        }
        true
    }
}

fn decode_predictive_parallel<M>(
    src: &[u8],
    p: &PredictiveParams,
    out: &mut [u16],
    stride: usize,
    map: &M,
    declared: usize,
    chunk: Option<usize>,
) -> bool
where
    M: Fn(u16) -> u16 + Sync,
{
    let (width, rows) = (p.width, p.rows);
    // Declared lengths can be a little short; a true path running past the span falls
    // back to sequential decoding, so a margin is cheap insurance.
    let span = if declared == 0 { src.len() } else { (declared + declared / 16 + (64 << 10)).min(src.len()) };
    let Some(chunk) = chunk.or_else(|| crate::par_entropy::default_chunk(span)) else { return false };
    let scan = DiffScan { src, table: p.tables[0].1, span: span as u64 * 8 };
    let Some(values) = crate::par_entropy::decode(&scan, rows * width, chunk) else { return false };
    // Rows depend on the rows above only through the first two columns of the previous
    // row of the same parity: chain those, then rebuild rows in parallel.
    let mut vp = p.vpred;
    let starts: Vec<[u16; 2]> = (0..rows)
        .map(|r| {
            let st = vp[r & 1];
            for c in 0..2 {
                vp[r & 1][c] = vp[r & 1][c].wrapping_add(values.get(r * width + c));
            }
            st
        })
        .collect();
    let n = out.len().min(rows * stride);
    flashraw_core::par::for_each_row_chunk(&mut out[..n], stride, 16, true, |r0, chunk| {
        let mut d = vec![0u16; width];
        for (i, line) in chunk.chunks_mut(stride).enumerate() {
            let r = r0 + i;
            values.copy_to(r * width, &mut d);
            let mut vpl = starts[r];
            let mut h = [0u16; 2];
            for (col, (v, &dv)) in line[..width].iter_mut().zip(&d).enumerate() {
                let c = col & 1;
                if col < 2 {
                    vpl[c] = vpl[c].wrapping_add(dv);
                    h[c] = vpl[c];
                } else {
                    h[c] = h[c].wrapping_add(dv);
                }
                *v = map(h[c]);
            }
        }
    });
    true
}

/// Register bit cache for rows that are provably inside the data (see the row budget
/// in [`decode_predictive`]): branchless refills keep 56..=63 bits buffered, so a
/// symbol and its difference bits never need a second load.
struct RowBits<'a> {
    data: &'a [u8],
    /// Next byte to load.
    ptr: usize,
    /// Valid bits, left aligned (zero below them).
    buf: u64,
    nbits: u32,
}

impl<'a> RowBits<'a> {
    fn start(data: &'a [u8], pos: u64) -> Self {
        let mut b = RowBits { data, ptr: (pos >> 3) as usize, buf: 0, nbits: 0 };
        b.refill();
        let skip = (pos & 7) as u32;
        b.buf <<= skip;
        b.nbits -= skip;
        b
    }

    /// Bit position in the data.
    fn position(&self) -> u64 {
        self.ptr as u64 * 8 - self.nbits as u64
    }

    #[inline(always)]
    fn refill(&mut self) {
        let w = u64::from_be_bytes(self.data[self.ptr..self.ptr + 8].try_into().unwrap());
        self.buf |= w >> self.nbits;
        self.ptr += ((63 - self.nbits) >> 3) as usize;
        self.nbits |= 56;
    }

    #[inline(always)]
    fn diff(&mut self, t: &DirectHuff) -> i32 {
        self.refill();
        let e = t.fast[(self.buf >> (64 - FAST_BITS)) as usize];
        if e & FLAG_FULL != 0 {
            let n = e & 0x1F;
            self.buf <<= n;
            self.nbits -= n;
            return (e >> 16) as u16 as i16 as i32;
        }
        self.diff_long(t, e)
    }

    #[cold]
    #[inline(never)]
    fn diff_long(&mut self, t: &DirectHuff, e: u32) -> i32 {
        let (clen, sym) = if e == SLOW_MARK {
            let d = t.direct((self.buf >> (64 - t.peek_bits)) as u32);
            ((d >> 8) as u32, (d & 0xFF) as u32)
        } else {
            (e & 0x1F, (e >> 8) & 0xFF)
        };
        let n = (sym & 15).saturating_sub(sym >> 4);
        let w2 = self.buf << clen;
        let b = if n == 0 { 0 } else { (w2 >> (64 - n)) as u32 };
        let used = clen + n;
        self.buf = if used >= 64 { 0 } else { self.buf << used };
        self.nbits -= used;
        symbol_diff(b, sym)
    }
}

#[inline(always)]
fn decode_row_fast<M: Fn(u16) -> u16>(bits: &mut RowBits, t: &DirectHuff, vp: &mut [u16; 2], line: &mut [u16], map: &M) {
    let d0 = bits.diff(t);
    vp[0] = vp[0].wrapping_add(d0 as u16);
    let d1 = bits.diff(t);
    vp[1] = vp[1].wrapping_add(d1 as u16);
    let (mut h0, mut h1) = (vp[0], vp[1]);
    line[0] = map(h0);
    line[1] = map(h1);
    let mut pairs = line[2..].chunks_exact_mut(2);
    for pair in &mut pairs {
        h0 = h0.wrapping_add(bits.diff(t) as u16);
        h1 = h1.wrapping_add(bits.diff(t) as u16);
        pair[0] = map(h0);
        pair[1] = map(h1);
    }
    if let [last] = pairs.into_remainder() {
        h0 = h0.wrapping_add(bits.diff(t) as u16);
        *last = map(h0);
    }
}

#[cold]
fn decode_row_exact<M: Fn(u16) -> u16>(
    bits: &mut ExactBits,
    t: &DirectHuff,
    vp: &mut [u16; 2],
    line: &mut [u16],
    map: &M,
) {
    let mut h = [0u16; 2];
    for (col, v) in line.iter_mut().enumerate() {
        let d = bits.diff(t) as u16;
        let c = col & 1;
        if col < 2 {
            vp[c] = vp[c].wrapping_add(d);
            h[c] = vp[c];
        } else {
            h[c] = h[c].wrapping_add(d);
        }
        *v = map(h[c]);
    }
}

pub const NIKON_TREES: [&[u8]; 6] = [
    // 12-bit lossy
    &[0, 1, 5, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 5, 4, 3, 6, 2, 7, 1, 0, 8, 9, 11, 10, 12, 0],
    // 12-bit lossy after split
    &[0, 1, 5, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0x39, 0x5a, 0x38, 0x27, 0x16, 5, 4, 3, 2, 1, 0, 11, 12, 12],
    // 12-bit lossless
    &[0, 1, 4, 2, 3, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 4, 6, 3, 7, 2, 8, 1, 9, 0, 10, 11, 12],
    // 14-bit lossy
    &[0, 1, 4, 3, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 5, 6, 4, 7, 8, 3, 9, 2, 1, 0, 10, 11, 12, 13, 14],
    // 14-bit lossy after split
    &[0, 1, 5, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 8, 0x5c, 0x4b, 0x3a, 0x29, 7, 6, 5, 4, 3, 2, 1, 0, 13, 14],
    // 14-bit lossless
    &[0, 1, 4, 2, 2, 3, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 7, 6, 8, 5, 9, 4, 10, 3, 11, 12, 2, 0, 1, 13, 14],
];

/// Decoded Nikon compression metadata (makernote tag 0x96 "NEFLinearizationTable").
#[derive(Debug, Clone)]
pub struct NikonMeta {
    /// Version bytes (0x44 0x10/0x20/0x40 lossy, 0x46 0x30 lossless, ...).
    pub version: [u8; 2],
    pub vpred: [[u16; 2]; 2],
    /// Index into [`NIKON_TREES`].
    pub tree: usize,
    /// Row at which the "after split" tree takes over (0 = none).
    pub split_row: usize,
    /// Linearisation curve indexed by the clamped predictor value (0x4000 entries).
    pub curve: Vec<u16>,
}

impl NikonMeta {
    /// Parse the blob at makernote tag 0x96 (or 0x8C). `meta` starts at the tag's value
    pub fn parse(meta: &[u8], order: ByteOrder, bits: u32) -> Result<Self> {
        if bits != 12 && bits != 14 {
            return Err(Error::unsupported_compression("NEF", format!("{bits}-bit Nikon Huffman")));
        }
        let rd = |off: usize| -> Result<u16> {
            let b = meta.get(off..off + 2).ok_or_else(|| Error::corrupt("Nikon linearization table truncated"))?;
            Ok(order.u16([b[0], b[1]]))
        };
        let ver0 = *meta.first().ok_or_else(|| Error::corrupt("empty Nikon linearization table"))?;
        let ver1 = *meta.get(1).ok_or_else(|| Error::corrupt("Nikon linearization table truncated"))?;
        let mut pos = 2usize;
        if ver0 == 0x49 || ver1 == 0x58 {
            pos += 2110;
        }
        let mut vpred = [[0u16; 2]; 2];
        for (i, v) in vpred.iter_mut().flatten().enumerate() {
            *v = rd(pos + 2 * i)?;
        }
        pos += 8;
        let mut tree = if ver0 == 0x46 { 2 } else { 0 };
        if bits == 14 {
            tree += 3;
        }
        // Linearisation curve (identity unless the table provides one).
        let mut curve: Vec<u16> = (0..=0xFFFFu32).map(|i| i as u16).collect();
        let mut max = (1usize << bits) & 0x7fff;
        let mut step = max;
        let csize = rd(pos)? as usize;
        pos += 2;
        if csize > 1 {
            step = max / (csize - 1);
        }
        if ver0 == 0x44 && (ver1 == 0x20 || (ver1 == 0x40 && step > 3)) && step > 0 {
            if ver1 == 0x40 {
                step /= 4;
                max /= 4;
            }
            for i in 0..csize {
                let idx = i * step;
                let v = rd(pos + 2 * i)?;
                if let Some(c) = curve.get_mut(idx) {
                    *c = v;
                }
            }
            if step > 0 {
                for i in 0..max {
                    let base = i - i % step;
                    let (a, b) = (curve[base] as u64, curve.get(base + step).copied().unwrap_or(0) as u64);
                    curve[i] = ((a * (step - i % step) as u64 + b * (i % step) as u64) / step as u64) as u16;
                }
            }
        } else if ver0 != 0x46 && csize <= 0x4001 {
            for (i, c) in curve.iter_mut().enumerate().take(csize) {
                *c = rd(pos + 2 * i)?;
            }
        }
        let split_row = if ver0 == 0x44 && (ver1 == 0x20 || ver1 == 0x40) { rd(562)? as usize } else { 0 };
        curve.truncate(0x4000);
        Ok(NikonMeta { version: [ver0, ver1], vpred, tree, split_row, curve })
    }

    /// Whether this is a lossless stream (no curve, identity mapping).
    pub fn is_lossless(&self) -> bool {
        self.version[0] == 0x46
    }

    /// Largest value the curve produces (the effective white level of lossy files).
    pub fn curve_max(&self) -> u16 {
        self.curve.iter().copied().max().unwrap_or(0)
    }
}

/// Decode a Nikon Huffman NEF payload. `src` should extend to the end of the file
#[allow(clippy::too_many_arguments)]
pub fn decode_nikon(
    src: &[u8],
    meta: &NikonMeta,
    width: usize,
    rows: usize,
    out: &mut [u16],
    stride: usize,
    parallel: bool,
    declared: usize,
) -> Result<bool> {
    let main = DirectHuff::from_counts(NIKON_TREES[meta.tree])?;
    let after;
    let mut tables: Vec<(usize, &DirectHuff)> = vec![(0, &main)];
    if meta.split_row > 0 && meta.tree + 1 < NIKON_TREES.len() {
        after = DirectHuff::from_counts(NIKON_TREES[meta.tree + 1])?;
        tables.push((meta.split_row, &after));
    }
    let p = PredictiveParams { width, rows, tables: &tables, vpred: meta.vpred };
    let curve: &[u16; 0x4000] =
        meta.curve.as_slice().try_into().map_err(|_| Error::corrupt("Nikon curve has the wrong size"))?;
    decode_predictive_par(
        src,
        &p,
        out,
        stride,
        |h| {
            let i = (h as i16).clamp(0, 0x3fff) as usize;
            curve[i]
        },
        parallel,
        declared,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit writer for synthetic streams.
    struct W {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }
    impl W {
        fn new() -> Self {
            W { out: vec![], acc: 0, n: 0 }
        }
        fn put(&mut self, v: u32, len: u32) {
            for i in (0..len).rev() {
                self.acc = (self.acc << 1) | ((v >> i) & 1) as u64;
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc as u8);
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        fn finish(mut self) -> Vec<u8> {
            while self.n != 0 {
                self.put(0, 1);
            }
            self.out
        }
    }

    fn codes(src: &[u8]) -> Vec<(u8, u32, u32)> {
        let mut out = vec![];
        let mut code = 0u32;
        let mut k = 16;
        for len in 1..=16u32 {
            for _ in 0..src[len as usize - 1] {
                out.push((src[k], code, len));
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        out
    }

    /// Encode a diff for a shl==0 symbol table.
    fn encode(w: &mut W, cs: &[(u8, u32, u32)], d: i32) {
        let len = if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() };
        let (_, c, l) = cs.iter().find(|(s, _, _)| *s as u32 == len).expect("symbol");
        w.put(*c, *l);
        if len > 0 {
            let v = if d < 0 { (d - 1) as u32 & ((1 << len) - 1) } else { d as u32 };
            w.put(v, len);
        }
    }

    fn reference_decode(src: &[u8], t: &DirectHuff, width: usize, rows: usize, vpred: [[u16; 2]; 2]) -> Vec<u16> {
        let mut out = vec![0u16; width * rows];
        let mut vp = vpred;
        let mut b = ExactBits::new(src);
        for row in 0..rows {
            decode_row_exact(&mut b, t, &mut vp[row & 1], &mut out[row * width..(row + 1) * width], &|h| h);
        }
        out
    }

    #[test]
    fn lossless_roundtrip_all_trees() {
        for &tree in &[2usize, 5] {
            let src = NIKON_TREES[tree];
            let t = DirectHuff::from_counts(src).unwrap();
            let cs = codes(src);
            let (width, rows) = (37usize, 9usize);
            let maxv = if tree == 2 { 4095 } else { 16383 };
            let img: Vec<u16> = (0..width * rows).map(|i| ((i * 7919 + i * i * 31) % (maxv + 1)) as u16).collect();
            let vpred = [[100u16, 200], [300, 400]];
            let mut w = W::new();
            let mut vp = vpred;
            for row in 0..rows {
                let mut h = [0u16; 2];
                for col in 0..width {
                    let v = img[row * width + col];
                    let c = col & 1;
                    let pred = if col < 2 { vp[row & 1][c] } else { h[c] };
                    let d = v as i32 - pred as i32;
                    encode(&mut w, &cs, d);
                    if col < 2 {
                        vp[row & 1][c] = v;
                    }
                    h[c] = v;
                }
            }
            let mut data = w.finish();
            data.extend_from_slice(&[0u8; 4096]);
            let mut out = vec![0u16; width * rows];
            let p = PredictiveParams { width, rows, tables: &[(0, &t)], vpred };
            decode_predictive(&data, &p, &mut out, width, |h| h).unwrap();
            assert_eq!(out, img, "tree {tree}");
            assert_eq!(reference_decode(&data, &t, width, rows, vpred), img);
        }
    }

    #[test]
    fn fast_path_matches_exact_path_on_random_streams() {
        let mut x = 0x1234_5678_9abc_def0u64;
        let data: Vec<u8> = (0..200_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        for src in NIKON_TREES {
            let t = DirectHuff::from_counts(src).unwrap();
            let (width, rows) = (301, 40);
            let vpred = [[1, 2], [3, 4]];
            let mut fast = vec![0u16; width * rows];
            let p = PredictiveParams { width, rows, tables: &[(0, &t)], vpred };
            decode_predictive(&data, &p, &mut fast, width, |h| h).unwrap();
            assert_eq!(fast, reference_decode(&data, &t, width, rows, vpred));
        }
    }

    #[test]
    fn truncation_freezes_predictors() {
        let t = DirectHuff::from_counts(NIKON_TREES[2]).unwrap();
        let data = [0xA5u8; 10];
        let (width, rows) = (16, 8);
        let mut out = vec![0u16; width * rows];
        let p = PredictiveParams { width, rows, tables: &[(0, &t)], vpred: [[0, 0], [0, 0]] };
        let b = decode_predictive(&data, &p, &mut out, width, |h| h).unwrap();
        assert!(b.is_dead());
        // After death, rows repeat the frozen vertical predictors and differences are zero.
        let last = &out[(rows - 1) * width..];
        assert!(last.chunks(2).all(|c| c == &last[..2]));
    }

    #[test]
    fn shl_symbols() {
        // len 9, shl 3: 6 transmitted bits.
        let sym = 0x39;
        assert_eq!(symbol_diff(0b111111, sym), (0b111111 << 3) | 0b100);
        let d = symbol_diff(0, sym);
        assert!(d < 0);
    }

    #[test]
    fn meta_parsing_rejects_garbage() {
        assert!(NikonMeta::parse(&[], ByteOrder::Big, 12).is_err());
        assert!(NikonMeta::parse(&[0x44, 0x20, 0, 0], ByteOrder::Big, 12).is_err());
        assert!(NikonMeta::parse(&[0x46, 0x30, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], ByteOrder::Big, 10).is_err());
        let m = NikonMeta::parse(&[0x46, 0x30, 0, 1, 0, 2, 0, 3, 0, 4, 0, 0], ByteOrder::Big, 14).unwrap();
        assert_eq!(m.vpred, [[1, 2], [3, 4]]);
        assert_eq!(m.tree, 5);
        assert!(m.is_lossless());
        assert_eq!(m.curve[1234], 1234);
    }

    #[test]
    fn parallel_matches_sequential() {
        let t = DirectHuff::from_counts(NIKON_TREES[2]).unwrap();
        let (width, rows) = (96usize, 50usize);
        let mut x = 99u32;
        let data: Vec<u8> = (0..width * rows * 2)
            .map(|_| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                (x >> 24) as u8
            })
            .collect();
        let p = PredictiveParams { width, rows, tables: &[(0, &t)], vpred: [[100, 200], [300, 400]] };
        let stride = width + 3;
        let mut seq = vec![0u16; stride * rows];
        decode_predictive(&data, &p, &mut seq, stride, |h| h ^ 0x55).unwrap();
        for chunk in [3usize, 50, 999] {
            let mut par = vec![0u16; stride * rows];
            assert!(decode_predictive_parallel(&data, &p, &mut par, stride, &|h| h ^ 0x55, 0, Some(chunk)), "{chunk}");
            assert_eq!(par, seq, "chunk {chunk}");
        }
    }
}
