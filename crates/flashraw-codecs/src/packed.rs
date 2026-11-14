//! Uncompressed sensor data stored as fixed-width samples packed into bytes or
//! little-endian words: 8, 10, 12, 14 and 16-bit layouts (and any width from 1 to 16
//! through the generic path), with optional row padding and truncation recovery.
//!
//! # Bit model
//!
//! Every [`PackOrder`] is a bit stream cut into `bits`-wide fields, one per sample:
//!
//! * `LsbBytes`: the bytes form one little-endian integer, fields are taken from the
//!   least significant end.
//! * `MsbBytes`: the bytes form one big-endian integer, fields are taken from the most
//!   significant end (TIFF/DNG uncompressed data).
//! * `Msb16` / `Msb32`: the same as `MsbBytes` once each little-endian 16-bit (32-bit)
//!   word has been put into big-endian byte order, that is, stream byte `j` is stored
//!   byte `j ^ 1` (`j ^ 3`).
//!
//! Bytes past the end of the input read as zero, byte by byte at their stored position.
//!
//! # Implementation
//!
//! A sample never spans more than 64 bits measured from the start of its storage word,
//! so every sample can be cut out of one 64-bit "window" loaded at its first word
//! (re-ordered so that stream bits run from the most significant end, or from the least
//! significant end for `LsbBytes`). The reference reads one window per sample. The fast
//! row decoder works on groups of samples whose total width is a multiple of 32 bits,
//! so that the window positions inside a group are compile-time constants; the last
//! few samples of a row go through the reference. 8 and 16-bit layouts
//! are copies, and 12-bit byte-stream and `Msb16` rows have vector kernels (x86-64
//! SSSE3+SSE4.1, aarch64 NEON, wasm32 SIMD128).

use flashraw_core::{Error, PackOrder, Result, par};

/// Geometry and bit layout of a packed sample raster.
#[derive(Debug, Clone, Copy)]
pub struct PackedLayout {
    pub width: usize,
    pub height: usize,
    /// Bits per sample (1..=16).
    pub bits: u32,
    pub order: PackOrder,
    /// Distance in bytes between the starts of consecutive rows.
    pub row_stride: usize,
}

impl PackedLayout {
    /// A layout whose rows follow each other with no padding.
    pub fn tight(width: usize, height: usize, bits: u32, order: PackOrder) -> Self {
        let mut layout = PackedLayout { width, height, bits, order, row_stride: 0 };
        layout.row_stride = layout.row_bytes();
        layout
    }

    /// Bytes holding the samples of one row (`width * bits` rounded up to whole bytes).
    pub fn row_bytes(&self) -> usize {
        (self.width * self.bits as usize).div_ceil(8)
    }
}

// ---------------------------------------------------------------------------------------
// Orders as small integers (usable as const generic parameters).

const MSB_BYTES: u8 = 0;
const LSB_BYTES: u8 = 1;
const MSB_16: u8 = 2;
const MSB_32: u8 = 3;

fn order_code(order: PackOrder) -> u8 {
    match order {
        PackOrder::MsbBytes => MSB_BYTES,
        PackOrder::LsbBytes => LSB_BYTES,
        PackOrder::Msb16 => MSB_16,
        PackOrder::Msb32 => MSB_32,
    }
}

/// Size in bytes of the storage word of an order (1 for the byte streams).
#[inline(always)]
const fn word_bytes(ord: u8) -> usize {
    match ord {
        MSB_16 => 2,
        MSB_32 => 4,
        _ => 1,
    }
}

/// Turn 8 stored bytes (read as a little-endian `u64`) that start on a word boundary
/// into the 64 stream bits they hold: most significant bit first for the `Msb*` orders,
/// least significant bit first for `LsbBytes`.
#[inline(always)]
const fn stream_bits(le: u64, ord: u8) -> u64 {
    match ord {
        LSB_BYTES => le,
        MSB_BYTES => le.swap_bytes(),
        MSB_32 => le.rotate_left(32),
        _ => {
            // Four little-endian u16 words w0..w3 (w0 lowest) -> w0 w1 w2 w3 from the top.
            let halves = le.rotate_left(32);
            ((halves & 0x0000_FFFF_0000_FFFF) << 16) | ((halves >> 16) & 0x0000_FFFF_0000_FFFF)
        }
    }
}

/// Cut a `bits`-wide field (1..=32) starting `off` stream bits into a window.
#[inline(always)]
const fn field(window: u64, off: usize, bits: usize, ord: u8) -> u16 {
    if ord == LSB_BYTES {
        ((window >> off) & ((1u64 << bits) - 1)) as u16
    } else {
        ((window << off) >> (64 - bits)) as u16
    }
}

#[inline(always)]
fn load_le64(bytes: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(b)
}

/// The 8 stored bytes at `at`, zero-extended past the end of `src`.
#[inline]
fn load_le64_padded(src: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    if at < src.len() {
        let n = (src.len() - at).min(8);
        b[..n].copy_from_slice(&src[at..at + n]);
    }
    u64::from_le_bytes(b)
}

// ---------------------------------------------------------------------------------------
// Reference.

