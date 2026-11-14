//! Samsung SRW compressed-sensor decoders.
//!
//! | TIFF compression | name | summary |
//! |---|---|---|
//! | 32770 with tag 40976 | V0 | per-row offsets, 16-sample blocks, 2x2-transposed row pairs |
//! | 32772 | V1 | one predictive Huffman stream with a fixed 10-bit prefix code |
//! | 32773 | V2 | 16-byte aligned rows, 16-sample blocks with motion-referenced prediction |
//!
//! V0 and V2 read their bits from a stream of little-endian 32-bit words whose bits are taken from
//! the most significant end. A word that is not completely inside the input reads as all ones.

use crate::nikon_huffman::{DirectHuff, PredictiveParams, decode_predictive};
use flashraw_core::{Error, Result, par};

// ---------------------------------------------------------------------------------------------
// Word bit stream
// ---------------------------------------------------------------------------------------------

/// MSB-first reader over little-endian 32-bit words. Words are fetched one at a time, and only
/// when a read needs more bits than are buffered, so `p` is always the byte position right after
/// the last word a stream of the consumed length needs.
struct Bits<'a> {
    src: &'a [u8],
    /// Byte position of the next word to fetch.
    p: usize,
    /// Buffered bits, left aligned (bits below `cnt` are zero).
    acc: u64,
    cnt: u32,
    /// A word was fetched that lies (partly) outside `src`.
    over: bool,
}

impl<'a> Bits<'a> {
    #[inline]
    fn at(src: &'a [u8], p: usize) -> Self {
        Bits { src, p, acc: 0, cnt: 0, over: false }
    }

    /// Drop the buffered bits and continue with the word at byte position `p`.
    #[inline]
    fn restart(&mut self, p: usize) {
        self.p = p;
        self.acc = 0;
        self.cnt = 0;
    }

    #[cold]
    #[inline(never)]
    fn refill_slow(&mut self) -> u32 {
        self.over = true;
        !0
    }

    #[inline(always)]
    fn refill(&mut self) {
        let w = match self.src.get(self.p..).and_then(|s| s.first_chunk::<4>()) {
            Some(b) => u32::from_le_bytes(*b),
            None => self.refill_slow(),
        };
        self.acc |= u64::from(w) << (32 - self.cnt);
        self.cnt += 32;
        self.p = self.p.wrapping_add(4);
    }

    /// The next `n` bits, `1 <= n <= 32`.
    #[inline(always)]
    fn get(&mut self, n: u32) -> u32 {
        debug_assert!((1..=32).contains(&n));
        if self.cnt < n {
            self.refill();
        }
        let v = (self.acc >> (64 - n)) as u32;
        self.acc <<= n;
        self.cnt -= n;
        v
    }
}

/// Sign extension of the low `n` bits of `v` (`1 <= n <= 32`).
#[inline(always)]
fn sext(v: u32, n: u32) -> i32 {
    let s = 32 - n;
    ((v << s) as i32) >> s
}

// ---------------------------------------------------------------------------------------------
// V0
// ---------------------------------------------------------------------------------------------

