//! zlib payloads (DNG compression 8 and 32946) and the TIFF/DNG predictors applied after inflating them.
//!
//! * zlib streams (RFC 1950 framing around RFC 1951 DEFLATE data, Adler-32 checked), either into a new buffer
//!   bounded by a ceiling or straight into a caller buffer;
//! * horizontal integer differencing (TIFF predictor 2, DNG 34892 and 34893): a running sum along each of the
//!   interleaved sample lanes, modulo 2^16;
//! * the floating-point predictor (TIFF Technical Note 3, DNG 34894 and 34895): a running sum over the bytes of a
//!   row stored as byte planes (most significant plane first), followed by regrouping the planes into samples;
//! * conversion of 16-bit (IEEE binary16), 24-bit (DNG: sign, 7-bit exponent biased by 63, 16-bit fraction) and
//!   32-bit (IEEE binary32) samples to `f32`.
//!
//! Every function is pure and total: any byte contents and lengths are accepted without panicking.

use flashraw_core::{Error, Result};

// ------------------------------------------------------------------------------------------------- zlib

/// DEFLATE cannot expand its input by more than about 1032:1: the longest match (258 bytes) costs at least two bits.
const DEFLATE_MAX_RATIO: usize = 1032;
/// Added to the expansion bound so that tiny streams (stored blocks, headers) still get a useful reservation.
const RESERVE_EXTRA: usize = 1024;

/// Inflate the zlib stream `data` into a new buffer of at most `max_out` bytes.
///
/// The stream's header and Adler-32 trailer are checked. A stream that is corrupt, ends early, has a mismatching
/// checksum or would produce more than `max_out` bytes fails with [`Error::CorruptFile`]. Only the output a stream
/// of this length can possibly produce is reserved up front, whatever the ceiling.
pub fn inflate_zlib(data: &[u8], max_out: usize) -> Result<Vec<u8>> {
    let possible = data.len().saturating_mul(DEFLATE_MAX_RATIO).saturating_add(RESERVE_EXTRA);
    let options = zune_inflate::DeflateOptions::default().set_limit(max_out).set_size_hint(possible.min(max_out));
    zune_inflate::DeflateDecoder::new_with_options(data, options)
        .decode_zlib()
        .map_err(|e| Error::corrupt(format!("deflate: {e:?}")))
}

/// Inflate the zlib stream `data` into `out` and return the number of bytes produced.
///
/// The stream must end (final block and matching Adler-32) within `data` and its output must fit in `out`; bytes of
/// `out` past the produced length are left as they were, and input after the end of the stream is ignored. On
/// failure ([`Error::CorruptFile`]) the content of `out` is unspecified.
pub fn inflate_zlib_into(data: &[u8], out: &mut [u8]) -> Result<usize> {
    match zlib_rs::decompress_slice(out, data, zlib_rs::InflateConfig::default()) {
        (produced, zlib_rs::ReturnCode::Ok) => Ok(produced.len()),
        (_, code) => Err(Error::corrupt(format!("deflate: {code:?}"))),
    }
}

// ------------------------------------------------------------------------------------------------- running sums

/// An element type whose addition wraps around (bytes for the float predictor, 16-bit words for the integer one).
trait Wrapping: Copy {
    fn plus(self, other: Self) -> Self;

    /// Running sum with a stride of one element.
    fn prefix_sum(v: &mut [Self]) {
        lanes_sum::<Self, 1>(v);
    }
}

impl Wrapping for u8 {
    #[inline(always)]
    fn plus(self, other: Self) -> Self {
        self.wrapping_add(other)
    }

    /// A single running sum is one long serial chain. The slice is cut into four parts summed side by side (four
    /// independent chains), then each part is offset by the totals of the parts before it.
    fn prefix_sum(v: &mut [u8]) {
        let part = v.len() / 4;
        if part < 32 {
            lanes_sum::<u8, 1>(v);
            return;
        }
        let (p0, rest) = v.split_at_mut(part);
        let (p1, rest) = rest.split_at_mut(part);
        let (p2, p3) = rest.split_at_mut(part);
        let mut sums = [0u8; 4];
        for (((x0, x1), x2), x3) in p0.iter_mut().zip(p1.iter_mut()).zip(p2.iter_mut()).zip(p3.iter_mut()) {
            sums[0] = sums[0].wrapping_add(*x0);
            *x0 = sums[0];
            sums[1] = sums[1].wrapping_add(*x1);
            *x1 = sums[1];
            sums[2] = sums[2].wrapping_add(*x2);
            *x2 = sums[2];
            sums[3] = sums[3].wrapping_add(*x3);
            *x3 = sums[3];
        }
        // The last part also holds the remainder of the division by four.
        let mut sum = sums[3];
        for x in &mut p3[part..] {
            sum = sum.wrapping_add(*x);
            *x = sum;
        }
        let mut offset = 0u8;
        for (p, total) in [(p1, sums[0]), (p2, sums[1]), (p3, sums[2])] {
            offset = offset.wrapping_add(total);
            p.iter_mut().for_each(|x| *x = x.wrapping_add(offset));
        }
    }
}