/// The plain reference unpacker: one sample at a time, any supported layout.
///
/// `bits` above 16 keep the low 16 bits of each field; above 32 bits it panics.
pub fn unpack_row_scalar(src: &[u8], dst: &mut [u16], bits: u32, order: PackOrder) {
    if bits == 0 {
        dst.fill(0);
        return;
    }
    assert!(bits <= 32, "packed sample width {bits} is above 32 bits");
    let bits = bits as usize;
    match order_code(order) {
        MSB_BYTES => samples_one_by_one::<MSB_BYTES>(src, dst, bits),
        LSB_BYTES => samples_one_by_one::<LSB_BYTES>(src, dst, bits),
        MSB_16 => samples_one_by_one::<MSB_16>(src, dst, bits),
        _ => samples_one_by_one::<MSB_32>(src, dst, bits),
    }
}

#[inline]
fn samples_one_by_one<const ORD: u8>(src: &[u8], dst: &mut [u16], bits: usize) {
    let word = word_bytes(ORD);
    let len = src.len();
    // Samples whose first byte is at least 8 bytes before the end can load directly.
    let direct = if len >= 8 { (((len - 8) * 8 + 7) / bits + 1).min(dst.len()) } else { 0 };
    let (head, tail) = dst.split_at_mut(direct);
    let mut pos = 0;
    if ORD == MSB_16 && bits <= 16 {
        // A sample of at most 16 bits lies within two 16-bit words: reorder just those.
        for out in head {
            let at = (pos / 8) & !1;
            let two = (load_le64(src, at) as u32).rotate_left(16);
            *out = field((two as u64) << 32, pos - at * 8, bits, ORD);
            pos += bits;
        }
    } else {
        for out in head {
            let at = (pos / 8) & !(word - 1);
            *out = field(stream_bits(load_le64(src, at), ORD), pos - at * 8, bits, ORD);
            pos += bits;
        }
    }
    for out in tail {
        let at = (pos / 8) & !(word - 1);
        *out = field(stream_bits(load_le64_padded(src, at), ORD), pos - at * 8, bits, ORD);
        pos += bits;
    }
}

// ---------------------------------------------------------------------------------------
// Fast generic path: groups of samples spanning a whole number of 32-bit words.

const fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Samples per group: the smallest count whose width is a multiple of 32 bits.
const fn group_samples(bits: usize) -> usize {
    32 / gcd(bits, 32)
}

/// Byte offset of the 64-bit window that sample `k` of a group is cut from: windows are
/// placed greedily, a new one (at the storage word holding the sample's first bit)
/// whenever the sample does not end inside the current one. A window therefore never
/// starts after the group's last word and never reaches more than 8 bytes past it.
const fn window_start(bits: usize, word: usize, k: usize) -> usize {
    let mut start = 0;
    let mut j = 0;
    while j <= k {
        if j * bits + bits > start * 8 + 64 {
            start = (j * bits / 8) / word * word;
        }
        j += 1;
    }
    start
}

/// Decode one group of samples from `chunk`, which holds at least the group's bytes
/// plus 8 bytes of slack.
#[inline(always)]
fn decode_group<const B: usize, const ORD: u8>(chunk: &[u8], out: &mut [u16]) {
    let g = const { group_samples(B) };
    let words = const { group_samples(B) * B / 32 };
    let chunk = &chunk[..words * 4 + 8];
    let out = &mut out[..g];
    // Fully unrolled so that every offset is a constant and samples of the same window
    // share one load.
    macro_rules! each_sample {
        ($($k:literal)*) => {$(
            if $k < g {
                let at = const { window_start(B, word_bytes(ORD), $k) };
                out[$k] = field(stream_bits(load_le64(chunk, at), ORD), $k * B - 8 * at, B, ORD);
            }
        )*};
    }
    each_sample!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31);
}

/// Decode every sample of `dst` (bits `B` in 1..=15) with zero extension past `src`.
fn row_grouped<const B: usize, const ORD: u8>(src: &[u8], dst: &mut [u16]) {
    let g = const { group_samples(B) };
    let gbytes = const { group_samples(B) * B / 8 };
    let span = gbytes + 8;
    // Whole groups whose bytes (and window slack) are inside `src`.
    let fast = if src.len() >= span { ((src.len() - span) / gbytes + 1).min(dst.len() / g) } else { 0 };
    for (i, out) in dst[..fast * g].chunks_exact_mut(g).enumerate() {
        decode_group::<B, ORD>(&src[i * gbytes..], out);
    }
    let done = fast * g;
    if done < dst.len() {
        finish_row::<B, ORD>(&src[fast * gbytes..], &mut dst[done..]);
    }
}

/// The end of a row (or a row too short for whole groups): 12-bit byte streams as
/// 3-byte pairs, then sample by sample with zero extension.
#[inline]
fn finish_row<const B: usize, const ORD: u8>(src: &[u8], dst: &mut [u16]) {
    if B == 12 && (ORD == MSB_BYTES || ORD == LSB_BYTES) {
        let pairs = (src.len() / 3).min(dst.len() / 2);
        for (o, c) in dst[..pairs * 2].chunks_exact_mut(2).zip(src.chunks_exact(3)) {
            let (b0, b1, b2) = (c[0] as u16, c[1] as u16, c[2] as u16);
            if ORD == MSB_BYTES {
                o[0] = (b0 << 4) | (b1 >> 4);
                o[1] = ((b1 & 0x0F) << 8) | b2;
            } else {
                o[0] = b0 | ((b1 & 0x0F) << 8);
                o[1] = (b1 >> 4) | (b2 << 4);
            }
        }
        if pairs * 2 < dst.len() {
            samples_one_by_one::<ORD>(&src[pairs * 3..], &mut dst[pairs * 2..], B);
        }
    } else {
        samples_one_by_one::<ORD>(src, dst, B);
    }
}