/// Decode Samsung V0 (compression 32770 with a row offset table).
///
/// `row_offsets[row]` is the byte offset of the row's bit stream within `src`. `out` receives
/// `width * height` samples and keeps its previous value where the format skips a sample.
pub fn decode_v0_with(
    src: &[u8],
    row_offsets: &[u32],
    width: usize,
    height: usize,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    let total = width
        .checked_mul(height)
        .filter(|&t| out.len() >= t)
        .ok_or_else(|| Error::InvalidArgument("Samsung v0: buffer or offsets too small".into()))?;
    if row_offsets.len() < height {
        return Err(Error::InvalidArgument("Samsung v0: buffer or offsets too small".into()));
    }
    if width == 0 || height == 0 {
        return Ok(());
    }
    let out = &mut out[..total];
    // Blocks of 16 samples spill into the next row when the width is not a multiple of 16 (and
    // across several rows when it is below 16). Rows 0 and 1 (which skip samples) and narrow
    // images are decoded with the flat-buffer model; wider images need no spill from row 2 on.
    let flat_rows = if width >= 16 { height.min(2) } else { height };
    for r in 0..flat_rows {
        flat_row(src, row_offsets[r] as usize, r, width, out);
    }
    if flat_rows < height {
        let nblk = width.div_ceil(16);
        let rows = height - flat_rows;
        let mut dirs = vec![0u8; nblk * rows];
        let (head, tail) = out.split_at_mut(flat_rows * width);
        // Pass 1: residuals of every row (independent bit streams).
        const ROWS_PER_TASK: usize = 8;
        let items: Vec<(usize, &mut [u16], &mut [u8])> = tail
            .chunks_mut(width * ROWS_PER_TASK)
            .zip(dirs.chunks_mut(nblk * ROWS_PER_TASK))
            .enumerate()
            .map(|(i, (o, d))| (flat_rows + i * ROWS_PER_TASK, o, d))
            .collect();
        par::map(items, parallel, |(first, o, d)| {
            for (i, (row, dr)) in o.chunks_exact_mut(width).zip(d.chunks_exact_mut(nblk)).enumerate() {
                parse_row(src, row_offsets[first + i] as usize, row, dr);
            }
        });
        // Pass 2: add the predictions (rows r-1 and r-2 are final before row r).
        let geo = par::Wavefront { row_len: width, halo: 2, margin: 0, block: 16 * 32 };
        let dirs = &dirs;
        let top = &*head;
        par::wavefront(tail, top, geo, parallel, |first, band, above, lo, a, b| {
            reconstruct(band, above, lo, a, b, width, dirs, nblk, first);
        });
    }
    transpose_pairs(out, width, parallel);
    Ok(())
}

/// Residual bit length every group of a row starts with: the first two rows use longer codes.
const V0_FIRST_ROWS: usize = 2;
const V0_LEN_FIRST_ROWS: i32 = 7;
const V0_LEN_OTHER_ROWS: i32 = 4;

/// Start value of the four bit-length registers.
#[inline]
fn initial_lengths(row: usize) -> [i32; 4] {
    [if row < V0_FIRST_ROWS { V0_LEN_FIRST_ROWS } else { V0_LEN_OTHER_ROWS }; 4]
}

/// Change of one bit-length register, from its 2-bit code: keep, grow by one, shrink by one, or
/// replace by an explicit 4-bit value.
#[inline(always)]
fn apply_length_code(len: &mut i32, code: u32, bits: &mut Bits) {
    *len = match code {
        0 => *len,
        1 => *len + 1,
        2 => *len - 1,
        _ => bits.get(4) as i32,
    };
}

/// Block header: direction bit, then the update of the four length registers.
#[inline(always)]
fn block_header(bits: &mut Bits, len: &mut [i32; 4]) -> bool {
    let up = bits.get(1) != 0;
    let codes = [bits.get(2), bits.get(2), bits.get(2), bits.get(2)];
    for (l, code) in len.iter_mut().zip(codes) {
        apply_length_code(l, code, bits);
    }
    up
}

/// First sample of each group of four in a block (even then odd positions, low then high half).
const GROUP_START: [usize; 4] = [0, 8, 1, 9];

/// Decode one row with the flat-buffer model: samples are addressed by their linear index and may
/// land in the following row(s). A prediction source above the first rows is skipped (no bits are
/// read for that sample); stores and predictions outside the buffer are dropped and read as 0.
/// There is no other limit on the index: the only bound is the buffer itself.
fn flat_row(src: &[u8], off: usize, r: usize, w: usize, out: &mut [u16]) {
    let total = out.len();
    let mut bits = Bits::at(src, off);
    let mut len = initial_lengths(r);
    let mut col = 0;
    while col < w {
        let dir = block_header(&mut bits, &mut len);
        for (g, &start) in GROUP_START.iter().enumerate() {
            let n = len[g];
            for t in 0..4 {
                let c = start + 2 * t;
                let d = r * w + col + c;
                let src_idx = if dir {
                    let up = if c % 2 == 0 { 1 } else { 2 };
                    if r < up {
                        continue;
                    }
                    Some((r - up) * w + col + c)
                } else if col > 0 {
                    Some(r * w + col - if c % 2 == 0 { 2 } else { 1 })
                } else {
                    None
                };
                let delta = if n > 0 {
                    let n = n.min(32) as u32;
                    sext(bits.get(n), n)
                } else {
                    0
                };
                let pred = match src_idx {
                    None => 128,
                    Some(s) if s < total => i32::from(out[s]),
                    Some(_) => 0,
                };
                if d < total {
                    out[d] = delta.wrapping_add(pred) as u16;
                }
            }
        }
        col += 16;
    }
}

