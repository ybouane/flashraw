//! The "load flags" family of bit-packed layouts.
//!
//! Many Nikon, Pentax and Samsung uncompressed raws are described by a handful of
//! "load flags" rather than by standard TIFF packing:
//!
//! | flag bits | meaning                                                                |
//! |-----------|------------------------------------------------------------------------|
//! | `1`       | one padding byte after every 10 samples (Nikon D100 "compressed")       |
//! | `2`       | rows stored as two interlaced fields (even rows, then odd rows)         |
//! | `4`       | the second field starts at a separate offset                           |
//! | `8 \| 16` | bits are gathered in little-endian units of 16/24/32 bits, MSB first   |
//! | `64`      | swap each pair of output columns                                       |
//! | `128`     | rows are padded to an even number of bytes                             |
//!
//! [`PackedSpec::fast_layout`] maps the common combinations onto
//! [`crate::packed`] (parallel, SIMD); everything else uses the exact sequential
//! path in [`decode`], which is the specification.

use crate::packed::{self, PackedLayout};
use flashraw_core::{Error, PackOrder, Result};

/// A load-flags packed raster description.
#[derive(Debug, Clone, Copy)]
pub struct PackedSpec {
    pub width: usize,
    pub height: usize,
    /// Bits per sample (1..=16).
    pub bits: u32,
    /// Load flags (see the module docs).
    pub flags: u32,
    /// Byte offset (relative to the start of `src`) of the second field when `flags & 4`.
    pub field2_offset: Option<usize>,
}

impl PackedSpec {
    /// Bytes per row before the 10-sample padding.
    fn bwide(&self) -> usize {
        let b = self.width * self.bits as usize / 8;
        b + (b & ((self.flags as usize >> 7) & 1))
    }

    /// Padding bits dropped at the end of each row (negative: rows share bytes).
    fn rbits(&self) -> i64 {
        self.bwide() as i64 * 8 - (self.width * self.bits as usize) as i64
    }

    /// Bytes per row in the file.
    pub fn row_bytes(&self) -> usize {
        let b = self.bwide();
        if self.flags & 1 != 0 { b * 16 / 15 } else { b }
    }

    /// Size of the gathering unit in bits.
    fn bite(&self) -> u32 {
        8 + (self.flags & 24)
    }

    /// The equivalent standard packed layout, when one exists.
    pub fn fast_layout(&self) -> Option<PackedLayout> {
        if self.flags & 7 != 0 || self.rbits() < 0 {
            return None;
        }
        let rbits = self.rbits() as usize;
        let swap = self.flags & 64 != 0;
        let order = match (self.bite(), swap) {
            (8, false) => PackOrder::MsbBytes,
            // 24-bit little-endian units with swapped column pairs == LSB-first 12-bit packing.
            (24, true) if self.bits == 12 => PackOrder::LsbBytes,
            (16, false) if self.bits == 16 || (self.width * self.bits as usize) % 16 == 0 => PackOrder::Msb16,
            (32, false) if (self.width * self.bits as usize) % 32 == 0 => PackOrder::Msb32,
            _ => return None,
        };
        if rbits % 8 != 0 || (order != PackOrder::MsbBytes && order != PackOrder::LsbBytes && rbits != 0) {
            return None;
        }
        Some(PackedLayout {
            width: self.width,
            height: self.height,
            bits: self.bits,
            order,
            row_stride: self.row_bytes(),
        })
    }

    /// Minimum input size for a complete image.
    pub fn required_bytes(&self) -> usize {
        if self.flags & 4 != 0 {
            let half = self.height.div_ceil(2);
            self.field2_offset.unwrap_or(0) + (self.height - half) * self.row_bytes()
        } else {
            self.height * self.row_bytes()
        }
    }
}

/// Decode `src` into `out` (rows of `stride` samples). Truncated input is an error
/// (decoding aborts at the first row that starts past EOF).
pub fn decode(src: &[u8], spec: &PackedSpec, out: &mut [u16], stride: usize, parallel: bool, simd: bool) -> Result<()> {
    if spec.bits == 0 || spec.bits > 16 || spec.width == 0 {
        return Err(Error::corrupt(format!("unsupported packed layout ({} bits)", spec.bits)));
    }
    if stride < spec.width || out.len() < stride * spec.height {
        return Err(Error::InvalidArgument("packed: output too small".into()));
    }
    if let Some(layout) = spec.fast_layout() {
        return packed::unpack(src, layout, out, stride, parallel, simd, false);
    }
    decode_exact(src, spec, out, stride)
}

