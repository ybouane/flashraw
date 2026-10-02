//! Unpacking of uncompressed, bit-packed sensor data.
//!
//! Every kernel has a scalar reference implementation (`*_scalar`) that is the
//! specification. Fast paths (word-at-a-time scalar, SSE4.1, NEON, WASM SIMD128)
//! are tested bit-for-bit against it (see tests and `tests/simd_equivalence.rs`).

use flashraw_core::bits::{BitPumpLsb, BitPumpMsb, BitPumpMsb16, BitPumpMsb32, BitRead};
use flashraw_core::{Error, PackOrder, Result, par};

/// Description of a packed raster.
#[derive(Debug, Clone, Copy)]
pub struct PackedLayout {
    pub width: usize,
    pub height: usize,
    /// Bits per sample (1..=16).
    pub bits: u32,
    pub order: PackOrder,
    /// Bytes from one row start to the next (>= ceil(width * bits / 8)).
    pub row_stride: usize,
}

impl PackedLayout {
    /// Layout with rows tightly packed (no row padding).
    pub fn tight(width: usize, height: usize, bits: u32, order: PackOrder) -> Self {
        PackedLayout { width, height, bits, order, row_stride: (width * bits as usize).div_ceil(8) }
    }
    pub fn row_bytes(&self) -> usize {
        (self.width * self.bits as usize).div_ceil(8)
    }
}

/// Unpack `src` into `dst` rows of `dst_stride` samples. Rows beyond the available
/// source data are left untouched and reported as an error unless `lenient`.
pub fn unpack(
    src: &[u8],
    layout: PackedLayout,
    dst: &mut [u16],
    dst_stride: usize,
    parallel: bool,
    simd: bool,
    lenient: bool,
) -> Result<()> {
    let PackedLayout { width, height, bits, row_stride, .. } = layout;
    if bits == 0 || bits > 16 {
        return Err(Error::corrupt(format!("unsupported packed bit depth {bits}")));
    }
    if dst_stride < width || dst.len() < dst_stride * height.saturating_sub(1) + width.min(dst.len()) {
        return Err(Error::InvalidArgument("destination too small for packed unpack".into()));
    }
    let row_bytes = layout.row_bytes();
    let avail_rows =
        src.len().saturating_sub(row_bytes).checked_div(row_stride).map_or(height, |n| (n + 1).min(height));
    let avail_rows = if src.len() < row_bytes { 0 } else { avail_rows };
    if avail_rows < height && !lenient {
        return Err(Error::corrupt(format!("packed data truncated: {avail_rows}/{height} rows present")));
    }
    let rows_per_chunk = (64 * 1024 / dst_stride.max(1)).max(1);
    let n = (dst_stride * height).min(dst.len());
    let dst = &mut dst[..n];
    let body = |first: usize, chunk: &mut [u16]| {
        for (i, row) in chunk.chunks_mut(dst_stride).enumerate() {
            let y = first + i;
            if y >= avail_rows {
                break;
            }
            let s = &src[y * row_stride..];
            let s = &s[..row_bytes.min(s.len())];
            unpack_row(s, &mut row[..width], bits, layout.order, simd);
        }
    };
    // 8/16-bit samples are plain copies: memory-bound, so fewer workers are faster.
    if matches!(bits, 8 | 16) {
        par::for_each_row_chunk_bounded(dst, dst_stride, par::MEMORY_BOUND_WORKERS, parallel, body);
    } else {
        par::for_each_row_chunk(dst, dst_stride, rows_per_chunk, parallel, body);
    }
    Ok(())
}

/// Unpack one row of `dst.len()` samples.
pub fn unpack_row(src: &[u8], dst: &mut [u16], bits: u32, order: PackOrder, simd: bool) {
    match (bits, order) {
        (16, PackOrder::LsbBytes) => unpack_u16le(src, dst),
        (16, PackOrder::MsbBytes) => unpack_u16be(src, dst),
        (8, PackOrder::MsbBytes | PackOrder::LsbBytes) => unpack_u8(src, dst),
        (12, PackOrder::MsbBytes) => unpack12_msb(src, dst, simd),
        (12, PackOrder::LsbBytes) => unpack12_lsb(src, dst, simd),
        (14, PackOrder::MsbBytes) => unpack14_msb(src, dst),
        (14, PackOrder::LsbBytes) => unpack14_lsb(src, dst),
        (10, PackOrder::MsbBytes) => unpack10_msb(src, dst),
        (10, PackOrder::LsbBytes) => unpack10_lsb(src, dst),
        _ => unpack_row_scalar(src, dst, bits, order),
    }
}

