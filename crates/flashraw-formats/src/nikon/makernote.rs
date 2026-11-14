//! Nikon maker note (EXIF tag 0x927C): the values the reader needs.
//!
//! Layout of the note. Cameras from the D100 / Coolpix 5000 era on write `"Nikon\0"`, a
//! version byte (2) and two unused bytes, then an embedded TIFF header (`II` or `MM`, 42,
//! IFD offset); every offset inside the note is relative to that header. The D1 family
//! writes a plain IFD without header, with offsets relative to the file.
//!
//! Several blocks are obscured by an XOR keystream keyed by the serial number (tag 0x1D)
//! and the shutter count (tag 0xA7); the keystream itself is in [`super::crypt`]. This
//! module decides which blocks to decrypt and where the white-balance multipliers sit in
//! each version of the colour-balance block.

use super::crypt;
use flashraw_core::bytes::ByteOrder;
use flashraw_core::tiff::{Entry, Ifd, TiffOptions, parse_ifd_at, types};

/// Values extracted from a Nikon makernote.
#[derive(Debug, Clone, Default)]
pub struct MakerNote {
    pub order: Option<ByteOrder>,
    pub iso: Option<u32>,
    /// As-shot multipliers in R, G, B, G2 order (before any camera-specific
    /// rewriting); zero when unknown.
    pub cam_mul: [f32; 4],
    /// Red and blue levels of tag 0x000C as stored (relative to green).
    pub rb_levels: Option<[f32; 2]>,
    /// Black level from tag 0x003D in R, G1, G2, B order.
    pub black: Option<[u16; 4]>,
    /// Black level from the Coolpix NRW block (tag 0x0014).
    pub nrw_black: Option<u32>,
    pub nef_compression: Option<u16>,
    /// Absolute file offset of the Huffman/linearisation metadata (0x0096, else 0x008C).
    pub lin_offset: Option<usize>,
    /// Colour balance (0x0097) version, e.g. 219 for "0219".
    pub color_balance_version: u32,
    /// Embedded previews: absolute (offset, length) pairs.
    pub previews: Vec<(usize, usize)>,
    /// Lens min/max focal length and max aperture at those (0x0084).
    pub lens_spec: Option<[f32; 4]>,
    pub serial: Option<String>,
    /// Image rotation in degrees (clockwise) saved by Nikon's own software in the edit
    /// record (tag 0x0E01).
    pub edit_rotation: Option<u16>,
    /// The camera's image area inside the sensor buffer (tag 0x0045): x, y, width, height.
    /// Reduced-size Bayer files (RAW M/S) store more photosites than the camera's M/S
    /// image has pixels and record the area it was scaled from here.
    pub crop_area: Option<[u16; 4]>,
}

// ---------------------------------------------------------------------------------------------
// Colour-balance layouts (tag 0x0097)
// ---------------------------------------------------------------------------------------------

/// Order in which the four stored multipliers `x0..x3` appear; [`Stored::to_cam_mul`]
/// puts them into `cam_mul = [R, G1, B, G2]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stored {
    /// R, B, G, G
    Rbgg,
    /// R, G, G, B
    Rggb,
    /// R, G, B, G
    Rgbg,
    /// G, R, B, G
    Grbg,
}

impl Stored {
    fn to_cam_mul(self, x: [u16; 4]) -> [f32; 4] {
        let f = |i: usize| f32::from(x[i]);
        match self {
            Stored::Rbgg => [f(0), f(2), f(1), f(3)],
            Stored::Rggb => [f(0), f(1), f(3), f(2)],
            Stored::Rgbg => [f(0), f(1), f(2), f(3)],
            Stored::Grbg => [f(1), f(0), f(2), f(3)],
        }
    }
}

/// A colour-balance version whose multipliers are stored in clear, at a fixed offset of
/// the tag value.
struct PlainLayout {
    version: u32,
    offset: usize,
    stored: Stored,
}