/// Read one block's residuals (as the low 16 bits of the signed value) into `blk`.
#[inline(always)]
fn parse_block(bits: &mut Bits, len: &[i32; 4], blk: &mut [u16; 16]) {
    for (g, &start) in GROUP_START.iter().enumerate() {
        let n = len[g];
        if n > 0 {
            let n = n.min(32) as u32;
            for t in 0..4 {
                blk[start + 2 * t] = sext(bits.get(n), n) as u16;
            }
        } else {
            for t in 0..4 {
                blk[start + 2 * t] = 0;
            }
        }
    }
}

/// Residuals of one row (index >= 2, width >= 16) into `row`; one direction byte per block.
fn parse_row(src: &[u8], off: usize, row: &mut [u16], dirs: &mut [u8]) {
    let mut bits = Bits::at(src, off);
    let mut len = [4i32; 4];
    for (b, dir) in dirs.iter_mut().enumerate() {
        *dir = block_header(&mut bits, &mut len) as u8;
        let col = b * 16;
        match row.get_mut(col..col + 16) {
            Some(s) => parse_block(&mut bits, &len, s.try_into().unwrap()),
            None => {
                let mut tmp = [0u16; 16];
                parse_block(&mut bits, &len, &mut tmp);
                let m = row.len() - col;
                row[col..].copy_from_slice(&tmp[..m]);
            }
        }
    }
}

/// Turn residuals into samples for columns `a..b` of every row of `band`.
///
/// `above` holds the two rows before the band, columns `lo..b` (`lo == a`).
#[allow(clippy::too_many_arguments)]
fn reconstruct(
    band: &mut [u16],
    above: &[u16],
    lo: usize,
    a: usize,
    b: usize,
    w: usize,
    dirs: &[u8],
    nblk: usize,
    first: usize,
) {
    let span = b - lo;
    let nrows = band.len() / w;
    for i in 0..nrows {
        let (done, rest) = band.split_at_mut(i * w);
        let cur = &mut rest[..w];
        let row_dirs = &dirs[(first + i) * nblk..][..nblk];
        // Rows r-2 and r-1, columns a..b.
        let up2: &[u16] =
            if i >= 2 { &done[(i - 2) * w + a..][..b - a] } else { &above[i * span + (a - lo)..][..b - a] };
        let up1: &[u16] = if i >= 1 { &done[(i - 1) * w + a..][..b - a] } else { &above[span + (a - lo)..][..b - a] };
        let mut col = a;
        while col < b {
            let dir = row_dirs[col / 16] != 0;
            if col + 16 <= b {
                let blk: &mut [u16; 16] = (&mut cur[col..col + 16]).try_into().unwrap();
                if dir {
                    // Even positions follow the row above, odd ones the row two above.
                    let refs: [&[u16; 16]; 2] =
                        [up1[col - a..][..16].try_into().unwrap(), up2[col - a..][..16].try_into().unwrap()];
                    for (j, v) in blk.iter_mut().enumerate() {
                        *v = v.wrapping_add(refs[j & 1][j]);
                    }
                } else if col > 0 {
                    let (x, y) = (cur[col - 2], cur[col - 1]);
                    let blk: &mut [u16; 16] = (&mut cur[col..col + 16]).try_into().unwrap();
                    let left = [x, y];
                    for (j, v) in blk.iter_mut().enumerate() {
                        *v = v.wrapping_add(left[j & 1]);
                    }
                } else {
                    for v in blk.iter_mut() {
                        *v = v.wrapping_add(128);
                    }
                }
            } else {
                // Last, partial block of the row.
                let m = b - col;
                let (x, y) = if col > 0 { (cur[col - 2], cur[col - 1]) } else { (128, 128) };
                for c in 0..m {
                    let p = if dir {
                        if c % 2 == 0 { up1[col - a + c] } else { up2[col - a + c] }
                    } else if c % 2 == 0 {
                        x
                    } else {
                        y
                    };
                    cur[col + c] = cur[col + c].wrapping_add(p);
                }
            }
            col += 16;
        }
    }
}

