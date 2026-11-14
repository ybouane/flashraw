//! Nikon compressed NEF (TIFF compression 34713) and the predictive Huffman engine shared with
//! Pentax PEF Huffman and Samsung SRW V1.
//!
//! The image is one MSB-first bit stream of Huffman symbols; each symbol is followed by difference
//! bits, and samples are rebuilt from two interleaved horizontal predictors seeded by the first two
//! samples of the row two above. Nikon adds a linearisation curve and an optional second table that
//! takes over at a given row.
//!
//! Layout of this module:
//!
//! * [`DirectHuff`]: a table indexed by the next `P` bits. Next to the plain entries it keeps, per
//!   index, everything needed to turn the following bits into a signed difference without
//!   branching (code length, number of difference bits, the parameters of the sign rule).
//! * [`ExactBits`]: the reference reader. A sample is read symbol first, difference bits second,
//!   with zero padding past the data and a one-way "dead" state once the stream has been overrun.
//! * Rows are decoded by a register-resident bit window (rows that provably stay inside the
//!   data) or by the exact reader (everything near or after the end).
//! * Large single-table streams may be entropy-decoded in speculative chunks ([`crate::par_entropy`]).

use crate::par_entropy::{self, EndCheck, SymbolStream};
use flashraw_core::bytes::ByteOrder;
use flashraw_core::{Error, Result, par};

/// Index width of the combined lookup used by fast paths (an implementation detail callers never see).
pub const FAST_BITS: u32 = 13;

/// Index width of the fused lookup (code and difference bits resolved by one load).
const FUSED_BITS: u32 = 13;
/// Fused entry marker: the sample needs more than [`FUSED_BITS`] bits or a split lookup.
const ESCAPE: u32 = 1 << 31;
/// Samples decoded per window load.
const GROUP: usize = 4;

/// A Huffman table in direct-lookup form (`peek_bits` bits index entries of `code_len << 8 | symbol`).
#[derive(Clone)]
pub struct DirectHuff {
    peek_bits: u32,
    /// `code_len << 8 | symbol` for each of the `1 << peek_bits` indices.
    direct: Vec<u16>,
    /// Decoded form of every entry for the windowed path; `None` when the table has code lengths
    /// above 16 or an unusual size.
    fast: Option<FastTab>,
}

/// Fused lookup: for each pattern of the next [`FUSED_BITS`] bits, either the difference (low 16
/// bits) and the total number of bits used by code and difference (bits 16..22), or [`ESCAPE`].
#[derive(Clone)]
struct FastTab {
    fused: Box<[u32; 1 << FUSED_BITS]>,
    /// Largest `code plus difference bits` over all table entries.
    max_bits: u32,
}

impl std::fmt::Debug for DirectHuff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectHuff").field("peek_bits", &self.peek_bits).finish()
    }
}

/// Difference value for a symbol (`len | shl << 4`) given the `len - shl` transmitted bits.
pub fn symbol_diff(bits: u32, sym: u32) -> i32 {
    let len = sym & 15;
    let shl = (sym >> 4) & 15;
    let mut v = (bits.wrapping_mul(2).wrapping_add(1) << shl) as i32 >> 1;
    if len != 0 && (v >> (len - 1)) & 1 == 0 {
        v = v.wrapping_sub((1i32 << len) - i32::from(shl == 0));
    }
    v
}

