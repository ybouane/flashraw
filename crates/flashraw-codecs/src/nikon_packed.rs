//! Bit-packed rasters described by layout flags (stub: see the module specification).

use crate::packed::{self, PackedLayout};
use flashraw_core::{Error, PackOrder, Result};

/// A flag-described packed raster.
#[derive(Debug, Clone, Copy)]
pub struct PackedSpec {
    pub width: usize,
    pub height: usize,
    /// Bits per sample (1..=16).
    pub bits: u32,
    /// Layout flags.
    pub flags: u32,
    /// Byte offset of the second field.
    pub field2_offset: Option<usize>,
}

/// Numeric flag bits as stored in files and camera tables (boundary only).
const FLAG_PAD10: u32 = 1;
const FLAG_FIELDS: u32 = 2;
const FLAG_FIELD2: u32 = 4;
const FLAG_UNIT_SHIFT: u32 = 3;
const FLAG_UNIT_MASK: u32 = 3;
const FLAG_SWAP: u32 = 64;
const FLAG_EVEN: u32 = 128;

/// Samples between two guard bytes when guard bytes are present.
const GUARD_GROUP: usize = 10;
/// A guard byte is stored after every `GUARD_GROUP` samples; one stored byte in every 15
/// of a row is such a byte.
const GUARD_SPAN_BYTES: usize = 15;

/// Order in which stored rows land in the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interleave {
    /// Stored row `i` is image row `i`.
    Sequential,
    /// The stored rows are two fields: the first half fills the even image rows, the rest the
    /// odd ones. `restart` makes the second field begin at a separate byte offset.
    Fields { restart: bool },
}

/// Extra bytes inside a stored row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Padding {
    None,
    /// One unused byte after each full group of [`GUARD_GROUP`] samples.
    GuardBytes,
}

/// Typed form of the layout flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    interleave: Interleave,
    padding: Padding,
    /// Gathering unit in bytes (1..=4).
    unit_bytes: usize,
    /// Neighbouring samples are exchanged in pairs.
    swap_pairs: bool,
    /// Stored rows are rounded up to an even byte count.
    even_rows: bool,
    /// The "second field" flag as given (consulted by the size computation even without
    /// interleaving).
    second_field_flag: bool,
}

impl Layout {
    fn from_flags(flags: u32) -> Self {
        let second_field_flag = flags & FLAG_FIELD2 != 0;
        Layout {
            interleave: if flags & FLAG_FIELDS != 0 {
                Interleave::Fields { restart: second_field_flag }
            } else {
                Interleave::Sequential
            },
            padding: if flags & FLAG_PAD10 != 0 { Padding::GuardBytes } else { Padding::None },
            unit_bytes: 1 + ((flags >> FLAG_UNIT_SHIFT) & FLAG_UNIT_MASK) as usize,
            swap_pairs: flags & FLAG_SWAP != 0,
            even_rows: flags & FLAG_EVEN != 0,
            second_field_flag,
        }
    }

    /// No padding, no field handling: rows are consecutive.
    fn is_plain(&self) -> bool {
        self.padding == Padding::None && self.interleave == Interleave::Sequential && !self.second_field_flag
    }

    /// Image row that stored row `stored` of `height` rows belongs to.
    fn image_row(&self, stored: usize, height: usize) -> usize {
        match self.interleave {
            Interleave::Sequential => stored,
            Interleave::Fields { .. } => {
                let first_field_rows = height.div_ceil(2);
                if stored < first_field_rows { 2 * stored } else { 2 * (stored - first_field_rows) + 1 }
            }
        }
    }

    /// Whether the stream restarts at the second-field offset before this image row.
    fn restarts_at(&self, image_row: usize) -> bool {
        matches!(self.interleave, Interleave::Fields { restart: true }) && image_row == 1
    }

    /// Stored bytes of a row of `nominal` payload bytes.
    fn stored_row_bytes(&self, nominal: usize) -> usize {
        match self.padding {
            Padding::None => nominal,
            Padding::GuardBytes => nominal + nominal / GUARD_SPAN_BYTES,
        }
    }
}

