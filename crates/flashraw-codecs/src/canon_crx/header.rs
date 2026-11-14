//! The `CMP1` codec header of a CR3 raw track.

use flashraw_core::{Error, Result};

/// The `CMP1` codec header of a CR3 raw track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrxHeader {
    /// 0x100 or 0x200.
    pub version: u16,
    /// Full sensor buffer size (all planes interleaved).
    pub width: u32,
    pub height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub bits: u8,
    /// 4 (Bayer planes) or 1 (8-bit single plane).
    pub planes: u8,
    /// 0 = RGGB, 1 = GRBG, 2 = GBRG, 3 = BGGR.
    pub cfa_layout: u8,
    /// 0, 1 or 3.
    pub enc_type: u8,
    /// Wavelet levels (0 = lossless).
    pub levels: u8,
    /// Size of the per-sample header at the start of each sample.
    pub mdat_header_size: u32,
    /// Output precision of encoding type 3 (equal to `bits` otherwise).
    pub median_bits: u8,
}

/// `(dx, dy)` of the planes R, G1, G2, B inside the 2x2 cell, per CFA layout.
const PLANE_CELL: [[(u8, u8); 4]; 4] = [
    [(0, 0), (1, 0), (0, 1), (1, 1)],
    [(1, 0), (0, 0), (1, 1), (0, 1)],
    [(0, 1), (1, 1), (0, 0), (1, 0)],
    [(1, 1), (0, 1), (1, 0), (0, 0)],
];

fn be16(d: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([d[at], d[at + 1]])
}

