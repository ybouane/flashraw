//! Panasonic / Leica RW2 raw-data decoders, formats 4 to 8.
//!
//! * Formats 4 and 5 live in a *blocked stream*: 16 KiB blocks whose bytes are stored rotated.
//! * Format 4 packs 14 delta-coded samples into every 16-byte packet; format 5 packs 9 or 10
//!   plain samples; format 7 does the same without the rotation; format 6 packs 11 or 14
//!   delta-coded samples per 16-byte block.
//! * Format 8 codes 2x2 pixel blocks of independent rectangular strips with a small Huffman code.
//!
//! Every packet is read as a 128-bit little-endian number. The delta-coded formats consume fields
//! from the most significant bit downwards; the plain packings fill from the least significant bit.

use flashraw_core::{Error, Result, par};

/// Size in bytes of the blocks formats 4 and 5 are stored in.
pub const BLOCK: usize = 0x4000;

/// Bytes in one packet.
const PACKET_BYTES: usize = 16;

/// Packets per block.
const BLOCK_PACKETS: usize = BLOCK / PACKET_BYTES;

/// Bits in one block.
const BLOCK_BITS: u32 = (BLOCK * 8) as u32;

/// Read access to the logical byte stream of a format 4/5 payload.
///
/// Logical byte `i` of block `b` is stored at payload offset `b * BLOCK + (i + BLOCK - split) % BLOCK`.
/// Bytes whose stored position lies past the end of the payload repeat the byte with the same index
/// of the nearest earlier block that has one (zero if there is none).
#[derive(Clone, Copy)]
pub struct BlockedStream<'a> {
    data: &'a [u8],
    split: usize,
}

impl<'a> BlockedStream<'a> {
    /// `data` is the payload from its first byte to the end of the file; `split` is at most [`BLOCK`].
    pub fn new(data: &'a [u8], split: usize) -> Result<Self> {
        if split > BLOCK {
            return Err(Error::corrupt("Panasonic block split beyond block size"));
        }
        Ok(BlockedStream { data, split: split % BLOCK })
    }

    /// Number of payload bytes present.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Logical byte `i` of block `b`, including the repeat rule for missing data.
    fn byte(&self, b: usize, i: usize) -> u8 {
        let off = (i + BLOCK - self.split) % BLOCK;
        let len = self.data.len();
        if off >= len {
            return 0;
        }
        let b = b.min((len - 1 - off) / BLOCK);
        self.data[b * BLOCK + off]
    }

    /// The 16-byte packet with index `p` of the logical stream.
    #[inline]
    pub fn packet(&self, p: usize) -> [u8; 16] {
        let (b, i) = (p / BLOCK_PACKETS, (p % BLOCK_PACKETS) * 16);
        let base = b * BLOCK;
        // Fast path: the whole block is present and the packet does not straddle the rotation point.
        if let Some(blk) = self.data.get(base..base + BLOCK) {
            let s = self.split;
            let at = if i + 16 <= s {
                Some(BLOCK - s + i)
            } else if i >= s {
                Some(i - s)
            } else {
                None
            };
            if let Some(at) = at {
                let mut out = [0u8; 16];
                out.copy_from_slice(&blk[at..at + 16]);
                return out;
            }
        }
        std::array::from_fn(|k| self.byte(b, i + k))
    }

    /// Number of packets that can be read (whole blocks, the last one possibly short).
    pub fn packets_available(&self) -> usize {
        self.data.len().div_ceil(BLOCK) * BLOCK_PACKETS
    }
}

fn too_small() -> Error {
    Error::InvalidArgument("Panasonic output buffer too small".into())
}

/// Rows per parallel task: a few tasks per thread.
fn rows_per_task(rows: usize) -> usize {
    rows.div_ceil(par::threads() * 4).max(1)
}

// ---------------------------------------------------------------------------------------------
// Format 4

/// Pixels in one format-4 packet.
pub const V4_PIXELS: usize = 14;

/// Per-channel running state of a format-4 group.
#[derive(Clone, Copy, Default)]
struct V4Chan {
    pred: i32,
    base: u32,
}

/// Shift applied to the stored byte of a format-4 or format-6 group, per 2-bit selector value.
const SHIFT_OF_SELECTOR: [u32; 4] = [0, 1, 2, 4];

/// The shift at which the prediction is re-based by its low bits alone.
const V4_COARSE_SHIFT: u32 = 4;

/// Pixel positions of a group at which a channel without a base reads the 4-bit tail even when
/// its 8-bit base byte is zero (the last pair of the group).
const V4_TAIL_ALWAYS: [bool; V4_PIXELS] = {
    let mut t = [false; V4_PIXELS];
    t[12] = true;
    t[13] = true;
    t
};

/// One prediction update of a format-4 pixel whose channel already has a base value: the stored
/// byte `j` is added at scale `1 << shift` to the biased old prediction (`j == 0` keeps it).
///
/// The bias is `0x80 << shift`. When the biased value is negative, or at the coarsest shift, only
/// its low `shift` bits survive.
#[inline(always)]
fn v4_step(pred: i32, j: u32, shift: u32) -> i32 {
    let biased = pred.wrapping_sub(0x80 << shift);
    let whole = i32::from(biased >= 0 && shift != V4_COARSE_SHIFT);
    // `-whole` is all ones when the biased value is kept whole, else 0 (only the low bits remain).
    let kept = biased & (((1 << shift) - 1) | whole.wrapping_neg());
    let next = kept.wrapping_add((j << shift) as i32);
    if j != 0 { next } else { pred }
}