/// 8-bit samples of a byte stream: a widening copy.
#[inline]
fn row_u8(src: &[u8], dst: &mut [u16]) {
    let n = src.len().min(dst.len());
    for (o, &b) in dst[..n].iter_mut().zip(&src[..n]) {
        *o = b as u16;
    }
    dst[n..].fill(0);
}

/// 16-bit little-endian samples (`LsbBytes` and `Msb16`): a copy on little-endian
/// machines.
fn row_u16_le(src: &[u8], dst: &mut [u16]) {
    let n = (src.len() / 2).min(dst.len());
    #[cfg(target_endian = "little")]
    // SAFETY: `n * 2` bytes are inside `src` and `n` samples inside `dst`; every bit
    // pattern is a valid u16 and the regions cannot overlap (shared vs unique borrow).
    unsafe {
        std::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr().cast::<u8>(), n * 2);
    }
    #[cfg(not(target_endian = "little"))]
    for (o, c) in dst[..n].iter_mut().zip(src.chunks_exact(2)) {
        *o = u16::from_le_bytes([c[0], c[1]]);
    }
    finish_u16(src, dst, n, 2, |b, out| out[0] = u16::from_le_bytes([b[0], b[1]]));
}

/// 16-bit big-endian samples: a byte swap per sample.
fn row_u16_be(src: &[u8], dst: &mut [u16]) {
    let n = (src.len() / 2).min(dst.len());
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: AVX2 was just detected.
        unsafe { swap_pairs_avx2(&src[..n * 2], &mut dst[..n]) };
    } else {
        swap_pairs(&src[..n * 2], &mut dst[..n]);
    }
    #[cfg(not(target_arch = "x86_64"))]
    swap_pairs(&src[..n * 2], &mut dst[..n]);
    finish_u16(src, dst, n, 2, |b, out| out[0] = u16::from_be_bytes([b[0], b[1]]));
}

#[inline(always)]
fn swap_pairs(src: &[u8], dst: &mut [u16]) {
    for (o, c) in dst.iter_mut().zip(src.chunks_exact(2)) {
        *o = u16::from_be_bytes([c[0], c[1]]);
    }
}

/// The same plain loop, compiled for 256-bit registers (the compiler vectorises it; the
/// byte swap becomes one shuffle per 16 samples).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn swap_pairs_avx2(src: &[u8], dst: &mut [u16]) {
    swap_pairs(src, dst)
}

/// 16-bit samples in 32-bit little-endian words read from the top: each word gives its
/// high half, then its low half.
fn row_u16_msb32(src: &[u8], dst: &mut [u16]) {
    let words = (src.len() / 4).min(dst.len() / 2);
    for (o, c) in dst[..words * 2].chunks_exact_mut(2).zip(src.chunks_exact(4)) {
        o[0] = u16::from_le_bytes([c[2], c[3]]);
        o[1] = u16::from_le_bytes([c[0], c[1]]);
    }
    finish_u16(src, dst, words * 2, 4, |b, out| {
        out[0] = u16::from_le_bytes([b[2], b[3]]);
        out[1] = u16::from_le_bytes([b[0], b[1]]);
    });
}

/// Finish a 16-bit row from sample `done` (a multiple of the samples per unit) by
/// decoding `unit`-byte units from zero-padded copies.
#[inline]
fn finish_u16(src: &[u8], dst: &mut [u16], mut done: usize, unit: usize, f: impl Fn(&[u8; 4], &mut [u16; 2])) {
    let per = unit / 2;
    while done < dst.len() {
        let base = done * 2;
        if base >= src.len() {
            dst[done..].fill(0);
            return;
        }
        let mut b = [0u8; 4];
        let n = (src.len() - base).min(unit);
        b[..n].copy_from_slice(&src[base..base + n]);
        let mut out = [0u16; 2];
        f(&b, &mut out);
        let take = (dst.len() - done).min(per);
        dst[done..done + take].copy_from_slice(&out[..take]);
        done += take;
    }
}

/// Row decoder without explicit vector code (`bits` in 1..=16).
fn row_portable(src: &[u8], dst: &mut [u16], bits: u32, ord: u8) {
    macro_rules! by_depth {
        ($($b:literal)*) => {
            match bits {
                $($b => match ord {
                    MSB_BYTES => row_grouped::<$b, MSB_BYTES>(src, dst),
                    LSB_BYTES => row_grouped::<$b, LSB_BYTES>(src, dst),
                    MSB_16 => row_grouped::<$b, MSB_16>(src, dst),
                    _ => row_grouped::<$b, MSB_32>(src, dst),
                },)*
                _ => unreachable!("bit depth checked by the caller"),
            }
        };
    }
    match (bits, ord) {
        (8, MSB_BYTES | LSB_BYTES) => row_u8(src, dst),
        (16, LSB_BYTES | MSB_16) => row_u16_le(src, dst),
        (16, MSB_BYTES) => row_u16_be(src, dst),
        (16, _) => row_u16_msb32(src, dst),
        _ => by_depth!(1 2 3 4 5 6 7 8 9 10 11 12 13 14 15),
    }
}