impl Wrapping for u16 {
    #[inline(always)]
    fn plus(self, other: Self) -> Self {
        self.wrapping_add(other)
    }
}

/// Running sum with a stride of `S` elements: each element after the first `S` becomes itself plus the (already
/// summed) element `S` before it. The `S` partial sums are kept in registers rather than reloaded from memory.
#[inline(always)]
fn lanes_sum<T: Wrapping, const S: usize>(v: &mut [T]) {
    if v.len() <= S {
        return;
    }
    let (first, rest) = v.split_at_mut(S);
    let mut sums = [first[0]; S];
    sums.copy_from_slice(first);
    let mut groups = rest.chunks_exact_mut(S);
    for group in &mut groups {
        for (sum, x) in sums.iter_mut().zip(group.iter_mut()) {
            *sum = sum.plus(*x);
            *x = *sum;
        }
    }
    for (sum, x) in sums.iter().zip(groups.into_remainder()) {
        *x = sum.plus(*x);
    }
}

/// Running sum with stride `stride` over the whole slice, in increasing order. A stride of zero reads the
/// recurrence literally: every element is added to itself.
fn running_sum<T: Wrapping>(v: &mut [T], stride: usize) {
    match stride {
        0 => v.iter_mut().for_each(|x| *x = x.plus(*x)),
        1 => T::prefix_sum(v),
        2 => lanes_sum::<T, 2>(v),
        3 => lanes_sum::<T, 3>(v),
        4 => lanes_sum::<T, 4>(v),
        5 => lanes_sum::<T, 5>(v),
        6 => lanes_sum::<T, 6>(v),
        7 => lanes_sum::<T, 7>(v),
        8 => lanes_sum::<T, 8>(v),
        12 => lanes_sum::<T, 12>(v),
        16 => lanes_sum::<T, 16>(v),
        _ => {
            for i in stride..v.len() {
                v[i] = v[i].plus(v[i - stride]);
            }
        }
    }
}

/// Reverse horizontal integer differencing of one row of `cpp`-component pixels at a distance of `factor` pixels
/// (TIFF predictor 2: factor 1; DNG 34892: 2; DNG 34893: 4), in place, modulo 2^16.
///
/// Samples widened from 8 bits are summed modulo 2^16 too, as the format leaves them. A distance of zero doubles
/// every sample (the recurrence read literally); a distance at least the row length changes nothing.
pub fn undo_horizontal_u16(row: &mut [u16], cpp: usize, factor: usize) {
    running_sum(row, cpp.saturating_mul(factor));
}

// ------------------------------------------------------------------------------------------------- float samples

/// Exponent field of 2^-14, the smallest binary16 normal, as binary32 bits.
const HALF_MIN_NORMAL: u32 = 113 << 23;

/// binary16 bits to binary32 bits (exact widening; NaN payloads move to the top of the fraction unchanged).
#[inline(always)]
fn half_to_single_bits(h: u16) -> u32 {
    let h = u32::from(h);
    let sign = (h & 0x8000) << 16;
    // Exponent and fraction moved to their binary32 positions; the exponent is still biased by 15.
    let body = (h & 0x7fff) << 13;
    let exponent = h & 0x7c00;
    let normal = body + (112 << 23);
    let special = body + (224 << 23);
    // Subnormal: give the fraction the exponent of 2^-14, then subtract 2^-14 (exact).
    let tiny = (f32::from_bits(body + HALF_MIN_NORMAL) - f32::from_bits(HALF_MIN_NORMAL)).to_bits();
    let magnitude = if exponent == 0 {
        tiny
    } else if exponent == 0x7c00 {
        special
    } else {
        normal
    };
    sign | magnitude
}

/// 24-bit float bits (low 24 bits of `v`) to binary32 bits. Exponent 0 with a nonzero fraction is read as an
/// ordinary exponent (no gradual underflow); exponent 127 is infinity or NaN.
#[inline(always)]
fn f24_to_single_bits(v: u32) -> u32 {
    let sign = (v & 0x80_0000) << 8;
    let body = v & 0x7f_ffff;
    let shifted = body << 7;
    let magnitude = if body == 0 {
        0
    } else if body >> 16 == 127 {
        shifted + (128 << 23)
    } else {
        shifted + (64 << 23)
    };
    sign | magnitude
}

