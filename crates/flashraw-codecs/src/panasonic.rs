//! Panasonic / Leica RW2 payload codecs (RawFormat 4–8).
//!
//! | RawFormat | Bodies | Coding | Parallel unit |
//! |---|---|---|---|
//! | 4 | most RW2 bodies up to 2019 | 14 pixels per 16-byte packet, 2-bit shift + 8-bit deltas | packet (row) |
//! | 5 | GH5S, G9 high-res | 10×12-bit or 9×14-bit LSB-packed per 16 bytes | row |
//! | 6 | S1/S1R/S1H/S5, GH5 II | 11×14-bit or 14×12-bit, delta coded per 16 bytes | row |
//! | 7 | S1R/S5 high-res | 9×14-bit LSB-packed per 16 bytes | row |
//! | 8 | GH6, G9 II, S5 II, S9 ... | Huffman-coded 2×2 differences in vertical strips | strip |
//!
//! Formats 4 and 5 store the stream in 0x4000-byte blocks whose last `split` bytes
//! come first on disk ([`BlockedStream`]). Format 4 reads every 16-byte packet as a
//! 128-bit little-endian integer from its most significant bit down, and each packet
//! carries exactly 128 bits for its 14 pixels; that makes every packet independent
//! even though the format is defined as one sequential stream.
//!
//! All decoders are bit-exact, including how a short final block is padded with the
//! previous block's bytes.

use flashraw_core::par;
use flashraw_core::{Error, Result};

/// Size of the rotation blocks of formats 4 and 5.
pub const BLOCK: usize = 0x4000;

/// A format 4/5 stream: blocks of [`BLOCK`] bytes whose first `split` bytes (in
/// stream order) are stored after the remaining `BLOCK - split` bytes.
#[derive(Clone, Copy)]
pub struct BlockedStream<'a> {
    data: &'a [u8],
    split: usize,
}

impl<'a> BlockedStream<'a> {
    pub fn new(data: &'a [u8], split: usize) -> Result<Self> {
        if split > BLOCK {
            return Err(Error::corrupt("Panasonic block split beyond block size"));
        }
        Ok(BlockedStream { data, split })
    }

    /// Stream bytes available (a short final block is padded with stale data).
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// File offset (relative to the payload) of stream byte `i` of block `b`.
    #[inline(always)]
    fn file_pos(&self, b: usize, i: usize) -> usize {
        b * BLOCK + (i + BLOCK - self.split) % BLOCK
    }

    /// Stream byte `i` of block `b`, with these semantics for a truncated final
    /// block: bytes that were not read keep the value of the previous block's buffer.
    fn byte(&self, mut b: usize, i: usize) -> u8 {
        loop {
            if let Some(&v) = self.data.get(self.file_pos(b, i)) {
                return v;
            }
            if b == 0 {
                return 0;
            }
            b -= 1;
        }
    }

    /// The 16-byte packet `p` (stream bytes `16p..16p+16`).
    #[inline(always)]
    pub fn packet(&self, p: usize) -> [u8; 16] {
        let b = p * 16 / BLOCK;
        let i = p * 16 % BLOCK;
        let start = self.file_pos(b, i);
        // Fast path: the packet does not straddle the split or the end of the data.
        if (b + 1) * BLOCK <= self.data.len()
            && start + 16 <= (b + 1) * BLOCK
            && (i >= self.split || i + 16 <= self.split)
        {
            let mut out = [0u8; 16];
            out.copy_from_slice(&self.data[start..start + 16]);
            return out;
        }
        std::array::from_fn(|k| self.byte(b, i + k))
    }

    /// Number of whole stream bytes needed to hold `packets` packets.
    pub fn packets_available(&self) -> usize {
        // Any packet inside an existing (possibly short) block can be produced.
        self.data.len().div_ceil(BLOCK) * BLOCK / 16
    }
}

// ---------------------------------------------------------------------------------
// Format 4

/// Pixels per format-4 packet.
pub const V4_PIXELS: usize = 14;

/// Decode one 16-byte format-4 packet into 14 pixels.
#[inline]
pub fn v4_packet(packet: &[u8; 16], out: &mut [u16]) {
    // The packet is read from bit 127 of its little-endian value downwards.
    let mut v = u128::from_le_bytes(*packet);
    let mut take = |n: u32| -> i32 {
        let r = (v >> (128 - n)) as i32;
        v <<= n;
        r
    };
    let mut sh = 0i32;
    let mut pred = [0i32; 2];
    let mut nonz = [0i32; 2];
    for (i, o) in out.iter_mut().enumerate().take(V4_PIXELS) {
        let c = i & 1;
        if i % 3 == 2 {
            sh = 4 >> (3 - take(2));
        }
        if nonz[c] != 0 {
            let j = take(8);
            if j != 0 {
                pred[c] -= 0x80 << sh;
                if pred[c] < 0 || sh == 4 {
                    pred[c] &= !(-1i32 << sh);
                }
                pred[c] += j << sh;
            }
        } else {
            nonz[c] = take(8);
            if nonz[c] != 0 || i > 11 {
                pred[c] = nonz[c] << 4 | take(4);
            }
        }
        *o = pred[c] as u16;
    }
}