/// All three are verified: a corpus sample reproduces the camera's own white balance.
const PLAIN_LAYOUTS: [PlainLayout; 3] = [
    // D100
    PlainLayout { version: 100, offset: 0x48, stored: Stored::Rbgg },
    // D2H
    PlainLayout { version: 102, offset: 0x0A, stored: Stored::Rggb },
    // D70, D70s
    PlainLayout { version: 103, offset: 0x14, stored: Stored::Rgbg },
];

/// An encrypted colour-balance version: the 324-byte block starts `start` bytes after the
/// value position, the multipliers sit at `offset` of the decrypted block.
struct CipherLayout {
    version: u32,
    start: usize,
    offset: usize,
    stored: Stored,
}

/// Bytes of the encrypted colour-balance block.
const CIPHER_BLOCK: usize = 324;
/// Start of the block for every version but the D50's (205).
const CIPHER_START: usize = 0x11C;

/// Versions 200..=217. 200 to 203 have no corpus sample (unverified); every other
/// version is verified against tag 0x000C of at least one file (205: the two greens
/// decrypt to the same value).
const CIPHER_LAYOUTS: [CipherLayout; 18] = {
    use Stored::{Grbg, Rggb};
    const fn l(version: u32, start: usize, offset: usize, stored: Stored) -> CipherLayout {
        CipherLayout { version, start, offset, stored }
    }
    [
        l(200, CIPHER_START, 6, Rggb),
        l(201, CIPHER_START, 6, Rggb),
        l(202, CIPHER_START, 6, Rggb),
        l(203, CIPHER_START, 6, Rggb),
        l(204, CIPHER_START, 6, Rggb),
        l(205, 4, 14, Rggb),
        l(206, CIPHER_START, 6, Rggb),
        l(207, CIPHER_START, 6, Rggb),
        l(208, CIPHER_START, 6, Rggb),
        l(209, CIPHER_START, 10, Grbg),
        l(210, CIPHER_START, 6, Rggb),
        l(211, CIPHER_START, 16, Grbg),
        l(212, CIPHER_START, 10, Grbg),
        l(213, CIPHER_START, 10, Rggb),
        l(214, CIPHER_START, 10, Grbg),
        l(215, CIPHER_START, 4, Grbg),
        l(216, CIPHER_START, 4, Grbg),
        l(217, CIPHER_START, 4, Grbg),
    ]
};

fn u16_at(bytes: &[u8], at: usize, order: ByteOrder) -> Option<u16> {
    let b = bytes.get(at..at.checked_add(2)?)?;
    let p = [b[0], b[1]];
    Some(match order {
        ByteOrder::Little => u16::from_le_bytes(p),
        ByteOrder::Big => u16::from_be_bytes(p),
    })
}

fn u32_at(bytes: &[u8], at: usize, order: ByteOrder) -> Option<u32> {
    let b = bytes.get(at..at.checked_add(4)?)?;
    let p = [b[0], b[1], b[2], b[3]];
    Some(match order {
        ByteOrder::Little => u32::from_le_bytes(p),
        ByteOrder::Big => u32::from_be_bytes(p),
    })
}

fn four_u16(bytes: &[u8], at: usize, order: ByteOrder) -> Option<[u16; 4]> {
    Some([
        u16_at(bytes, at, order)?,
        u16_at(bytes, at + 2, order)?,
        u16_at(bytes, at + 4, order)?,
        u16_at(bytes, at + 6, order)?,
    ])
}

/// The version of a colour-balance block: its first four bytes read as decimal digits
/// (a byte that is not a digit still contributes its distance from `'0'`).
fn balance_version(value: &[u8]) -> u32 {
    value.iter().take(4).fold(0i32, |v, &c| v.wrapping_mul(10).wrapping_add(i32::from(c) - 48)) as u32
}

// ---------------------------------------------------------------------------------------------
// Coolpix colour block (tag 0x0014)
// ---------------------------------------------------------------------------------------------

