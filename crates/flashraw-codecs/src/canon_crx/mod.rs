//! Canon CRX: the raw codec of CR3 files (EOS R/M/D series since 2018, PowerShots).
//!
//! The sensor is split into four colour planes (one per 2x2 CFA position; one plane
//! for 8-bit monochrome data), and each plane into a grid of tiles. Each (tile,
//! plane) is coded independently:
//!
//! * **Lossless RAW** (`levels == 0`): one band per tile plane, coded line by line
//!   with an edge-aware predictor and adaptive Golomb–Rice residuals ([`entropy`]).
//! * **C-RAW** (`levels == 3`): a 3-level integer 5/3 wavelet ([`wavelet`]); the LL
//!   band is predictive, the detail bands use a context-adaptive coder. Coefficients
//!   are dequantised with a per-band step that is either updated per line (header
//!   version 0x100) or read from a per-tile quantisation map (version 0x200).
//!
//! Finally samples are offset by the median (`1 << (bits - 1)`) and clamped
//! (encoding type 0), or converted from Canon's decorrelated colour planes
//! (encoding type 3).
//!
//! Decoding parallelises over (tile, plane) units; within a unit the streams are
//! sequential.

mod bits;
mod entropy;
mod layout;
mod plane;
mod wavelet;

use flashraw_core::{Error, Result, par};
use layout::Image;

/// The `CMP1` codec header of a CR3 raw track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrxHeader {
    /// 0x100 (lossless RAW and C-RAW with per-line quantisation) or 0x200 (C-RAW with
    /// quantisation maps).
    pub version: u16,
    /// Full sensor buffer size (all planes interleaved).
    pub width: u32,
    pub height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub bits: u8,
    /// 4 (Bayer planes) or 1 (8-bit single plane).
    pub planes: u8,
    /// Position of the four planes in the 2x2 CFA cell (0 = RGGB, 1 = GRBG, 2 = GBRG, 3 = BGGR).
    pub cfa_layout: u8,
    /// 0 = planes are CFA colours, 1 = signed samples, 3 = decorrelated colour planes.
    pub enc_type: u8,
    /// Wavelet levels (0 = lossless).
    pub levels: u8,
    /// Size of the per-sample header in the media data.
    pub mdat_header_size: u32,
    /// Output precision of encoding type 3.
    pub median_bits: u8,
}

impl CrxHeader {
    /// Parse and validate a `CMP1` box payload.
    pub fn parse(d: &[u8]) -> Result<CrxHeader> {
        if d.len() < 32 {
            return Err(Error::corrupt("CRX: CMP1 header too short"));
        }
        let be16 = |o: usize| u16::from_be_bytes([d[o], d[o + 1]]);
        let be32 = |o: usize| u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
        let bits = d[24];
        let mut h = CrxHeader {
            version: be16(4),
            width: be32(8),
            height: be32(12),
            tile_width: be32(16),
            tile_height: be32(20),
            bits,
            planes: d[25] >> 4,
            cfa_layout: d[25] & 0xF,
            enc_type: d[26] >> 4,
            levels: d[26] & 0xF,
            mdat_header_size: be32(28),
            median_bits: bits,
        };
        let has_tile_cols = d[27] >> 7;
        let has_tile_rows = (d[27] >> 6) & 1;
        let ext_header = d.get(32).is_some_and(|b| b >> 7 != 0);
        if ext_header && d.len() >= 56 && h.planes == 4 && d.get(56).is_some_and(|b| (b >> 6) & 1 != 0) {
            if let Some(&mb) = d.get(84) {
                h.median_bits = mb;
            }
        }
        let bad = |what: &str| Err(Error::unsupported_compression("CR3", format!("CRX {what}")));
        if (h.version != 0x100 && h.version != 0x200) || h.mdat_header_size == 0 {
            return bad("header version");
        }
        match h.enc_type {
            1 if h.bits > 15 => return bad("bit depth"),
            0 | 3 if h.bits > 14 => return bad("bit depth"),
            0 | 1 | 3 => {}
            _ => return bad("encoding type"),
        }
        if h.planes == 1 {
            if h.cfa_layout != 0 || h.enc_type != 0 || h.bits != 8 {
                return bad("single-plane layout");
            }
        } else if h.planes != 4
            || h.width & 1 != 0
            || h.height & 1 != 0
            || h.tile_width & 1 != 0
            || h.tile_height & 1 != 0
            || h.cfa_layout > 3
            || h.bits == 8
        {
            return bad("plane layout");
        }
        if h.tile_width > h.width || h.tile_height > h.height {
            return bad("tile size");
        }
        if h.levels > 3 || has_tile_cols > 1 || has_tile_rows > 1 {
            return bad("wavelet levels");
        }
        if h.bits == 0 || (h.enc_type == 3 && !(1..=16).contains(&h.median_bits)) {
            return bad("bit depth");
        }
        Ok(h)
    }