impl FastTab {
    fn build(peek_bits: u32, direct: &[u16]) -> Option<FastTab> {
        if peek_bits == 0 || peek_bits > 16 || direct.len() != 1usize << peek_bits {
            return None;
        }
        let bits_of = |e: u16| -> (u32, u32, u32) {
            let sym = u32::from(e & 0xff);
            (u32::from(e >> 8), sym, (sym & 15).saturating_sub(sym >> 4))
        };
        let mut max_bits = 0;
        for &e in direct {
            let (clen, _, n) = bits_of(e);
            if clen > 16 {
                return None;
            }
            max_bits = max_bits.max(clen + n);
        }
        let mut fused = Box::new([ESCAPE; 1 << FUSED_BITS]);
        for (i, slot) in fused.iter_mut().enumerate() {
            let e = if peek_bits <= FUSED_BITS {
                direct[i >> (FUSED_BITS - peek_bits)]
            } else {
                let span = 1usize << (peek_bits - FUSED_BITS);
                let group = &direct[i * span..(i + 1) * span];
                if group.iter().any(|&g| g != group[0]) {
                    continue;
                }
                group[0]
            };
            let (clen, sym, n) = bits_of(e);
            if clen + n > FUSED_BITS {
                continue;
            }
            let b = if n == 0 { 0 } else { (i as u32 >> (FUSED_BITS - clen - n)) & ((1 << n) - 1) };
            *slot = (symbol_diff(b, sym) as u16 as u32) | (clen + n) << 16;
        }
        Some(FastTab { fused, max_bits })
    }
}

impl DirectHuff {
    /// Build from 16 code-length counts followed by the symbols in code order.
    pub fn from_counts(source: &[u8]) -> Result<Self> {
        if source.len() < 16 {
            return Err(Error::corrupt("Huffman table too short"));
        }
        let counts = &source[..16];
        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        if total == 0 {
            return Err(Error::corrupt("empty Huffman table"));
        }
        let symbols = source[16..].get(..total).ok_or_else(|| Error::corrupt("Huffman table symbols truncated"))?;
        let peek_bits = counts.iter().rposition(|&c| c != 0).map_or(1, |i| i as u32 + 1);
        let size = 1usize << peek_bits;
        let mut direct = vec![0u16; size];
        let mut at = 0usize;
        let mut next_symbol = symbols.iter();
        'fill: for (i, &count) in counts.iter().enumerate() {
            let len = i as u32 + 1;
            let span = 1usize << (peek_bits - len.min(peek_bits));
            for _ in 0..count {
                let Some(&sym) = next_symbol.next() else { break 'fill };
                if at + span > size {
                    break 'fill;
                }
                direct[at..at + span].fill((len as u16) << 8 | u16::from(sym));
                at += span;
            }
        }
        Ok(Self::with_fast(peek_bits, direct))
    }

    /// Build from a direct table of `1 << peek_bits` entries (`code_len << 8 | symbol`).
    pub fn from_direct(peek_bits: u32, direct: Vec<u16>) -> Self {
        debug_assert_eq!(direct.len(), 1usize << peek_bits);
        Self::with_fast(peek_bits, direct)
    }

    fn with_fast(peek_bits: u32, direct: Vec<u16>) -> Self {
        let fast = FastTab::build(peek_bits, &direct);
        DirectHuff { peek_bits, direct, fast }
    }

    /// Number of bits that index the table.
    pub fn peek_bits(&self) -> u32 {
        self.peek_bits
    }
}

/// 64 bits of `data` starting at byte `at`, zero padded past the end.
#[inline(always)]
fn load_be64(data: &[u8], at: usize) -> u64 {
    match data.get(at..).and_then(|t| t.first_chunk::<8>()) {
        Some(b) => u64::from_be_bytes(*b),
        None => {
            let mut b = [0u8; 8];
            if let Some(t) = data.get(at..) {
                b[..t.len()].copy_from_slice(t);
            }
            u64::from_be_bytes(b)
        }
    }
}

/// MSB-first bit reader with exact end-of-data semantics.
#[derive(Clone)]
pub struct ExactBits<'a> {
    data: &'a [u8],
    pos: u64,
    dead: bool,
    dead_row: Option<usize>,
}