/// One pixel of a format-4 group. `i` is the pixel index in the group, `shift` the current shift,
/// `field` supplies the bit fields (n bits each, most significant first).
///
/// A channel is either waiting for its base (an 8-bit byte, then 4 more bits when the byte is
/// non-zero or the position is in [`V4_TAIL_ALWAYS`]) or running (one byte per pixel).
#[inline(always)]
fn v4_pixel(ch: &mut V4Chan, i: usize, shift: u32, mut field: impl FnMut(u32) -> u32) -> u16 {
    if ch.base == 0 {
        ch.base = field(8);
        if ch.base != 0 || V4_TAIL_ALWAYS[i] {
            let tail = field(4);
            ch.pred = (ch.base * 16 + tail) as i32;
        }
    } else {
        let j = field(8);
        ch.pred = v4_step(ch.pred, j, shift);
    }
    ch.pred as u16
}

/// Bit position (from the bottom) of the lowest bit of the byte of pixel `i` when both channels
/// start with a non-zero base: the layout is then fixed.
const V4_BYTE_AT: [u32; V4_PIXELS] = [0, 0, 94, 86, 78, 68, 60, 52, 42, 34, 26, 16, 8, 0];

/// Decode one 16-byte format-4 packet into the first 14 entries of `out`.
#[inline]
pub fn v4_packet(packet: &[u8; 16], out: &mut [u16]) {
    let out: &mut [u16; V4_PIXELS] = (&mut out[..V4_PIXELS]).try_into().unwrap();
    let v = u128::from_le_bytes(*packet);
    // Both channels start with a non-zero 8-bit base: every field sits at a fixed position.
    if (v >> 120) as u32 != 0 && (v >> 108) as u8 != 0 {
        let mut pred = [((v >> 116) & 0xfff) as i32, ((v >> 104) & 0xfff) as i32];
        out[0] = pred[0] as u16;
        out[1] = pred[1] as u16;
        let mut shift = 0;
        for i in 2..V4_PIXELS {
            let at = V4_BYTE_AT[i];
            if i % 3 == 2 {
                shift = SHIFT_OF_SELECTOR[((v >> (at + 8)) & 3) as usize];
            }
            let c = i & 1;
            pred[c] = v4_step(pred[c], ((v >> at) & 0xff) as u32, shift);
            out[i] = pred[c] as u16;
        }
        return;
    }
    v4_packet_general(v, out);
}

/// The general packet: fields are read in sequence because a channel without a base reads fewer bits.
fn v4_packet_general(mut v: u128, out: &mut [u16; V4_PIXELS]) {
    let mut chans = [V4Chan::default(); 2];
    let mut shift = 0u32;
    for (i, o) in out.iter_mut().enumerate() {
        let mut take = |n: u32| -> u32 {
            let r = (v >> (128 - n)) as u32;
            v <<= n;
            r
        };
        if i % 3 == 2 {
            shift = SHIFT_OF_SELECTOR[take(2) as usize & 3];
        }
        *o = v4_pixel(&mut chans[i & 1], i, shift, take);
    }
}

/// Decode a whole format-4 image into `out[..width * height]`.
pub fn decode_v4(s: &BlockedStream, width: usize, height: usize, out: &mut [u16], parallel: bool) -> Result<()> {
    let n = width.saturating_mul(height);
    if width == 0 || height == 0 || out.len() < n {
        return Err(too_small());
    }
    if width % V4_PIXELS != 0 {
        if s.packets_available() < n.div_ceil(V4_PIXELS) {
            return Err(Error::corrupt("truncated Panasonic data"));
        }
        return decode_v4_sequential(s, width, height, out);
    }
    decode_v4_rows(s, width, height, 0, &mut out[..n], parallel)
}

/// Decode rows `first_row..first_row + out.len() / width` of a format-4 image (width a multiple of 14).
pub fn decode_v4_rows(
    s: &BlockedStream,
    width: usize,
    height: usize,
    first_row: usize,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    let rows = out.len() / width.max(1);
    if width == 0 || width % V4_PIXELS != 0 || first_row.saturating_add(rows) > height {
        return Err(Error::InvalidArgument("invalid Panasonic v4 row range".into()));
    }
    let per_row = width / V4_PIXELS;
    let needed = per_row.saturating_mul(height);
    let avail = s.packets_available();
    if avail < needed {
        return Err(Error::corrupt(format!("truncated Panasonic data: {avail} of {needed} packets")));
    }
    par::for_each_row_chunk(&mut out[..width * rows], width, rows_per_task(rows), parallel, |r0, chunk| {
        let p0 = (first_row + r0) * per_row;
        for (p, px) in (p0..).zip(chunk.chunks_exact_mut(V4_PIXELS)) {
            v4_packet(&s.packet(p), px);
        }
    });
    Ok(())
}

/// Bit cursor of the sequential format-4 reader: it walks a 16 KiB block image from the top down.
struct SeqBits<'a> {
    data: &'a [u8],
    split: usize,
    image: Vec<u8>,
    cursor: u32,
    next_block: usize,
}