/// Reference implementation for every packing (bit pump based).
pub fn unpack_row_scalar(src: &[u8], dst: &mut [u16], bits: u32, order: PackOrder) {
    match order {
        PackOrder::MsbBytes => {
            let mut p = BitPumpMsb::new(src);
            dst.iter_mut().for_each(|d| *d = p.get(bits) as u16);
        }
        PackOrder::LsbBytes => {
            let mut p = BitPumpLsb::new(src);
            dst.iter_mut().for_each(|d| *d = p.get(bits) as u16);
        }
        PackOrder::Msb16 => {
            let mut p = BitPumpMsb16::new(src);
            dst.iter_mut().for_each(|d| *d = p.get(bits) as u16);
        }
        PackOrder::Msb32 => {
            let mut p = BitPumpMsb32::new(src);
            dst.iter_mut().for_each(|d| *d = p.get(bits) as u16);
        }
    }
}

fn unpack_u16le(src: &[u8], dst: &mut [u16]) {
    let n = dst.len().min(src.len() / 2);
    for (d, s) in dst[..n].iter_mut().zip(src.chunks_exact(2)) {
        *d = u16::from_le_bytes([s[0], s[1]]);
    }
    tail(src, dst, n, 16, PackOrder::LsbBytes);
}

fn unpack_u16be(src: &[u8], dst: &mut [u16]) {
    let n = dst.len().min(src.len() / 2);
    for (d, s) in dst[..n].iter_mut().zip(src.chunks_exact(2)) {
        *d = u16::from_be_bytes([s[0], s[1]]);
    }
    tail(src, dst, n, 16, PackOrder::MsbBytes);
}

fn unpack_u8(src: &[u8], dst: &mut [u16]) {
    let n = dst.len().min(src.len());
    for (d, &s) in dst[..n].iter_mut().zip(src) {
        *d = s as u16;
    }
    dst[n..].iter_mut().for_each(|d| *d = 0);
}

/// Fill any samples not covered by complete groups with the scalar reference.
#[inline]
fn tail(src: &[u8], dst: &mut [u16], done: usize, bits: u32, order: PackOrder) {
    if done < dst.len() {
        let byte = done * bits as usize / 8; // groups are byte aligned
        unpack_row_scalar(src.get(byte..).unwrap_or(&[]), &mut dst[done..], bits, order);
    }
}

fn unpack12_msb(src: &[u8], dst: &mut [u16], simd: bool) {
    let mut done = 0;
    if simd {
        done = simd12::unpack(src, dst, true);
    }
    let groups = ((dst.len() - done) / 2).min(src.len().saturating_sub(done / 2 * 3) / 3);
    for g in 0..groups {
        let s = &src[(done / 2 + g) * 3..];
        let (b0, b1, b2) = (s[0] as u16, s[1] as u16, s[2] as u16);
        dst[done + 2 * g] = (b0 << 4) | (b1 >> 4);
        dst[done + 2 * g + 1] = ((b1 & 0xF) << 8) | b2;
    }
    tail(src, dst, done + groups * 2, 12, PackOrder::MsbBytes);
}

fn unpack12_lsb(src: &[u8], dst: &mut [u16], simd: bool) {
    let mut done = 0;
    if simd {
        done = simd12::unpack(src, dst, false);
    }
    let groups = ((dst.len() - done) / 2).min(src.len().saturating_sub(done / 2 * 3) / 3);
    for g in 0..groups {
        let s = &src[(done / 2 + g) * 3..];
        let (b0, b1, b2) = (s[0] as u16, s[1] as u16, s[2] as u16);
        dst[done + 2 * g] = b0 | ((b1 & 0xF) << 8);
        dst[done + 2 * g + 1] = (b1 >> 4) | (b2 << 4);
    }
    tail(src, dst, done + groups * 2, 12, PackOrder::LsbBytes);
}

macro_rules! word_unpack {
    ($name:ident, $bits:expr, $bytes:expr, $n:expr, $be:expr, $order:expr) => {
        fn $name(src: &[u8], dst: &mut [u16]) {
            // $n samples per $bytes bytes; read 8 bytes at a time (requires 8 readable bytes).
            let groups = (dst.len() / $n).min(src.len().saturating_sub(8 - $bytes) / $bytes);
            for g in 0..groups {
                let s = &src[g * $bytes..g * $bytes + 8];
                let w = if $be {
                    u64::from_be_bytes(s.try_into().unwrap())
                } else {
                    u64::from_le_bytes(s.try_into().unwrap())
                };
                let m = (1u64 << $bits) - 1;
                for k in 0..$n {
                    dst[g * $n + k] =
                        if $be { (w >> (64 - $bits * (k + 1))) & m } else { (w >> ($bits * k)) & m } as u16;
                }
            }
            tail(src, dst, groups * $n, $bits, $order);
        }
    };
}