/// Exact sequential decode of any load-flags layout (the specification).
pub fn decode_exact(src: &[u8], spec: &PackedSpec, out: &mut [u16], stride: usize) -> Result<()> {
    let (w, h, bits) = (spec.width, spec.height, spec.bits as i32);
    let bite = spec.bite() as i32;
    let rbits = spec.rbits();
    let half = h.div_ceil(2);
    let swap = ((spec.flags >> 6) & 1) as usize;
    let mut pos = 0usize;
    let mut bitbuf: u64 = 0;
    let mut vbits: i64 = 0;
    let mut eof = false;
    for irow in 0..h {
        let mut row = irow;
        if spec.flags & 2 != 0 {
            row = irow % half * 2 + irow / half;
            if row == 1 && spec.flags & 4 != 0 {
                vbits = 0;
                pos = spec.field2_offset.ok_or_else(|| Error::corrupt("missing second field offset"))?;
            }
        }
        if eof {
            return Err(Error::corrupt(format!("packed data truncated at row {irow}/{h}")));
        }
        let line =
            out.get_mut(row * stride..row * stride + w).ok_or_else(|| Error::corrupt("packed row out of range"))?;
        for col in 0..w {
            vbits -= bits as i64;
            while vbits < 0 {
                bitbuf = if bite >= 64 { 0 } else { bitbuf << bite };
                for i in (0..bite).step_by(8) {
                    let b = match src.get(pos) {
                        Some(&b) => b as u64,
                        None => {
                            eof = true;
                            0
                        }
                    };
                    pos += 1;
                    bitbuf |= b << i;
                }
                vbits += bite as i64;
            }
            let val = ((bitbuf << (64 - bits as i64 - vbits)) >> (64 - bits)) as u16;
            line[col ^ swap] = val;
            if spec.flags & 1 != 0 && col % 10 == 9 {
                pos += 1;
            }
        }
        vbits -= rbits;
    }
    Ok(())
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
    fn fast_layouts_match_exact_emulation() {
        for (bits, flags, width, fast) in [
            (12u32, 0u32, 40usize, true),
            (12, 80, 40, true),
            (12, 24, 40, true),
            (14, 0, 64, true),
            (12, 128, 42, true),
            (12, 128, 41, false),
            (16, 0, 10, true),
            (12, 16, 40, false),
            (12, 0, 41, false),
        ] {
            let spec = PackedSpec { width, height: 7, bits, flags, field2_offset: None };
            let src = pseudo(spec.required_bytes() + 8, bits as u64 * 31 + flags as u64);
            let mut a = vec![0u16; width * 7];
            let mut b = vec![0u16; width * 7];
            decode_exact(&src, &spec, &mut a, width).unwrap();
            assert_eq!(spec.fast_layout().is_some(), fast, "{bits} {flags} {width}");
            decode(&src, &spec, &mut b, width, false, true).unwrap();
            assert_eq!(a, b, "bits {bits} flags {flags}");
        }
    }

    #[test]
    fn lsb24_swap_is_lsb_packing() {
        // Two samples 0xABC, 0x123 in LSB-first 12-bit packing: BC 3A 12.
        let src = [0xBC, 0x3A, 0x12];
        let spec = PackedSpec { width: 2, height: 1, bits: 12, flags: 80, field2_offset: None };
        let mut out = [0u16; 2];
        decode_exact(&src, &spec, &mut out, 2).unwrap();
        assert_eq!(out, [0xABC, 0x123]);
    }

    #[test]
    fn padding_every_ten_samples() {
        // 10 samples of 12 bits = 15 bytes + 1 pad byte.
        let mut src = vec![0u8; 32];
        src[0] = 0xFF;
        src[1] = 0xF0; // sample 0 = 0xFFF
        src[15] = 0xAA; // padding byte, ignored
        src[16] = 0x80; // sample 10 = 0x800
        let spec = PackedSpec { width: 20, height: 1, bits: 12, flags: 1, field2_offset: None };
        let mut out = vec![0u16; 20];
        decode(&src, &spec, &mut out, 20, false, false).unwrap();
        assert_eq!(out[0], 0xFFF);
        assert_eq!(out[10], 0x800);
        assert_eq!(spec.row_bytes(), 32);
    }

    #[test]
    fn interlaced_fields() {
        let spec = PackedSpec { width: 2, height: 4, bits: 8, flags: 2, field2_offset: None };
        let src = [0u8, 1, 2, 3, 4, 5, 6, 7];
        let mut out = vec![0u16; 8];
        decode(&src, &spec, &mut out, 2, false, false).unwrap();
        // Stored rows: 0, 2, 1, 3.
        assert_eq!(out, vec![0, 1, 4, 5, 2, 3, 6, 7]);
    }

    #[test]
    fn truncation_is_an_error() {
        let spec = PackedSpec { width: 16, height: 8, bits: 12, flags: 1, field2_offset: None };
        let src = vec![0u8; 40];
        let mut out = vec![0u16; 16 * 8];
        assert!(decode(&src, &spec, &mut out, 16, false, false).is_err());
        let spec = PackedSpec { width: 16, height: 8, bits: 12, flags: 0, field2_offset: None };
        assert!(decode(&src, &spec, &mut out, 16, false, false).is_err());
    }
}