impl<'a> ExactBits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        ExactBits { data, pos: 0, dead: false, dead_row: None }
    }

    /// Bits consumed so far.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Whether the stream ran past the end of its data.
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// First row (of a [`decode_predictive`] call) that needed bits past the end of the data.
    pub fn dead_row(&self) -> Option<usize> {
        self.dead_row
    }

    #[inline(always)]
    fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let w = load_be64(self.data, (self.pos >> 3) as usize) << (self.pos & 7);
        (w >> (64 - n.min(32))) as u32
    }

    #[inline(always)]
    fn consume(&mut self, n: u32) {
        self.pos += u64::from(n);
        if self.pos > self.data.len() as u64 * 8 {
            self.dead = true;
        }
    }

    /// Read `n <= 25` bits.
    pub fn get(&mut self, n: u32) -> u32 {
        if n == 0 || self.dead {
            return 0;
        }
        let v = self.peek(n);
        self.consume(n);
        v
    }

    /// Decode one symbol.
    pub fn huff(&mut self, t: &DirectHuff) -> u32 {
        if self.dead {
            return 0;
        }
        let e = t.direct.get(self.peek(t.peek_bits) as usize).copied().unwrap_or(0);
        self.consume(u32::from(e >> 8));
        u32::from(e & 0xff)
    }

    /// Decode one symbol and its difference.
    pub fn diff(&mut self, t: &DirectHuff) -> i32 {
        let sym = self.huff(t);
        let n = (sym & 15).saturating_sub(sym >> 4);
        let b = self.get(n);
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

/// 64 bits of `data` at bit `pos`, the first `57` of them valid. The caller guarantees the bytes exist.
#[inline(always)]
fn window_at(data: &[u8], pos: u64) -> u64 {
    let byte = (pos >> 3) as usize;
    u64::from_be_bytes(*data[byte..].first_chunk::<8>().unwrap()) << (pos & 7)
}

/// Difference and bit count of a sample the fused table cannot resolve; `w` starts at the code.
#[cold]
#[inline(never)]
fn escape_sample(t: &DirectHuff, w: u64) -> (u16, u32) {
    let e = t.direct[(w >> (64 - t.peek_bits)) as usize];
    let clen = u32::from(e >> 8);
    let sym = u32::from(e & 0xff);
    let n = (sym & 15).saturating_sub(sym >> 4);
    let b = if n == 0 { 0 } else { ((w << clen) >> (64 - n)) as u32 };
    (symbol_diff(b, sym) as u16, clen + n)
}

/// Decode `N <= GROUP` samples whose columns start at an even column, from one window load.
#[inline(always)]
fn decode_group<M: Fn(u16) -> u16, const N: usize>(
    data: &[u8],
    t: &DirectHuff,
    f: &FastTab,
    pos: &mut u64,
    h: &mut [u16; 2],
    out: &mut [u16; N],
    map: &M,
) {
    let mut w = window_at(data, *pos);
    let mut used = 0u32;
    for (j, o) in out.iter_mut().enumerate() {
        let e = f.fused[(w >> (64 - FUSED_BITS)) as usize];
        let c = j & 1;
        if e & ESCAPE != 0 {
            *pos += u64::from(used);
            let (d, bits) = escape_sample(t, window_at(data, *pos));
            *pos += u64::from(bits);
            w = window_at(data, *pos);
            used = 0;
            h[c] = h[c].wrapping_add(d);
        } else {
            h[c] = h[c].wrapping_add(e as u16);
            let bits = e >> 16;
            w <<= bits;
            used += bits;
        }
        *o = map(h[c]);
    }
    *pos += u64::from(used);
}

/// Decode one row inside the data with the windowed reader; returns the bit position after it.
fn row_windowed<M: Fn(u16) -> u16>(
    data: &[u8],
    pos: u64,
    t: &DirectHuff,
    f: &FastTab,
    vp: &mut [u16; 2],
    out: &mut [u16],
    map: &M,
) -> u64 {
    let mut pos = pos;
    let mut h = *vp;
    let (head, rest) = out.split_first_chunk_mut::<2>().unwrap();
    decode_group(data, t, f, &mut pos, &mut h, head, map);
    *vp = h;
    let mut groups = rest.chunks_exact_mut(GROUP);
    for g in &mut groups {
        decode_group(data, t, f, &mut pos, &mut h, g.first_chunk_mut::<GROUP>().unwrap(), map);
    }
    let tail = groups.into_remainder();
    if let Some((a, tail)) = tail.split_first_chunk_mut::<2>() {
        decode_group(data, t, f, &mut pos, &mut h, a, map);
        if let [x] = tail {
            decode_group(data, t, f, &mut pos, &mut h, std::slice::from_mut(x).first_chunk_mut::<1>().unwrap(), map);
        }
    } else if let [x] = tail {
        decode_group(data, t, f, &mut pos, &mut h, std::slice::from_mut(x).first_chunk_mut::<1>().unwrap(), map);
    }
    pos
}

/// Decode one row with the exact reader.
fn row_exact<M: Fn(u16) -> u16>(bits: &mut ExactBits<'_>, t: &DirectHuff, vp: &mut [u16; 2], out: &mut [u16], map: &M) {
    let mut h = [0u16; 2];
    for (col, o) in out.iter_mut().enumerate() {
        let d = bits.diff(t) as u16;
        let c = col & 1;
        if col < 2 {
            vp[c] = vp[c].wrapping_add(d);
            h[c] = vp[c];
        } else {
            h[c] = h[c].wrapping_add(d);
        }
        *o = map(h[c]);
    }
}

fn validate(p: &PredictiveParams, out_len: usize, stride: usize) -> Result<()> {
    let width = p.width;
    if width < 2 || stride < width {
        return Err(Error::InvalidArgument("predictive Huffman: invalid width".into()));
    }
    if p.rows > 0 {
        let need = (p.rows - 1).checked_mul(stride).and_then(|v| v.checked_add(width));
        if need.is_none_or(|n| out_len < n) {
            return Err(Error::InvalidArgument("predictive Huffman: output too small".into()));
        }
    }
    match p.tables.first() {
        None => Err(Error::InvalidArgument("no Huffman table".into())),
        Some(&(start, _)) if start != 0 => {
            Err(Error::InvalidArgument("first Huffman table must start at row 0".into()))
        }
        Some(_) => Ok(()),
    }
}

/// Decode a predictive Huffman image into `out` (rows of `stride` samples), mapping each
/// reconstructed predictor value through `map`. Returns the reader so callers can inspect truncation.
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
    validate(p, out.len(), stride)?;
    let width = p.width;
    let mut bits = ExactBits::new(src);
    let mut vpred = p.vpred;
    let end_bits = src.len() as u64 * 8;
    for r in 0..p.rows {
        let table = p.tables.iter().rfind(|(start, _)| *start <= r).map_or(p.tables[0].1, |t| t.1);
        let row = &mut out[r * stride..r * stride + width];
        let vp = &mut vpred[r & 1];
        if bits.dead {
            for (col, o) in row.iter_mut().enumerate() {
                *o = map(vp[col & 1]);
            }
            continue;
        }
        match &table.fast {
            Some(f) if bits.pos + (width as u64 + 1) * u64::from(f.max_bits) + 128 <= end_bits => {
                bits.pos = row_windowed(src, bits.pos, table, f, vp, row, &map);
            }
            _ => {
                row_exact(&mut bits, table, vp, row, &map);
                if bits.dead && bits.dead_row.is_none() {
                    bits.dead_row = Some(r);
                }
            }
        }
    }
    Ok(bits)
}