    /// `(dx, dy)` of plane `p` inside the 2x2 CFA cell.
    pub fn plane_position(&self, p: usize) -> (usize, usize) {
        // Plane order is always R, G1, G2, B; the layout says where R sits.
        const POS: [[(usize, usize); 4]; 4] = [
            [(0, 0), (1, 0), (0, 1), (1, 1)],
            [(1, 0), (0, 0), (1, 1), (0, 1)],
            [(0, 1), (1, 1), (0, 0), (1, 0)],
            [(1, 1), (0, 1), (1, 0), (0, 0)],
        ];
        POS[self.cfa_layout as usize & 3][p & 3]
    }

    /// Maximum sample value of the decoded data.
    pub fn white_level(&self) -> u32 {
        (1u32 << self.bits.min(16)) - 1
    }
}

/// Decode a CRX media sample into `out`, a `header.width x header.height` buffer.
///
/// `data` starts at the sample (its per-sample header) and may extend to the end of
/// the file: subband offsets are resolved within it.
pub fn decode(h: &CrxHeader, data: &[u8], out: &mut [u16], parallel: bool) -> Result<()> {
    let (w, ht) = (h.width as usize, h.height as usize);
    if out.len() < w * ht {
        return Err(Error::InvalidArgument("CRX output buffer too small".into()));
    }
    let img = Image::parse(h, data)?;
    let planes = h.planes as usize;
    let levels = h.levels as usize;
    let units = img.tiles.len() * planes;
    // Each unit decodes into its own buffer (tile plane, row-major); the interleave
    // into the CFA buffer is a separate parallel pass.
    let results: Vec<Result<Vec<u16>>> = par::map((0..units).collect(), parallel, |u| {
        let tile = &img.tiles[u / planes];
        let mut buf = vec![0u16; tile.width * tile.height];
        let conv = Converter::new(h);
        plane::decode_plane(data, tile, u % planes, levels, |y, row| {
            conv.convert(row, &mut buf[y * tile.width..(y + 1) * tile.width]);
        })?;
        Ok(buf)
    });
    let mut bufs = Vec::with_capacity(units);
    for r in results {
        bufs.push(r?);
    }
    interleave(h, &img, &bufs, out, parallel);
    Ok(())
}

/// Per-sample conversion of decoded plane values to stored samples.
struct Converter {
    enc_type: u8,
    planes: u8,
    bits: u32,
}

impl Converter {
    fn new(h: &CrxHeader) -> Self {
        Converter { enc_type: h.enc_type, planes: h.planes, bits: h.bits as u32 }
    }

