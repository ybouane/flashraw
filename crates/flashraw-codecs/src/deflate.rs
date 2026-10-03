//! Deflate (zlib) payloads and TIFF/DNG predictors.
//!
//! DNG compression 8 stores tiles as zlib streams, optionally with predictor
//! 2 (horizontal integer differencing), 34892-34894 (2/4-sample horizontal
//! differencing) or 3/34894-34895 (floating-point byte-plane differencing).

use flashraw_core::{Error, Result};

/// Inflate a zlib stream, refusing to produce more than `max_out` bytes.
pub fn inflate_zlib(data: &[u8], max_out: usize) -> Result<Vec<u8>> {
    let opts = zune_inflate::DeflateOptions::default().set_limit(max_out).set_size_hint(max_out);
    let mut d = zune_inflate::DeflateDecoder::new_with_options(data, opts);
    d.decode_zlib().map_err(|e| Error::corrupt(format!("deflate: {e:?}")))
}

/// Inflate a zlib stream into `out`, returning the number of bytes produced. Decodes in
/// place into caller memory (no allocation); errors on corrupt data. Output beyond the
/// stream's length is left untouched; a stream larger than `out` is an error.
pub fn inflate_zlib_into(data: &[u8], out: &mut [u8]) -> Result<usize> {
    let (res, rc) = zlib_rs::decompress_slice(out, data, zlib_rs::InflateConfig::default());
    match rc {
        zlib_rs::ReturnCode::Ok => Ok(res.len()),
        rc => Err(Error::corrupt(format!("deflate: {rc:?}"))),
    }
}

/// Undo horizontal integer differencing on u16 samples, in place, for one row
/// of `width` pixels with `cpp` interleaved samples and difference `factor`
/// (1 for TIFF predictor 2; 2 or 4 for DNG predictors 34892/34893).
pub fn undo_horizontal_u16(row: &mut [u16], cpp: usize, factor: usize) {
    let step = cpp * factor;
    for i in step..row.len() {
        row[i] = row[i].wrapping_add(row[i - step]);
    }
}

/// Undo DNG floating-point predictor (TIFF predictor 3) for one row: bytes were
/// split into byte planes (most significant first) and then byte-differenced.
/// `row` holds `width * cpp * bytes_per_sample` bytes; output is native samples in
/// big-endian byte order ready for [`float_bytes_to_f32`].
pub fn undo_float_predictor(row: &mut [u8], tmp: &mut Vec<u8>, width_samples: usize, bps_bytes: usize, factor: usize) {
    let n = width_samples * bps_bytes;
    if row.len() < n {
        return;
    }
    let row = &mut row[..n];
    for i in factor..n {
        row[i] = row[i].wrapping_add(row[i - factor]);
    }
    tmp.clear();
    tmp.resize(n, 0);
    for s in 0..width_samples {
        for b in 0..bps_bytes {
            tmp[s * bps_bytes + b] = row[b * width_samples + s];
        }
    }
    row.copy_from_slice(tmp);
}