impl PackedSpec {
    fn layout(&self) -> Layout {
        Layout::from_flags(self.flags)
    }

    /// Payload bits of one row.
    fn row_bits(&self) -> usize {
        self.width * self.bits as usize
    }

    /// Nominal bytes of a row before the guard-byte expansion.
    fn nominal_bytes(&self) -> usize {
        let n = self.row_bits() / 8;
        if self.layout().even_rows { n + (n & 1) } else { n }
    }

    /// Bits that follow the payload inside the nominal row (negative: rows share a byte).
    fn row_gap_bits(&self) -> i32 {
        (8 * self.nominal_bytes()) as i32 - self.row_bits() as i32
    }

    /// Nominal bytes per row in the file.
    pub fn row_bytes(&self) -> usize {
        self.layout().stored_row_bytes(self.nominal_bytes())
    }

    /// The equivalent plain packed layout, when one exists.
    pub fn fast_layout(&self) -> Option<PackedLayout> {
        let lay = self.layout();
        if !lay.is_plain() {
            return None;
        }
        let gap = self.row_gap_bits();
        if gap < 0 {
            return None;
        }
        let row_bits = self.row_bits();
        let order = match (lay.unit_bytes, lay.swap_pairs) {
            (1, false) => PackOrder::MsbBytes,
            (3, true) if self.bits == 12 => PackOrder::LsbBytes,
            (2, false) if self.bits == 16 || row_bits % 16 == 0 => PackOrder::Msb16,
            (4, false) if row_bits % 32 == 0 => PackOrder::Msb32,
            _ => return None,
        };
        if gap % 8 != 0 || (matches!(order, PackOrder::Msb16 | PackOrder::Msb32) && gap != 0) {
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
        let rb = self.row_bytes();
        if self.layout().second_field_flag {
            self.field2_offset.unwrap_or(0) + (self.height - self.height.div_ceil(2)) * rb
        } else {
            self.height * rb
        }
    }
}

/// Decode `src` into `out` (rows of `stride` samples).
pub fn decode(
    src: &[u8],
    spec: &PackedSpec,
    out: &mut [u16],
    stride: usize,
    parallel: bool,
    simd: bool,
    lenient: bool,
) -> Result<()> {
    check_bits(spec)?;
    if stride < spec.width || out.len() < stride * spec.height {
        return Err(Error::InvalidArgument("packed: output too small".into()));
    }
    if let Some(layout) = spec.fast_layout() {
        return packed::unpack(src, layout, out, stride, parallel, simd, lenient);
    }
    run(src, spec, out, stride, lenient)
}

/// Exact sequential strict decode of any layout.
pub fn decode_exact(src: &[u8], spec: &PackedSpec, out: &mut [u16], stride: usize) -> Result<()> {
    check_bits(spec)?;
    run(src, spec, out, stride, false)
}

fn check_bits(spec: &PackedSpec) -> Result<()> {
    if spec.bits == 0 || spec.bits > 16 || spec.width == 0 {
        return Err(Error::corrupt(format!("unsupported packed layout ({} bits)", spec.bits)));
    }
    Ok(())
}

/// Bit reader over little-endian units. `buf` holds the most recent bits in its low
/// `avail` bits (`avail` may go negative while a pending skip is outstanding).
struct Reader<'a> {
    src: &'a [u8],
    pos: usize,
    unit: usize,
    buf: u64,
    avail: i32,
    dry: bool,
}