    #[inline]
    fn convert(&self, row: &[i32], out: &mut [u16]) {
        let median = 1i32 << (self.bits - 1);
        if self.enc_type == 1 {
            // Signed samples, stored as 16-bit two's complement.
            let (lo, hi) = (-median, median - 1);
            for (o, &v) in out.iter_mut().zip(row) {
                *o = v.clamp(lo, hi) as i16 as u16;
            }
        } else if self.enc_type == 3 && self.planes == 4 {
            // Decorrelated planes are combined after all four are decoded; keep the
            for (o, &v) in out.iter_mut().zip(row) {
                *o = v as i16 as u16;
            }
        } else {
            let max = (1i32 << self.bits) - 1;
            for (o, &v) in out.iter_mut().zip(row) {
                *o = median.wrapping_add(v).clamp(0, max) as u16;
            }
        }
    }
}

/// Place tile-plane buffers into the CFA buffer (and combine encoding-type-3 planes).
fn interleave(h: &CrxHeader, img: &Image, bufs: &[Vec<u16>], out: &mut [u16], parallel: bool) {
    let w = h.width as usize;
    let planes = h.planes as usize;
    if planes == 1 {
        par::for_each_row_chunk(&mut out[..w * img.plane_height], w, 16, parallel, |y0, chunk| {
            for (r, row) in chunk.chunks_exact_mut(w).enumerate() {
                let y = y0 + r;
                for (ti, t) in img.tiles.iter().enumerate() {
                    if y < t.y || y >= t.y + t.height {
                        continue;
                    }
                    let src = &bufs[ti][(y - t.y) * t.width..(y - t.y + 1) * t.width];
                    row[t.x..t.x + t.width].copy_from_slice(src);
                }
            }
        });
        return;
    }
    let pos: [(usize, usize); 4] = std::array::from_fn(|p| h.plane_position(p));
    let e3 = h.enc_type == 3;
    let mb = h.median_bits as u32;
    // One chunk = one plane row = two CFA rows.
    par::for_each_row_chunk(&mut out[..w * 2 * img.plane_height], 2 * w, 8, parallel, |py0, chunk| {
        for (r, pair) in chunk.chunks_exact_mut(2 * w).enumerate() {
            let py = py0 + r;
            for (ti, t) in img.tiles.iter().enumerate() {
                if py < t.y || py >= t.y + t.height {
                    continue;
                }
                let line = |p: usize| {
                    let b = &bufs[ti * 4 + p];
                    &b[(py - t.y) * t.width..(py - t.y + 1) * t.width]
                };
                let src = [line(0), line(1), line(2), line(3)];
                if e3 {
                    for i in 0..t.width {
                        let v = combine_e3([src[0][i], src[1][i], src[2][i], src[3][i]], mb);
                        for (p, &(dx, dy)) in pos.iter().enumerate() {
                            pair[dy * w + 2 * (t.x + i) + dx] = v[p];
                        }
                    }
                } else {
                    for (p, &(dx, dy)) in pos.iter().enumerate() {
                        let dst = &mut pair[dy * w + 2 * t.x..dy * w + 2 * (t.x + t.width)];
                        for (d, &s) in dst.iter_mut().skip(dx).step_by(2).zip(src[p]) {
                            *d = s;
                        }
                    }
                }
            }
        }
    });
}