word_unpack!(unpack14_msb, 14, 7, 4, true, PackOrder::MsbBytes);
word_unpack!(unpack14_lsb, 14, 7, 4, false, PackOrder::LsbBytes);
word_unpack!(unpack10_msb, 10, 5, 4, true, PackOrder::MsbBytes);
word_unpack!(unpack10_lsb, 10, 5, 4, false, PackOrder::LsbBytes);

/// 12-bit SIMD kernels: 12 source bytes → 8 samples per 128-bit vector.
#[allow(dead_code)] // tables/helpers unused on targets without a SIMD kernel
mod simd12 {
    /// Shuffle placing (hi, lo) byte pairs for MSB packing into little-endian u16 lanes.
    pub(super) const SHUF_MSB: [u8; 16] = [1, 0, 2, 1, 4, 3, 5, 4, 7, 6, 8, 7, 10, 9, 11, 10];
    /// Shuffle for LSB packing: lane = bytes (lo, hi) in natural order.
    pub(super) const SHUF_LSB: [u8; 16] = [0, 1, 1, 2, 3, 4, 4, 5, 6, 7, 7, 8, 9, 10, 10, 11];

    /// Returns the number of samples written (a multiple of 8).
    #[allow(unreachable_code)]
    pub fn unpack(src: &[u8], dst: &mut [u16], msb: bool) -> usize {
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("sse4.1") && std::arch::is_x86_feature_detected!("ssse3") {
                // SAFETY: the required CPU features were detected at runtime.
                return unsafe { x86::unpack(src, dst, msb) };
            }
            return 0;
        }
        #[cfg(target_arch = "aarch64")]
        {
            // SAFETY: NEON is mandatory on aarch64.
            return unsafe { neon::unpack(src, dst, msb) };
        }
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            return wasm::unpack(src, dst, msb);
        }
        let _ = (src, dst, msb);
        0
    }

    /// Number of 8-sample groups that can be processed reading 16 bytes per 12 consumed.
    #[inline]
    pub(super) fn groups(src: &[u8], dst: &[u16]) -> usize {
        (dst.len() / 8).min(src.len().saturating_sub(4) / 12)
    }

    #[cfg(target_arch = "x86_64")]
    mod x86 {
        use std::arch::x86_64::*;

        /// # Safety
        /// Caller must ensure SSSE3 and SSE4.1 are available.
        #[target_feature(enable = "ssse3,sse4.1")]
        pub unsafe fn unpack(src: &[u8], dst: &mut [u16], msb: bool) -> usize {
            let n = super::groups(src, dst);
            let shuf = if msb { super::SHUF_MSB } else { super::SHUF_LSB };
            // SAFETY (whole block): `groups` guarantees for each g < n that bytes
            // [12g, 12g+16) are in `src` and samples [8g, 8g+8) are in `dst`;
            // unaligned load/store intrinsics are used.
            unsafe {
                let shuf = _mm_loadu_si128(shuf.as_ptr() as *const __m128i);
                let mask = _mm_set1_epi16(0x0FFF);
                for g in 0..n {
                    let v = _mm_loadu_si128(src.as_ptr().add(g * 12) as *const __m128i);
                    let v = _mm_shuffle_epi8(v, shuf);
                    let shifted = _mm_srli_epi16(v, 4);
                    let masked = _mm_and_si128(v, mask);
                    // MSB: even lanes shifted, odd lanes masked. LSB: the reverse.
                    let out = if msb {
                        _mm_blend_epi16(masked, shifted, 0b0101_0101)
                    } else {
                        _mm_blend_epi16(masked, shifted, 0b1010_1010)
                    };
                    _mm_storeu_si128(dst.as_mut_ptr().add(g * 8) as *mut __m128i, out);
                }
            }
            n * 8
        }
    }

    #[cfg(target_arch = "aarch64")]
    mod neon {
        use std::arch::aarch64::*;

        /// # Safety
        /// NEON must be available (always true on aarch64).
        #[target_feature(enable = "neon")]
        pub unsafe fn unpack(src: &[u8], dst: &mut [u16], msb: bool) -> usize {
            let n = super::groups(src, dst);
            let shuf = if msb { super::SHUF_MSB } else { super::SHUF_LSB };
            let even_sel: [u16; 8] = if msb {
                [0xFFFF, 0, 0xFFFF, 0, 0xFFFF, 0, 0xFFFF, 0]
            } else {
                [0, 0xFFFF, 0, 0xFFFF, 0, 0xFFFF, 0, 0xFFFF]
            };
            // SAFETY: bounds guaranteed by `groups` as in the x86 kernel.
            unsafe {
                let tbl = vld1q_u8(shuf.as_ptr());
                let sel = vld1q_u16(even_sel.as_ptr());
                let mask = vdupq_n_u16(0x0FFF);
                for g in 0..n {
                    let v = vld1q_u8(src.as_ptr().add(g * 12));
                    let v = vreinterpretq_u16_u8(vqtbl1q_u8(v, tbl));
                    let shifted = vshrq_n_u16(v, 4);
                    let masked = vandq_u16(v, mask);
                    let out = vbslq_u16(sel, shifted, masked);
                    vst1q_u16(dst.as_mut_ptr().add(g * 8), out);
                }
            }
            n * 8
        }
    }

    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    mod wasm {
        use core::arch::wasm32::*;

        pub fn unpack(src: &[u8], dst: &mut [u16], msb: bool) -> usize {
            let n = super::groups(src, dst);
            let shuf = if msb { super::SHUF_MSB } else { super::SHUF_LSB };
            let sel = if msb {
                u16x8(0xFFFF, 0, 0xFFFF, 0, 0xFFFF, 0, 0xFFFF, 0)
            } else {
                u16x8(0, 0xFFFF, 0, 0xFFFF, 0, 0xFFFF, 0, 0xFFFF)
            };
            // SAFETY: bounds guaranteed by `groups`; v128 loads/stores are unaligned.
            unsafe {
                let tbl = v128_load(shuf.as_ptr() as *const v128);
                let mask = u16x8_splat(0x0FFF);
                for g in 0..n {
                    let v = v128_load(src.as_ptr().add(g * 12) as *const v128);
                    let v = i8x16_swizzle(v, tbl);
                    let shifted = u16x8_shr(v, 4);
                    let masked = v128_and(v, mask);
                    v128_store(dst.as_mut_ptr().add(g * 8) as *mut v128, v128_bitselect(shifted, masked, sel));
                }
            }
            n * 8
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pseudo(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn fast_paths_match_reference() {
        for bits in 1..=16u32 {
            for order in [PackOrder::MsbBytes, PackOrder::LsbBytes, PackOrder::Msb16, PackOrder::Msb32] {
                for width in [1usize, 7, 8, 15, 16, 17, 63, 64, 333] {
                    let src = pseudo((width * bits as usize).div_ceil(8) + 3, bits as u64 * 1000 + width as u64);
                    let mut a = vec![0u16; width];
                    let mut b = vec![0u16; width];
                    let mut c = vec![0u16; width];
                    unpack_row_scalar(&src, &mut a, bits, order);
                    unpack_row(&src, &mut b, bits, order, true);
                    unpack_row(&src, &mut c, bits, order, false);
                    assert_eq!(a, b, "bits {bits} {order:?} width {width} simd");
                    assert_eq!(a, c, "bits {bits} {order:?} width {width} nosimd");
                }
            }
        }
    }

    #[test]
    fn short_sources_match_reference() {
        for bits in 1..=16u32 {
            for order in [PackOrder::MsbBytes, PackOrder::LsbBytes, PackOrder::Msb16, PackOrder::Msb32] {
                for len in 0..12usize {
                    let src = pseudo(len, bits as u64 + len as u64);
                    let mut a = vec![0xAAAAu16; 31];
                    let mut b = vec![0x5555u16; 31];
                    unpack_row_scalar(&src, &mut a, bits, order);
                    unpack_row(&src, &mut b, bits, order, true);
                    assert_eq!(a, b, "bits {bits} {order:?} len {len}");
                }
            }
        }
    }

    #[test]
    fn exact_length_sources() {
        // Source buffers with no slack after the last group (SIMD/word paths must not over-read).
        for bits in [10u32, 12, 14, 16] {
            for width in [8usize, 16, 24, 32, 40, 48] {
                let src = pseudo(width * bits as usize / 8, 9);
                for order in [PackOrder::MsbBytes, PackOrder::LsbBytes] {
                    let mut a = vec![0u16; width];
                    let mut b = vec![0u16; width];
                    unpack_row_scalar(&src, &mut a, bits, order);
                    unpack_row(&src, &mut b, bits, order, true);
                    assert_eq!(a, b);
                }
            }
        }
    }

    #[test]
    fn whole_image_with_stride_and_truncation() {
        let (w, h) = (100usize, 50usize);
        let layout = PackedLayout { width: w, height: h, bits: 12, order: PackOrder::MsbBytes, row_stride: 160 };
        let src = pseudo(160 * h, 5);
        let mut dst = vec![0u16; w * h];
        unpack(&src, layout, &mut dst, w, true, true, false).unwrap();
        for y in 0..h {
            let mut r = vec![0u16; w];
            unpack_row_scalar(&src[y * 160..], &mut r, 12, PackOrder::MsbBytes);
            assert_eq!(&dst[y * w..(y + 1) * w], &r[..]);
        }
        assert!(unpack(&src[..160 * 10], layout, &mut dst, w, true, true, false).is_err());
        assert!(unpack(&src[..160 * 10], layout, &mut dst, w, true, true, true).is_ok());
    }
}