/// Speculative view of the stream for [`par_entropy`]: one difference per symbol.
struct DiffStream<'a> {
    data: &'a [u8],
    table: &'a DirectHuff,
    fast: &'a FastTab,
    span_bits: u64,
}

impl SymbolStream for DiffStream<'_> {
    type Pump<'p>
        = u64
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
        self.data.len() as u64 * 8
    }
    fn span_bits(&self) -> u64 {
        self.span_bits
    }
    fn end_check(&self, pos: u64) -> EndCheck {
        if pos <= self.total_bits() { EndCheck::Valid } else { EndCheck::Unknown }
    }
    fn pump_at(&self, bit: u64) -> u64 {
        bit
    }
    fn position(p: &u64) -> u64 {
        *p
    }
    #[inline(always)]
    fn decode(&self, p: &mut u64, _phase: usize, out: &mut [u16]) -> bool {
        let w = load_be64(self.data, (*p >> 3) as usize) << (*p & 7);
        let e = self.fast.fused[(w >> (64 - FUSED_BITS)) as usize];
        let (d, bits) = if e & ESCAPE != 0 { escape_sample(self.table, w) } else { (e as u16, e >> 16) };
        out[0] = d;
        // A sample that consumes nothing is not a decodable position.
        let ok = bits != 0;
        *p += u64::from(bits.max(u32::from(!ok)));
        ok
    }
}