/// Decode a format-4 image (`width * height` samples into `out`). Rows are
/// independent when `width` is a multiple of 14 (always the case in practice);
/// otherwise the stream is decoded sequentially.
pub fn decode_v4(s: &BlockedStream, width: usize, height: usize, out: &mut [u16], parallel: bool) -> Result<()> {
    check_out(out, width, height)?;
    if width % V4_PIXELS != 0 {
        let needed = (width * height).div_ceil(V4_PIXELS);
        if s.packets_available() < needed {
            return Err(Error::corrupt("Panasonic data truncated"));
        }
        return decode_v4_sequential(s, width, height, out);
    }
    decode_v4_rows(s, width, height, 0, &mut out[..width * height], parallel)
}

/// Decode rows `first_row..first_row + out.len() / width` of a format-4 image whose
/// width is a multiple of 14 (rows are independent).
pub fn decode_v4_rows(
    s: &BlockedStream,
    width: usize,
    height: usize,
    first_row: usize,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    let rows = out.len() / width.max(1);
    if width == 0 || width % V4_PIXELS != 0 || first_row + rows > height {
        return Err(Error::InvalidArgument("Panasonic v4 row range".into()));
    }
    let needed = (width * height).div_ceil(V4_PIXELS);
    if s.packets_available() < needed {
        return Err(Error::corrupt(format!("Panasonic data truncated: {} of {needed} packets", s.packets_available())));
    }
    let per_row = width / V4_PIXELS;
    par::for_each_row_chunk(&mut out[..width * rows], width, 16, parallel, |first, chunk| {
        for (r, row) in chunk.chunks_exact_mut(width).enumerate() {
            let base = (first_row + first + r) * per_row;
            for (k, px) in row.chunks_exact_mut(V4_PIXELS).enumerate() {
                v4_packet(&s.packet(base + k), px);
            }
        }
    });
    Ok(())
}

/// Bit-exact sequential reader for Panasonic packets (any width).
pub fn decode_v4_sequential(s: &BlockedStream, width: usize, height: usize, out: &mut [u16]) -> Result<()> {
    check_out(out, width, height)?;
    let mut buf = vec![0u8; BLOCK + 2];
    let mut block = 0usize;
    let mut vpos = 0u32;
    let mut get = |nb: u32, buf: &mut Vec<u8>| -> i32 {
        if vpos == 0 {
            for (i, b) in buf.iter_mut().take(BLOCK).enumerate() {
                if let Some(&v) = s.data.get(s.file_pos(block, i)) {
                    *b = v;
                }
            }
            block += 1;
        }
        vpos = vpos.wrapping_sub(nb) & 0x1ffff;
        let byte = (vpos >> 3 ^ 0x3ff0) as usize;
        let w = buf[byte] as u32 | (buf[byte + 1] as u32) << 8;
        (w >> (vpos & 7) & !(!0u32 << nb)) as i32
    };
    let (mut sh, mut pred, mut nonz) = (0i32, [0i32; 2], [0i32; 2]);
    for row in 0..height {
        for col in 0..width {
            let i = col % V4_PIXELS;
            if i == 0 {
                pred = [0; 2];
                nonz = [0; 2];
            }
            if i % 3 == 2 {
                sh = 4 >> (3 - get(2, &mut buf));
            }
            let c = i & 1;
            if nonz[c] != 0 {
                let j = get(8, &mut buf);
                if j != 0 {
                    pred[c] -= 0x80 << sh;
                    if pred[c] < 0 || sh == 4 {
                        pred[c] &= !(-1i32 << sh);
                    }
                    pred[c] += j << sh;
                }
            } else {
                nonz[c] = get(8, &mut buf);
                if nonz[c] != 0 || i > 11 {
                    pred[c] = nonz[c] << 4 | get(4, &mut buf);
                }
            }
            out[row * width + col] = pred[col & 1] as u16;
        }
    }
    Ok(())
}