/// Decode one row of DNG floating-point samples straight into `out`.
///
/// `row` holds the row's bytes (modified in place). With `factor > 0` the row uses the
/// floating-point predictor: bytes are byte-differenced with distance `factor`, then
/// stored as byte planes (most significant plane first). With `factor == 0` the samples
/// are stored contiguously in file byte order (`little_endian`). Fuses what
/// [`undo_float_predictor`] + [`float_bytes_to_f32`] do, without a temporary row.
pub fn decode_float_row(row: &mut [u8], samples: usize, bps_bytes: usize, factor: usize, little_endian: bool, out: &mut [f32]) {
    let n = samples * bps_bytes;
    if row.len() < n || out.len() < samples || !(2..=4).contains(&bps_bytes) {
        return;
    }
    let row = &mut row[..n];
    let out = &mut out[..samples];
    if factor == 0 {
        match (bps_bytes, little_endian) {
            (4, true) => out.iter_mut().zip(row.chunks_exact(4)).for_each(|(o, c)| *o = f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
            (4, false) => out.iter_mut().zip(row.chunks_exact(4)).for_each(|(o, c)| *o = f32::from_be_bytes([c[0], c[1], c[2], c[3]])),
            (2, le) => out.iter_mut().zip(row.chunks_exact(2)).for_each(|(o, c)| {
                *o = f16_to_f32(if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            }),
            (_, le) => out.iter_mut().zip(row.chunks_exact(3)).for_each(|(o, c)| {
                let (a, b, d) = if le { (c[2], c[1], c[0]) } else { (c[0], c[1], c[2]) };
                *o = f24_to_f32((a as u32) << 16 | (b as u32) << 8 | d as u32)
            }),
        }
        return;
    }
    // Undo byte differencing (sequential prefix sum with distance `factor`).
    if factor == 1 {
        let mut acc = row[0];
        for b in row[1..].iter_mut() {
            acc = acc.wrapping_add(*b);
            *b = acc;
        }
    } else {
        for i in factor..n {
            row[i] = row[i].wrapping_add(row[i - factor]);
        }
    }
    // Assemble samples from the byte planes.
    match bps_bytes {
        4 => {
            let (p0, rest) = row.split_at(samples);
            let (p1, rest) = rest.split_at(samples);
            let (p2, p3) = rest.split_at(samples);
            for ((((o, &a), &b), &c), &d) in out.iter_mut().zip(p0).zip(p1).zip(p2).zip(p3) {
                *o = f32::from_bits(u32::from_be_bytes([a, b, c, d]));
            }
        }
        3 => {
            let (p0, rest) = row.split_at(samples);
            let (p1, p2) = rest.split_at(samples);
            for (((o, &a), &b), &c) in out.iter_mut().zip(p0).zip(p1).zip(p2) {
                *o = f24_to_f32((a as u32) << 16 | (b as u32) << 8 | c as u32);
            }
        }
        _ => {
            let (p0, p1) = row.split_at(samples);
            for ((o, &a), &b) in out.iter_mut().zip(p0).zip(p1) {
                *o = f16_to_f32(u16::from_be_bytes([a, b]));
            }
        }
    }
}

/// Convert half-float bits to f32.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1F) as u32;
    let mant = (h & 0x3FF) as u32;
    let bits = if exp == 0 {
        if mant == 0 {
            sign
        } else {
            // Subnormal: normalise.
            let mut e = 127 - 15 + 1;
            let mut m = mant;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | ((e as u32) << 23) | ((m & 0x3FF) << 13)
        }
    } else if exp == 31 {
        sign | 0x7F80_0000 | (mant << 13)
    } else {
        sign | ((exp + 127 - 15) << 23) | (mant << 13)
    };
    f32::from_bits(bits)
}

/// Convert 24-bit DNG float (1/7/16) bits to f32.
pub fn f24_to_f32(v: u32) -> f32 {
    let sign = (v >> 23) & 1;
    let exp = (v >> 16) & 0x7F;
    let mant = v & 0xFFFF;
    let bits = if exp == 0 && mant == 0 {
        sign << 31
    } else if exp == 0x7F {
        (sign << 31) | 0x7F80_0000 | (mant << 7)
    } else {
        (sign << 31) | ((exp + 127 - 63) << 23) | (mant << 7)
    };
    f32::from_bits(bits)
}

/// Decode big-endian float samples of `bps_bytes` (2, 3 or 4) into `out`.
pub fn float_bytes_to_f32(src: &[u8], bps_bytes: usize, out: &mut [f32]) {
    match bps_bytes {
        2 => {
            src.chunks_exact(2).zip(out.iter_mut()).for_each(|(c, o)| *o = f16_to_f32(u16::from_be_bytes([c[0], c[1]])))
        }
        3 => src
            .chunks_exact(3)
            .zip(out.iter_mut())
            .for_each(|(c, o)| *o = f24_to_f32((c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32)),
        _ => src
            .chunks_exact(4)
            .zip(out.iter_mut())
            .for_each(|(c, o)| *o = f32::from_bits(u32::from_be_bytes([c[0], c[1], c[2], c[3]]))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats() {
        assert_eq!(f16_to_f32(0x3C00), 1.0);
        assert_eq!(f16_to_f32(0xC000), -2.0);
        assert_eq!(f16_to_f32(0x0001), 5.960_464_5e-8);
        assert!(f16_to_f32(0x7C00).is_infinite());
    }

    #[test]
    fn f24() {
        // 1.0 = exp 63, mant 0
        assert_eq!(f24_to_f32(63 << 16), 1.0);
        assert_eq!(f24_to_f32((1 << 23) | (64 << 16)), -2.0);
    }

    #[test]
    fn horizontal() {
        let mut r = vec![10u16, 1, 1, 1, 65535];
        undo_horizontal_u16(&mut r, 1, 1);
        assert_eq!(r, vec![10, 11, 12, 13, 12]);
    }

    #[test]
    fn float_predictor_roundtrip() {
        let vals = [1.5f32, -2.25, 1000.0, 0.0];
        // encode: big-endian bytes → byte planes → differences
        let be: Vec<u8> = vals.iter().flat_map(|v| v.to_be_bytes()).collect();
        let mut planes = vec![0u8; 16];
        for s in 0..4 {
            for b in 0..4 {
                planes[b * 4 + s] = be[s * 4 + b];
            }
        }
        let mut enc = planes.clone();
        for i in (1..16).rev() {
            enc[i] = enc[i].wrapping_sub(enc[i - 1]);
        }
        let mut tmp = vec![];
        undo_float_predictor(&mut enc, &mut tmp, 4, 4, 1);
        let mut out = [0f32; 4];
        float_bytes_to_f32(&enc, 4, &mut out);
        assert_eq!(out, vals);
    }

    #[test]
    fn fused_float_row_matches_reference() {
        let mut x: u32 = 12345;
        for bps in [2usize, 3, 4] {
            for factor in [0usize, 1, 2, 4] {
                for le in [false, true] {
                    let samples = 37;
                    let bytes: Vec<u8> = (0..samples * bps)
                        .map(|_| {
                            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                            (x >> 24) as u8
                        })
                        .collect();
                    // Reference: old two-step path.
                    let mut r = bytes.clone();
                    let mut tmp = vec![];
                    if factor > 0 {
                        undo_float_predictor(&mut r, &mut tmp, samples, bps, factor);
                    } else if le {
                        for c in r.chunks_exact_mut(bps) {
                            c.reverse();
                        }
                    }
                    let mut want = vec![0f32; samples];
                    float_bytes_to_f32(&r, bps, &mut want);
                    let mut got = vec![0f32; samples];
                    let mut b = bytes.clone();
                    decode_float_row(&mut b, samples, bps, factor, le, &mut got);
                    for (g, w) in got.iter().zip(&want) {
                        assert_eq!(g.to_bits(), w.to_bits(), "bps {bps} factor {factor} le {le}");
                    }
                }
            }
        }
    }

    #[test]
    fn inflate_limits() {
        assert!(inflate_zlib(&[0x78, 0x9c, 0xff], 100).is_err());
        assert!(inflate_zlib_into(&[0x78, 0x9c, 0xff], &mut [0u8; 100]).is_err());
        // "hello" compressed with zlib.
        let z = [0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x06, 0x2c, 0x02, 0x15];
        let mut out = [0u8; 16];
        assert_eq!(inflate_zlib_into(&z, &mut out).unwrap(), 5);
        assert_eq!(&out[..5], b"hello");
        assert_eq!(inflate_zlib(&z, 16).unwrap(), b"hello");
        assert!(inflate_zlib_into(&z, &mut [0u8; 3]).is_err());
    }
}