/// Encoding type 3: reconstruct R, G1, G2, B from the luma-like plane 0, the two
/// chroma-like planes 1/3 and the green difference plane 2 (fixed-point 10-bit).
#[inline]
fn combine_e3(p: [u16; 4], median_bits: u32) -> [u16; 4] {
    let [p0, p1, p2, p3] = p.map(|v| v as i16 as i32);
    let median = (1i32 << (median_bits - 1)) << 10;
    let max = (1i32 << median_bits) - 1;
    let base = median + (p0 << 10);
    let g = base - 168 * p1 - 585 * p3;
    let g = if g < 0 { -(((-g + 512) >> 9) & !1) } else { ((g + 512) >> 9) & !1 };
    let r = (base + 1510 * p3 + 512) >> 10;
    let g1 = (p2 + g + 1) >> 1;
    let g2 = (g - p2 + 1) >> 1;
    let b = (base + 1927 * p1 + 512) >> 10;
    [r, g1, g2, b].map(|v| v.clamp(0, max) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmp1(version: u16, w: u32, h: u32, tw: u32, th: u32, bits: u8, planes_cfa: u8, enc_lev: u8) -> Vec<u8> {
        let mut d = vec![0u8; 52];
        d[4..6].copy_from_slice(&version.to_be_bytes());
        d[8..12].copy_from_slice(&w.to_be_bytes());
        d[12..16].copy_from_slice(&h.to_be_bytes());
        d[16..20].copy_from_slice(&tw.to_be_bytes());
        d[20..24].copy_from_slice(&th.to_be_bytes());
        d[24] = bits;
        d[25] = planes_cfa;
        d[26] = enc_lev;
        d[28..32].copy_from_slice(&112u32.to_be_bytes());
        d
    }

    #[test]
    fn header_parse_and_validation() {
        let h = CrxHeader::parse(&cmp1(0x100, 6888, 4546, 3444, 4546, 14, 0x40, 0x03)).unwrap();
        assert_eq!((h.width, h.height, h.planes, h.levels, h.bits, h.median_bits), (6888, 4546, 4, 3, 14, 14));
        assert_eq!(h.white_level(), 16383);
        assert!(CrxHeader::parse(&cmp1(0x300, 64, 64, 64, 64, 14, 0x40, 0)).is_err());
        assert!(CrxHeader::parse(&cmp1(0x100, 65, 64, 64, 64, 14, 0x40, 0)).is_err());
        assert!(CrxHeader::parse(&cmp1(0x100, 64, 64, 128, 64, 14, 0x40, 0)).is_err());
        assert!(CrxHeader::parse(&cmp1(0x100, 64, 64, 64, 64, 15, 0x40, 0)).is_err());
        assert!(CrxHeader::parse(&cmp1(0x100, 64, 64, 64, 64, 14, 0x40, 0x04)).is_err());
        assert!(CrxHeader::parse(&cmp1(0x100, 64, 64, 64, 64, 14, 0x44, 0)).is_err());
        assert!(CrxHeader::parse(&cmp1(0x100, 64, 64, 64, 64, 8, 0x10, 0)).is_ok());
        assert!(CrxHeader::parse(&cmp1(0x100, 64, 64, 64, 64, 8, 0x40, 0)).is_err());
        assert!(CrxHeader::parse(&[0u8; 20]).is_err());
    }

    #[test]
    fn plane_positions_match_cfa_layouts() {
        // Layout 0 = RGGB: R at (0,0), B at (1,1). Layout 3 = BGGR.
        let mut h = CrxHeader::parse(&cmp1(0x100, 64, 64, 64, 64, 14, 0x40, 0)).unwrap();
        assert_eq!(h.plane_position(0), (0, 0));
        assert_eq!(h.plane_position(3), (1, 1));
        h.cfa_layout = 1; // GRBG
        assert_eq!(h.plane_position(0), (1, 0));
        assert_eq!(h.plane_position(3), (0, 1));
        h.cfa_layout = 3;
        assert_eq!(h.plane_position(0), (1, 1));
    }

    #[test]
    fn e3_neutral() {
        // Zero planes decode to the median on every channel.
        assert_eq!(combine_e3([0, 0, 0, 0], 14), [8192, 8192, 8192, 8192]);
        // Clamping.
        assert_eq!(combine_e3([0x7fff, 0, 0, 0], 14)[0], 16383);
    }

    #[test]
    fn decode_rejects_garbage_without_panicking() {
        let h = CrxHeader::parse(&cmp1(0x100, 128, 64, 128, 64, 14, 0x40, 0)).unwrap();
        let mut out = vec![0u16; 128 * 64];
        let mut seed = 7u32;
        for len in [0usize, 10, 112, 200, 4096] {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (seed >> 24) as u8
                })
                .collect();
            assert!(decode(&h, &data, &mut out, false).is_err());
        }
    }
}