/// Unpack `dst.len()` samples of one row; samples past the end of `src` read zero bits.
#[inline]
pub fn unpack_row(src: &[u8], dst: &mut [u16], bits: u32, order: PackOrder, simd: bool) {
    if bits == 8 && matches!(order, PackOrder::MsbBytes | PackOrder::LsbBytes) {
        return row_u8(src, dst);
    }
    unpack_row_other(src, dst, bits, order, simd)
}

fn unpack_row_other(src: &[u8], dst: &mut [u16], bits: u32, order: PackOrder, simd: bool) {
    if bits == 0 || bits > 16 {
        return unpack_row_scalar(src, dst, bits, order);
    }
    let ord = order_code(order);
    if simd && bits == 12 {
        let done = vector::row12(src, dst, ord);
        if done > 0 {
            // The vector kernel stops on a 12-byte group boundary (a 16-bit word boundary
            // as well).
            return row_portable(&src[done / 8 * 12..], &mut dst[done..], bits, ord);
        }
    }
    row_portable(src, dst, bits, ord)
}

/// Number of samples that lie entirely within the first `len` bytes of a row.
pub fn complete_samples(len: usize, bits: u32, order: PackOrder) -> usize {
    if bits == 0 {
        return 0;
    }
    let word = word_bytes(order_code(order));
    (len / word * word * 8) / bits as usize
}

/// Unpack the complete samples of a cut-short row and zero the rest; returns how many
/// samples were complete.
pub fn unpack_row_partial(src: &[u8], dst: &mut [u16], bits: u32, order: PackOrder, simd: bool) -> usize {
    let n = complete_samples(src.len(), bits, order).min(dst.len());
    let (head, rest) = dst.split_at_mut(n);
    unpack_row(src, head, bits, order, simd);
    rest.fill(0);
    n
}

// ---------------------------------------------------------------------------------------
// Whole rasters.