/// Rebuild the image from the differences recovered by speculation. Returns false when the
/// geometry does not fit (the caller then decodes sequentially).
fn rebuild_parallel<M>(vals: &par_entropy::Values, p: &PredictiveParams, out: &mut [u16], stride: usize, map: &M)
where
    M: Fn(u16) -> u16 + Sync,
{
    let (width, rows) = (p.width, p.rows);
    // The only coupling between rows: the first two samples follow the row two above.
    let mut vpred = p.vpred;
    let mut starts = Vec::with_capacity(rows);
    for r in 0..rows {
        let vp = &mut vpred[r & 1];
        vp[0] = vp[0].wrapping_add(vals.get(r * width));
        vp[1] = vp[1].wrapping_add(vals.get(r * width + 1));
        starts.push(*vp);
    }
    let used = (rows - 1) * stride + width;
    let per_task = rows.div_ceil(par::threads() * 4).max(1);
    par::for_each_row_chunk(&mut out[..used], stride, per_task, true, |first, chunk| {
        let mut diffs = vec![0u16; width];
        for (i, row) in chunk.chunks_mut(stride).enumerate() {
            let r = first + i;
            vals.copy_to(r * width, &mut diffs);
            let [mut h0, mut h1] = starts[r];
            row[0] = map(h0);
            row[1] = map(h1);
            for (o, d) in row[2..width].chunks_mut(2).zip(diffs[2..].chunks(2)) {
                h0 = h0.wrapping_add(d[0]);
                o[0] = map(h0);
                if let (Some(oo), Some(&dd)) = (o.get_mut(1), d.get(1)) {
                    h1 = h1.wrapping_add(dd);
                    *oo = map(h1);
                }
            }
        }
    });
}

/// [`decode_predictive`] that may entropy-decode large single-table streams on all threads when
/// `parallel` is set; output and result are identical. Returns the first row that ran past its data.
/// `declared` is the payload length the container states (`src` may run on to the end of the file).
pub fn decode_predictive_par<M>(
    src: &[u8],
    p: &PredictiveParams,
    out: &mut [u16],
    stride: usize,
    map: M,
    parallel: bool,
    declared: usize,
) -> Result<Option<usize>>
where
    M: Fn(u16) -> u16 + Sync,
{
    validate(p, out.len(), stride)?;
    let fast_table = if parallel
        && p.tables.len() == 1
        && p.rows.saturating_mul(p.width) >= crate::ljpeg_par::PARALLEL_MIN_SAMPLES
    {
        p.tables[0].1.fast.as_ref()
    } else {
        None
    };
    if let Some(fast) = fast_table {
        let total_bits = src.len() as u64 * 8;
        let span_bits = if declared == 0 {
            total_bits
        } else {
            ((declared + declared / 16 + (64 << 10)) as u64 * 8).min(total_bits)
        };
        let stream = DiffStream { data: src, table: p.tables[0].1, fast, span_bits };
        let symbols = p.rows * p.width;
        let decoded = par_entropy::default_chunk((span_bits / 8) as usize)
            .and_then(|chunk| par_entropy::decode(&stream, symbols, chunk));
        if let Some(vals) = decoded {
            rebuild_parallel(&vals, p, out, stride, &map);
            return Ok(None);
        }
    }
    decode_predictive(src, p, out, stride, map).map(|b| b.dead_row())
}