/// Block sizes (the entry's count) of the early Coolpix cameras.
const COOLPIX_BLOCK_E5400: u32 = 2560;
const COOLPIX_BLOCK_E5700: u32 = 1280;
/// Positions of the red and blue words of the 2560-byte block (both big-endian, scaled by 256).
const E5400_RED_AT: usize = 0x4E0;
const E5400_BLUE_AT: usize = 0x4E2;
/// NRW block 0100 (P6000): the four multipliers.
const NRW_0100_MULS_AT: usize = 10 + 0x13DE;
/// Later NRW blocks: the black level, and the four multipliers behind it.
const NRW_BLACK_AT: usize = 10 + 0x16;
const NRW_MULS_AT: usize = NRW_BLACK_AT + 2 + 0x16;

/// `[2r, g, 2b, g2]` from the four stored words (r, g, g2, b) at `at`.
fn nrw_multipliers(block: &[u8], at: usize, order: ByteOrder) -> Option<[f32; 4]> {
    let w = |i: usize| u32_at(block, at + 4 * i, order);
    let (r, g, g2, b) = (w(0)?, w(1)?, w(2)?, w(3)?);
    Some([r as f32 * 2.0, g as f32, b as f32 * 2.0, g2 as f32])
}

fn coolpix_block(mn: &mut MakerNote, e: &Entry, order: ByteOrder) {
    if e.typ != types::UNDEFINED {
        return;
    }
    let block = e.data;
    match e.count {
        COOLPIX_BLOCK_E5400 => {
            let be = |at| u16_at(block, at, ByteOrder::Big);
            if let (Some(r), Some(b)) = (be(E5400_RED_AT), be(E5400_BLUE_AT)) {
                mn.cam_mul = [f32::from(r) / 256.0, 1.0, f32::from(b) / 256.0, 1.0];
            }
        }
        COOLPIX_BLOCK_E5700 => mn.cam_mul = [1.0; 4],
        _ => {
            if !block.starts_with(b"NRW ") {
                return;
            }
            if block.get(4..8) == Some(b"0100") {
                if let Some(m) = nrw_multipliers(block, NRW_0100_MULS_AT, order) {
                    mn.cam_mul = m;
                }
                return;
            }
            mn.nrw_black = u16_at(block, NRW_BLACK_AT, order).map(u32::from);
            if mn.cam_mul[0] < 0.1 {
                if let Some(m) = nrw_multipliers(block, NRW_MULS_AT, order) {
                    mn.cam_mul = m;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Nikon Capture edit data (tag 0x0E01)
// ---------------------------------------------------------------------------------------------

/// Bytes before the first record.
const EDIT_HEADER: usize = 22;
/// Fixed part of a record: id, 14 bytes of other data, size.
const EDIT_RECORD_HEAD: usize = 22;
/// Record holding the rotation in degrees clockwise.
const EDIT_ROTATION_ID: u32 = 0x76A4_3207;

/// Rotation from the first rotation record of the edit data, if any. A record's size
/// counts its own size field, so its payload has `size - 4` bytes.
fn edit_rotation(value: &[u8]) -> Option<u16> {
    let mut pos = EDIT_HEADER;
    while pos.checked_add(EDIT_RECORD_HEAD)? <= value.len() {
        let id = u32_at(value, pos, ByteOrder::Little)?;
        let size = u32_at(value, pos + 18, ByteOrder::Little)? as usize;
        if size < 4 {
            return None;
        }
        let payload = pos + EDIT_RECORD_HEAD;
        if id == EDIT_ROTATION_ID {
            return u16_at(value, payload, ByteOrder::Little);
        }
        pos = payload.checked_add(size - 4)?;
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------------

/// Where the note's IFD is: the base all offsets are relative to, the byte order, the
/// IFD offset from that base.
fn locate(data: &[u8], pos: usize, file_order: ByteOrder) -> Option<(usize, ByteOrder, u32)> {
    let head = data.get(pos..pos.checked_add(10)?)?;
    if &head[..6] != b"Nikon\0" {
        // The D1 family: a plain IFD in the file's order, offsets from the file start.
        return Some((0, file_order, u32::try_from(pos).ok()?));
    }
    if head[6] != 2 {
        return None;
    }
    let base = pos + 10;
    let order = match data.get(base..base + 2)? {
        b"II" => ByteOrder::Little,
        b"MM" => ByteOrder::Big,
        _ => return None,
    };
    Some((base, order, u32_at(data, base + 4, order)?))
}

/// A preview IFD pointed to by tag 0x0011: absolute (offset, length) of its JPEG.
fn preview_of(ifd: &Ifd, base: usize) -> Option<(usize, usize)> {
    let off = ifd.u32(0x0201)? as usize;
    let len = ifd.u32(0x0202)? as usize;
    Some((base.checked_add(off)?, len))
}

fn four_shorts(e: &Entry) -> Option<[u16; 4]> {
    Some([e.u16(0)?, e.u16(1)?, e.u16(2)?, e.u16(3)?])
}

/// Parse the maker note stored in `entry` (an entry of the file's EXIF IFD, parsed from `data` with
/// absolute offsets). `model` is the EXIF model without the make prefix. Never fails: whatever cannot
/// be read stays at its default.
pub fn parse(data: &[u8], entry: &Entry, file_order: ByteOrder, model: &str) -> MakerNote {
    let mut mn = MakerNote::default();
    let Some((base, order, ifd_offset)) = locate(data, entry.value_pos, file_order) else { return mn };
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    let note = &data[base..];
    let Ok(ifd) = parse_ifd_at(note, order, ifd_offset, opts) else { return mn };
    mn.order = Some(order);

    let mut serial_bytes: &[u8] = &[];
    let mut shutter: Option<[u8; 4]> = None;
    let mut cipher: Option<(&CipherLayout, usize)> = None;
    let (mut lin_8c, mut lin_96) = (None, None);

    for e in &ifd.entries {
        match e.tag {
            0x0002 if mn.iso.is_none() => mn.iso = e.u32(1).filter(|&v| v > 0),
            0x000C if e.typ == types::RATIONAL && e.count == 4 => {
                let v: Vec<f32> = (0..4).map(|i| e.f32(i).unwrap_or(0.0)).collect();
                mn.cam_mul = [v[0], v[2], v[1], v[3]];
                mn.rb_levels = Some([v[0], v[1]]);
            }
            0x0011 => {
                let preview_ifd = e.u32(0).and_then(|o| parse_ifd_at(note, order, o, opts).ok());
                if let Some(p) = preview_ifd.as_ref().and_then(|i| preview_of(i, base)) {
                    mn.previews = vec![p];
                }
            }
            0x0014 => coolpix_block(&mut mn, e, order),
            0x001D => {
                serial_bytes = e.data;
                let s = e.string();
                mn.serial = (!s.is_empty()).then_some(s);
            }
            0x003D => mn.black = four_shorts(e),
            0x0045 => mn.crop_area = four_shorts(e),
            0x0084 => {
                let v = e.f32s();
                if v.len() >= 4 && v[0] > 0.0 {
                    mn.lens_spec = Some([v[0], v[1], v[2], v[3]]);
                }
            }
            0x0093 => mn.nef_compression = e.u16(0),
            0x008C => lin_8c = Some(base + e.value_pos),
            0x0096 => lin_96 = Some(base + e.value_pos),
            0x0097 => {
                let version = balance_version(e.data);
                mn.color_balance_version = version;
                cipher = None;
                if let Some(p) = PLAIN_LAYOUTS.iter().find(|p| p.version == version) {
                    if let Some(x) = four_u16(e.data, p.offset, order) {
                        mn.cam_mul = p.stored.to_cam_mul(x);
                    }
                } else if let Some(c) = CIPHER_LAYOUTS.iter().find(|c| c.version == version) {
                    cipher = Some((c, e.value_pos));
                }
            }
            0x00A7 => shutter = e.data.get(..4).and_then(|b| <[u8; 4]>::try_from(b).ok()),
            0x0E01 => mn.edit_rotation = edit_rotation(e.data),
            _ => {}
        }
    }
    mn.lin_offset = lin_96.or(lin_8c);

    // The encrypted colour balance needs the serial number and the shutter count, which
    // may stand anywhere in the IFD: it is decrypted last and wins over every other source.
    if let (Some((layout, value_pos)), Some(count)) = (cipher, shutter) {
        let from = value_pos.saturating_add(layout.start);
        let block = from.checked_add(CIPHER_BLOCK).and_then(|end| note.get(from..end));
        if let Some(block) = block {
            let mut block = block.to_vec();
            crypt::Key::new(crypt::serial_index(serial_bytes, model), count).decrypt(&mut block);
            if let Some(x) = four_u16(&block, layout.offset, order) {
                mn.cam_mul = layout.stored.to_cam_mul(x);
            }
        }
    }
    mn
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashraw_core::tiff::Tiff;

    /// Builds a little-endian type-2 maker note inside a minimal TIFF whose EXIF IFD
    /// holds it.
    struct Note {
        entries: Vec<(u16, u16, u32, Vec<u8>)>,
    }

    impl Note {
        fn new() -> Self {
            Note { entries: Vec::new() }
        }
        fn add(mut self, tag: u16, typ: u16, count: u32, v: Vec<u8>) -> Self {
            self.entries.push((tag, typ, count, v));
            self
        }
        fn shorts(self, tag: u16, v: &[u16]) -> Self {
            let bytes = v.iter().flat_map(|x| x.to_le_bytes()).collect();
            self.add(tag, 3, v.len() as u32, bytes)
        }
        fn undefined(self, tag: u16, v: &[u8]) -> Self {
            self.add(tag, 7, v.len() as u32, v.to_vec())
        }
        fn rationals(self, tag: u16, v: &[(u32, u32)]) -> Self {
            let bytes = v.iter().flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes())).collect();
            self.add(tag, 5, v.len() as u32, bytes)
        }

        /// The note: "Nikon\0", version 2, an embedded TIFF header, the IFD, the values.
        fn note_bytes(&self) -> Vec<u8> {
            let mut n = b"Nikon\0\x02\x10\0\0II*\0\x08\0\0\0".to_vec();
            let mut values_at = 8 + 2 + self.entries.len() * 12 + 4;
            let mut ifd = (self.entries.len() as u16).to_le_bytes().to_vec();
            let mut area = Vec::<u8>::new();
            for (tag, typ, count, v) in &self.entries {
                ifd.extend(tag.to_le_bytes());
                ifd.extend(typ.to_le_bytes());
                ifd.extend(count.to_le_bytes());
                if v.len() <= 4 {
                    let mut f = v.clone();
                    f.resize(4, 0);
                    ifd.extend(f);
                } else {
                    ifd.extend((values_at as u32).to_le_bytes());
                    area.extend(v.iter().copied());
                    values_at += v.len();
                }
            }
            ifd.extend([0u8; 4]);
            n.extend(ifd);
            n.extend(area);
            n
        }

        /// A file whose EXIF maker note entry holds the note (at offset 44).
        fn file(&self) -> Vec<u8> {
            let note = self.note_bytes();
            let mut f = b"II*\0\x08\0\0\0".to_vec();
            f.extend(1u16.to_le_bytes());
            f.extend([0x69, 0x87, 4, 0, 1, 0, 0, 0]);
            f.extend(26u32.to_le_bytes());
            f.extend(0u32.to_le_bytes());
            assert_eq!(f.len(), 26);
            f.extend(1u16.to_le_bytes());
            f.extend(0x927Cu16.to_le_bytes());
            f.extend(7u16.to_le_bytes());
            f.extend((note.len() as u32).to_le_bytes());
            f.extend(44u32.to_le_bytes());
            f.extend(0u32.to_le_bytes());
            assert_eq!(f.len(), 44);
            f.extend(note);
            f
        }
    }

    fn parse_file(file: &[u8], model: &str) -> MakerNote {
        let tiff = Tiff::parse(file).unwrap();
        let e = tiff.find_entry(0x927C).expect("maker note");
        parse(file, e, tiff.order, model)
    }

    #[test]
    fn plain_values() {
        let file = Note::new()
            .shorts(0x0002, &[0, 400])
            .rationals(0x000C, &[(2, 1), (3, 2), (1, 1), (1, 1)])
            .shorts(0x003D, &[600, 601, 602, 603])
            .shorts(0x0093, &[7])
            .undefined(0x0096, &[1; 16])
            .file();
        let mn = parse_file(&file, "D800");
        assert_eq!(mn.order, Some(ByteOrder::Little));
        assert_eq!(mn.iso, Some(400));
        assert_eq!(mn.cam_mul, [2.0, 1.0, 1.5, 1.0]);
        assert_eq!(mn.rb_levels, Some([2.0, 1.5]));
        assert_eq!(mn.black, Some([600, 601, 602, 603]));
        assert_eq!(mn.nef_compression, Some(7));
        assert!(mn.lin_offset.is_some());
    }

    #[test]
    fn plain_colour_balance_layouts() {
        for (version, off, stored, expect) in [
            (b"0100", 0x48usize, [744u16, 368, 256, 256], [744.0, 256.0, 368.0, 256.0]),
            (b"0102", 0x0A, [563, 256, 256, 373], [563.0, 256.0, 373.0, 256.0]),
            (b"0103", 0x14, [555, 256, 388, 256], [555.0, 256.0, 388.0, 256.0]),
        ] {
            let mut v = vec![0u8; off + 8];
            v[..4].copy_from_slice(version);
            for (i, x) in stored.iter().enumerate() {
                v[off + 2 * i..off + 2 * i + 2].copy_from_slice(&x.to_le_bytes());
            }
            let mn = parse_file(&Note::new().undefined(0x0097, &v).file(), "D70");
            assert_eq!(mn.cam_mul, expect);
            // Too short for the multipliers: unset.
            let mn = parse_file(&Note::new().undefined(0x0097, &v[..off + 7]).file(), "D70");
            assert_eq!(mn.cam_mul, [0.0; 4]);
            assert_eq!(mn.color_balance_version, balance_version(version));
        }
    }

    #[test]
    fn encrypted_colour_balance_roundtrip() {
        // Encrypt a block that holds known multipliers with the keystream the reader
        // derives from the serial number and shutter count, and check every version.
        for layout in &CIPHER_LAYOUTS {
            let serial = b"4012345\0";
            let count = [1u8, 2, 3, 4];
            let model = "D3";
            let key = crypt::Key::new(crypt::serial_index(serial, model), count);
            let mut block = vec![0u8; CIPHER_BLOCK];
            let x = [500u16, 256, 257, 400];
            for (i, v) in x.iter().enumerate() {
                block[layout.offset + 2 * i..layout.offset + 2 * i + 2].copy_from_slice(&v.to_le_bytes());
            }
            key.decrypt(&mut block); // the keystream is an involution
            let mut value = format!("{:04}", layout.version).into_bytes();
            value.resize(layout.start, 0);
            value.extend(&block);
            let file = Note::new().undefined(0x001D, serial).undefined(0x0097, &value).undefined(0x00A7, &count).file();
            let mn = parse_file(&file, model);
            assert_eq!(mn.color_balance_version, layout.version);
            assert_eq!(mn.cam_mul, layout.stored.to_cam_mul(x), "version {}", layout.version);
        }
    }

    #[test]
    fn cipher_layouts_are_consecutive() {
        for (i, l) in CIPHER_LAYOUTS.iter().enumerate() {
            assert_eq!(l.version, 200 + i as u32);
        }
    }

    #[test]
    fn edit_record_rotation() {
        let mut v = vec![0u8; EDIT_HEADER];
        let mut rec = |id: u32, payload: &[u8]| {
            v.extend(id.to_le_bytes());
            v.extend([0u8; 14]);
            v.extend((payload.len() as u32 + 4).to_le_bytes());
            v.extend(payload);
        };
        rec(0x1234, &[9; 6]);
        rec(EDIT_ROTATION_ID, &90u16.to_le_bytes());
        assert_eq!(edit_rotation(&v), Some(90));
        // Truncated inside the payload, size below 4, no record: nothing, never a panic.
        assert_eq!(edit_rotation(&v[..v.len() - 1]), None);
        assert_eq!(edit_rotation(&v[..EDIT_HEADER]), None);
        let mut bad = v.clone();
        bad[EDIT_HEADER + 18..EDIT_HEADER + 22].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(edit_rotation(&bad), None);
        let mut huge = v.clone();
        huge[EDIT_HEADER + 18..EDIT_HEADER + 22].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(edit_rotation(&huge), None);
    }

    #[test]
    fn coolpix_blocks() {
        // 2560-byte block: red and blue words, big-endian, scale 256.
        let mut b = vec![0u8; 2560];
        b[E5400_RED_AT..E5400_RED_AT + 2].copy_from_slice(&559u16.to_be_bytes());
        b[E5400_BLUE_AT..E5400_BLUE_AT + 2].copy_from_slice(&378u16.to_be_bytes());
        let mn = parse_file(&Note::new().undefined(0x0014, &b).file(), "E5400");
        assert_eq!(mn.cam_mul, [559.0 / 256.0, 1.0, 378.0 / 256.0, 1.0]);
        let mn = parse_file(&Note::new().undefined(0x0014, &[0u8; 1280]).file(), "E5700");
        assert_eq!(mn.cam_mul, [1.0; 4]);
        // NRW block of the later cameras: black, then the four multipliers.
        let mut n = vec![0u8; 200];
        n[..8].copy_from_slice(b"NRW 0101");
        n[NRW_BLACK_AT..NRW_BLACK_AT + 2].copy_from_slice(&200u16.to_le_bytes());
        for (i, v) in [430u32, 512, 510, 567].iter().enumerate() {
            n[NRW_MULS_AT + 4 * i..NRW_MULS_AT + 4 * i + 4].copy_from_slice(&v.to_le_bytes());
        }
        let mn = parse_file(&Note::new().undefined(0x0014, &n).file(), "COOLPIX P7000");
        assert_eq!(mn.nrw_black, Some(200));
        assert_eq!(mn.cam_mul, [860.0, 512.0, 1134.0, 510.0]);
    }

    #[test]
    fn damaged_notes_do_not_panic() {
        let base = Note::new()
            .shorts(0x0002, &[0, 400])
            .undefined(0x0097, b"0210\0\0\0\0")
            .undefined(0x00A7, &[1, 2, 3, 4])
            .undefined(0x0E01, &[7u8; 60])
            .file();
        for cut in 0..base.len() {
            if let Ok(t) = Tiff::parse(&base[..cut]) {
                if let Some(e) = t.find_entry(0x927C) {
                    let _ = parse(&base[..cut], e, t.order, "D3");
                }
            }
        }
        for i in 44..base.len() {
            let mut f = base.clone();
            f[i] = f[i].wrapping_add(0x55);
            if let Ok(t) = Tiff::parse(&f) {
                if let Some(e) = t.find_entry(0x927C) {
                    let _ = parse(&f, e, t.order, "D3");
                }
            }
        }
    }
}