/// Unpack a whole raster from `src` into `dst` (rows `dst_stride` samples apart).
/// Truncated input is an error unless `lenient`, which keeps every complete sample.
///
/// Only cells `dst[y * dst_stride + x]` with `x < width`, `y < height` are written. In
/// recovery mode the first incomplete row gets its complete samples and zeros; the rows
/// after it are left as they were.
pub fn unpack(
    src: &[u8],
    layout: PackedLayout,
    dst: &mut [u16],
    dst_stride: usize,
    parallel: bool,
    simd: bool,
    lenient: bool,
) -> Result<()> {
    let PackedLayout { width, height, bits, order, row_stride } = layout;
    if bits == 0 || bits > 16 {
        return Err(Error::corrupt(format!("unsupported packed bit depth {bits}")));
    }
    let too_small = || Error::InvalidArgument("destination too small for packed unpack".into());
    if dst_stride < width {
        return Err(too_small());
    }
    // Cells up to the end of the last row (0 when nothing is to be written). A one-row
    // destination is only checked once there is something to write (below).
    let needed = match height {
        0 => 0,
        1 if dst.is_empty() => 0,
        _ => dst_stride.checked_mul(height - 1).and_then(|v| v.checked_add(width)).ok_or_else(too_small)?,
    };
    if height > 1 && dst.len() < needed {
        return Err(too_small());
    }

    let row_bytes = layout.row_bytes();
    // Rows y < height with y * row_stride + row_bytes <= src.len() (all of them when every
    // row reads the same bytes, row_stride == 0).
    let complete_rows = match src.len().checked_sub(row_bytes) {
        None => 0,
        Some(spare) => spare.checked_div(row_stride).map_or(height, |extra| height.min(extra + 1)),
    };
    if complete_rows < height && !lenient {
        return Err(Error::corrupt(format!("truncated packed data: {complete_rows}/{height} rows present")));
    }
    // The cut-short row after the complete ones gets written if any of its bytes exist.
    let partial = complete_rows < height && complete_rows * row_stride < src.len();
    let rows = complete_rows + partial as usize;
    if width == 0 || needed == 0 || rows == 0 {
        return Ok(());
    }
    if dst.len() < needed {
        // A one-row destination shorter than the row, with a row to write.
        return Err(too_small());
    }
    let cells = dst_stride * (rows - 1) + width;

    let work = |first: usize, chunk: &mut [u16]| {
        for (i, row) in chunk.chunks_mut(dst_stride).enumerate() {
            let y = first + i;
            let row = &mut row[..width];
            let start = y * row_stride;
            if y < complete_rows {
                unpack_row(&src[start..start + row_bytes], row, bits, order, simd);
            } else {
                unpack_row_partial(&src[start..], row, bits, order, simd);
            }
        }
    };
    // Memory-bound: a few workers saturate the memory bus. 16-bit copies on macOS are
    // fastest on one thread.
    let workers = if cfg!(target_os = "macos") && bits == 16 { 1 } else { par::memory_bound_workers() };
    par::for_each_row_chunk_bounded(&mut dst[..cells], dst_stride, workers, parallel, work);
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Vector kernels for 12-bit byte-stream rows: 8 samples from 12 bytes per 128-bit step.
//
// Each 16-bit lane receives the two bytes that hold its sample (as a little-endian
// lane value), then even and odd lanes are completed with a shift or a mask:
//   MsbBytes: lane 2i = B0<<8|B1 shifted right 4,  lane 2i+1 = B1<<8|B2 masked to 12 bits
//   LsbBytes: lane 2i = B1<<8|B0 masked to 12 bits, lane 2i+1 = B2<<8|B1 shifted right 4
// Msb16 is MsbBytes on stream byte j = stored byte j ^ 1; a 12-byte group starts on a word
// boundary, so the swapped bytes stay inside it and only the shuffle changes.

mod vector {
    use super::{LSB_BYTES, MSB_16, MSB_BYTES};

    /// Source byte of each output byte (lane value = low byte, high byte).
    #[allow(dead_code)]
    const SHUFFLE_MSB: [u8; 16] = [1, 0, 2, 1, 4, 3, 5, 4, 7, 6, 8, 7, 10, 9, 11, 10];
    #[allow(dead_code)]
    const SHUFFLE_LSB: [u8; 16] = [0, 1, 1, 2, 3, 4, 4, 5, 6, 7, 7, 8, 9, 10, 10, 11];
    #[allow(dead_code)]
    const SHUFFLE_MSB16: [u8; 16] = {
        let mut t = SHUFFLE_MSB;
        let mut i = 0;
        while i < 16 {
            t[i] ^= 1;
            i += 1;
        }
        t
    };

    /// The shuffle of an order and whether its even lanes are the masked ones (LsbBytes);
    /// `None` for the orders without a kernel.
    #[allow(dead_code)]
    fn layout(ord: u8) -> Option<(&'static [u8; 16], bool)> {
        match ord {
            MSB_BYTES => Some((&SHUFFLE_MSB, false)),
            LSB_BYTES => Some((&SHUFFLE_LSB, true)),
            MSB_16 => Some((&SHUFFLE_MSB16, false)),
            _ => None,
        }
    }

    /// Number of 8-sample steps whose 16-byte load stays inside `src`.
    #[allow(dead_code)]
    #[inline(always)]
    fn steps(src: &[u8], dst: &[u16]) -> usize {
        let by_src = if src.len() >= 16 { (src.len() - 16) / 12 + 1 } else { 0 };
        by_src.min(dst.len() / 8)
    }

    /// Decode a prefix of a 12-bit row; returns the samples written (a multiple of 8).
    #[cfg(target_arch = "x86_64")]
    pub(super) fn row12(src: &[u8], dst: &mut [u16], ord: u8) -> usize {
        let Some((order, lsb)) = layout(ord) else { return 0 };
        if !(std::arch::is_x86_feature_detected!("ssse3") && std::arch::is_x86_feature_detected!("sse4.1")) {
            return 0;
        }
        let n = steps(src, dst);
        // Two steps per 256-bit operation where available, the odd step with 128 bits.
        let wide = if std::arch::is_x86_feature_detected!("avx2") { n / 2 } else { 0 };
        // SAFETY: the CPU features were detected above; `n` steps stay inside both slices.
        unsafe {
            match (lsb, wide > 0) {
                (true, true) => x86_wide::<true>(src, dst, wide, order),
                (false, true) => x86_wide::<false>(src, dst, wide, order),
                _ => {}
            }
            let (s, d) = (&src[wide * 24..], &mut dst[wide * 16..]);
            if lsb { x86::<true>(s, d, n - wide * 2, order) } else { x86::<false>(s, d, n - wide * 2, order) }
        }
        n * 8
    }

    /// `pairs` double steps: 16 samples from 24 bytes, each 128-bit half as in [`x86`].
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn x86_wide<const LSB: bool>(src: &[u8], dst: &mut [u16], pairs: usize, order: &[u8; 16]) {
        use std::arch::x86_64::*;
        assert!(pairs == 0 || ((pairs - 1) * 24 + 12 + 16 <= src.len() && pairs * 16 <= dst.len()));
        // SAFETY: double step i loads 16 bytes at 24 * i and at 24 * i + 12, both ending
        // inside `src`, and stores 16 samples at 16 * i, inside `dst` (checked above).
        unsafe {
            let shuffle = _mm256_broadcastsi128_si256(_mm_loadu_si128(order.as_ptr().cast()));
            let mask = _mm256_set1_epi16(0x0FFF);
            for i in 0..pairs {
                let p = src.as_ptr().add(i * 24);
                let lo = _mm_loadu_si128(p.cast());
                let hi = _mm_loadu_si128(p.add(12).cast());
                let v = _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(lo), hi);
                let v = _mm256_shuffle_epi8(v, shuffle);
                let shifted = _mm256_srli_epi16::<4>(v);
                let masked = _mm256_and_si256(v, mask);
                let r = if LSB {
                    _mm256_blend_epi16::<0b1010_1010>(masked, shifted)
                } else {
                    _mm256_blend_epi16::<0b1010_1010>(shifted, masked)
                };
                _mm256_storeu_si256(dst.as_mut_ptr().add(i * 16).cast(), r);
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "ssse3,sse4.1")]
    unsafe fn x86<const LSB: bool>(src: &[u8], dst: &mut [u16], n: usize, order: &[u8; 16]) {
        use std::arch::x86_64::*;
        assert!(n == 0 || ((n - 1) * 12 + 16 <= src.len() && n * 8 <= dst.len()));
        // SAFETY: every load reads 16 bytes at 12 * i + 16 <= src.len(); every store
        // writes 8 samples at 8 * i + 8 <= dst.len() (checked above).
        unsafe {
            let shuffle = _mm_loadu_si128(order.as_ptr().cast());
            let mask = _mm_set1_epi16(0x0FFF);
            for i in 0..n {
                let v = _mm_shuffle_epi8(_mm_loadu_si128(src.as_ptr().add(i * 12).cast()), shuffle);
                let shifted = _mm_srli_epi16::<4>(v);
                let masked = _mm_and_si128(v, mask);
                let r = if LSB {
                    _mm_blend_epi16::<0b1010_1010>(masked, shifted)
                } else {
                    _mm_blend_epi16::<0b1010_1010>(shifted, masked)
                };
                _mm_storeu_si128(dst.as_mut_ptr().add(i * 8).cast(), r);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    pub(super) fn row12(src: &[u8], dst: &mut [u16], ord: u8) -> usize {
        use std::arch::aarch64::*;
        let Some((order, lsb)) = layout(ord) else { return 0 };
        let n = steps(src, dst);
        // Lanes taking the shifted value: even lanes (MsbBytes) or odd lanes (LsbBytes).
        let pick: [u16; 8] = if lsb { [0, !0, 0, !0, 0, !0, 0, !0] } else { [!0, 0, !0, 0, !0, 0, !0, 0] };
        // SAFETY: NEON is part of the aarch64 baseline; every load reads 16 bytes at
        // 12 * i + 16 <= src.len() and every store writes 8 samples at 8 * i + 8 <= dst.len().
        unsafe {
            let shuffle = vld1q_u8(order.as_ptr());
            let mask = vdupq_n_u16(0x0FFF);
            let pick = vld1q_u16(pick.as_ptr());
            for i in 0..n {
                let v = vreinterpretq_u16_u8(vqtbl1q_u8(vld1q_u8(src.as_ptr().add(i * 12)), shuffle));
                let r = vbslq_u16(pick, vshrq_n_u16::<4>(v), vandq_u16(v, mask));
                vst1q_u16(dst.as_mut_ptr().add(i * 8), r);
            }
        }
        n * 8
    }

    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    pub(super) fn row12(src: &[u8], dst: &mut [u16], ord: u8) -> usize {
        use std::arch::wasm32::*;
        let Some((order, lsb)) = layout(ord) else { return 0 };
        let n = steps(src, dst);
        // SAFETY: simd128 is enabled at compile time; every load reads 16 bytes at
        // 12 * i + 16 <= src.len() and every store writes 8 samples at 8 * i + 8 <= dst.len().
        unsafe {
            let shuffle = v128_load(order.as_ptr().cast());
            let mask = u16x8_splat(0x0FFF);
            // Lanes taking the shifted value (the low lane of each u32 is the even lane).
            let pick = if lsb { u32x4_splat(0xFFFF_0000) } else { u32x4_splat(0x0000_FFFF) };
            for i in 0..n {
                let v = i8x16_swizzle(v128_load(src.as_ptr().add(i * 12).cast()), shuffle);
                let r = v128_bitselect(u16x8_shr(v, 4), v128_and(v, mask), pick);
                v128_store(dst.as_mut_ptr().add(i * 8).cast(), r);
            }
        }
        n * 8
    }

    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        all(target_arch = "wasm32", target_feature = "simd128")
    )))]
    pub(super) fn row12(_src: &[u8], _dst: &mut [u16], _ord: u8) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDERS: [PackOrder; 4] = [PackOrder::MsbBytes, PackOrder::LsbBytes, PackOrder::Msb16, PackOrder::Msb32];

    fn bytes(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed | 1;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s as u8
            })
            .collect()
    }

    #[test]
    fn closed_forms() {
        let b = [0xAB, 0xCD, 0xEF, 0x12];
        let mut out = [0u16; 2];
        unpack_row_scalar(&b[..3], &mut out, 12, PackOrder::MsbBytes);
        assert_eq!(out, [0xABC, 0xDEF]);
        unpack_row_scalar(&b[..3], &mut out, 12, PackOrder::LsbBytes);
        assert_eq!(out, [0xDAB, 0xEFC]);
        unpack_row_scalar(&b, &mut out, 16, PackOrder::Msb32);
        assert_eq!(out, [0x12EF, 0xCDAB]);
        unpack_row_scalar(&b, &mut out, 16, PackOrder::MsbBytes);
        assert_eq!(out, [0xABCD, 0xEF12]);
        // Odd-length Msb16: the missing high byte of the last word comes first and is zero.
        let mut one = [0u16; 2];
        unpack_row_scalar(&[0x34], &mut one, 8, PackOrder::Msb16);
        assert_eq!(one, [0, 0x34]);
    }

    #[test]
    fn rows_equal_reference() {
        for bits in 0..=17u32 {
            for order in ORDERS {
                for width in (0..70).chain([200, 1001]) {
                    let need = (width * bits as usize).div_ceil(8);
                    for len in [0, 1, 5, need.saturating_sub(2), need, need + 1, need + 9, need + 40] {
                        let src = bytes(len, ((bits as u64) << 32) | width as u64);
                        let mut want = vec![1u16; width];
                        unpack_row_scalar(&src, &mut want, bits, order);
                        for simd in [false, true] {
                            let mut got = vec![2u16; width];
                            unpack_row(&src, &mut got, bits, order, simd);
                            assert_eq!(got, want, "{bits} {order:?} w {width} len {len} simd {simd}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn partial_rows() {
        let src = bytes(37, 7);
        for bits in 1..=16u32 {
            for order in ORDERS {
                let mut full = vec![0u16; 64];
                unpack_row(&src, &mut full, bits, order, true);
                let mut part = vec![0xFFFFu16; 64];
                let n = unpack_row_partial(&src, &mut part, bits, order, true);
                assert_eq!(n, complete_samples(37, bits, order).min(64));
                assert_eq!(&part[..n], &full[..n]);
                assert!(part[n..].iter().all(|&v| v == 0));
            }
        }
        assert_eq!(complete_samples(7, 12, PackOrder::Msb32), 2);
        assert_eq!(complete_samples(7, 10, PackOrder::Msb16), 4);
        assert_eq!(complete_samples(7, 0, PackOrder::MsbBytes), 0);
    }

    #[test]
    fn raster_errors() {
        let l = PackedLayout::tight(4, 2, 12, PackOrder::MsbBytes);
        let mut dst = [0u16; 8];
        let msg = |r: Result<()>| r.unwrap_err().to_string();
        assert!(
            msg(unpack(&[0; 12], PackedLayout { bits: 17, ..l }, &mut dst, 4, false, true, true))
                .contains("bit depth 17")
        );
        assert!(msg(unpack(&[0; 12], l, &mut dst[..7], 4, false, true, true)).contains("too small"));
        assert!(msg(unpack(&[0; 12], l, &mut dst, 3, false, true, true)).contains("too small"));
        assert!(msg(unpack(&[0; 11], l, &mut dst, 4, false, true, false)).contains("1/2 rows"));
        let one = PackedLayout::tight(4, 1, 12, PackOrder::MsbBytes);
        assert!(matches!(unpack(&[0; 6], one, &mut dst[..3], 4, false, true, true), Err(Error::InvalidArgument(_))));
        assert!(unpack(&[0; 6], one, &mut [], 4, false, true, true).is_ok());
    }

    #[test]
    fn raster_recovery_and_padding() {
        // 3 rows of 5 12-bit samples, rows 10 bytes apart (8 data + 2 padding), cut in row 1.
        let l = PackedLayout { width: 5, height: 3, bits: 12, order: PackOrder::LsbBytes, row_stride: 10 };
        let src = bytes(10 + 4, 3);
        let mut dst = vec![0xBEEFu16; 6 * 3 + 2];
        unpack(&src, l, &mut dst, 6, true, true, true).unwrap();
        let mut row0 = [0u16; 5];
        unpack_row(&src[..8], &mut row0, 12, PackOrder::LsbBytes, false);
        assert_eq!(&dst[..5], &row0);
        assert_eq!(dst[5], 0xBEEF);
        let mut row1 = [0u16; 5];
        unpack_row_partial(&src[10..], &mut row1, 12, PackOrder::LsbBytes, false);
        assert_eq!(&dst[6..11], &row1);
        assert_eq!(&row1[2..], &[0, 0, 0]);
        assert!(dst[11..].iter().all(|&v| v == 0xBEEF));
    }

    #[test]
    fn raster_matches_rows_in_parallel() {
        let (w, h) = (3001, 200);
        for (bits, order) in
            [(12, PackOrder::MsbBytes), (14, PackOrder::LsbBytes), (12, PackOrder::Msb32), (16, PackOrder::MsbBytes)]
        {
            let tight = PackedLayout::tight(w, h, bits, order);
            let l = PackedLayout { row_stride: tight.row_bytes() + 3, ..tight };
            let src = bytes(l.row_stride * h, bits as u64);
            let mut a = vec![0u16; w * h];
            let mut b = vec![0u16; w * h];
            unpack(&src, l, &mut a, w, true, true, false).unwrap();
            unpack(&src, l, &mut b, w, false, false, false).unwrap();
            assert!(a == b);
            let mut row = vec![0u16; w];
            let start = l.row_stride * 77;
            unpack_row_scalar(&src[start..start + tight.row_bytes()], &mut row, bits, order);
            assert_eq!(&a[77 * w..78 * w], &row[..]);
        }
    }

    /// FNV-1a over 16-bit values and markers.
    struct Digest(u64);
    impl Digest {
        fn eat(&mut self, v: u64) {
            for b in v.to_le_bytes() {
                self.0 = (self.0 ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3);
            }
        }
        fn eat_all(&mut self, vs: &[u16]) {
            vs.iter().for_each(|&v| self.eat(v as u64));
        }
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            if n == 0 { 0 } else { (self.next() % n as u64) as usize }
        }
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
    }

    /// Digests of the row functions over every depth 0..=17, every order, many widths and
    /// source lengths around the exact row length, with and without the vector kernels.
    /// The values were recorded while this implementation and the previous one were
    /// compared case by case on exactly these inputs (and agreed on every one), so they
    /// pin the behaviour of both.
    #[test]
    fn row_digests_are_pinned() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut got = Vec::new();
        for order in ORDERS {
            let mut d = Digest(0xCBF2_9CE4_8422_2325);
            for bits in 0..=17u32 {
                for width in (0..=80usize).chain([127, 128, 129, 333, 1000, 6001]) {
                    let need = (width * bits as usize).div_ceil(8);
                    for len in [
                        0,
                        1,
                        2,
                        3,
                        7,
                        need.saturating_sub(3),
                        need.saturating_sub(1),
                        need,
                        need + 1,
                        need + 4,
                        need + 17,
                    ] {
                        let src = rng.bytes(len);
                        let mut want = vec![0xA5A5u16; width];
                        unpack_row_scalar(&src, &mut want, bits, order);
                        d.eat_all(&want);
                        for simd in [false, true] {
                            let mut row = vec![0x5A5Au16; width];
                            unpack_row(&src, &mut row, bits, order, simd);
                            assert_eq!(row, want, "bits {bits} {order:?} w {width} len {len} simd {simd}");
                            let mut part = vec![0xA5A5u16; width];
                            let n = unpack_row_partial(&src, &mut part, bits, order, simd);
                            d.eat(n as u64);
                            d.eat_all(&part);
                        }
                    }
                }
                for len in 0..200 {
                    d.eat(complete_samples(len, bits, order) as u64);
                }
            }
            got.push(d.0);
        }
        assert_eq!(got, ROW_DIGESTS);
    }
    const ROW_DIGESTS: [u64; 4] =
        [0xb64e_0fc7_cf37_876f, 0x9175_1a47_83d8_f4b2, 0xc442_a22f_d9c8_8dab, 0x4a78_25e0_aa06_f49f];

    /// Digest of `unpack` over random geometries (row strides 0, shorter than, equal to and
    /// longer than a row; full, longer, short and empty sources; destination strides and
    /// lengths that fit or not; every flag combination), including the result kind and
    /// message and every destination cell (pre-filled, so untouched cells count). Recorded
    /// while the previous implementation agreed on every case, except one-row rasters with
    /// `0 < dst.len() < width` and something to write, where it panicked and this one
    /// returns `InvalidArgument` (an approved change).
    #[test]
    fn raster_digest_is_pinned() {
        let mut rng = Rng(0xD1B5_4A32_D192_ED03);
        let mut d = Digest(0xCBF2_9CE4_8422_2325);
        for case in 0..60_000 {
            let big = case % 1000 == 999;
            let bits = [0u32, 1, 2, 3, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 16, 12, 12, 14, 17][rng.below(20)];
            let order = ORDERS[rng.below(4)];
            let (w, h) = if big { (1000 + rng.below(6000), 1 + rng.below(40)) } else { (rng.below(70), rng.below(10)) };
            let rb = (w * bits as usize).div_ceil(8);
            let stride = match rng.below(6) {
                0 => 0,
                1 => rb.saturating_sub(1 + rng.below(3)),
                2 | 3 => rb,
                _ => rb + rng.below(40),
            };
            let full = stride * h.saturating_sub(1) + rb;
            let len = match rng.below(5) {
                0 => full,
                1 => full + rng.below(64),
                2 => rng.below(full + 1),
                3 => full.saturating_sub(rng.below(16)),
                _ => 0,
            };
            let src = rng.bytes(len);
            let dst_stride = match rng.below(8) {
                0 => w.saturating_sub(1),
                1 => w + rng.below(8),
                _ => w,
            };
            let need = dst_stride * h.saturating_sub(1) + w;
            let dst_len = match rng.below(8) {
                0 => need.saturating_sub(1 + rng.below(4)),
                1 => need + rng.below(dst_stride + 3),
                2 => dst_stride * h,
                _ => need,
            };
            let (parallel, simd, lenient) = (rng.below(2) == 0, rng.below(2) == 0, rng.below(3) != 0);
            let layout = PackedLayout { width: w, height: h, bits, order, row_stride: stride };
            let mut dst = vec![0xBEEFu16; dst_len];
            let r = unpack(&src, layout, &mut dst, dst_stride, parallel, simd, lenient);
            let text = match &r {
                Ok(()) => "ok".to_string(),
                Err(e) => format!("{}: {e}", e.kind()),
            };
            text.bytes().for_each(|b| d.eat(b as u64));
            d.eat_all(&dst);
        }
        assert_eq!(d.0, RASTER_DIGEST);
    }
    const RASTER_DIGEST: u64 = 0xab07_80aa_2d13_4742;
}