impl Reader<'_> {
    #[inline(always)]
    fn fetch(&mut self) {
        let end = self.pos + self.unit;
        let mut v = 0u64;
        if end <= self.src.len() {
            for (k, &b) in self.src[self.pos..end].iter().enumerate() {
                v |= (b as u64) << (8 * k);
            }
        } else {
            self.dry = true;
            for k in 0..self.unit {
                v |= (self.src.get(self.pos + k).copied().unwrap_or(0) as u64) << (8 * k);
            }
        }
        self.pos = end;
        self.buf = (self.buf << (8 * self.unit)) | v;
        self.avail += 8 * self.unit as i32;
    }

    /// Decode a whole row by absolute bit addressing when every byte it can touch is inside
    /// the source (so nothing can run dry) and the layout keeps the pad bytes byte-aligned.
    /// Leaves the reader exactly where the incremental algorithm would be, and returns false
    /// (having changed nothing) when the row needs the general loop.
    fn try_direct_row(&mut self, row: &mut [u16], bits: u32, pad: bool, flip: usize) -> bool {
        let w = row.len();
        let start = 8 * self.pos as i64 - self.avail as i64;
        if start < 0 || (pad && (start % 8 != 0 || (GUARD_GROUP as u32 * bits) % 8 != 0)) {
            return false;
        }
        let guard_bytes = if pad { w / GUARD_GROUP } else { 0 };
        let end = start as usize + w * bits as usize + 8 * guard_bytes;
        if end.div_ceil(8) + 4 > self.src.len() {
            return false;
        }
        let src = self.src;
        let mut bit = start as usize;
        let shift = 32 - bits;
        let mut put = |col: usize, bit: usize| {
            let byte = bit >> 3;
            let word = u32::from_be_bytes([src[byte], src[byte + 1], src[byte + 2], src[byte + 3]]);
            row[col ^ flip] = ((word << (bit & 7)) >> shift) as u16;
        };
        let full_groups = if pad { w / GUARD_GROUP } else { 0 };
        let mut col = 0;
        for _ in 0..full_groups {
            for k in 0..GUARD_GROUP {
                put(col + k, bit);
                bit += bits as usize;
            }
            col += GUARD_GROUP;
            bit += 8;
        }
        while col < w {
            put(col, bit);
            bit += bits as usize;
            col += 1;
        }
        debug_assert_eq!(bit, end);
        self.pos = bit.div_ceil(8);
        self.avail = (8 * self.pos - bit) as i32;
        let lo = self.pos.saturating_sub(8);
        self.buf = src[lo..self.pos].iter().fold(0u64, |a, &b| (a << 8) | b as u64);
        true
    }

    #[inline(always)]
    fn take(&mut self, bits: u32) -> u16 {
        while self.avail < bits as i32 {
            self.fetch();
        }
        self.avail -= bits as i32;
        ((self.buf >> self.avail) & ((1u64 << bits) - 1)) as u16
    }
}

