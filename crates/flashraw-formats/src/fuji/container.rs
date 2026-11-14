//! The byte-level layout of a RAF file: the fixed header, the embedded JPEG's EXIF and
//! maker note, the big-endian record directory and the sensor section.

use flashraw_core::bytes::{ReadAt, ascii_field};
use flashraw_core::exif::read_ifd0;
use flashraw_core::tiff::{Tiff, TiffOptions, parse_ifd_at, tags};
use flashraw_core::*;

pub(super) const SIGNATURE: &[u8] = b"FUJIFILMCCD-RAW";
const HEADER_LEN: usize = 0x94;
/// The directory is not trusted beyond this many records.
const MAX_RECORDS: u32 = 255;

/// Fixed fields of the file header (all numbers big-endian).
pub(super) struct Header {
    pub model: String,
    pub version: String,
    pub jpeg_offset: usize,
    pub jpeg_len: usize,
    pub directory_offset: usize,
    pub section_offset: usize,
    /// A second raw frame is recorded (dual-exposure bodies).
    pub second_frame: bool,
}

impl Header {
    pub fn parse(data: &[u8]) -> Result<Header> {
        if data.len() < HEADER_LEN {
            return Err(Error::corrupt("truncated RAF header"));
        }
        let word = |at: usize| data.u32be(at).unwrap_or(0) as usize;
        let jpeg_offset = word(0x54);
        Ok(Header {
            model: ascii_field(&data[0x1C..0x3C]),
            version: String::from_utf8_lossy(&data[0x3C..0x40]).trim().to_string(),
            jpeg_offset,
            jpeg_len: word(0x58),
            directory_offset: word(0x5C),
            section_offset: word(0x64),
            // Every real file has its JPEG after the header; a smaller offset means the
            // field at 0x78 belongs to something else.
            second_frame: jpeg_offset > 120 && word(0x78) != 0,
        })
    }
}

/// What the embedded JPEG's EXIF contributes besides the common metadata.
pub(super) struct Embedded {
    /// Byte order of the EXIF TIFF (`None` without readable EXIF).
    pub order: Option<ByteOrder>,
    /// Maker-note crop mode (0 when absent).
    pub crop_mode: u32,
}

/// Fill `meta` from the EXIF of the embedded JPEG and read the Fujifilm maker note.
pub(super) fn read_embedded(data: &[u8], hdr: &Header, meta: &mut RawMetadata) -> Embedded {
    let mut out = Embedded { order: None, crop_mode: 0 };
    let Some(rest) = data.get(hdr.jpeg_offset..) else { return out };
    let jpeg = &rest[..hdr.jpeg_len.min(rest.len())];
    if jpeg.len() < 16 || jpeg[..2] != [0xFF, 0xD8] || &jpeg[6..10] != b"Exif" {
        return out;
    }
    let Ok(tiff) = Tiff::parse(&jpeg[12..]) else { return out };
    out.order = Some(tiff.order);
    if let Some(ifd0) = tiff.ifds.first() {
        read_ifd0(ifd0, meta);
    }
    let note =
        tiff.find_entry(tags::MAKER_NOTE).map(|e| e.bytes()).filter(|n| n.len() > 12 && n.starts_with(b"FUJIFILM"));
    if let Some(note) = note {
        read_maker_note(note, meta, &mut out);
    }
    out
}

/// The maker note is a little-endian IFD whose offsets count from the note's first byte.
fn read_maker_note(note: &[u8], meta: &mut RawMetadata, out: &mut Embedded) {
    const CROP_MODE: u16 = 0x104D;
    const SERIAL: u16 = 0x0010;
    let Ok(at) = note.u32le(8) else { return };
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    let Ok(ifd) = parse_ifd_at(note, ByteOrder::Little, at, opts) else { return };
    out.crop_mode = ifd.u32(CROP_MODE).unwrap_or(0);
    if meta.serial.is_none() {
        meta.serial = ifd.string(SERIAL);
    }
}

/// Layout of the 0xC000 blob: which camera generation wrote it and the recommended size.
#[derive(Clone, Copy, Default)]
pub(super) struct SizeBlob {
    /// 1..=4 for blobs carrying a size, 4096 for the short blob, else 0.
    pub generation: u32,
    /// `(width, height)`.
    pub size: Option<(u32, u32)>,
}

impl SizeBlob {
    fn parse(body: &[u8]) -> SizeBlob {
        const SHORT: usize = 4096;
        const LONG: usize = 20000;
        const MAGIC: u32 = 0x5354_5257; // "WRTS" read little-endian
        if body.len() == SHORT {
            return SizeBlob { generation: 4096, size: None };
        }
        if body.len() <= LONG {
            return SizeBlob::default();
        }
        let a = body.u16le(0).unwrap_or(0);
        let b = body.u16le(2).unwrap_or(0);
        let s = body.u32le(4).unwrap_or(0);
        let (generation, at) = if a != 0 && b == 0 && a < 10000 {
            (1, 0)
        } else if a > 10000 && b == 0 {
            (2, 4)
        } else if a == 0 && s == MAGIC {
            (4, 8)
        } else if a == 0 {
            (3, 4)
        } else {
            return SizeBlob::default();
        };
        let width = body.u32le(at).unwrap_or(0);
        let height = body.u32le(at + 4).unwrap_or(0);
        SizeBlob { generation, size: Some((width, height)) }
    }
}