impl SeqBits<'_> {
    fn load(&mut self) {
        let k = self.next_block;
        for (i, b) in self.image[..BLOCK].iter_mut().enumerate() {
            let pos = k.wrapping_mul(BLOCK).wrapping_add((i + BLOCK - self.split) % BLOCK);
            if let Some(&v) = self.data.get(pos) {
                *b = v;
            }
        }
        self.next_block += 1;
    }

    /// The next `n` bits (`n <= 8`), read downwards from the top of the block image.
    ///
    /// `cursor` counts bits from the bottom of the 16 KiB image. The packets are consumed from
    /// the last one to the first, and inside a packet from its most significant bit (the top of
    /// its last byte) downwards, so bit `cursor` lives in packet `1023 - cursor / 128`, byte
    /// `(cursor % 128) / 8` of that packet, bit `cursor % 8` of the byte. A field that starts
    /// in the low bits of a byte continues into the next byte of the image.
    fn take(&mut self, n: u32) -> u32 {
        debug_assert!(n <= 8);
        if self.cursor == 0 {
            self.load();
        }
        // Step down by `n`, wrapping from the bottom of the image to its top.
        self.cursor = (self.cursor + BLOCK_BITS - n) % BLOCK_BITS;
        let c = self.cursor as usize;
        let packet = BLOCK_PACKETS - 1 - c / (PACKET_BYTES * 8);
        let byte = packet * PACKET_BYTES + c % (PACKET_BYTES * 8) / 8;
        let two = u32::from(self.image[byte]) | u32::from(self.image[byte + 1]) << 8;
        (two >> (c % 8)) & ((1 << n) - 1)
    }
}