/// The six Nikon Huffman tables (16 counts, then symbols); see tables.json. A table may list
/// fewer symbols than its counts announce: the missing ones are zeros, added by [`nikon_table`].
pub const NIKON_TREES: [&[u8]; 6] = [
    &[
        0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x04,
        0x03, 0x06, 0x02, 0x07, 0x01, 0x00, 0x08, 0x09, 0x0b, 0x0a, 0x0c,
    ],
    &[
        0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x39, 0x5a,
        0x38, 0x27, 0x16, 0x05, 0x04, 0x03, 0x02, 0x01, 0x00, 0x0b, 0x0c, 0x0c,
    ],
    &[
        0x00, 0x01, 0x04, 0x02, 0x03, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x04,
        0x06, 0x03, 0x07, 0x02, 0x08, 0x01, 0x09, 0x00, 0x0a, 0x0b, 0x0c,
    ],
    &[
        0x00, 0x01, 0x04, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x06,
        0x04, 0x07, 0x08, 0x03, 0x09, 0x02, 0x01, 0x00, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
    ],
    &[
        0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x5c,
        0x4b, 0x3a, 0x29, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x00, 0x0d, 0x0e,
    ],
    &[
        0x00, 0x01, 0x04, 0x02, 0x02, 0x03, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x06,
        0x08, 0x05, 0x09, 0x04, 0x0a, 0x03, 0x0b, 0x0c, 0x02, 0x00, 0x01, 0x0d, 0x0e,
    ],
];

/// Build the decoding table of `NIKON_TREES[index]`, appending the zero symbols that the stored
/// form leaves out.
pub fn nikon_table(index: usize) -> Result<DirectHuff> {
    let stored = NIKON_TREES.get(index).copied().ok_or_else(|| Error::corrupt("unknown Nikon Huffman table"))?;
    let announced: usize = stored.iter().take(16).map(|&c| usize::from(c)).sum();
    let mut full = stored.to_vec();
    if full.len() < 16 + announced {
        full.resize(16 + announced, 0);
    }
    DirectHuff::from_counts(&full)
}

/// Decoded Nikon compression metadata (makernote tag 0x96 "NEFLinearizationTable").
#[derive(Debug, Clone)]
pub struct NikonMeta {
    /// The first two bytes of the tag.
    pub version: [u8; 2],
    pub vpred: [[u16; 2]; 2],
    /// Index into [`NIKON_TREES`] of the main table.
    pub tree: usize,
    /// Row at which the next table takes over (0 = none).
    pub split_row: usize,
    /// Linearisation curve indexed by the clamped predictor value (0x4000 entries).
    pub curve: Vec<u16>,
}

const CURVE_LEN: usize = 0x4000;

/// Versions that store the curve as sparse control points to be interpolated: 0x44 0x20 keeps
/// one point per `step` input values, and 0x44 0x40 keeps them at quarter resolution, i.e.
/// one per `step / 4` input values of a curve a quarter as long. Returns the factor by which
/// step and curve length shrink (1 or 4), or `None` when the points are stored one per value.
/// A quarter-resolution curve needs `step >= 4`, since a smaller step would shrink to zero.
fn stored_curve_divisor(ver0: u8, ver1: u8, step: usize) -> Option<usize> {
    let full_resolution = ver1 == 0x20 && step > 0;
    let quarter_resolution = ver1 == 0x40 && step > 3;
    match (ver0 == 0x44, full_resolution, quarter_resolution) {
        (true, true, _) => Some(1),
        (true, _, true) => Some(4),
        _ => None,
    }
}