/// The records of the directory that the reader uses.
#[derive(Default)]
pub(super) struct Directory {
    /// Full raw size `(height, width)`.
    pub raw_size: Option<(u32, u32)>,
    /// Vendor crop origin `(top, left)`.
    pub crop_origin: Option<(u32, u32)>,
    /// Vendor crop size `(height, width)`.
    pub crop_size: Option<(u32, u32)>,
    /// Recommended image size `(height, width)`.
    pub image_size: Option<(u32, u32)>,
    /// SuperCCD geometry flags.
    pub layout: bool,
    pub wide: bool,
    /// X-Trans pattern as colour indices 0..=2, in buffer order.
    pub xtrans: Option<[u8; 36]>,
    /// As-shot white balance numbers `(R, G, B)`.
    pub white_balance: Option<[u32; 3]>,
    /// Black levels `(R, G, B, G2)`.
    pub black: Option<[f32; 4]>,
    pub size_blob: SizeBlob,
}

/// Four big-endian words `G R G2 B`, or the average of two such sets, reordered to
/// `R G B G2`.
fn word_group(body: &[u8]) -> [u32; 4] {
    let w = |i: usize| u32::from(body.u16be(i * 2).unwrap_or(0));
    let mut v = [w(0), w(1), w(2), w(3)];
    if body.len() == 16 {
        for (i, x) in v.iter_mut().enumerate() {
            *x = (*x + w(i + 4)) / 2;
        }
    }
    [v[1], v[0], v[3], v[2]]
}

fn pair(body: &[u8]) -> Option<(u32, u32)> {
    Some((u32::from(body.u16be(0).ok()?), u32::from(body.u16be(2).ok()?)))
}

impl Directory {
    /// Read the directory at `offset`; anything unreadable yields fewer records.
    pub fn read(data: &[u8], offset: usize) -> Directory {
        let mut dir = Directory::default();
        let Ok(count) = data.u32be(offset) else { return dir };
        if count > MAX_RECORDS {
            return dir;
        }
        let mut pos = offset.saturating_add(4);
        let mut armed = false;
        for _ in 0..count {
            let Some(head) = data.get(pos..pos.saturating_add(4)) else { break };
            let tag = u16::from_be_bytes([head[0], head[1]]);
            let len = usize::from(u16::from_be_bytes([head[2], head[3]]));
            let body_start = pos.saturating_add(4);
            let Some(body) = data.get(body_start..body_start.saturating_add(len)) else { break };
            pos = body_start.saturating_add(len);
            match tag {
                0x100 if len >= 4 => {
                    dir.raw_size = pair(body);
                    armed = true;
                }
                0x110 if len >= 4 && armed => dir.crop_origin = pair(body),
                0x111 if len >= 4 && armed => dir.crop_size = pair(body),
                0x121 if len >= 4 => {
                    dir.image_size = pair(body).map(|(h, w)| (h, if w == 4284 { 4287 } else { w }));
                }
                0x130 if len >= 2 => {
                    dir.layout = body[0] >> 7 != 0;
                    dir.wide = body[1] & 8 == 0;
                }
                0x131 if len >= 36 => {
                    let mut p = [0u8; 36];
                    // Stored back to front.
                    for (i, slot) in p.iter_mut().enumerate() {
                        *slot = body[35 - i].min(2);
                    }
                    dir.xtrans = Some(p);
                }
                0x2FF0 if len >= 8 => {
                    let [r, g, b, _] = word_group(body);
                    dir.white_balance = Some([r, g, b]);
                }
                0x4000 if len == 8 || len == 16 => dir.black = Some(word_group(body).map(|v| v as f32)),
                0xC000 => dir.size_blob = SizeBlob::parse(body),
                _ => {}
            }
        }
        dir
    }
}

/// The sensor section when it is a small TIFF (cameras since about 2010).
pub(super) struct TiffSection {
    pub width: u32,
    pub height: u32,
    pub bits: u32,
    /// `Some(msb_first)` when the packing flag is present.
    pub packing: Option<bool>,
    /// Payload offset relative to the section start.
    pub payload_offset: usize,
    /// Declared payload length.
    pub declared: usize,
    pub black: Vec<u32>,
    /// As-shot balance `(R, G, B)`.
    pub white_balance: Option<[u32; 3]>,
}

impl TiffSection {
    /// `section` is the file from the section offset on; `None` unless it is a TIFF
    /// whose raw IFD carries size and payload location.
    pub fn parse(section: &[u8]) -> Option<TiffSection> {
        const RAW_IFD: u16 = 0xF000;
        const OPTS: TiffOptions =
            TiffOptions { follow_sub_ifds: false, max_depth: 6, max_ifds: 256, max_entries: 4096, lenient: true };
        let tiff = Tiff::parse_with(section, OPTS).ok()?;
        let at = tiff.ifds.first()?.u32(RAW_IFD)?;
        let ifd = parse_ifd_at(section, tiff.order, at, OPTS).ok()?;
        let width = ifd.u32(0xF001)?;
        let height = ifd.u32(0xF002)?;
        let payload_offset = ifd.u32(0xF007)? as usize;
        let white_balance = ifd.get(0xF00E).map(|e| e.u32s()).filter(|v| v.len() >= 3).map(|v| [v[1], v[0], v[2]]);
        Some(TiffSection {
            width,
            height,
            bits: ifd.u32(0xF003).unwrap_or(12),
            packing: ifd.get(0xF006).map(|e| e.u32(0).unwrap_or(0) != 0),
            payload_offset,
            declared: ifd.u32(0xF008).unwrap_or(0) as usize,
            black: ifd.get(0xF00A).map(|e| e.u32s()).unwrap_or_default(),
            white_balance,
        })
    }
}
