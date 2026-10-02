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
    fn inflate_limits() {
        assert!(inflate_zlib(&[0x78, 0x9c, 0xff], 100).is_err());
    }
}