/// Exchange, in every 2x2 cell of each row pair, the two off-diagonal samples.
fn transpose_pairs(out: &mut [u16], w: usize, parallel: bool) {
    let h = out.len() / w;
    let pairs = h / 2;
    if pairs == 0 || w < 2 {
        return;
    }
    par::for_each_row_chunk(&mut out[..pairs * 2 * w], w, 64, parallel, |_, chunk| {
        for pair in chunk.chunks_exact_mut(2 * w) {
            let (r0, r1) = pair.split_at_mut(w);
            for (p, q) in r0.chunks_exact_mut(2).zip(r1.chunks_exact_mut(2)) {
                std::mem::swap(&mut p[1], &mut q[0]);
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------
// V1
// ---------------------------------------------------------------------------------------------

/// V1 difference sizes (in bits) in the order their prefix codes are assigned.
const V1_ORDER: [u16; 14] = [4, 7, 6, 5, 3, 0, 9, 10, 11, 12, 13, 1, 8, 2];

/// Prefix-code length (1 to 10 bits) of each V1 difference size, indexed by that size.
const V1_LENGTH_OF: [u16; 14] = [6, 5, 4, 4, 3, 2, 2, 3, 4, 7, 8, 9, 10, 10];

/// The `(code length, difference bits)` pairs of the V1 code in assignment order.
const fn v1_code() -> [(u16, u16); 14] {
    let mut t = [(0, 0); 14];
    let mut k = 0;
    while k < 14 {
        t[k] = (V1_LENGTH_OF[V1_ORDER[k] as usize], V1_ORDER[k]);
        k += 1;
    }
    t
}

/// Fixed prefix code of V1: (code length, difference bits), in code-assignment order.
const V1_CODE: [(u16, u16); 14] = v1_code();

/// Decode Samsung V1 (compression 32772). Returns the row in which the data ran out
/// (`None` when the stream was complete).
pub fn decode_v1(src: &[u8], width: usize, height: usize, out: &mut [u16]) -> Result<Option<usize>> {
    let mut direct = Vec::with_capacity(1024);
    for &(code_len, diff_bits) in &V1_CODE {
        direct.resize(direct.len() + (1usize << (10 - code_len)), (code_len << 8) | diff_bits);
    }
    let table = DirectHuff::from_direct(10, direct);
    let params = PredictiveParams { width, rows: height, tables: &[(0, &table)], vpred: [[0; 2]; 2] };
    Ok(decode_predictive(src, &params, out, width, |v| v)?.dead_row())
}

// ---------------------------------------------------------------------------------------------
// V2
// ---------------------------------------------------------------------------------------------

/// Number of motion selectors (0..=6; 7 means "no motion": predict from the left neighbours).
const MOTION_KINDS: usize = 7;

/// The motion reference of a selector as a neighbourhood: `(dx, weight)` taps on the same
/// relative position of the rows above (which rows depends on the row parity and the sample
/// position, see `v2_row`), `dx` columns away. The weights sum to 2, so the reference is
/// `(sum(weight * sample) + 1) >> 1`: a single tap is the sample itself, two taps their rounded mean.
const MOTION_TAPS: [&[(isize, u32)]; MOTION_KINDS] =
    [&[(-4, 2)], &[(-2, 2)], &[(-2, 1), (0, 1)], &[(0, 2)], &[(0, 1), (2, 1)], &[(2, 2)], &[(4, 2)]];

/// Column of the first (leftmost) tap, per selector.
const MOTION_SHIFT: [isize; MOTION_KINDS] = {
    let mut t = [0; MOTION_KINDS];
    let mut m = 0;
    while m < MOTION_KINDS {
        t[m] = MOTION_TAPS[m][0].0;
        m += 1;
    }
    t
};

/// Distance from the first tap to the second one; 0 when the reference has a single tap.
const MOTION_SPAN: [isize; MOTION_KINDS] = {
    let mut t = [0; MOTION_KINDS];
    let mut m = 0;
    while m < MOTION_KINDS {
        let taps = MOTION_TAPS[m];
        let mut weight = 0;
        let mut k = 0;
        while k < taps.len() {
            weight += taps[k].1;
            k += 1;
        }
        assert!(weight == 2 && taps.len() <= 2);
        if taps.len() == 2 {
            t[m] = taps[1].0 - taps[0].0;
        }
        m += 1;
    }
    t
};

/// What the decoder uses a V2 header field for.
#[derive(Clone, Copy)]
enum V2Use {
    /// Sample bit depth minus one.
    BitDepthLess1,
    /// Option flags (bit 0: lengths always coded, bit 1: 1-bit motion, bit 2: no scale updates).
    Options,
    /// Prediction of the first block of every row.
    InitialPrediction,
}

/// A field of the fixed 128-bit V2 header.
struct V2Field {
    /// Bit offset from the start of the stream.
    offset: u32,
    bits: u32,
    /// `None`: not used by the decoder.
    used_for: Option<V2Use>,
}

const fn field(offset: u32, bits: u32, used_for: Option<V2Use>) -> V2Field {
    V2Field { offset, bits, used_for }
}

/// Layout of the V2 header; each row starts at the next 16-byte boundary.
const V2_HEADER: [V2Field; 13] = [
    field(0, 16, None),
    field(16, 4, None),
    field(20, 4, Some(V2Use::BitDepthLess1)),
    field(24, 8, None),
    field(32, 16, None),
    field(48, 16, None),
    field(64, 16, None),
    field(80, 4, None),
    field(84, 4, Some(V2Use::Options)),
    field(88, 16, None),
    field(104, 8, None),
    field(112, 2, None),
    field(114, 14, Some(V2Use::InitialPrediction)),
];

/// The used header values.
struct V2Header {
    depth_less_1: u32,
    options: u32,
    initial: u16,
}

/// Read every header field in order and keep the ones that are used.
fn read_v2_header(bits: &mut Bits) -> V2Header {
    let mut h = V2Header { depth_less_1: 0, options: 0, initial: 0 };
    let mut consumed = 0;
    for f in &V2_HEADER {
        debug_assert_eq!(f.offset, consumed);
        let v = bits.get(f.bits);
        consumed += f.bits;
        match f.used_for {
            Some(V2Use::BitDepthLess1) => h.depth_less_1 = v,
            Some(V2Use::Options) => h.options = v,
            Some(V2Use::InitialPrediction) => h.initial = v as u16,
            None => {}
        }
    }
    h
}

/// Which of the three length-history slots the group `i` (of four) of a block uses, per row parity.
const V2_LENGTH_SLOT: [[usize; 4]; 2] = [[2, 2, 0, 0], [0, 0, 1, 1]];

/// Block position of each of the 16 residuals in coding order, per row parity. Residuals are
/// coded as two runs of eight, one for the even and one for the odd sample positions: an even
/// row codes the even positions first, an odd row the odd positions first.
const V2_POSITION: [[usize; 16]; 2] = {
    let mut t = [[0; 16]; 2];
    let mut parity = 0;
    while parity < 2 {
        let mut i = 0;
        while i < 16 {
            let lane = (parity + i / 8) % 2;
            t[parity][i] = 2 * (i % 8) + lane;
            i += 1;
        }
        parity += 1;
    }
    t
};

/// Parameters shared by all rows of a V2 image.
struct V2Frame {
    w: usize,
    h: usize,
    depth: i32,
    max: i32,
    opt: u32,
    init: u16,
}

/// Samples per block, the smallest width.
const V2_BLOCK: usize = 16;

/// Decode Samsung V2 (compression 32773). `src` starts at the image data.
///
/// The size guard is ours: a row must hold at least one block and an image at least one row (the
/// motion and length checks further down reject everything else that cannot be decoded).
pub fn decode_v2(src: &[u8], width: usize, height: usize, out: &mut [u16]) -> Result<()> {
    if height == 0 || width < V2_BLOCK {
        return Err(Error::corrupt("Samsung v2: image too small"));
    }
    let total = width.checked_mul(height);
    if total.is_none_or(|t| out.len() < t) {
        return Err(Error::InvalidArgument("Samsung v2: output too small".into()));
    }
    let mut bits = Bits::at(src, 0);
    let header = read_v2_header(&mut bits);
    let (opt, init) = (header.options, header.initial);
    let depth = header.depth_less_1 as i32 + 1;
    let f = V2Frame { w: width, h: height, depth, max: (1 << depth) - 1, opt, init };
    for r in 0..height {
        let start = (bits.p + 15) & !15;
        bits.restart(start);
        v2_row(&mut bits, &f, r, out)?;
        if bits.over {
            return Err(Error::corrupt(format!("Samsung v2: truncated data at row {r}/{height}")));
        }
    }
    Ok(())
}

/// Decode row `r` of a V2 image.
fn v2_row(bits: &mut Bits, f: &V2Frame, r: usize, out: &mut [u16]) -> Result<()> {
    let (w, opt) = (f.w, f.opt);
    let row0 = r * w;
    let start = if r < 2 { 7 } else { 4 };
    let mut hist = [(start, start); 3];
    let mut scale: i32 = 0;
    let mut motion = 7u32;
    let odd_row = r % 2 == 1;
    for col in (0..=w - V2_BLOCK).step_by(V2_BLOCK) {
        if opt & 4 == 0 && col % 64 == 0 {
            match bits.get(2) {
                0 => {}
                1 => scale -= 2,
                2 => scale += 2,
                _ => scale = bits.get(12) as i32,
            }
        }
        if opt & 2 != 0 {
            motion = if bits.get(1) != 0 { 3 } else { 7 };
        } else if bits.get(1) == 0 {
            motion = bits.get(3);
        }
        // Base prediction.
        let mut base = [0i32; 16];
        if motion == 7 {
            let (a, b) = if col == 0 { (f.init, f.init) } else { (out[row0 + col - 2], out[row0 + col - 1]) };
            for k in 0..8 {
                base[2 * k] = i32::from(a);
                base[2 * k + 1] = i32::from(b);
            }
        } else {
            if r < 2 {
                return Err(Error::corrupt("Samsung v2: motion prediction on the first rows"));
            }
            let m = motion as usize;
            let (sh, span) = (MOTION_SHIFT[m], MOTION_SPAN[m]);
            let (r1, r2) = (((r - 1) * w + col) as isize + sh, ((r - 2) * w + col) as isize + sh);
            // Reference of the even / odd block positions i is `e0 + i` / `o0 + i`.
            let (e0, o0) = if odd_row { (r2, r1 - 1) } else { (r1 + 1, r2) };
            let lo = e0.min(o0 + 1);
            let hi = (e0 + 14).max(o0 + 15) + span;
            if lo < 0 || hi >= (w * f.h) as isize {
                return Err(Error::corrupt("Samsung v2: motion reference out of range"));
            }
            let win = &out[lo as usize..=hi as usize];
            let (e, o1) = ((e0 - lo) as usize, (o0 + 1 - lo) as usize);
            for k in 0..8 {
                let (i0, i1) = (e + 2 * k, o1 + 2 * k);
                if span != 0 {
                    let s = span as usize;
                    base[2 * k] = (i32::from(win[i0]) + i32::from(win[i0 + s]) + 1) >> 1;
                    base[2 * k + 1] = (i32::from(win[i1]) + i32::from(win[i1 + s]) + 1) >> 1;
                } else {
                    base[2 * k] = i32::from(win[i0]);
                    base[2 * k + 1] = i32::from(win[i1]);
                }
            }
        }
        // Bit lengths of the four groups of four samples.
        let mut dl = [0i32; 4];
        if opt & 1 != 0 || bits.get(1) == 0 {
            let flags = [bits.get(2), bits.get(2), bits.get(2), bits.get(2)];
            for (i, &fl) in flags.iter().enumerate() {
                let k = V2_LENGTH_SLOT[usize::from(odd_row)][i];
                let (a, b) = hist[k];
                let v = match fl {
                    0 => a,
                    1 => a + 1,
                    2 => a - 1,
                    _ => bits.get(4) as i32,
                };
                dl[i] = v;
                hist[k] = (b, v);
                if v < 0 || v > f.depth + 1 {
                    return Err(Error::corrupt("Samsung v2: invalid difference length"));
                }
            }
        }
        // Residuals.
        let mut res = [0i32; 16];
        for (g, &n) in dl.iter().enumerate() {
            if n != 0 {
                let n = n as u32;
                for t in 0..4 {
                    let i = 4 * g + t;
                    let pos = V2_POSITION[usize::from(odd_row)][i];
                    res[pos] = sext(bits.get(n), n);
                }
            }
        }
        let mul = scale.wrapping_mul(2).wrapping_add(1);
        let dst: &mut [u16; 16] = (&mut out[row0 + col..row0 + col + 16]).try_into().unwrap();
        for j in 0..16 {
            let v = base[j].wrapping_add(res[j].wrapping_mul(mul)).wrapping_add(scale);
            dst[j] = v.clamp(0, f.max) as u16;
        }
    }
    Ok(())
}

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;