/// Sequential format-4 decoder for any width.
pub fn decode_v4_sequential(s: &BlockedStream, width: usize, height: usize, out: &mut [u16]) -> Result<()> {
    let n = width.saturating_mul(height);
    if width == 0 || height == 0 || out.len() < n {
        return Err(too_small());
    }
    let mut bits = SeqBits { data: s.data, split: s.split, image: vec![0u8; BLOCK + 2], cursor: 0, next_block: 0 };
    let mut chans = [V4Chan::default(); 2];
    let mut shift = 0u32;
    for row in out[..n].chunks_exact_mut(width) {
        for (x, o) in row.iter_mut().enumerate() {
            let i = x % V4_PIXELS;
            if i == 0 {
                chans = [V4Chan::default(); 2];
            }
            if i % 3 == 2 {
                shift = SHIFT_OF_SELECTOR[bits.take(2) as usize];
            }
            *o = v4_pixel(&mut chans[i & 1], i, shift, |n| bits.take(n));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Formats 5 and 7: plain packing

/// Pixels per 16-byte packet of formats 5 and 7 for 12 or 14 bits per sample.
pub fn packed_pixels(bits: u32) -> Option<usize> {
    match bits {
        12 => Some(10),
        14 => Some(9),
        _ => None,
    }
}

/// Unpack the `N` samples of `BITS` bits (least significant first) of one packet.
#[inline(always)]
fn unpack<const BITS: usize, const N: usize>(packet: &[u8; 16], dst: &mut [u16]) {
    let v = u128::from_le_bytes(*packet);
    let mask = (1u128 << BITS) - 1;
    for (m, d) in dst[..N].iter_mut().enumerate() {
        *d = ((v >> (BITS * m)) & mask) as u16;
    }
}

#[inline(always)]
fn unpack_any(bits: u32, packet: &[u8; 16], dst: &mut [u16]) {
    if bits == 12 { unpack::<12, 10>(packet, dst) } else { unpack::<14, 9>(packet, dst) }
}

/// Decode a whole format-5 image.
pub fn decode_v5(
    s: &BlockedStream,
    bits: u32,
    width: usize,
    height: usize,
    out: &mut [u16],
    parallel: bool,
) -> Result<()> {
    let n = width.saturating_mul(height);
    if width == 0 || height == 0 || out.len() < n {
        return Err(too_small());
    }
    decode_v5_rows(s, bits, width, height, 0, &mut out[..n], parallel)
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
    if width == 0 || first_row.saturating_add(rows) > height {
        return Err(Error::InvalidArgument("invalid Panasonic v5 row range".into()));
    }
    let Some(ppp) = packed_pixels(bits) else {
        return Err(Error::corrupt(format!("Panasonic v5 with {bits} bits")));
    };
    let per_row = width.div_ceil(ppp);
    if s.packets_available() < per_row.saturating_mul(height) {
        return Err(Error::corrupt("truncated Panasonic v5 data"));
    }
    let row_fn: fn(&[u8], &mut [u16]) = if bits == 12 { row7_12 } else { row7_14 };
    let whole = width / ppp;
    par::for_each_row_chunk(&mut out[..width * rows], width, rows_per_task(rows), parallel, |r0, chunk| {
        let mut p = (first_row + r0) * per_row;
        let mut bytes = vec![0u8; per_row * 16];
        for row in chunk.chunks_exact_mut(width) {
            for (k, dst) in bytes.chunks_exact_mut(16).enumerate() {
                dst.copy_from_slice(&s.packet(p + k));
            }
            p += per_row;
            row_fn(&bytes[..whole * 16], &mut row[..whole * ppp]);
            if whole < per_row {
                let mut tmp = [0u16; 10];
                unpack_any(bits, bytes[whole * 16..].first_chunk().unwrap(), &mut tmp);
                row[whole * ppp..].copy_from_slice(&tmp[..width - whole * ppp]);
            }
        }
    });
    Ok(())
}

/// Checks shared by formats 6 and 7; returns the number of rows in the band and bytes per row.
#[allow(clippy::too_many_arguments)]
fn direct_check(
    name: &str,
    data_len: usize,
    bits: u32,
    per_unit: Option<usize>,
    width: usize,
    height: usize,
    first_row: usize,
    out_len: usize,
) -> Result<(usize, usize)> {
    let rows = out_len / width.max(1);
    if width == 0 || first_row.saturating_add(rows) > height {
        return Err(Error::InvalidArgument(format!("invalid Panasonic {name} row range")));
    }
    let Some(per_unit) = per_unit else {
        return Err(Error::corrupt(format!("Panasonic {name} with {bits} bits")));
    };
    let row_bytes = width / per_unit * 16;
    let banded = height / 16 * 16;
    if row_bytes == 0 || data_len < banded.saturating_mul(row_bytes) {
        return Err(Error::corrupt(format!("truncated Panasonic {name} data")));
    }
    Ok((rows, row_bytes))
}

/// Geometry of a directly addressed payload (formats 6 and 7): rows of whole 16-byte units, only
/// the first `height / 16 * 16` rows present.
#[derive(Clone, Copy)]
struct DirectLayout {
    row_bytes: usize,
    width: usize,
    banded: usize,
    first_row: usize,
}

/// Run `row_fn(source bytes of a row, the row)` over every present row of the band; rows beyond the
/// last whole band of 16 are zero-filled. The samples after the last whole unit of a row are left alone.
fn direct_rows(
    data: &[u8],
    g: DirectLayout,
    out: &mut [u16],
    parallel: bool,
    memory_bound: bool,
    row_fn: impl Fn(&[u8], &mut [u16]) + Sync + Send,
) {
    let work = |r0: usize, chunk: &mut [u16]| {
        for (k, row) in chunk.chunks_exact_mut(g.width).enumerate() {
            let y = g.first_row + r0 + k;
            if y >= g.banded {
                row.fill(0);
            } else {
                row_fn(&data[y * g.row_bytes..(y + 1) * g.row_bytes], row);
            }
        }
    };
    if memory_bound {
        par::for_each_row_chunk_bounded(out, g.width, 4, parallel, work);
    } else {
        par::for_each_row_chunk(out, g.width, rows_per_task(out.len() / g.width), parallel, work);
    }
}

/// Apply `unit` to the 16-byte units of one row and the matching groups of `per_unit` samples.
#[inline(always)]
fn row_of_units<const PER: usize>(src: &[u8], row: &mut [u16], unit: impl Fn(&[u8; 16], &mut [u16])) {
    for (pk, dst) in src.chunks_exact(16).zip(row.chunks_exact_mut(PER)) {
        unit(pk.try_into().unwrap(), dst);
    }
}

fn row7_14(src: &[u8], row: &mut [u16]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("ssse3") && std::arch::is_x86_feature_detected!("sse4.1") {
        // SAFETY: the required CPU features were just detected.
        return unsafe { x86::row14(src, row) };
    }
    row_of_units::<9>(src, row, unpack::<14, 9>)
}

fn row7_12(src: &[u8], row: &mut [u16]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("ssse3") && std::arch::is_x86_feature_detected!("sse4.1") {
        // SAFETY: the required CPU features were just detected.
        return unsafe { x86::row12(src, row) };
    }
    row_of_units::<10>(src, row, unpack::<12, 10>)
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    /// Rows of 14-bit packing: 9 samples per 16 bytes. Eight samples come from two vectors of
    /// 32-bit windows, the ninth is read separately.
    #[target_feature(enable = "ssse3,sse4.1")]
    pub unsafe fn row14(src: &[u8], row: &mut [u16]) {
        // SAFETY (whole function): loads and stores stay inside the unit slices obtained from
        // `chunks_exact`, which hold 16 bytes and 9 samples.
        unsafe {
            let lo = _mm_setr_epi8(0, 1, 2, 3, 1, 2, 3, 4, 3, 4, 5, 6, 5, 6, 7, 8);
            let hi = _mm_setr_epi8(7, 8, 9, 10, 8, 9, 10, 11, 10, 11, 12, 13, 12, 13, 14, 15);
            let mul_lo = _mm_setr_epi32(1 << 18, 1 << 12, 1 << 14, 1 << 16);
            for (pk, dst) in src.chunks_exact(16).zip(row.chunks_exact_mut(9)) {
                let v = _mm_loadu_si128(pk.as_ptr().cast());
                let a = _mm_srli_epi32::<18>(_mm_mullo_epi32(_mm_shuffle_epi8(v, lo), mul_lo));
                let b = _mm_srli_epi32::<18>(_mm_mullo_epi32(_mm_shuffle_epi8(v, hi), mul_lo));
                _mm_storeu_si128(dst.as_mut_ptr().cast(), _mm_packus_epi32(a, b));
                dst[8] = u16::from_le_bytes([pk[14], pk[15]]) & 0x3fff;
            }
        }
    }

    /// Rows of 12-bit packing: 10 samples per 16 bytes; eight from one vector, two separately.
    #[target_feature(enable = "ssse3,sse4.1")]
    pub unsafe fn row12(src: &[u8], row: &mut [u16]) {
        // SAFETY (whole function): as for `row14`, with 10 samples per unit.
        unsafe {
            let sh = _mm_setr_epi8(0, 1, 1, 2, 3, 4, 4, 5, 6, 7, 7, 8, 9, 10, 10, 11);
            let mul = _mm_setr_epi16(16, 1, 16, 1, 16, 1, 16, 1);
            for (pk, dst) in src.chunks_exact(16).zip(row.chunks_exact_mut(10)) {
                let v = _mm_loadu_si128(pk.as_ptr().cast());
                let r = _mm_srli_epi16::<4>(_mm_mullo_epi16(_mm_shuffle_epi8(v, sh), mul));
                let r = _mm_and_si128(r, _mm_set1_epi16(0xfff));
                _mm_storeu_si128(dst.as_mut_ptr().cast(), r);
                dst[8] = u16::from_le_bytes([pk[12], pk[13]]) & 0xfff;
                dst[9] = (u16::from_le_bytes([pk[13], pk[14]]) >> 4) & 0xfff;
            }
        }
    }
}

/// Decode a whole format-7 image.
pub fn decode_v7(data: &[u8], bits: u32, width: usize, height: usize, out: &mut [u16], parallel: bool) -> Result<()> {
    let n = width.saturating_mul(height);
    if width == 0 || height == 0 || out.len() < n {
        return Err(too_small());
    }
    decode_v7_rows(data, bits, width, height, 0, &mut out[..n], parallel)
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
    let ppp = packed_pixels(bits);
    let (rows, row_bytes) = direct_check("v7", data.len(), bits, ppp, width, height, first_row, out.len())?;
    let g = DirectLayout { row_bytes, width, banded: height / 16 * 16, first_row };
    let out = &mut out[..width * rows];
    if bits == 12 {
        direct_rows(data, g, out, parallel, true, row7_12);
    } else {
        direct_rows(data, g, out, parallel, true, row7_14);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Format 6

/// Constants of the two format-6 bit depths.
struct V6Mode {
    /// Pixels per block.
    pixels: usize,
    /// Width of the first two values and of the others.
    first_bits: u32,
    delta_bits: u32,
    /// Selector base, carry limit, in-range mask and saturation value.
    base: u32,
    limit: u32,
    range: u32,
    sat: u32,
}

const V6_14: V6Mode =
    V6Mode { pixels: 11, first_bits: 14, delta_bits: 10, base: 0x200, limit: 0x2000, range: 0xffff, sat: 0x3fff };
const V6_12: V6Mode =
    V6Mode { pixels: 14, first_bits: 12, delta_bits: 8, base: 0x80, limit: 0x800, range: 0x3fff, sat: 0xfff };

/// Running state of one channel of a format-6 block.
#[derive(Clone, Copy, Default)]
struct V6Chan {
    /// The channel has seen a non-zero first value; from then on stored values are deltas.
    started: bool,
    /// The previous reconstructed value.
    last: u32,
}

impl V6Chan {
    /// Reconstructed value for the stored field `raw`. `scale` multiplies a delta; `floor` is the
    /// level above which the previous value is carried over (only below `mode.limit`).
    #[inline(always)]
    fn next(&mut self, mode: &V6Mode, raw: u32, scale: u32, floor: u32) -> u32 {
        let e = if self.started {
            let mut e = raw.wrapping_mul(scale);
            if floor < mode.limit && self.last > floor {
                e = e.wrapping_add(self.last - floor);
            }
            e
        } else {
            self.started = raw != 0;
            raw
        };
        self.last = e;
        e
    }
}

/// Constant removed from every reconstructed format-6 value.
const V6_OFFSET: u32 = 15;

/// Final value of a format-6 sample from the reconstructed value `e`.
///
/// `x = e - 15` (wrapping) is kept when it fits the range; otherwise the sample saturates when
/// `x` is a positive overflow (top bit clear, i.e. `e` is above the range) and is 0 when `e` was
/// below 15 (the subtraction wrapped, top bit set). This is the same decision as the sign test of
/// `e + 0x7fff_fff1`, because `0x7fff_fff1 + 15 == 2^31`: adding `2^31` flips the top bit of `x`.
#[inline(always)]
fn v6_sample(mode: &V6Mode, e: u32) -> u16 {
    let x = e.wrapping_sub(V6_OFFSET);
    if x <= mode.range {
        (x & mode.range) as u16
    } else if x >> 31 == 0 {
        mode.sat as u16
    } else {
        0
    }
}

/// Decode one format-6 block into `dst[..mode.pixels]`.
///
/// Two leading values (one per channel) are followed by groups of three pixels, each group
/// starting with a 2-bit selector that picks the scale of its three deltas. Pixels alternate
/// between the two channels.
#[inline(always)]
fn v6_block(mode: &V6Mode, block: &[u8; 16], dst: &mut [u16]) {
    let mut v = u128::from_le_bytes(*block);
    let mut take = |n: u32| -> u32 {
        let r = (v >> (128 - n)) as u32;
        v <<= n;
        r
    };
    let mut chans = [V6Chan::default(); 2];
    let (mut scale, mut floor) = (1u32, 0u32);
    for (j, d) in dst[..mode.pixels].iter_mut().enumerate() {
        let raw = if j < 2 {
            take(mode.first_bits)
        } else {
            if j % 3 == 2 {
                let shift = SHIFT_OF_SELECTOR[take(2) as usize & 3];
                (scale, floor) = (1u32 << shift, mode.base << shift);
            }
            take(mode.delta_bits)
        };
        *d = v6_sample(mode, chans[j & 1].next(mode, raw, scale, floor));
    }
}

/// Decode a whole format-6 image.
pub fn decode_v6(data: &[u8], bits: u32, width: usize, height: usize, out: &mut [u16], parallel: bool) -> Result<()> {
    let n = width.saturating_mul(height);
    if width == 0 || height == 0 || out.len() < n {
        return Err(too_small());
    }
    decode_v6_rows(data, bits, width, height, 0, &mut out[..n], parallel)
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
    let mode = match bits {
        12 => Some(&V6_12),
        14 => Some(&V6_14),
        _ => None,
    };
    let (rows, row_bytes) =
        direct_check("v6", data.len(), bits, mode.map(|m| m.pixels), width, height, first_row, out.len())?;
    let g = DirectLayout { row_bytes, width, banded: height / 16 * 16, first_row };
    let out = &mut out[..width * rows];
    if bits == 12 {
        direct_rows(data, g, out, parallel, false, |src, row| {
            row_of_units::<14>(src, row, |pk, dst| v6_block(&V6_12, pk, dst))
        });
    } else {
        direct_rows(data, g, out, parallel, false, |src, row| {
            row_of_units::<11>(src, row, |pk, dst| v6_block(&V6_14, pk, dst))
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Format 8

/// Format-8 parameters, taken from RW2 tags 0x39 to 0x41.
#[derive(Debug, Clone, Default)]
pub struct V8Params {
    /// Tone-curve points (tag 0x3A), six entries.
    pub gamma_points: [u32; 6],
    /// Tone-curve slope codes (tag 0x39), six entries.
    pub gamma_slopes: [u32; 6],
    /// Upper limit of reconstructed values (tag 0x3B).
    pub clip: u32,
    /// Starting predictions for the top-left, top-right, bottom-left and bottom-right pixel of a 2x2 block (tags 0x3C to 0x3F).
    pub initial: [u16; 4],
    /// `(length, code)` per symbol (tag 0x40).
    pub codes: Vec<(u8, u16)>,
    /// Per-symbol number of magnitude bits that are not stored (tag 0x41).
    pub shift_down: Vec<u8>,
}

/// One independently coded rectangle of a format-8 image.
#[derive(Debug, Clone, Copy)]
pub struct V8Strip<'a> {
    /// The strip's bytes as present in the file.
    pub data: &'a [u8],
    /// Declared length of the strip's bit stream.
    pub bits: u64,
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

/// Symbol number of a code that matches nothing; it consumes one bit and yields difference 0.
const V8_BAD: u16 = 17;

/// What one decoded symbol means.
#[derive(Clone, Copy, Default)]
struct V8Sym {
    shift: u32,
    /// Added when the sign bit is clear.
    neg: i32,
    /// Rounding offset.
    add: i32,
}

/// A format-8 decoder for one file's parameters.
pub struct V8Decoder {
    /// Indexed by the next stream bits (first bit lowest): symbol | code length << 5 | extra bits << 10.
    table: Vec<u16>,
    mask: u64,
    sym: [V8Sym; 18],
    initial: [i32; 4],
    clip: i32,
    /// Tone curve, absent when it is the identity.
    curve: Option<Box<[u16]>>,
}

/// The 64 stream bits from bit `pos` on, first bit lowest (zeros past the end of `data`).
#[inline(always)]
fn window(data: &[u8], pos: u64) -> u64 {
    let b = (pos >> 3) as usize;
    let w = match data.get(b..b.wrapping_add(8)) {
        Some(s) => u64::from_le_bytes(s.try_into().unwrap()),
        None => window_slow(data, b),
    };
    w >> (pos & 7)
}

#[cold]
#[inline(never)]
fn window_slow(data: &[u8], b: usize) -> u64 {
    let mut w = [0u8; 8];
    if let Some(t) = data.get(b..) {
        let n = t.len().min(8);
        w[..n].copy_from_slice(&t[..n]);
    }
    u64::from_le_bytes(w)
}

/// Order in which the four differences of a 2x2 block are coded.
const V8_ORDER: [usize; 4] = [0, 2, 1, 3];

impl V8Decoder {
    pub fn new(p: &V8Params) -> Result<Self> {
        if p.codes.is_empty() || p.codes.len() > 17 {
            return Err(Error::corrupt("invalid Panasonic v8 Huffman table size"));
        }
        let max_len = p.codes.iter().map(|c| c.0 as u32).filter(|&l| l <= 16).max().unwrap_or(0).max(1);
        let mut sym = [V8Sym::default(); 18];
        let mut extra = [0u16; 17];
        for s in 0..p.codes.len() {
            let sd = p.shift_down.get(s).copied().unwrap_or(0).min(31) as u32;
            extra[s] = (s as u32).saturating_sub(sd) as u16;
            sym[s] = V8Sym {
                shift: sd,
                neg: if s == 0 { 0 } else { -(1i32 << s) + i32::from(sd == 0) },
                add: if sd > 0 { 1 << (sd - 1) } else { 0 },
            };
        }
        // Lowest symbol index wins: fill from the highest down. Entry 0 marks "unset" (a real
        // entry has a code length of at least 1).
        let size = 1usize << max_len;
        let mut table = vec![0u16; size];
        for (s, &(len, code)) in p.codes.iter().enumerate().rev() {
            let len = len as u32;
            if len == 0 || len > max_len {
                continue;
            }
            let entry = s as u16 | (len as u16) << 5 | extra[s] << 10;
            let prefix = (code as u32 & ((1 << len) - 1)) << (max_len - len);
            for suffix in 0..1u32 << (max_len - len) {
                let idx = ((prefix | suffix).reverse_bits() >> (32 - max_len)) as usize;
                table[idx] = entry;
            }
        }
        for e in &mut table {
            if *e == 0 {
                *e = V8_BAD | 1 << 5;
            }
        }
        let curve = {
            let mut t = Vec::with_capacity(0x10000);
            let mut identity = true;
            for i in 0..=0xffffu32 {
                let v = crate::panasonic_gamma::gamma_curve(&p.gamma_points, &p.gamma_slopes, p.clip, i);
                identity &= v as u32 == i;
                t.push(v);
            }
            (!identity).then(|| t.into_boxed_slice())
        };
        Ok(V8Decoder {
            table,
            mask: size as u64 - 1,
            sym,
            initial: p.initial.map(i32::from),
            clip: p.clip.min(i32::MAX as u32) as i32,
            curve,
        })
    }

    /// Decode one difference at stream position `pos`; returns it and the new position, and
    /// sets `bad` for a code that matches nothing.
    #[inline(always)]
    fn diff(&self, data: &[u8], pos: u64, bad: &mut u32) -> (i32, u64) {
        let w = window(data, pos);
        let e = self.table[(w & self.mask) as usize] as u32;
        let (s, len, nbits) = (e & 31, (e >> 5) & 31, e >> 10);
        let t = w >> len;
        // The first extra bit is the sign and the most significant bit of the magnitude field.
        let raw = ((t as u32).reverse_bits() as u64 >> (32 - nbits)) as i32;
        let sy = &self.sym[s as usize];
        let neg_mask = ((t & 1) as i32).wrapping_sub(1);
        *bad |= u32::from(s == V8_BAD as u32);
        let d = (raw << sy.shift).wrapping_add(sy.add).wrapping_add(sy.neg & neg_mask);
        (d, pos + (len + nbits) as u64)
    }

    /// Decode all strips into the `raw_width` x `raw_height` buffer `out`.
    pub fn decode(
        &self,
        strips: &[V8Strip],
        out: &mut [u16],
        raw_width: usize,
        raw_height: usize,
        parallel: bool,
    ) -> Result<()> {
        if out.len() < raw_width.saturating_mul(raw_height) || raw_width == 0 || strips.is_empty() {
            return Err(Error::InvalidArgument("Panasonic v8 output too small".into()));
        }
        let rect =
            |s: &V8Strip| (s.left as u64, s.top as u64, s.left as u64 + s.width as u64, s.top as u64 + s.height as u64);
        let mut area = 0u128;
        for (i, s) in strips.iter().enumerate() {
            let (l, t, r, b) = rect(s);
            if s.width < 2
                || s.height < 2
                || s.width % 2 != 0
                || s.height % 2 != 0
                || r > raw_width as u64
                || b > raw_height as u64
            {
                return Err(Error::corrupt("invalid Panasonic v8 strip geometry"));
            }
            for o in &strips[..i] {
                let (l2, t2, r2, b2) = rect(o);
                if l < r2 && l2 < r && t < b2 && t2 < b {
                    return Err(Error::corrupt("overlapping Panasonic v8 strips"));
                }
            }
            area += s.width as u128 * s.height as u128;
        }
        if area != raw_width as u128 * raw_height as u128 {
            return Err(Error::corrupt("Panasonic v8 strips do not cover the image"));
        }
        // The rectangles are inside the buffer and pairwise disjoint (checked above), so each
        // strip may hold mutable views of its own rows.
        let base = SharedOut(out.as_mut_ptr());
        let jobs: Vec<(&V8Strip, Vec<&mut [u16]>)> = strips
            .iter()
            .map(|s| {
                let rows = (0..s.height as usize)
                    .map(|y| {
                        let start = (s.top as usize + y) * raw_width + s.left as usize;
                        // SAFETY: `start + width <= raw_width * raw_height <= out.len()` and the
                        // rectangle is disjoint from every other strip's.
                        unsafe { std::slice::from_raw_parts_mut(base.0.add(start), s.width as usize) }
                    })
                    .collect();
                (s, rows)
            })
            .collect();
        let speculate = parallel && strips.len() < par::threads();
        let results = par::map(jobs, parallel, |(s, rows)| self.strip(s, rows, speculate));
        results.into_iter().collect()
    }

    /// Decode one strip into its row segments (`rows[y]` is the strip's part of row `y`).
    pub fn decode_strip(&self, s: &V8Strip, rows: Vec<&mut [u16]>) -> Result<()> {
        self.strip(s, rows, false)
    }

    fn strip(&self, s: &V8Strip, rows: Vec<&mut [u16]>, speculate: bool) -> Result<()> {
        let (w, h) = (s.width as usize, s.height as usize);
        if w / 2 == 0 || h / 2 == 0 || rows.len() < h || rows.iter().any(|r| r.len() < w) {
            return Err(Error::corrupt("invalid Panasonic v8 strip"));
        }
        let mut rows: Vec<&mut [u16]> = rows.into_iter().map(|r| &mut r[..w / 2 * 2]).collect();
        let budget = s.bits.div_ceil(64).saturating_mul(64);
        if speculate && w * h >= crate::ljpeg_par::PARALLEL_MIN_SAMPLES / 4 {
            if let Some(done) = self.strip_speculative(s, &mut rows, budget) {
                return done;
            }
        }
        if self.curve.is_some() {
            self.strip_sequential::<true>(s, &mut rows, budget)
        } else {
            self.strip_sequential::<false>(s, &mut rows, budget)
        }
    }

    /// Reconstruct and store one row pair from a supply of block differences; returns the
    /// predictions after its first block.
    #[inline(always)]
    fn pair<const CURVE: bool>(
        &self,
        r0: &mut [u16],
        r1: &mut [u16],
        mut cur: [i32; 4],
        mut next: impl FnMut() -> [i32; 4],
    ) -> [i32; 4] {
        let mut first = cur;
        let clip = self.clip;
        let put = |v: i32| -> u16 {
            match (&self.curve, CURVE) {
                (Some(t), true) => t[v as u16 as usize],
                _ => v as u16,
            }
        };
        for (bx, (a, b)) in r0.chunks_exact_mut(2).zip(r1.chunks_exact_mut(2)).enumerate() {
            let d = next();
            for k in 0..4 {
                cur[k] = cur[k].wrapping_add(d[k]).max(0).min(clip);
            }
            a[0] = put(cur[0]);
            a[1] = put(cur[1]);
            b[0] = put(cur[2]);
            b[1] = put(cur[3]);
            if bx == 0 {
                first = cur;
            }
        }
        first
    }

    fn strip_sequential<const CURVE: bool>(&self, s: &V8Strip, rows: &mut [&mut [u16]], budget: u64) -> Result<()> {
        let data = s.data;
        let mut pos = 0u64;
        let mut bad = 0u32;
        let mut start = self.initial;
        for pair in rows.chunks_exact_mut(2).take(s.height as usize / 2) {
            let (top, bottom) = pair.split_at_mut(1);
            start = self.pair::<CURVE>(top[0], bottom[0], start, || {
                let mut d = [0i32; 4];
                for k in V8_ORDER {
                    let (v, p) = self.diff(data, pos, &mut bad);
                    d[k] = v;
                    pos = p;
                }
                d
            });
            if bad != 0 {
                return Err(Error::corrupt("invalid Panasonic v8 Huffman code"));
            }
            if pos > budget {
                return Err(Error::corrupt("truncated Panasonic v8 strip"));
            }
        }
        Ok(())
    }

    /// Decode the symbols on all threads, then rebuild the pixels; `None` when the sequential
    /// decoder has to judge the stream.
    fn strip_speculative(&self, s: &V8Strip, rows: &mut [&mut [u16]], budget: u64) -> Option<Result<()>> {
        let (bw, pairs) = (s.width as usize / 2, s.height as usize / 2);
        let chunk = crate::par_entropy::default_chunk(s.data.len())?;
        let stream = V8Stream { dec: self, data: s.data, budget };
        let values = crate::par_entropy::decode(&stream, bw * pairs * 4, chunk)?;
        let diff_at =
            |sym: usize| -> i32 { (values.get(2 * sym) as u32 | (values.get(2 * sym + 1) as u32) << 16) as i32 };
        // Prediction at the start of every row pair: the first block of the previous pair.
        let mut starts = Vec::with_capacity(pairs);
        let mut cur = self.initial;
        for q in 0..pairs {
            starts.push(cur);
            for (j, &k) in V8_ORDER.iter().enumerate() {
                cur[k] = cur[k].wrapping_add(diff_at(q * bw * 4 + j)).max(0).min(self.clip);
            }
        }
        let per_task = pairs.div_ceil(par::threads() * 4).max(1);
        let tasks: Vec<(usize, &mut [&mut [u16]])> =
            rows.chunks_mut(2 * per_task).enumerate().map(|(i, c)| (i * per_task, c)).collect();
        par::map(tasks, true, |(q0, rows)| {
            let mut scratch = vec![0u16; bw * 8];
            for (i, pair) in rows.chunks_exact_mut(2).enumerate() {
                let q = q0 + i;
                values.copy_to(q * bw * 8, &mut scratch);
                let (a, b) = pair.split_at_mut(1);
                let mut it = scratch.chunks_exact(8);
                let next = || {
                    let c = it.next().unwrap();
                    let mut d = [0i32; 4];
                    for (j, &k) in V8_ORDER.iter().enumerate() {
                        d[k] = (c[2 * j] as u32 | (c[2 * j + 1] as u32) << 16) as i32;
                    }
                    d
                };
                if self.curve.is_some() {
                    self.pair::<true>(a[0], b[0], starts[q], next);
                } else {
                    self.pair::<false>(a[0], b[0], starts[q], next);
                }
            }
        });
        Some(Ok(()))
    }
}

struct SharedOut(*mut u16);
// SAFETY: only used to hand out views of disjoint rectangles (see `V8Decoder::decode`).
unsafe impl Send for SharedOut {}
unsafe impl Sync for SharedOut {}

/// The symbols of one strip as seen by the speculative parallel decoder.
struct V8Stream<'a> {
    dec: &'a V8Decoder,
    data: &'a [u8],
    budget: u64,
}

impl crate::par_entropy::SymbolStream for V8Stream<'_> {
    type Pump<'p>
        = u64
    where
        Self: 'p;
    const WIDTH: usize = 2;
    fn cycle(&self) -> usize {
        1
    }
    fn period(&self) -> usize {
        1
    }
    fn total_bits(&self) -> u64 {
        self.data.len() as u64 * 8
    }
    fn end_check(&self, pos: u64) -> crate::par_entropy::EndCheck {
        use crate::par_entropy::EndCheck;
        if pos <= self.budget { EndCheck::Valid } else { EndCheck::Invalid }
    }
    fn pump_at(&self, bit: u64) -> u64 {
        bit
    }
    fn position(p: &u64) -> u64 {
        *p
    }
    #[inline(always)]
    fn decode(&self, p: &mut u64, _phase: usize, out: &mut [u16]) -> bool {
        let mut bad = 0;
        let (d, pos) = self.dec.diff(self.data, *p, &mut bad);
        *p = pos;
        out[0] = d as u32 as u16;
        out[1] = (d as u32 >> 16) as u16;
        bad == 0
    }
}