fn be32(d: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

fn unsupported(what: &str) -> Error {
    Error::unsupported_compression("CR3", format!("CRX {what}"))
}

impl CrxHeader {
    /// Parse and validate a `CMP1` box payload (the bytes after the 8-byte box header).
    pub fn parse(d: &[u8]) -> Result<CrxHeader> {
        if d.len() < 32 {
            return Err(Error::corrupt("CRX: CMP1 header too short"));
        }
        let h = CrxHeader {
            version: be16(d, 4),
            width: be32(d, 8),
            height: be32(d, 12),
            tile_width: be32(d, 16),
            tile_height: be32(d, 20),
            bits: d[24],
            planes: d[25] >> 4,
            cfa_layout: d[25] & 15,
            enc_type: d[26] >> 4,
            levels: d[26] & 15,
            mdat_header_size: be32(d, 28),
            median_bits: median_bits(d),
        };
        h.validate()?;
        Ok(h)
    }

    /// Check the header against the supported modes. Each row names the failure it reports and
    /// the predicate a supported header satisfies; the first violated row decides the message
    /// (every row yields the same error kind).
    fn validate(&self) -> Result<()> {
        type Check = (&'static str, fn(&CrxHeader) -> bool);
        const CHECKS: [Check; 11] = [
            ("encoding type", |h| matches!(h.enc_type, 0 | 1 | 3)),
            ("wavelet levels", |h| h.levels <= 3),
            ("header version", |h| matches!(h.version, 0x100 | 0x200)),
            ("header version", |h| h.mdat_header_size != 0),
            ("bit depth", |h| (1..=if h.enc_type == 1 { 15 } else { 14 }).contains(&h.bits)),
            ("bit depth", |h| h.enc_type != 3 || (1..=16).contains(&h.median_bits)),
            ("plane layout", |h| matches!(h.planes, 1 | 4)),
            ("single-plane layout", |h| h.planes != 1 || (h.cfa_layout == 0 && h.enc_type == 0 && h.bits == 8)),
            ("plane layout", |h| {
                h.planes != 4
                    || (h.cfa_layout <= 3
                        && h.bits != 8
                        && [h.width, h.height, h.tile_width, h.tile_height].iter().all(|v| v % 2 == 0))
            }),
            ("tile size", |h| h.tile_width <= h.width),
            ("tile size", |h| h.tile_height <= h.height),
        ];
        match CHECKS.iter().find(|(_, holds)| !holds(self)) {
            Some((what, _)) => Err(unsupported(what)),
            None => Ok(()),
        }
    }

    /// `(dx, dy)` of plane `p` inside the 2x2 CFA cell.
    pub fn plane_position(&self, p: usize) -> (usize, usize) {
        let (dx, dy) = PLANE_CELL[(self.cfa_layout & 3) as usize][p & 3];
        (dx as usize, dy as usize)
    }

    /// Maximum sample value of the decoded data.
    pub fn white_level(&self) -> u32 {
        (1u32 << self.bits.min(16)) - 1
    }
}

/// Output precision of encoding type 3: the sample depth unless an extended header names another one.
fn median_bits(d: &[u8]) -> u8 {
    let bits = d[24];
    let extended = d.get(32).is_some_and(|b| b & 0x80 != 0) && d.len() >= 57 && d[25] >> 4 == 4 && d[56] & 0x40 != 0;
    match d.get(84) {
        Some(&m) if extended => m,
        _ => bits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmp1(f: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut d = vec![0u8; 52];
        d[4..6].copy_from_slice(&0x100u16.to_be_bytes());
        d[8..12].copy_from_slice(&128u32.to_be_bytes());
        d[12..16].copy_from_slice(&64u32.to_be_bytes());
        d[16..20].copy_from_slice(&128u32.to_be_bytes());
        d[20..24].copy_from_slice(&64u32.to_be_bytes());
        d[24] = 14;
        d[25] = 0x40;
        d[26] = 0;
        d[28..32].copy_from_slice(&112u32.to_be_bytes());
        f(&mut d);
        d
    }

    fn kind(d: &[u8]) -> &'static str {
        CrxHeader::parse(d).map(|_| "ok").unwrap_or_else(|e| e.kind())
    }

    #[test]
    fn accepts_the_basic_header() {
        let h = CrxHeader::parse(&cmp1(|_| {})).unwrap();
        assert_eq!((h.width, h.height, h.bits, h.planes, h.levels), (128, 64, 14, 4, 0));
        assert_eq!(h.median_bits, 14);
        assert_eq!(h.white_level(), 0x3fff);
    }

    #[test]
    fn rejects_by_class() {
        assert_eq!(kind(&[0u8; 31]), "CorruptFile");
        assert_eq!(kind(&cmp1(|d| d[5] = 0x01)), "UnsupportedCompression"); // version 0x101
        assert_eq!(kind(&cmp1(|d| d[28..32].fill(0))), "UnsupportedCompression");
        assert_eq!(kind(&cmp1(|d| d[24] = 15)), "UnsupportedCompression");
        assert_eq!(kind(&cmp1(|d| d[26] = 0x20)), "UnsupportedCompression");
        assert_eq!(kind(&cmp1(|d| d[26] = 0x04)), "UnsupportedCompression");
        assert_eq!(kind(&cmp1(|d| d[24] = 0)), "UnsupportedCompression");
        assert_eq!(kind(&cmp1(|d| d[19] = 0x81)), "UnsupportedCompression"); // odd tile width
        assert_eq!(kind(&cmp1(|d| d[26] = 0x10)), "ok");
        assert_eq!(
            kind(&cmp1(|d| {
                d[26] = 0x10;
                d[24] = 15;
            })),
            "ok"
        );
    }

    #[test]
    fn single_plane_needs_eight_bits_and_layout_zero() {
        let one = |d: &mut Vec<u8>| {
            d[25] = 0x10;
            d[24] = 8;
        };
        assert_eq!(kind(&cmp1(one)), "ok");
        assert_eq!(
            kind(&cmp1(|d| {
                one(d);
                d[25] = 0x11;
            })),
            "UnsupportedCompression"
        );
        assert_eq!(kind(&cmp1(|d| d[24] = 8)), "UnsupportedCompression");
    }

    #[test]
    fn plane_positions() {
        let mut h = CrxHeader::parse(&cmp1(|_| {})).unwrap();
        assert_eq!([0, 1, 2, 3].map(|p| h.plane_position(p)), [(0, 0), (1, 0), (0, 1), (1, 1)]);
        h.cfa_layout = 1;
        assert_eq!([0, 1, 2, 3].map(|p| h.plane_position(p)), [(1, 0), (0, 0), (1, 1), (0, 1)]);
        h.cfa_layout = 3;
        assert_eq!([0, 1, 2, 3].map(|p| h.plane_position(p)), [(1, 1), (0, 1), (1, 0), (0, 0)]);
    }

    #[test]
    fn median_bits_extension() {
        let d = cmp1(|d| {
            d[26] = 0x30;
            d.resize(90, 0);
            d[32] = 0x80;
            d[56] = 0x40;
            d[84] = 12;
        });
        assert_eq!(CrxHeader::parse(&d).unwrap().median_bits, 12);
        let d = cmp1(|d| {
            d[26] = 0x30;
            d.resize(90, 0);
            d[32] = 0x80;
            d[56] = 0x40;
            d[84] = 17;
        });
        assert_eq!(kind(&d), "UnsupportedCompression");
    }

    impl CrxHeader {
        fn old_validate(&self) -> Result<()> {
            if (self.version != 0x100 && self.version != 0x200) || self.mdat_header_size == 0 {
                return Err(unsupported("header version"));
            }
            let depth_limit = match self.enc_type {
                1 => 15,
                0 | 3 => 14,
                _ => return Err(unsupported("encoding type")),
            };
            if self.bits > depth_limit {
                return Err(unsupported("bit depth"));
            }
            if self.planes == 1 {
                if self.cfa_layout != 0 || self.enc_type != 0 || self.bits != 8 {
                    return Err(unsupported("single-plane layout"));
                }
            } else {
                let even = |v: u32| v & 1 == 0;
                if self.planes != 4
                    || !(even(self.width) && even(self.height) && even(self.tile_width) && even(self.tile_height))
                    || self.cfa_layout > 3
                    || self.bits == 8
                {
                    return Err(unsupported("plane layout"));
                }
            }
            if self.tile_width > self.width || self.tile_height > self.height {
                return Err(unsupported("tile size"));
            }
            if self.levels > 3 {
                return Err(unsupported("wavelet levels"));
            }
            if self.bits == 0 || (self.enc_type == 3 && !(1..=16).contains(&self.median_bits)) {
                return Err(unsupported("bit depth"));
            }
            Ok(())
        }
    }

    #[test]
    fn validation_table_accepts_exactly_what_the_chain_accepted() {
        let mut cases = 0u32;
        for version in [0x100u16, 0x200, 0x101] {
            for mdat in [0u32, 112] {
                for enc_type in 0..8u8 {
                    for bits in 0..18u8 {
                        for planes in 0..6u8 {
                            for cfa_layout in 0..5u8 {
                                for levels in 0..5u8 {
                                    for median_bits in [0u8, 1, 16, 17] {
                                        for geometry in 0..4 {
                                            let (width, height) = (128u32, 64u32);
                                            let (tile_width, tile_height, width, height) = match geometry {
                                                0 => (128, 64, width, height),
                                                1 => (128, 64, width + 1, height),
                                                2 => (130, 64, width, height),
                                                _ => (128, 66, width, height + 2),
                                            };
                                            let h = CrxHeader {
                                                version,
                                                width,
                                                height,
                                                tile_width: if geometry == 3 { 128 } else { tile_width },
                                                tile_height: if geometry == 3 { 65 } else { tile_height },
                                                bits,
                                                planes,
                                                cfa_layout,
                                                enc_type,
                                                levels,
                                                mdat_header_size: mdat,
                                                median_bits,
                                            };
                                            let (a, b) = (h.old_validate(), h.validate());
                                            assert_eq!(a.is_ok(), b.is_ok(), "{h:?}");
                                            if let (Err(x), Err(y)) = (&a, &b) {
                                                assert_eq!(x.kind(), y.kind());
                                            }
                                            cases += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(cases > 1_000_000);
    }
}