/// Widen IEEE 754 binary16 bits to `f32` (exact for every pattern, NaNs included).
pub fn f16_to_f32(h: u16) -> f32 {
    f32::from_bits(half_to_single_bits(h))
}

/// Widen DNG 24-bit floating-point bits (sign bit 23, exponent bits 22..16 biased by 63, fraction bits 15..0) to
/// `f32`. Bits 24 to 31 of `v` are ignored.
pub fn f24_to_f32(v: u32) -> f32 {
    f32::from_bits(f24_to_single_bits(v))
}

#[inline(always)]
fn join24(high: u8, mid: u8, low: u8) -> u32 {
    u32::from(high) << 16 | u32::from(mid) << 8 | u32::from(low)
}

/// Convert big-endian floating-point samples to `f32`: 2-byte binary16 when `bps_bytes` is 2, 3-byte DNG floats
/// when it is 3, and 4-byte binary32 for any other value. Converts as many whole samples as both slices hold;
/// later entries of `out` are not touched.
pub fn float_bytes_to_f32(src: &[u8], bps_bytes: usize, out: &mut [f32]) {
    contiguous_to_f32(src, bps_bytes, false, out);
}

/// Samples stored one after another in the given byte order (2, 3 or, for any other width, 4 bytes each).
fn contiguous_to_f32(src: &[u8], bps_bytes: usize, little_endian: bool, out: &mut [f32]) {
    match (bps_bytes, little_endian) {
        (2, false) => {
            for (o, b) in out.iter_mut().zip(src.chunks_exact(2)) {
                *o = f32::from_bits(half_to_single_bits(u16::from_be_bytes([b[0], b[1]])));
            }
        }
        (2, true) => {
            for (o, b) in out.iter_mut().zip(src.chunks_exact(2)) {
                *o = f32::from_bits(half_to_single_bits(u16::from_le_bytes([b[0], b[1]])));
            }
        }
        (3, false) => {
            for (o, b) in out.iter_mut().zip(src.chunks_exact(3)) {
                *o = f32::from_bits(f24_to_single_bits(join24(b[0], b[1], b[2])));
            }
        }
        (3, true) => {
            for (o, b) in out.iter_mut().zip(src.chunks_exact(3)) {
                *o = f32::from_bits(f24_to_single_bits(join24(b[2], b[1], b[0])));
            }
        }
        (_, false) => {
            for (o, b) in out.iter_mut().zip(src.chunks_exact(4)) {
                *o = f32::from_bits(u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
            }
        }
        (_, true) => {
            for (o, b) in out.iter_mut().zip(src.chunks_exact(4)) {
                *o = f32::from_bits(u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            }
        }
    }
}

/// Samples stored as byte planes (`planes` = plane 0 ‖ plane 1 ‖ …, most significant first, `n` bytes each) to
/// `f32`. `width` is 2, 3 or 4; `planes` holds at least `n * width` bytes and `out` at least `n` entries.
fn planes_to_f32(planes: &[u8], n: usize, width: usize, out: &mut [f32]) {
    let out = &mut out[..n];
    let plane = |k: usize| &planes[k * n..(k + 1) * n];
    match width {
        2 => {
            for ((o, &high), &low) in out.iter_mut().zip(plane(0)).zip(plane(1)) {
                *o = f32::from_bits(half_to_single_bits(u16::from(high) << 8 | u16::from(low)));
            }
        }
        3 => {
            for (((o, &high), &mid), &low) in out.iter_mut().zip(plane(0)).zip(plane(1)).zip(plane(2)) {
                *o = f32::from_bits(f24_to_single_bits(join24(high, mid, low)));
            }
        }
        _ => {
            for ((((o, &b0), &b1), &b2), &b3) in out.iter_mut().zip(plane(0)).zip(plane(1)).zip(plane(2)).zip(plane(3))
            {
                *o = f32::from_bits(u32::from_be_bytes([b0, b1, b2, b3]));
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------- float predictor

/// Reverse the floating-point predictor of one row of `width_samples` samples of `bps_bytes` bytes, in place.
///
/// The first `width_samples * bps_bytes` bytes of `row` are summed with a byte distance of `factor` (zero adds each
/// byte to itself) and then regrouped from byte planes into big-endian samples. A row shorter than that is left
/// unchanged, and bytes after it are never touched. `tmp` is scratch space whose content afterwards is unspecified.
pub fn undo_float_predictor(row: &mut [u8], tmp: &mut Vec<u8>, width_samples: usize, bps_bytes: usize, factor: usize) {
    let total = width_samples * bps_bytes;
    if total == 0 || row.len() < total {
        return;
    }
    let row = &mut row[..total];
    running_sum(row, factor);
    tmp.clear();
    tmp.extend_from_slice(row);
    for (k, plane) in tmp.chunks_exact(width_samples).enumerate() {
        for (sample, &byte) in row.chunks_exact_mut(bps_bytes).zip(plane) {
            sample[k] = byte;
        }
    }
}

/// Decode one row of `samples` floating-point samples of `bps_bytes` bytes (2, 3 or 4) into `out[..samples]`.
///
/// With `factor` = 0 the samples are stored one after another in the byte order given by `little_endian`. Otherwise
/// the row is in floating-point predictor form (byte planes, byte differences at distance `factor`) and the byte
/// order flag does not matter. A row shorter than `samples * bps_bytes`, an `out` shorter than `samples` or another
/// sample width leave `out` untouched. `row` is used as working memory: its content afterwards is unspecified.
pub fn decode_float_row(
    row: &mut [u8],
    samples: usize,
    bps_bytes: usize,
    factor: usize,
    little_endian: bool,
    out: &mut [f32],
) {
    if !(2..=4).contains(&bps_bytes) || samples == 0 || out.len() < samples {
        return;
    }
    let total = samples * bps_bytes;
    if row.len() < total {
        return;
    }
    let row = &mut row[..total];
    if factor == 0 {
        contiguous_to_f32(row, bps_bytes, little_endian, &mut out[..samples]);
    } else {
        running_sum(row, factor);
        planes_to_f32(row, samples, bps_bytes, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; zlib_rs::compress_bound(data.len())];
        let (c, rc) = zlib_rs::compress_slice(&mut buf, data, zlib_rs::DeflateConfig::new(6));
        assert_eq!(rc, zlib_rs::ReturnCode::Ok);
        c.to_vec()
    }

    #[test]
    fn half_values() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x7bff), 65504.0);
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
        assert_eq!(f16_to_f32(0x03ff), 1023.0 * 2f32.powi(-24));
        assert_eq!(f16_to_f32(0x0400), 2f32.powi(-14));
        assert_eq!(f16_to_f32(0x8000).to_bits(), 0x8000_0000);
        assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
        assert_eq!(f16_to_f32(0xfc00), f32::NEG_INFINITY);
        // A signalling NaN keeps its payload; no quiet bit is added.
        assert_eq!(f16_to_f32(0x7c01).to_bits(), 0x7f80_2000);
        assert_eq!(f16_to_f32(0xfe00).to_bits(), 0xffc0_0000);
    }

    #[test]
    fn f24_values() {
        assert_eq!(f24_to_f32(63 << 16), 1.0);
        assert_eq!(f24_to_f32(0x80_0000 | 64 << 16 | 0x8000), -3.0);
        assert_eq!(f24_to_f32(0).to_bits(), 0);
        assert_eq!(f24_to_f32(0x80_0000).to_bits(), 0x8000_0000);
        assert_eq!(f24_to_f32(127 << 16), f32::INFINITY);
        assert_eq!(f24_to_f32(127 << 16 | 1).to_bits(), 0x7f80_0080);
        // Exponent 0 with a fraction is not subnormal.
        assert_eq!(f24_to_f32(0x8000), 1.5 * 2f32.powi(-63));
        // Bits above the 24th are ignored.
        assert_eq!(f24_to_f32(0xff00_0000 | 63 << 16), 1.0);
    }

    #[test]
    fn float_bytes_widths() {
        let mut out = [9.0f32; 3];
        float_bytes_to_f32(&[0x3c, 0x00, 0xc0, 0x00, 0x7b], 2, &mut out);
        assert_eq!(out, [1.0, -2.0, 9.0]);
        let mut out = [9.0f32; 2];
        float_bytes_to_f32(&1.5f32.to_be_bytes(), 7, &mut out);
        assert_eq!(out, [1.5, 9.0]);
        float_bytes_to_f32(&[63, 0, 0], 3, &mut out);
        assert_eq!(out, [1.0, 9.0]);
    }

    #[test]
    fn horizontal() {
        let mut row = vec![1u16, 2, 3, 4, 5, 6];
        undo_horizontal_u16(&mut row, 1, 1);
        assert_eq!(row, [1, 3, 6, 10, 15, 21]);
        let mut row = vec![1u16, 2, 3, 4, 5, 6];
        undo_horizontal_u16(&mut row, 2, 1);
        assert_eq!(row, [1, 2, 4, 6, 9, 12]);
        let mut row = vec![0xffffu16, 0, 2];
        undo_horizontal_u16(&mut row, 1, 1);
        assert_eq!(row, [0xffff, 0xffff, 1]);
        let mut row = vec![3u16, 0x8001];
        undo_horizontal_u16(&mut row, 0, 2);
        assert_eq!(row, [6, 2]);
        let mut row = vec![3u16, 4];
        undo_horizontal_u16(&mut row, 1, 4);
        assert_eq!(row, [3, 4]);
    }

    /// Rows encoded as a writer does (planes, then byte differences) come back as the original samples.
    #[test]
    fn float_predictor_round_trip() {
        let values: Vec<f32> = (0..37).map(|i| (i as f32 - 11.0) * 0.37).collect();
        for cpp in 1..=3 {
            let n = 36 / cpp * cpp;
            for d in [cpp, 2 * cpp, 4 * cpp] {
                let be: Vec<u8> = values[..n].iter().flat_map(|v| v.to_be_bytes()).collect();
                let mut planes = vec![0u8; be.len()];
                for j in 0..n {
                    for k in 0..4 {
                        planes[k * n + j] = be[j * 4 + k];
                    }
                }
                for i in (d..planes.len()).rev() {
                    planes[i] = planes[i].wrapping_sub(planes[i - d]);
                }
                let mut out = vec![0f32; n + 1];
                decode_float_row(&mut planes.clone(), n, 4, d, true, &mut out);
                assert_eq!(&out[..n], &values[..n]);
                assert_eq!(out[n], 0.0);
                let mut row = planes.clone();
                row.push(0xAB);
                undo_float_predictor(&mut row, &mut Vec::new(), n, 4, d);
                assert_eq!(&row[..be.len()], &be[..]);
                assert_eq!(row[be.len()], 0xAB);
            }
        }
    }

    #[test]
    fn float_row_degenerate() {
        let mut out = [7.0f32; 2];
        decode_float_row(&mut [], 0, 2, 1, false, &mut out);
        decode_float_row(&mut [0; 3], 2, 2, 1, false, &mut out);
        decode_float_row(&mut [0; 4], 2, 5, 1, false, &mut out);
        decode_float_row(&mut [0; 6], 3, 2, 1, false, &mut out);
        assert_eq!(out, [7.0; 2]);
        decode_float_row(&mut [0x00, 0x3c, 0x00, 0xc0], 2, 2, 0, true, &mut out);
        assert_eq!(out, [1.0, -2.0]);
        let mut row = vec![1u8, 2, 3];
        undo_float_predictor(&mut row, &mut Vec::new(), 2, 2, 1);
        assert_eq!(row, [1, 2, 3]);
        let mut row = vec![1u8, 2, 200, 4];
        undo_float_predictor(&mut row, &mut Vec::new(), 2, 2, 0);
        assert_eq!(row, [2, 144, 4, 8]);
    }

    #[test]
    fn inflate_known_stream() {
        // zlib stream of "hello": one fixed-Huffman block, Adler-32 0x062c0215.
        let z = [0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x06, 0x2c, 0x02, 0x15];
        assert_eq!(inflate_zlib(&z, 100).unwrap(), b"hello");
        let mut out = [0xEEu8; 8];
        assert_eq!(inflate_zlib_into(&z, &mut out).unwrap(), 5);
        assert_eq!(&out, b"hello\xEE\xEE\xEE");
    }

    #[test]
    fn inflate_errors() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let z = zlib(&data);
        assert_eq!(inflate_zlib(&z, data.len()).unwrap(), data);
        let e = inflate_zlib(&z, data.len() - 1).unwrap_err();
        assert_eq!(e.kind(), "CorruptFile");
        assert!(e.to_string().contains("deflate: "));
        let e = inflate_zlib(b"garbage garbage garbage", 1000).unwrap_err();
        assert_eq!(e.kind(), "CorruptFile");
        let mut small = vec![0u8; data.len() - 1];
        let e = inflate_zlib_into(&z, &mut small).unwrap_err();
        assert_eq!((e.kind(), e.to_string().as_str()), ("CorruptFile", "corrupt file: deflate: BufError"));
        let mut bad = z.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        let mut out = vec![0u8; data.len()];
        assert_eq!(inflate_zlib_into(&bad, &mut out).unwrap_err().to_string(), "corrupt file: deflate: DataError");
        assert_eq!(inflate_zlib_into(&z[..z.len() / 2], &mut out).unwrap_err().kind(), "CorruptFile");
    }
}