/// The sequential algorithm for every layout.
fn run(src: &[u8], spec: &PackedSpec, out: &mut [u16], stride: usize, lenient: bool) -> Result<()> {
    let (w, h, bits) = (spec.width, spec.height, spec.bits);
    let lay = spec.layout();
    if lay.swap_pairs && w % 2 != 0 {
        return Err(Error::corrupt("packed: odd width with column swap"));
    }
    let pad = lay.padding == Padding::GuardBytes;
    let gap = spec.row_gap_bits();
    let mut rd = Reader { src, pos: 0, unit: lay.unit_bytes, buf: 0, avail: 0, dry: false };

    for i in 0..h {
        let orow = lay.image_row(i, h);
        if lay.restarts_at(orow) {
            let off = spec.field2_offset.ok_or_else(|| Error::corrupt("missing second field offset"))?;
            rd.pos = off;
            rd.buf = 0;
            rd.avail = 0;
        }
        if !lenient && rd.dry {
            return Err(Error::corrupt(format!("truncated packed data at row {i}/{h}")));
        }
        let row =
            out.get_mut(orow * stride..orow * stride + w).ok_or_else(|| Error::corrupt("packed row out of range"))?;
        let flip = usize::from(lay.swap_pairs);
        if rd.unit == 1 && rd.try_direct_row(row, bits, pad, flip) {
            rd.avail -= gap;
            continue;
        }
        if !pad && !lenient {
            for col in 0..w {
                row[col ^ flip] = rd.take(bits);
            }
        } else {
            let mut since_guard = 0;
            for col in 0..w {
                if lenient {
                    while rd.avail < bits as i32 {
                        rd.fetch();
                        if rd.dry {
                            for c in col..w {
                                row[c ^ flip] = 0;
                            }
                            return Ok(());
                        }
                    }
                }
                row[col ^ flip] = rd.take(bits);
                since_guard += 1;
                if pad && since_guard == GUARD_GROUP {
                    since_guard = 0;
                    rd.pos += 1;
                }
            }
        }
        rd.avail -= gap;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn pseudo(len: usize, seed: u64) -> Vec<u8> {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..len).map(|_| rng(&mut s) as u8).collect()
    }

    fn spec(width: usize, height: usize, bits: u32, flags: u32) -> PackedSpec {
        PackedSpec { width, height, bits, flags, field2_offset: None }
    }

    #[test]
    fn fast_layouts_match_exact_emulation() {
        let cases = [
            (12, 0, 40, true),
            (12, 80, 40, true),
            (12, 24, 40, true),
            (14, 0, 64, true),
            (12, 128, 42, true),
            (12, 128, 41, false),
            (16, 0, 10, true),
            (12, 16, 40, false),
            (12, 0, 41, false),
        ];
        for (bits, flags, width, fast) in cases {
            let sp = spec(width, 7, bits, flags);
            let src = pseudo(sp.required_bytes() + 8, bits as u64 * 31 + flags as u64);
            let mut a = vec![0u16; width * 7];
            let mut b = vec![0u16; width * 7];
            decode_exact(&src, &sp, &mut a, width).unwrap();
            decode(&src, &sp, &mut b, width, false, true, false).unwrap();
            assert_eq!(a, b, "bits {bits} flags {flags} width {width}");
            assert_eq!(sp.fast_layout().is_some(), fast, "bits {bits} flags {flags} width {width}");
        }
    }

    #[test]
    fn unit24_swap_is_lsb_packing() {
        let mut out = [0u16; 2];
        decode_exact(&[0xBC, 0x3A, 0x12], &spec(2, 1, 12, 80), &mut out, 2).unwrap();
        assert_eq!(out, [0xABC, 0x123]);
    }

    #[test]
    fn pad_byte_every_ten_samples() {
        let sp = spec(20, 1, 12, 1);
        let mut src = vec![0u8; 32];
        src[0] = 0xFF;
        src[1] = 0xF0;
        src[15] = 0xAA;
        src[16] = 0x80;
        let mut out = [0u16; 20];
        decode_exact(&src, &sp, &mut out, 20).unwrap();
        assert_eq!((out[0], out[10]), (0xFFF, 0x800));
        assert_eq!(sp.row_bytes(), 32);
    }

    #[test]
    fn fields_are_interleaved() {
        let src: Vec<u8> = (0..8).collect();
        let mut out = [0u16; 8];
        decode_exact(&src, &spec(2, 4, 8, 2), &mut out, 2).unwrap();
        assert_eq!(out, [0, 1, 4, 5, 2, 3, 6, 7]);
    }

    #[test]
    fn missing_second_field_offset_fails() {
        let mut out = [0u16; 8];
        let e = decode(&[0; 8], &spec(2, 4, 8, 6), &mut out, 2, false, false, false).unwrap_err();
        assert!(e.to_string().contains("missing second field offset"));
        let mut sp = spec(2, 4, 8, 6);
        sp.field2_offset = Some(2);
        decode_exact(&(0..8).collect::<Vec<u8>>(), &sp, &mut out, 2).unwrap();
        assert_eq!(out, [0, 1, 2, 3, 2, 3, 4, 5]);
    }

    #[test]
    fn truncation_is_an_error() {
        for flags in [1, 0] {
            let mut out = vec![0u16; 16 * 8];
            let r = decode(&[0u8; 40], &spec(16, 8, 12, flags), &mut out, 16, false, false, false);
            assert!(r.is_err(), "flags {flags}");
        }
    }

    #[test]
    fn lenient_keeps_complete_samples() {
        let sp = spec(16, 4, 12, 1);
        let src = pseudo(sp.required_bytes(), 7);
        let mut full = vec![0u16; 64];
        decode_exact(&src, &sp, &mut full, 16).unwrap();
        let mut part = vec![0u16; 64];
        decode(&src[..40], &sp, &mut part, 16, false, false, true).unwrap();
        assert_eq!(part[..16], full[..16]);
        let kept = (16..32).take_while(|&i| part[i] == full[i]).count();
        assert!(part[16 + kept..32].iter().all(|&v| v == 0) || kept == 16);
        assert!(part[32..].iter().all(|&v| v == 0));
    }

    #[test]
    fn invalid_arguments() {
        let mut out = [0u16; 4];
        assert!(decode(&[], &spec(2, 2, 0, 0), &mut out, 2, false, false, false).is_err());
        assert!(decode(&[], &spec(2, 2, 17, 0), &mut out, 2, false, false, false).is_err());
        assert!(decode(&[], &spec(0, 2, 8, 0), &mut out, 2, false, false, false).is_err());
        let e = decode(&[0; 8], &spec(2, 3, 8, 0), &mut out, 2, false, false, false).unwrap_err();
        assert!(matches!(e, Error::InvalidArgument(_)));
    }

    /// Straightforward bit-by-bit model of the plain 8-bit-unit layout (no flags).
    #[test]
    fn plain_layout_matches_bitwise_model() {
        let mut s = 0x1234_5678_9ABC_DEF1u64;
        for _ in 0..300 {
            let bits = 1 + (rng(&mut s) % 16) as u32;
            let width = 1 + (rng(&mut s) % 40) as usize;
            let height = 1 + (rng(&mut s) % 5) as usize;
            let sp = spec(width, height, bits, 0);
            let src = pseudo(sp.required_bytes() + 8, rng(&mut s));
            let mut out = vec![0u16; width * height];
            decode_exact(&src, &sp, &mut out, width).unwrap();
            let rowbits = width * bits as usize;
            let rb = rowbits / 8;
            for r in 0..height {
                for c in 0..width {
                    let mut v = 0u16;
                    for k in 0..bits as usize {
                        // Rows start at the byte-rounded-down boundary plus the shared bits.
                        let bit = r * 8 * rb + c * bits as usize + k;
                        v = v << 1 | ((src[bit / 8] >> (7 - bit % 8)) & 1) as u16;
                    }
                    assert_eq!(out[r * width + c], v, "bits {bits} w {width} r {r} c {c}");
                }
            }
        }
    }

    /// Hash of every result of a seeded random sweep over all flag combinations, recorded
    /// when the module was checked against its predecessor.
    #[test]
    fn random_sweep_golden_hash() {
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut mix = |v: u64| h = (h ^ v).wrapping_mul(0x100_0000_01b3);
        for _ in 0..20_000 {
            let bits = 1 + (rng(&mut s) % 16) as u32;
            let mut flags = 0u32;
            for bit in [1u32, 2, 4, 8, 16, 64, 128] {
                if rng(&mut s) % 3 == 0 {
                    flags |= bit;
                }
            }
            let mut width = 1 + (rng(&mut s) % 70) as usize;
            if flags & 64 != 0 {
                width = width.max(2) & !1;
            }
            let height = 1 + (rng(&mut s) % 12) as usize;
            let field2_offset = match rng(&mut s) % 3 {
                0 => None,
                1 => Some((rng(&mut s) % 200) as usize),
                _ => Some((rng(&mut s) % 4000) as usize),
            };
            let spec = PackedSpec { width, height, bits, flags, field2_offset };
            let len = (rng(&mut s) % (spec.required_bytes() as u64 + 17)) as usize;
            let src: Vec<u8> = (0..len).map(|_| rng(&mut s) as u8).collect();
            let stride = width + (rng(&mut s) % 3) as usize;
            let lenient = rng(&mut s) % 2 == 0;
            let fill = if lenient { 0 } else { 0xFFFF };
            let (parallel, simd) = (rng(&mut s) % 2 == 0, rng(&mut s) % 2 == 0);
            let mut a = vec![fill; stride * height];
            let ra = decode(&src, &spec, &mut a, stride, parallel, simd, lenient);
            a.iter().for_each(|&v| mix(v as u64));
            mix(ra.is_ok() as u64);
        }
        assert_eq!(h, 0x6594a3f589749bc4);
    }
}