fn check_out(out: &[u16], width: usize, height: usize) -> Result<()> {
    if width == 0 || height == 0 || out.len() < width * height {
        return Err(Error::InvalidArgument("Panasonic output buffer too small".into()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------
// Formats 5 and 7: plain LSB-first packing of 10×12 or 9×14 bits per 16 bytes.

/// Pixels per 16-byte packet for formats 5 and 7.
pub fn packed_pixels(bits: u32) -> Option<usize> {
    match bits {
        12 => Some(10),
        14 => Some(9),
        _ => None,
    }
}

#[inline]
fn unpack_lsb(packet: &[u8; 16], bits: u32, out: &mut [u16]) {
    let v = u128::from_le_bytes(*packet);
    let mask = (1u128 << bits) - 1;
    for (k, o) in out.iter_mut().enumerate() {
        *o = ((v >> (bits as usize * k)) & mask) as u16;
    }
}

/// Format 5: packets run through the blocked stream; every row starts a new packet.
pub fn decode_v5(
    s: &BlockedStream,
    bits: u32,
    width: usize,
    height: usize,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    check_out(out, width, height)?;
    decode_v5_rows(s, bits, width, height, 0, &mut out[..width * height], parallel)
}

/// Decode rows `first_row..first_row + out.len() / width` of a format-5 image.
pub fn decode_v5_rows(
    s: &BlockedStream,
    bits: u32,
    width: usize,
    height: usize,
    first_row: usize,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    let rows = out.len() / width.max(1);
    if width == 0 || first_row + rows > height {
        return Err(Error::InvalidArgument("Panasonic v5 row range".into()));
    }
    let ppp = packed_pixels(bits).ok_or_else(|| Error::corrupt(format!("Panasonic v5 with {bits} bits")))?;
    let per_row = width.div_ceil(ppp);
    if s.packets_available() < per_row * height {
        return Err(Error::corrupt("Panasonic v5 data truncated"));
    }
    par::for_each_row_chunk(&mut out[..width * rows], width, 16, parallel, |first, chunk| {
        let mut tmp = [0u16; 10];
        for (r, row) in chunk.chunks_exact_mut(width).enumerate() {
            let base = (first_row + first + r) * per_row;
            for (k, px) in row.chunks_mut(ppp).enumerate() {
                unpack_lsb(&s.packet(base + k), bits, &mut tmp[..ppp]);
                px.copy_from_slice(&tmp[..px.len()]);
            }
        }
    });
    Ok(())
}

/// Format 7: rows of `width / ppp` packets stored back to back (no block rotation).
/// Only whole 16-row bands are decoded and trailing columns that do
/// not fill a packet stay zero.
pub fn decode_v7(data: &[u8], bits: u32, width: usize, height: usize, out: &mut [u16], parallel: bool) -> Result<()> {
    check_out(out, width, height)?;
    decode_v7_rows(data, bits, width, height, 0, &mut out[..width * height], parallel)
}

/// Decode rows `first_row..first_row + out.len() / width` of a format-7 image.
pub fn decode_v7_rows(
    data: &[u8],
    bits: u32,
    width: usize,
    height: usize,
    first_row: usize,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    let nrows = out.len() / width.max(1);
    if width == 0 || first_row + nrows > height {
        return Err(Error::InvalidArgument("Panasonic v7 row range".into()));
    }
    let ppp = packed_pixels(bits).ok_or_else(|| Error::corrupt(format!("Panasonic v7 with {bits} bits")))?;
    let row_bytes = width / ppp * 16;
    let rows = height / 16 * 16;
    if row_bytes == 0 || data.len() < rows * row_bytes {
        return Err(Error::corrupt("Panasonic v7 data truncated"));
    }
    par::for_each_row_chunk(&mut out[..width * nrows], width, 16, parallel, |first, chunk| {
        for (r, row) in chunk.chunks_exact_mut(width).enumerate() {
            let y = first_row + first + r;
            if y >= rows {
                row.fill(0);
                continue;
            }
            let src = &data[y * row_bytes..(y + 1) * row_bytes];
            for (p, px) in src.chunks_exact(16).zip(row.chunks_exact_mut(ppp)) {
                unpack_lsb(p.try_into().unwrap_or(&[0; 16]), bits, px);
            }
        }
    });
    Ok(())
}

// ---------------------------------------------------------------------------------
// Format 6

/// Decode a format-6 image: per row `width / ppb` packets of 16 bytes, each holding
/// 11 (14-bit) or 14 (12-bit) delta-coded pixels. Only whole 16-row bands are decoded.
pub fn decode_v6(data: &[u8], bits: u32, width: usize, height: usize, out: &mut [u16], parallel: bool) -> Result<()> {
    check_out(out, width, height)?;
    decode_v6_rows(data, bits, width, height, 0, &mut out[..width * height], parallel)
}

/// Decode rows `first_row..first_row + out.len() / width` of a format-6 image.
pub fn decode_v6_rows(
    data: &[u8],
    bits: u32,
    width: usize,
    height: usize,
    first_row: usize,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    let nrows = out.len() / width.max(1);
    if width == 0 || first_row + nrows > height {
        return Err(Error::InvalidArgument("Panasonic v6 row range".into()));
    }
    let twelve = match bits {
        12 => true,
        14 => false,
        _ => return Err(Error::corrupt(format!("Panasonic v6 with {bits} bits"))),
    };
    let ppb = if twelve { 14 } else { 11 };
    let blocks = width / ppb;
    let row_bytes = blocks * 16;
    let rows = height / 16 * 16;
    if row_bytes == 0 || data.len() < rows * row_bytes {
        return Err(Error::corrupt("Panasonic v6 data truncated"));
    }
    par::for_each_row_chunk(&mut out[..width * nrows], width, 16, parallel, |first, chunk| {
        for (r, row) in chunk.chunks_exact_mut(width).enumerate() {
            let y = first_row + first + r;
            if y >= rows {
                row.fill(0);
                continue;
            }
            let src = &data[y * row_bytes..(y + 1) * row_bytes];
            for (p, px) in src.chunks_exact(16).zip(row.chunks_exact_mut(ppb)) {
                v6_block(p.try_into().unwrap_or(&[0; 16]), twelve, px);
            }
        }
    });
    Ok(())
}

/// Split a 16-byte format-6 block into its fields (big-endian bit order over the
/// byte-reversed block): pixel codes and 2-bit shift selectors interleaved.
fn v6_fields(b: &[u8; 16], twelve: bool) -> ([u32; 18], usize) {
    // Byte i of the reversed block.
    let w = |i: usize| b[15 - i] as u32;
    let mut f = [0u32; 18];
    if twelve {
        f[0] = (w(0) << 4) | (w(1) >> 4);
        f[1] = ((w(1) & 0xf) << 8 | w(2)) & 0xfff;
        f[2] = (w(3) >> 6) & 0x3;
        f[3] = ((w(3) & 0x3f) << 2) | (w(4) >> 6);
        f[4] = ((w(4) & 0x3f) << 2) | (w(5) >> 6);
        f[5] = ((w(5) & 0x3f) << 2) | (w(6) >> 6);
        f[6] = (w(6) >> 4) & 0x3;
        f[7] = ((w(6) & 0xf) << 4) | (w(7) >> 4);
        f[8] = ((w(7) & 0xf) << 4) | (w(8) >> 4);
        f[9] = ((w(8) & 0xf) << 4) | (w(9) >> 4);
        f[10] = (w(9) >> 2) & 0x3;
        f[11] = ((w(9) & 0x3) << 6) | (w(10) >> 2);
        f[12] = ((w(10) & 0x3) << 6) | (w(11) >> 2);
        f[13] = ((w(11) & 0x3) << 6) | (w(12) >> 2);
        f[14] = w(12) & 0x3;
        f[15] = w(13);
        f[16] = w(14);
        f[17] = w(15);
        (f, 18)
    } else {
        f[0] = (w(0) << 6) | (w(1) >> 2);
        f[1] = (((w(1) & 0x3) << 12) | (w(2) << 4) | (w(3) >> 4)) & 0x3fff;
        f[2] = (w(3) >> 2) & 0x3;
        f[3] = ((w(3) & 0x3) << 8) | w(4);
        f[4] = (w(5) << 2) | (w(6) >> 6);
        f[5] = ((w(6) & 0x3f) << 4) | (w(7) >> 4);
        f[6] = (w(7) >> 2) & 0x3;
        f[7] = ((w(7) & 0x3) << 8) | w(8);
        f[8] = ((w(9) << 2) & 0x3fc) | (w(10) >> 6);
        f[9] = ((w(10) << 4) | (w(11) >> 4)) & 0x3ff;
        f[10] = (w(11) >> 2) & 0x3;
        f[11] = ((w(11) & 0x3) << 8) | w(12);
        f[12] = (((w(13) << 2) & 0x3fc) | w(14) >> 6) & 0x3ff;
        f[13] = ((w(14) << 4) | (w(15) >> 4)) & 0x3ff;
        (f, 14)
    }
}

#[inline]
fn v6_block(b: &[u8; 16], twelve: bool, out: &mut [u16]) {
    let (f, nf) = v6_fields(b, twelve);
    let mut next = 0usize;
    let mut field = || {
        let v = if next < nf { f[next] } else { 0 };
        next += 1;
        v
    };
    let (base0, base_cmp, spix_cmp, mask) =
        if twelve { (0x80u32, 0x800u32, 0x3fffu32, 0xfffu32) } else { (0x200, 0x2000, 0xffff, 0x3fff) };
    let mut oddeven = [0u32; 2];
    let mut nonzero = [0u32; 2];
    let (mut pmul, mut pixel_base) = (0u32, 0u32);
    for (pix, o) in out.iter_mut().enumerate() {
        if pix % 3 == 2 {
            let mut base = field();
            if base == 3 {
                base = 4;
            }
            pixel_base = base0 << base;
            pmul = 1 << base;
        }
        let mut epixel = field();
        let c = pix % 2;
        if oddeven[c] != 0 {
            epixel = epixel.wrapping_mul(pmul);
            if pixel_base < base_cmp && nonzero[c] > pixel_base {
                epixel = epixel.wrapping_add(nonzero[c] - pixel_base);
            }
            nonzero[c] = epixel;
        } else {
            oddeven[c] = epixel;
            if epixel != 0 {
                nonzero[c] = epixel;
            } else {
                epixel = nonzero[c];
            }
        }
        let spix = epixel.wrapping_sub(0xf);
        *o = if spix <= spix_cmp {
            (spix & spix_cmp) as u16
        } else {
            // Underflow clamps to zero, overflow saturates to the mask.
            ((((epixel.wrapping_add(0x7fff_fff1)) as i32) >> 31) as u32 & mask) as u16
        };
    }
}

// ---------------------------------------------------------------------------------
// Format 8

/// Format-8 parameters (RW2 tags 0x39–0x48).
#[derive(Debug, Clone, Default)]
pub struct V8Params {
    /// Gamma curve: 6 (point, slope) pairs (tags 0x3A and 0x39).
    pub gamma_points: [u32; 6],
    pub gamma_slopes: [u32; 6],
    /// Clip value (tag 0x3B).
    pub clip: u32,
    /// Initial predictions for the 2×2 block positions TL, TR, BL, BR (tags 0x3C–0x3F).
    pub initial: [u16; 4],
    /// Huffman codes: (code length, code) per symbol (tag 0x40); the symbol index is
    /// the difference bit length.
    pub codes: Vec<(u8, u16)>,
    /// Per-symbol "shift down" (tag 0x41); zero in all known files.
    pub shift_down: Vec<u8>,
}

/// One independently coded vertical strip.
#[derive(Debug, Clone, Copy)]
pub struct V8Strip<'a> {
    pub data: &'a [u8],
    /// Exact length of the strip's bit stream.
    pub bits: u64,
    pub left: u32,
    /// Vertical position; tiles below the first row of strips exist on high-resolution
    /// (pixel-shift) bodies.
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

/// Decoded symbol for each 16-bit prefix: (code length, symbol); `len == 0` = invalid.
struct V8Lut {
    /// Indexed by the next `bits` stream bits (the longest code length), so the table
    /// stays small enough for L1 (4096 entries for the usual 12-bit maximum).
    entries: Vec<(u8, u8)>,
    bits: u32,
}

impl V8Lut {
    fn new(codes: &[(u8, u16)]) -> Result<Self> {
        if codes.is_empty() || codes.len() > 17 {
            return Err(Error::corrupt("Panasonic v8 Huffman table size"));
        }
        let bits = codes.iter().map(|&(l, _)| l as u32).filter(|&l| l <= 16).max().unwrap_or(1).max(1);
        // Codes are matched in table order (first match wins; not necessarily a prefix code).
        let mut entries = vec![(0u8, 0u8); 1 << bits];
        for (v, e) in entries.iter_mut().enumerate() {
            for (sym, &(len, code)) in codes.iter().enumerate() {
                if len == 0 || len as u32 > bits {
                    continue;
                }
                let shift = bits - len as u32;
                if (v as u32 >> shift) == (code as u32 & ((1 << len) - 1)) {
                    *e = (len, sym as u8);
                    break;
                }
            }
        }
        Ok(V8Lut { entries, bits })
    }
}

/// MSB-first reader over bytes whose bits are stored LSB first.
struct RevBits<'a> {
    data: &'a [u8],
    pos: usize,
    cache: u64,
    bits: u32,
}

impl<'a> RevBits<'a> {
    fn new(data: &'a [u8]) -> Self {
        RevBits { data, pos: 0, cache: 0, bits: 0 }
    }
    #[inline(always)]
    fn fill(&mut self) {
        if self.bits > 32 {
            return;
        }
        // Load 4 bytes; past the end, pad with zeros (callers check the bit budget).
        let mut w = [0u8; 4];
        if let Some(s) = self.data.get(self.pos..self.pos + 4) {
            w.copy_from_slice(s);
        } else {
            for (k, b) in w.iter_mut().enumerate() {
                *b = self.data.get(self.pos + k).copied().unwrap_or(0);
            }
        }
        let v = u32::from_le_bytes(w).reverse_bits() as u64;
        self.cache |= v << (32 - self.bits);
        self.bits += 32;
        self.pos += 4;
    }
    #[inline(always)]
    fn skip(&mut self, n: u32) {
        self.cache = if n >= 64 { 0 } else { self.cache << n };
        self.bits -= n;
    }
    fn consumed(&self) -> u64 {
        self.pos as u64 * 8 - self.bits as u64
    }
}

/// Format-8 decoder: Huffman-coded differences to a running 2×2 prediction.
pub struct V8Decoder {
    lut: V8Lut,
    shift_down: [u8; 17],
    clip: u32,
    gamma: Option<Vec<u16>>,
    initial: [u16; 4],
}

impl V8Decoder {
    pub fn new(p: &V8Params) -> Result<Self> {
        let lut = V8Lut::new(&p.codes)?;
        let mut shift_down = [0u8; 17];
        for (d, &s) in shift_down.iter_mut().zip(p.shift_down.iter()) {
            *d = s.min(31);
        }
        // Apply the tag 0x39/0x3A tone curve unless it is the identity.
        let curve: Vec<u16> = (0..0x10000u32).map(|i| gamma_curve(p, i)).collect();
        let gamma = if curve.iter().enumerate().all(|(i, &v)| v as usize == i) { None } else { Some(curve) };
        Ok(V8Decoder { lut, shift_down, clip: p.clip, gamma, initial: p.initial })
    }

    /// Decode strips into `out` (`raw_width` samples per row). Strips decode in
    /// parallel, each straight into its own column band of the output.
    pub fn decode(
        &self,
        strips: &[V8Strip],
        out: &mut [u16],
        raw_width: usize,
        raw_height: usize,
        parallel: bool,
    ) -> Result<()> {
        if out.len() < raw_width * raw_height || raw_width == 0 || strips.is_empty() {
            return Err(Error::InvalidArgument("Panasonic v8 output too small".into()));
        }
        // Tiles must lie inside the image, not overlap, and cover it completely.
        let rect = |s: &V8Strip| (s.left as usize, s.top as usize, s.width as usize, s.height as usize);
        let mut area = 0usize;
        for (i, s) in strips.iter().enumerate() {
            let (l, t, w, h) = rect(s);
            if w < 2 || h < 2 || w % 2 != 0 || h % 2 != 0 || l + w > raw_width || t + h > raw_height {
                return Err(Error::corrupt("invalid Panasonic v8 strip geometry"));
            }
            for o in &strips[..i] {
                let (ol, ot, ow, oh) = rect(o);
                if l < ol + ow && ol < l + w && t < ot + oh && ot < t + h {
                    return Err(Error::corrupt("overlapping Panasonic v8 strips"));
                }
            }
            area += w * h;
        }
        if area != raw_width * raw_height {
            return Err(Error::corrupt("Panasonic v8 strips do not cover the image"));
        }
        // Hand every tile the matching segment of each of its output rows (disjoint borrows).
        let mut order: Vec<usize> = (0..strips.len()).collect();
        order.sort_by_key(|&i| (strips[i].top, strips[i].left));
        let mut segments: Vec<Vec<&mut [u16]>> = strips.iter().map(|s| Vec::with_capacity(s.height as usize)).collect();
        for (y, row) in out[..raw_width * raw_height].chunks_exact_mut(raw_width).enumerate() {
            let mut rest = row;
            let mut consumed = 0usize;
            for &i in &order {
                let (l, t, w, h) = rect(&strips[i]);
                if y < t || y >= t + h {
                    continue;
                }
                let (_, tail) = std::mem::take(&mut rest).split_at_mut(l - consumed);
                let (seg, tail) = tail.split_at_mut(w);
                segments[i].push(seg);
                rest = tail;
                consumed = l + w;
            }
        }
        let jobs: Vec<(usize, Vec<&mut [u16]>)> = segments.into_iter().enumerate().collect();
        let results = par::map(jobs, parallel, |(i, rows)| self.decode_strip(&strips[i], rows));
        results.into_iter().collect::<Result<Vec<()>>>().map(|_| ())
    }

    /// Decode one strip into `rows` (its `height` row segments of `width` samples).
    pub fn decode_strip(&self, s: &V8Strip, mut rows: Vec<&mut [u16]>) -> Result<()> {
        let (w, h) = (s.width as usize, s.height as usize);
        let half_w = w / 2;
        if half_w == 0 || h / 2 == 0 || rows.len() < h || rows.iter().any(|r| r.len() < w) {
            return Err(Error::corrupt("invalid Panasonic v8 strip"));
        }
        let budget = s.bits.div_ceil(64) * 64;
        let mut bits = RevBits::new(s.data);
        let clip = self.clip as i32;
        let mut line_base = self.initial.map(|v| v as i32);
        // Values are decoded per 2x2 block in the order TL, BL, TR, BR.
        const SLOT: [usize; 4] = [0, 2, 1, 3];
        for pair in 0..h / 2 {
            let mut base = line_base;
            let (top, bottom) = rows[pair * 2..pair * 2 + 2].split_at_mut(1);
            let (r0, r1) = (&mut *top[0], &mut *bottom[0]);
            for bx in 0..half_w {
                let mut blk = [0i32; 4];
                for slot in SLOT {
                    let delta = self.next_diff(&mut bits)?;
                    blk[slot] = (base[slot] + delta).clamp(0, clip);
                }
                base = blk;
                if bx == 0 {
                    line_base = blk;
                }
                let g = |v: i32| match &self.gamma {
                    Some(t) => t[v as usize & 0xffff],
                    None => v as u16,
                };
                r0[2 * bx] = g(blk[0]);
                r0[2 * bx + 1] = g(blk[1]);
                r1[2 * bx] = g(blk[2]);
                r1[2 * bx + 1] = g(blk[3]);
            }
            if bits.consumed() > budget {
                return Err(Error::corrupt("Panasonic v8 strip truncated"));
            }
        }
        Ok(())
    }

    #[inline(always)]
    fn next_diff(&self, bits: &mut RevBits) -> Result<i32> {
        // One refill covers the code (<= 16 bits) and its magnitude bits (<= 16).
        bits.fill();
        let window = bits.cache;
        let (len, sym) = self.lut.entries.get((window >> (64 - self.lut.bits)) as usize).copied().unwrap_or((0, 0));
        if len == 0 {
            return Err(Error::corrupt("invalid Panasonic v8 Huffman code"));
        }
        let i = sym as u32;
        let s = self.shift_down[sym as usize] as u32;
        let d = i.saturating_sub(s);
        let after = window << len;
        bits.skip(len as u32 + d);
        // The bit following the code decides the sign (JPEG-style), even when no
        // magnitude bits are stored (shift-down >= length).
        let next_bit_set = (after >> 63) == 1;
        let raw = if d > 0 { (after >> (64 - d)) as i32 } else { 0 };
        let v = raw << s;
        let delta1 = if next_bit_set {
            v
        } else if i > 0 {
            v - (1 << i) + if s > 0 { 0 } else { 1 }
        } else {
            0
        };
        let delta2 = if s > 0 { 1 << (s - 1) } else { 0 };
        Ok(delta1 + delta2)
    }
}

/// The format-8 tone curve from tags 0x39/0x3A (identity for every known file).
fn gamma_curve(p: &V8Params, idx: u32) -> u16 {
    let t3a = &p.gamma_points;
    let t39 = &p.gamma_slopes;
    let v2 = if idx & 0x10000 == 0 { idx & 0x1ffff } else { idx | 0xffff_0000 };
    let mut v4 = v2.min(0xffff);
    if v4 & 0x8000_0000 != 0 {
        v4 = 0;
    }
    let mut seg = 0usize;
    if v4 >= (t3a[1] & 0xffff) {
        seg = 1;
        if v4 >= (t3a[2] & 0xffff) {
            seg = 2;
            if v4 >= (t3a[3] & 0xffff) {
                seg = 3;
                if v4 >= (t3a[4] & 0xffff) {
                    seg = (((v4 as u64 | 0x5_0000_0000) - (t3a[5] & 0xffff) as u64) >> 32) as usize;
                }
            }
        }
    }
    let seg = seg.min(5);
    let v6 = t3a[seg];
    let v7 = t39[seg];
    let mut v8 = v4.wrapping_sub(v6 & 0xffff);
    let v9 = v7 & 0x1f;
    let clip = p.clip;
    let res: u64 = if v9 == 31 {
        if seg == 5 { 0xffff } else { ((t3a[seg + 1] >> 16) & 0xffff) as u64 }
    } else if v7 & 0x10 == 0 && v9 == 15 {
        ((v6 >> 16) & 0xffff) as u64
    } else {
        if v7 & 0x10 == 0 {
            if v9 != 0 {
                v8 = (v8.wrapping_add(1 << (v9 - 1))) >> v9;
            }
        } else {
            v8 = v8.wrapping_shl(v7 & 0xf);
        }
        v8 as u64 + ((v6 >> 16) & 0xffff) as u64
    };
    res.min(clip as u64) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pseudo(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn blocked_stream_rotation() {
        // Stream byte i of block 0 lives at file offset (i - split) mod BLOCK.
        let data: Vec<u8> = (0..BLOCK).map(|i| (i % 251) as u8).collect();
        let s = BlockedStream::new(&data, 0x2008).unwrap();
        assert_eq!(s.packet(0)[0], data[BLOCK - 0x2008]);
        assert_eq!(s.packet(0x2008 / 16 + 1)[0], data[8]);
        // Packet straddling the split.
        let p = s.packet(0x2000 / 16);
        for (k, &v) in p.iter().enumerate() {
            let i = 0x2000 + k;
            assert_eq!(v, data[(i + BLOCK - 0x2008) % BLOCK]);
        }
    }

    #[test]
    fn v4_parallel_matches_sequential_reference() {
        for (w, h, extra) in [(28usize, 9usize, 0usize), (14 * 37, 31, 100), (14 * 5, 4, 0)] {
            let packets = (w * h).div_ceil(14);
            let mut data = pseudo(packets * 16 + extra, w as u64);
            // Truncate inside the last block to exercise the stale-buffer padding.
            data.truncate(data.len() - extra.min(data.len() / 2));
            let s = BlockedStream::new(&data, 0x2008).unwrap();
            let mut a = vec![0u16; w * h];
            let mut b = vec![0u16; w * h];
            if decode_v4(&s, w, h, &mut a, true).is_ok() {
                decode_v4_sequential(&s, w, h, &mut b).unwrap();
                assert_eq!(a, b, "{w}x{h}");
            }
        }
        // Width not divisible by 14 uses the sequential path.
        let data = pseudo(4 * BLOCK, 7);
        let s = BlockedStream::new(&data, 0x2008).unwrap();
        let mut a = vec![0u16; 30 * 20];
        decode_v4(&s, 30, 20, &mut a, true).unwrap();
    }

    #[test]
    fn v4_packet_known_values() {
        // A packet of zeros: nonz stays 0 until pixels 12/13, which then read 4 bits.
        let mut out = [9u16; 14];
        v4_packet(&[0; 16], &mut out);
        assert_eq!(out, [0; 14]);
        // First pixel: 8-bit nonzero (0x12), then 4 bits (0x3) -> 0x123.
        let mut v: u128 = 0x12 << 120 | 0x3 << 116;
        v |= 0x01 << 108; // pixel 1: nonz = 1, low = 0
        let mut out = [0u16; 14];
        v4_packet(&v.to_le_bytes(), &mut out);
        assert_eq!(out[0], 0x123);
        assert_eq!(out[1], 0x010);
    }

    #[test]
    fn v5_v7_unpack_roundtrip() {
        for bits in [12u32, 14] {
            let ppp = packed_pixels(bits).unwrap();
            let (w, h) = (ppp * 3, 16);
            let vals: Vec<u16> = (0..w * h).map(|i| ((i * 2654435761) >> 7) as u16 & ((1 << bits) - 1)).collect();
            let mut data = Vec::new();
            for row in vals.chunks(w) {
                for px in row.chunks(ppp) {
                    let mut v: u128 = 0;
                    for (k, &p) in px.iter().enumerate() {
                        v |= (p as u128) << (bits as usize * k);
                    }
                    data.extend_from_slice(&v.to_le_bytes());
                }
            }
            let mut out = vec![0u16; w * h];
            decode_v7(&data, bits, w, h, &mut out, true).unwrap();
            assert_eq!(out, vals);
            // Format 5 with no rotation (split = 0) is the same packing.
            let s = BlockedStream::new(&data, 0).unwrap();
            let mut out5 = vec![0u16; w * h];
            decode_v5(&s, bits, w, h, &mut out5, false).unwrap();
            assert_eq!(out5, vals);
            assert!(decode_v7(&data[..data.len() / 2], bits, w, h, &mut out, true).is_err());
        }
    }

    #[test]
    fn v6_does_not_panic_and_rejects_truncation() {
        for bits in [12u32, 14] {
            let (w, h) = (154usize, 32usize);
            let ppb = if bits == 12 { 14 } else { 11 };
            let data = pseudo(w / ppb * 16 * h, bits as u64);
            let mut out = vec![0u16; w * h];
            decode_v6(&data, bits, w, h, &mut out, true).unwrap();
            // Random input may saturate to the 16-bit (14-bit mode) or 14-bit (12-bit mode) mask.
            assert!(out.iter().all(|&v| (v as u32) < if bits == 12 { 1 << 14 } else { 1 << 16 }));
            assert!(decode_v6(&data[..100], bits, w, h, &mut out, true).is_err());
        }
    }

    /// Reference format-8 encoder for tests: JPEG-style categories with codes
    /// assigned by symbol index.
    fn v8_encode(vals: &[u16], w: usize, h: usize, codes: &[(u8, u16)], init: [u16; 4]) -> Vec<u8> {
        let mut bitsv: Vec<bool> = Vec::new();
        let put = |v: u32, n: u32, bitsv: &mut Vec<bool>| {
            for k in (0..n).rev() {
                bitsv.push(v >> k & 1 == 1);
            }
        };
        let mut line_base = init.map(|v| v as i32);
        for pair in 0..h / 2 {
            let mut base = line_base;
            for bx in 0..w / 2 {
                let blk = [
                    vals[pair * 2 * w + 2 * bx] as i32,
                    vals[pair * 2 * w + 2 * bx + 1] as i32,
                    vals[(pair * 2 + 1) * w + 2 * bx] as i32,
                    vals[(pair * 2 + 1) * w + 2 * bx + 1] as i32,
                ];
                for slot in [0usize, 2, 1, 3] {
                    let d = blk[slot] - base[slot];
                    let cat = if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() };
                    let (len, code) = codes[cat as usize];
                    put(code as u32, len as u32, &mut bitsv);
                    if cat > 0 {
                        let m = if d > 0 { d as u32 } else { (d + (1 << cat) - 1) as u32 };
                        put(m, cat, &mut bitsv);
                    }
                }
                base = blk;
                if bx == 0 {
                    line_base = blk;
                }
            }
        }
        let mut bytes = vec![0u8; bitsv.len().div_ceil(8) + 8];
        for (i, &b) in bitsv.iter().enumerate() {
            if b {
                // LSB-first within each byte.
                bytes[i / 8] |= 1 << (i % 8);
            }
        }
        bytes
    }

    #[test]
    fn v8_roundtrip() {
        // Codes from a real file (lengths/codes per category 0..=16).
        let codes: Vec<(u8, u16)> = vec![
            (10, 1022),
            (11, 2046),
            (8, 254),
            (9, 510),
            (7, 126),
            (4, 14),
            (4, 12),
            (3, 4),
            (3, 2),
            (2, 0),
            (3, 3),
            (3, 5),
            (4, 13),
            (5, 30),
            (6, 62),
            (12, 4094),
            (12, 4095),
        ];
        let (w, h) = (24usize, 10usize);
        let vals: Vec<u16> = (0..w * h).map(|i| ((i * 7919) % 65536) as u16).collect();
        let data = v8_encode(&vals, w, h, &codes, [100, 200, 300, 400]);
        let p = V8Params {
            gamma_points: [0; 6],
            gamma_slopes: [65536; 6],
            clip: 65535,
            initial: [100, 200, 300, 400],
            codes: codes.clone(),
            shift_down: vec![0; 17],
        };
        let dec = V8Decoder::new(&p).unwrap();
        assert!(dec.gamma.is_none());
        let strip =
            V8Strip { data: &data, bits: (data.len() * 8) as u64, left: 0, top: 0, width: w as u32, height: h as u32 };
        let mut out = vec![0u16; w * h];
        dec.decode(&[strip], &mut out, w, h, true).unwrap();
        assert_eq!(out, vals);
        // Truncated strip.
        let short = V8Strip { data: &data[..data.len() / 3], bits: (data.len() / 3 * 8) as u64, ..strip };
        assert!(dec.decode(&[short], &mut out, w, h, true).is_err());
    }
}