impl NikonMeta {
    /// Parse the blob at makernote tag 0x96 (or 0x8C); `meta` starts at the tag's value and may run
    /// to the end of the file.
    pub fn parse(meta: &[u8], order: ByteOrder, bits: u32) -> Result<Self> {
        let rd = |off: usize| -> Result<u16> {
            match meta.get(off..off + 2) {
                Some(b) => Ok(order.u16([b[0], b[1]])),
                None => Err(Error::corrupt("truncated Nikon linearization table")),
            }
        };
        if bits != 12 && bits != 14 {
            return Err(Error::unsupported_compression("NEF", format!("{bits}-bit Nikon Huffman")));
        }
        let ver0 = *meta.first().ok_or_else(|| Error::corrupt("empty Nikon linearization table"))?;
        let ver1 = *meta.get(1).ok_or_else(|| Error::corrupt("truncated Nikon linearization table"))?;
        let mut pos = 2usize;
        if ver0 == 0x49 || ver1 == 0x58 {
            pos += 2110;
        }
        let vpred = [[rd(pos)?, rd(pos + 2)?], [rd(pos + 4)?, rd(pos + 6)?]];
        pos += 8;
        let tree = if ver0 == 0x46 { 2 } else { 0 } + if bits == 14 { 3 } else { 0 };
        let csize = usize::from(rd(pos)?);
        pos += 2;

        let mut work: Vec<u16> = (0..=u16::MAX).collect();
        let mut maxv = (1usize << bits) & 0x7fff;
        let mut step = if csize > 1 { maxv / (csize - 1) } else { maxv };
        if let Some(divisor) = stored_curve_divisor(ver0, ver1, step) {
            step /= divisor;
            maxv /= divisor;
            for i in 0..csize {
                let v = rd(pos + 2 * i)?;
                if let Some(slot) = work.get_mut(i * step) {
                    *slot = v;
                }
            }
            for i in 0..maxv {
                let rem = i % step;
                let base = i - rem;
                let lo = u64::from(work[base]);
                let hi = work.get(base + step).map_or(0, |&v| u64::from(v));
                work[i] = ((lo * (step - rem) as u64 + hi * rem as u64) / step as u64) as u16;
            }
        } else if ver0 != 0x46 && csize <= CURVE_LEN + 1 {
            for i in 0..csize {
                work[i] = rd(pos + 2 * i)?;
            }
        }
        let split_row = if ver0 == 0x44 && (ver1 == 0x20 || ver1 == 0x40) { usize::from(rd(562)?) } else { 0 };
        work.truncate(CURVE_LEN);
        Ok(NikonMeta { version: [ver0, ver1], vpred, tree, split_row, curve: work })
    }

    /// Whether this is a lossless stream (identity mapping).
    pub fn is_lossless(&self) -> bool {
        self.version[0] == 0x46
    }

    /// Largest value of the curve.
    pub fn curve_max(&self) -> u16 {
        self.curve.iter().copied().max().unwrap_or(0)
    }
}

/// Decode a Nikon Huffman payload; a truncated stream is an error ([`check_complete`]).
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
) -> Result<()> {
    let dead_row = decode_nikon_rows(src, meta, width, rows, out, stride, parallel, declared)?;
    check_complete(dead_row, rows)
}

/// Like [`decode_nikon`] but returns the row in which the data ended instead of failing.
#[allow(clippy::too_many_arguments)]
pub fn decode_nikon_rows(
    src: &[u8],
    meta: &NikonMeta,
    width: usize,
    rows: usize,
    out: &mut [u16],
    stride: usize,
    parallel: bool,
    declared: usize,
) -> Result<Option<usize>> {
    let first = nikon_table(meta.tree)?;
    let second =
        if meta.split_row > 0 && meta.tree + 1 < NIKON_TREES.len() { Some(nikon_table(meta.tree + 1)?) } else { None };
    let curve: &[u16; CURVE_LEN] =
        meta.curve.as_slice().try_into().map_err(|_| Error::corrupt("Nikon curve has the wrong size"))?;
    let tables: Vec<(usize, &DirectHuff)> = match &second {
        Some(t) => vec![(0, &first), (meta.split_row, t)],
        None => vec![(0, &first)],
    };
    let params = PredictiveParams { width, rows, tables: &tables, vpred: meta.vpred };
    let map = |h: u16| curve[(h as i16).clamp(0, 0x3fff) as usize];
    decode_predictive_par(src, &params, out, stride, map, parallel, declared)
}

/// `Err(CorruptFile)` when the data ended before the last row.
pub fn check_complete(dead_row: Option<usize>, rows: usize) -> Result<()> {
    match dead_row {
        Some(r) if r + 1 < rows => {
            Err(Error::corrupt(format!("compressed data ends at row {r}/{rows} (truncated file)")))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
#[path = "nikon_huffman_tests.rs"]
mod tests;
