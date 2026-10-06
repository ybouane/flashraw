//! Zero-copy TIFF / TIFF-EP / DNG IFD parsing.
//!
//! Most RAW formats are TIFF-based (DNG, CR2, NEF, ARW, PEF, ORF, RW2, SRW, 3FR,
//! IIQ maker data, ...). [`Tiff::parse`] reads the IFD chain plus SubIFDs and the
//! EXIF/GPS IFDs into an in-memory tree of [`Ifd`]s whose entries reference the
//! source bytes. Values are decoded lazily by the typed accessors on [`Entry`].
//!
//! Robustness: IFD offsets are tracked in a visited set (cyclic chains are cut),
//! recursion depth and total IFD count are bounded, and every value access is
//! bounds checked.

use crate::bytes::{ByteOrder, ReadAt};
use crate::{Error, Result};

/// TIFF field types.
pub mod types {
    pub const BYTE: u16 = 1;
    pub const ASCII: u16 = 2;
    pub const SHORT: u16 = 3;
    pub const LONG: u16 = 4;
    pub const RATIONAL: u16 = 5;
    pub const SBYTE: u16 = 6;
    pub const UNDEFINED: u16 = 7;
    pub const SSHORT: u16 = 8;
    pub const SLONG: u16 = 9;
    pub const SRATIONAL: u16 = 10;
    pub const FLOAT: u16 = 11;
    pub const DOUBLE: u16 = 12;
    pub const IFD: u16 = 13;
    pub const LONG8: u16 = 16;
}

/// Byte size of one value of TIFF type `t` (0 for unknown types).
pub fn type_size(t: u16) -> usize {
    match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 | 16 | 17 | 18 => 8,
        _ => 0,
    }
}

/// Well-known tag numbers.
#[allow(missing_docs)]
pub mod tags {
    pub const NEW_SUBFILE_TYPE: u16 = 0x00FE;
    pub const IMAGE_WIDTH: u16 = 0x0100;
    pub const IMAGE_LENGTH: u16 = 0x0101;
    pub const BITS_PER_SAMPLE: u16 = 0x0102;
    pub const COMPRESSION: u16 = 0x0103;
    pub const PHOTOMETRIC: u16 = 0x0106;
    pub const FILL_ORDER: u16 = 0x010A;
    pub const IMAGE_DESCRIPTION: u16 = 0x010E;
    pub const MAKE: u16 = 0x010F;
    pub const MODEL: u16 = 0x0110;
    pub const STRIP_OFFSETS: u16 = 0x0111;
    pub const ORIENTATION: u16 = 0x0112;
    pub const SAMPLES_PER_PIXEL: u16 = 0x0115;
    pub const ROWS_PER_STRIP: u16 = 0x0116;
    pub const STRIP_BYTE_COUNTS: u16 = 0x0117;
    pub const X_RESOLUTION: u16 = 0x011A;
    pub const PLANAR_CONFIG: u16 = 0x011C;
    pub const SOFTWARE: u16 = 0x0131;
    pub const DATE_TIME: u16 = 0x0132;
    pub const ARTIST: u16 = 0x013B;
    pub const PREDICTOR: u16 = 0x013D;
    pub const TILE_WIDTH: u16 = 0x0142;
    pub const TILE_LENGTH: u16 = 0x0143;
    pub const TILE_OFFSETS: u16 = 0x0144;
    pub const TILE_BYTE_COUNTS: u16 = 0x0145;
    pub const SUB_IFDS: u16 = 0x014A;
    pub const SAMPLE_FORMAT: u16 = 0x0153;
    pub const JPEG_INTERCHANGE_FORMAT: u16 = 0x0201;
    pub const JPEG_INTERCHANGE_FORMAT_LENGTH: u16 = 0x0202;
    pub const YCBCR_SUBSAMPLING: u16 = 0x0212;
    pub const XMP: u16 = 0x02BC;
    pub const CFA_REPEAT_PATTERN_DIM: u16 = 0x828D;
    pub const CFA_PATTERN_EP: u16 = 0x828E;
    pub const COPYRIGHT: u16 = 0x8298;
    pub const EXPOSURE_TIME: u16 = 0x829A;
    pub const F_NUMBER: u16 = 0x829D;
    pub const EXIF_IFD: u16 = 0x8769;
    pub const EXPOSURE_PROGRAM: u16 = 0x8822;
    pub const GPS_IFD: u16 = 0x8825;
    pub const ISO: u16 = 0x8827;
    pub const DATE_TIME_ORIGINAL: u16 = 0x9003;
    pub const OFFSET_TIME_ORIGINAL: u16 = 0x9011;
    pub const EXPOSURE_BIAS: u16 = 0x9204;
    pub const METERING_MODE: u16 = 0x9207;
    pub const FLASH: u16 = 0x9209;
    pub const FOCAL_LENGTH: u16 = 0x920A;
    pub const MAKER_NOTE: u16 = 0x927C;
    pub const SUBSEC_TIME_ORIGINAL: u16 = 0x9291;
    pub const PIXEL_X_DIMENSION: u16 = 0xA002;
    pub const PIXEL_Y_DIMENSION: u16 = 0xA003;
    pub const CFA_PATTERN: u16 = 0xA302;
    pub const FOCAL_LENGTH_35MM: u16 = 0xA405;
    pub const BODY_SERIAL: u16 = 0xA431;
    pub const LENS_SPEC: u16 = 0xA432;
    pub const LENS_MAKE: u16 = 0xA433;
    pub const LENS_MODEL: u16 = 0xA434;
    pub const LENS_SERIAL: u16 = 0xA435;
    // DNG
    pub const DNG_VERSION: u16 = 0xC612;
    pub const UNIQUE_CAMERA_MODEL: u16 = 0xC614;
    pub const CFA_PLANE_COLOR: u16 = 0xC616;
    pub const CFA_LAYOUT: u16 = 0xC617;
    pub const LINEARIZATION_TABLE: u16 = 0xC618;
    pub const BLACK_LEVEL_REPEAT_DIM: u16 = 0xC619;
    pub const BLACK_LEVEL: u16 = 0xC61A;
    pub const BLACK_LEVEL_DELTA_H: u16 = 0xC61B;
    pub const BLACK_LEVEL_DELTA_V: u16 = 0xC61C;
    pub const WHITE_LEVEL: u16 = 0xC61D;
    pub const DEFAULT_SCALE: u16 = 0xC61E;
    pub const DEFAULT_CROP_ORIGIN: u16 = 0xC61F;
    pub const DEFAULT_CROP_SIZE: u16 = 0xC620;
    pub const COLOR_MATRIX1: u16 = 0xC621;
    pub const COLOR_MATRIX2: u16 = 0xC622;
    pub const CAMERA_CALIBRATION1: u16 = 0xC623;
    pub const CAMERA_CALIBRATION2: u16 = 0xC624;
    pub const ANALOG_BALANCE: u16 = 0xC627;
    pub const AS_SHOT_NEUTRAL: u16 = 0xC628;
    pub const AS_SHOT_WHITE_XY: u16 = 0xC629;
    pub const BASELINE_EXPOSURE: u16 = 0xC62A;
    pub const BASELINE_NOISE: u16 = 0xC62B;
    pub const BASELINE_SHARPNESS: u16 = 0xC62C;
    pub const LINEAR_RESPONSE_LIMIT: u16 = 0xC62E;
    pub const PROFILE_NAME: u16 = 0xC6F8;
    pub const PROFILE_HUE_SAT_MAP_DIMS: u16 = 0xC6F9;
    pub const PROFILE_HUE_SAT_MAP_DATA1: u16 = 0xC6FA;
    pub const PROFILE_HUE_SAT_MAP_DATA2: u16 = 0xC6FB;
    pub const PROFILE_TONE_CURVE: u16 = 0xC6FC;
    pub const PROFILE_EMBED_POLICY: u16 = 0xC6FD;
    pub const PROFILE_LOOK_TABLE_DIMS: u16 = 0xC725;
    pub const PROFILE_LOOK_TABLE_DATA: u16 = 0xC726;
    pub const NOISE_PROFILE: u16 = 0xC761;
    pub const BASELINE_EXPOSURE_OFFSET: u16 = 0xC7A5;
    pub const DEFAULT_USER_CROP: u16 = 0xC7B5;
    pub const PROFILE_HUE_SAT_MAP_DATA3: u16 = 0xCD39;
    pub const DNG_PRIVATE_DATA: u16 = 0xC634;
    pub const MAKER_NOTE_SAFETY: u16 = 0xC635;
    pub const CALIBRATION_ILLUMINANT1: u16 = 0xC65A;
    pub const CALIBRATION_ILLUMINANT2: u16 = 0xC65B;
    pub const ACTIVE_AREA: u16 = 0xC68D;
    pub const MASKED_AREAS: u16 = 0xC68E;
    pub const FORWARD_MATRIX1: u16 = 0xC714;
    pub const FORWARD_MATRIX2: u16 = 0xC715;
    pub const OPCODE_LIST1: u16 = 0xC740;
    pub const OPCODE_LIST2: u16 = 0xC741;
    pub const OPCODE_LIST3: u16 = 0xC74E;
    pub const NEW_RAW_IMAGE_DIGEST: u16 = 0xC7A7;
    pub const CALIBRATION_ILLUMINANT3: u16 = 0xCD31;
    pub const COLOR_MATRIX3: u16 = 0xCD33;
    pub const FORWARD_MATRIX3: u16 = 0xCD34;
    pub const CAMERA_CALIBRATION3: u16 = 0xCD32;
    pub const ILLUMINANT_DATA1: u16 = 0xCD35;
    pub const ILLUMINANT_DATA2: u16 = 0xCD36;
    pub const ILLUMINANT_DATA3: u16 = 0xCD37;
    pub const ROW_INTERLEAVE_FACTOR: u16 = 0xC71F;
    pub const COLUMN_INTERLEAVE_FACTOR: u16 = 0xCD43;
    pub const JXL_DISTANCE: u16 = 0xCD49;
}

/// Parsing options.
#[derive(Debug, Clone, Copy)]
pub struct TiffOptions {
    /// Follow SubIFDs (0x14A), EXIF (0x8769) and GPS (0x8825) pointers.
    pub follow_sub_ifds: bool,
    pub max_depth: u32,
    pub max_ifds: usize,
    pub max_entries: usize,
    /// Accept IFDs whose last entries run past the end of data (keep the parsed prefix).
    pub lenient: bool,
}

impl Default for TiffOptions {
    fn default() -> Self {
        TiffOptions { follow_sub_ifds: true, max_depth: 6, max_ifds: 256, max_entries: 4096, lenient: true }
    }
}

/// A single IFD entry. `data` holds the raw value bytes (inline or referenced).
#[derive(Clone, Copy)]
pub struct Entry<'a> {
    pub tag: u16,
    pub typ: u16,
    pub count: u32,
    pub order: ByteOrder,
    /// Raw value bytes (`count * type_size` bytes, possibly truncated if lenient).
    pub data: &'a [u8],
    /// The raw 4-byte value/offset field (offset relative to the IFD base).
    pub value_field: u32,
    /// Absolute position of the value bytes inside the slice the TIFF was parsed from.
    pub value_pos: usize,
}

impl std::fmt::Debug for Entry<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Entry(tag={:#06x}, type={}, count={})", self.tag, self.typ, self.count)
    }
}

impl<'a> Entry<'a> {
    /// Number of values that are actually available.
    pub fn len(&self) -> usize {
        let sz = type_size(self.typ);
        self.data.len().checked_div(sz).map_or(self.data.len(), |n| n.min(self.count as usize))
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Value `i` as u32 (integer types; rationals are truncated).
    pub fn u32(&self, i: usize) -> Option<u32> {
        let d = self.data;
        let o = self.order;
        Some(match self.typ {
            types::BYTE | types::UNDEFINED | types::ASCII => *d.get(i)? as u32,
            types::SBYTE => *d.get(i)? as i8 as i32 as u32,
            types::SHORT => d.u16_at(o, i * 2).ok()? as u32,
            types::SSHORT => d.u16_at(o, i * 2).ok()? as i16 as i32 as u32,
            types::LONG | types::SLONG | types::IFD => d.u32_at(o, i * 4).ok()?,
            types::LONG8 => d.u64_at(o, i * 8).ok()? as u32,
            types::RATIONAL | types::SRATIONAL | types::FLOAT | types::DOUBLE => {
                let f = self.f64(i)?;
                if f.is_finite() && f >= 0.0 { f as u32 } else { return None }
            }
            _ => return None,
        })
    }
    pub fn u16(&self, i: usize) -> Option<u16> {
        self.u32(i).map(|v| v as u16)
    }
    /// Value `i` as signed integer.
    pub fn i32(&self, i: usize) -> Option<i32> {
        match self.typ {
            types::SBYTE | types::SSHORT | types::SLONG => self.u32(i).map(|v| v as i32),
            types::SRATIONAL | types::RATIONAL | types::FLOAT | types::DOUBLE => self.f64(i).map(|v| v as i32),
            _ => self.u32(i).map(|v| v as i32),
        }
    }
    /// Value `i` as f64 (any numeric type).
    pub fn f64(&self, i: usize) -> Option<f64> {
        let d = self.data;
        let o = self.order;
        Some(match self.typ {
            types::RATIONAL => {
                let n = d.u32_at(o, i * 8).ok()?;
                let den = d.u32_at(o, i * 8 + 4).ok()?;
                if den == 0 {
                    return None;
                }
                n as f64 / den as f64
            }
            types::SRATIONAL => {
                let n = d.u32_at(o, i * 8).ok()? as i32;
                let den = d.u32_at(o, i * 8 + 4).ok()? as i32;
                if den == 0 {
                    return None;
                }
                n as f64 / den as f64
            }
            types::FLOAT => f32::from_bits(d.u32_at(o, i * 4).ok()?) as f64,
            types::DOUBLE => f64::from_bits(d.u64_at(o, i * 8).ok()?),
            types::SBYTE | types::SSHORT | types::SLONG => self.i32(i)? as f64,
            _ => self.u32(i)? as f64,
        })
    }
    pub fn f32(&self, i: usize) -> Option<f32> {
        self.f64(i).map(|v| v as f32)
    }
    /// All values as u32.
    pub fn u32s(&self) -> Vec<u32> {
        (0..self.len()).filter_map(|i| self.u32(i)).collect()
    }
    /// All values as f64.
    pub fn f64s(&self) -> Vec<f64> {
        (0..self.len()).filter_map(|i| self.f64(i)).collect()
    }
    pub fn f32s(&self) -> Vec<f32> {
        (0..self.len()).filter_map(|i| self.f32(i)).collect()
    }
    /// Value as an ASCII string (trimmed at the first NUL, whitespace trimmed).
    pub fn string(&self) -> String {
        crate::bytes::ascii_field(self.data)
    }
    /// Raw value bytes.
    pub fn bytes(&self) -> &'a [u8] {
        self.data
    }
}

/// An Image File Directory.
#[derive(Debug, Clone)]
pub struct Ifd<'a> {
    /// Offset of this IFD relative to the TIFF base.
    pub offset: u32,
    pub order: ByteOrder,
    pub entries: Vec<Entry<'a>>,
    /// SubIFDs (tag 0x14A), in order.
    pub sub_ifds: Vec<Ifd<'a>>,
    pub exif: Option<Box<Ifd<'a>>>,
    pub gps: Option<Box<Ifd<'a>>>,
    /// Offset of the next IFD in the chain (0 if none).
    pub next: u32,
}

impl<'a> Ifd<'a> {
    pub fn get(&self, tag: u16) -> Option<&Entry<'a>> {
        self.entries.iter().find(|e| e.tag == tag)
    }
    pub fn has(&self, tag: u16) -> bool {
        self.get(tag).is_some()
    }
    pub fn u32(&self, tag: u16) -> Option<u32> {
        self.get(tag)?.u32(0)
    }
    pub fn f64(&self, tag: u16) -> Option<f64> {
        self.get(tag)?.f64(0)
    }
    pub fn string(&self, tag: u16) -> Option<String> {
        let s = self.get(tag)?.string();
        (!s.is_empty()).then_some(s)
    }
    /// Required u32 tag.
    pub fn req_u32(&self, tag: u16) -> Result<u32> {
        self.u32(tag).ok_or_else(|| Error::invalid_metadata(format!("missing TIFF tag {tag:#06x}")))
    }
    pub fn width(&self) -> u32 {
        self.u32(tags::IMAGE_WIDTH).unwrap_or(0)
    }
    pub fn height(&self) -> u32 {
        self.u32(tags::IMAGE_LENGTH).unwrap_or(0)
    }
    pub fn compression(&self) -> u32 {
        self.u32(tags::COMPRESSION).unwrap_or(1)
    }
    pub fn bps(&self) -> u32 {
        self.u32(tags::BITS_PER_SAMPLE).unwrap_or(0)
    }
    pub fn is_reduced_resolution(&self) -> bool {
        self.u32(tags::NEW_SUBFILE_TYPE).unwrap_or(0) & 1 != 0
    }

    /// This IFD and all nested IFDs (sub IFDs, EXIF, GPS), depth first.
    pub fn walk(&self) -> Vec<&Ifd<'a>> {
        let mut out = vec![self];
        for s in &self.sub_ifds {
            out.extend(s.walk());
        }
        if let Some(e) = &self.exif {
            out.extend(e.walk());
        }
        if let Some(g) = &self.gps {
            out.push(g);
        }
        out
    }
}

/// A parsed TIFF structure.
#[derive(Debug, Clone)]
pub struct Tiff<'a> {
    /// The bytes offsets are relative to (`file[base..]`).
    pub data: &'a [u8],
    pub order: ByteOrder,
    /// Top-level IFD chain.
    pub ifds: Vec<Ifd<'a>>,
}

struct ParseState {
    visited: Vec<u32>,
    count: usize,
}

impl<'a> Tiff<'a> {
    /// Parse a standard TIFF header (`II*\0` / `MM\0*`, or vendor magics) at the start of `data`.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        Self::parse_with(data, TiffOptions::default())
    }

    pub fn parse_with(data: &'a [u8], opts: TiffOptions) -> Result<Self> {
        let order = match data.get(0..2) {
            Some(b"II") => ByteOrder::Little,
            Some(b"MM") => ByteOrder::Big,
            _ => return Err(Error::UnsupportedFormat("not a TIFF header".into())),
        };
        // Magic is 42 for TIFF; ORF uses 0x4F52/0x5352, RW2 0x55, others vary — accept any.
        let first = data.u32_at(order, 4)?;
        Self::parse_from(data, order, first, opts)
    }

    /// Parse an IFD chain starting at `first_ifd` with an explicit byte order.
    pub fn parse_from(data: &'a [u8], order: ByteOrder, first_ifd: u32, opts: TiffOptions) -> Result<Self> {
        let mut st = ParseState { visited: Vec::new(), count: 0 };
        let mut ifds = Vec::new();
        let mut next = first_ifd;
        while next != 0 && ifds.len() < 64 {
            match parse_ifd(data, order, next, 0, &opts, &mut st) {
                Ok(ifd) => {
                    next = ifd.next;
                    ifds.push(ifd);
                }
                Err(e) => {
                    if ifds.is_empty() {
                        return Err(e);
                    }
                    break; // keep what we have: trailing garbage chains are common
                }
            }
        }
        if ifds.is_empty() {
            return Err(Error::corrupt("TIFF without IFDs"));
        }
        Ok(Tiff { data, order, ifds })
    }

    /// Every IFD in the file (chain, sub IFDs, EXIF, GPS), depth first.
    pub fn all_ifds(&self) -> Vec<&Ifd<'a>> {
        self.ifds.iter().flat_map(|i| i.walk()).collect()
    }

    /// First IFD (anywhere) that contains `tag`.
    pub fn find_ifd_with(&self, tag: u16) -> Option<&Ifd<'a>> {
        self.all_ifds().into_iter().find(|i| i.has(tag))
    }

    /// First entry for `tag` anywhere in the file.
    pub fn find_entry(&self, tag: u16) -> Option<&Entry<'a>> {
        self.all_ifds().into_iter().find_map(|i| i.get(tag))
    }

    /// Parse a nested IFD (e.g. a maker note directory) located at `offset` within `self.data`.
    pub fn sub_ifd_at(&self, offset: u32) -> Result<Ifd<'a>> {
        let mut st = ParseState { visited: Vec::new(), count: 0 };
        parse_ifd(self.data, self.order, offset, 0, &TiffOptions::default(), &mut st)
    }
}

/// Parse a single IFD (and its children) at `offset` of `data`.
pub fn parse_ifd_at<'a>(data: &'a [u8], order: ByteOrder, offset: u32, opts: TiffOptions) -> Result<Ifd<'a>> {
    let mut st = ParseState { visited: Vec::new(), count: 0 };
    parse_ifd(data, order, offset, 0, &opts, &mut st)
}

fn parse_ifd<'a>(
    data: &'a [u8],
    order: ByteOrder,
    offset: u32,
    depth: u32,
    opts: &TiffOptions,
    st: &mut ParseState,
) -> Result<Ifd<'a>> {
    if depth > opts.max_depth {
        return Err(Error::corrupt("IFD nesting too deep"));
    }
    if st.count >= opts.max_ifds {
        return Err(Error::corrupt("too many IFDs"));
    }
    if st.visited.contains(&offset) {
        return Err(Error::corrupt_at("recursive IFD reference", offset as u64));
    }
    st.visited.push(offset);
    st.count += 1;
    let base = offset as usize;
    let n = data.u16_at(order, base)? as usize;
    if n == 0 || n > opts.max_entries {
        return Err(Error::corrupt_at(format!("IFD with {n} entries"), offset as u64));
    }
    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        let p = base + 2 + i * 12;
        if p + 12 > data.len() {
            if opts.lenient && !entries.is_empty() {
                break;
            }
            return Err(Error::corrupt_at("IFD entries beyond end of data", p as u64));
        }
        let tag = data.u16_at(order, p)?;
        let typ = data.u16_at(order, p + 2)?;
        let count = data.u32_at(order, p + 4)?;
        let value_field = data.u32_at(order, p + 8)?;
        let sz = type_size(typ);
        if sz == 0 {
            continue; // unknown type: skip entry, keep going
        }
        let total = (count as u64) * sz as u64;
        let (vpos, vbytes) = if total <= 4 {
            (p + 8, &data[p + 8..p + 8 + total as usize])
        } else {
            let o = value_field as usize;
            if o >= data.len() {
                continue; // dangling pointer: skip entry
            }
            let end = (o as u64 + total).min(data.len() as u64) as usize;
            (o, &data[o..end])
        };
        entries.push(Entry { tag, typ, count, order, data: vbytes, value_field, value_pos: vpos });
    }
    let next_pos = base + 2 + n * 12;
    let next = data.u32_at(order, next_pos).unwrap_or(0);
    let mut ifd = Ifd { offset, order, entries, sub_ifds: Vec::new(), exif: None, gps: None, next };
    if opts.follow_sub_ifds {
        if let Some(e) = ifd.get(tags::SUB_IFDS).copied() {
            for i in 0..e.len().min(32) {
                if let Some(o) = e.u32(i) {
                    if let Ok(sub) = parse_ifd(data, order, o, depth + 1, opts, st) {
                        ifd.sub_ifds.push(sub);
                    }
                }
            }
        }
        if let Some(o) = ifd.u32(tags::EXIF_IFD) {
            if let Ok(sub) = parse_ifd(data, order, o, depth + 1, opts, st) {
                ifd.exif = Some(Box::new(sub));
            }
        }
        if let Some(o) = ifd.u32(tags::GPS_IFD) {
            if let Ok(sub) = parse_ifd(data, order, o, depth + 1, opts, st) {
                ifd.gps = Some(Box::new(sub));
            }
        }
    }
    Ok(ifd)
}

#[cfg(test)]
pub(crate) mod test_util {
    /// Minimal little-endian TIFF builder for tests.
    pub struct TiffBuilder {
        pub entries: Vec<(u16, u16, u32, Vec<u8>)>,
    }
    impl TiffBuilder {
        pub fn new() -> Self {
            TiffBuilder { entries: vec![] }
        }
        pub fn short(mut self, tag: u16, v: &[u16]) -> Self {
            self.entries.push((tag, 3, v.len() as u32, v.iter().flat_map(|x| x.to_le_bytes()).collect()));
            self
        }
        pub fn long(mut self, tag: u16, v: &[u32]) -> Self {
            self.entries.push((tag, 4, v.len() as u32, v.iter().flat_map(|x| x.to_le_bytes()).collect()));
            self
        }
        pub fn ascii(mut self, tag: u16, s: &str) -> Self {
            let mut b = s.as_bytes().to_vec();
            b.push(0);
            self.entries.push((tag, 2, b.len() as u32, b));
            self
        }
        pub fn build(mut self, next: u32) -> Vec<u8> {
            self.entries.sort_by_key(|e| e.0);
            let mut out = b"II*\0\x08\0\0\0".to_vec();
            let n = self.entries.len();
            let mut extra_off = 8 + 2 + n * 12 + 4;
            let mut extra: Vec<u8> = vec![];
            out.extend((n as u16).to_le_bytes());
            for (tag, typ, count, data) in &self.entries {
                out.extend(tag.to_le_bytes());
                out.extend(typ.to_le_bytes());
                out.extend(count.to_le_bytes());
                if data.len() <= 4 {
                    let mut d = data.clone();
                    d.resize(4, 0);
                    out.extend(d);
                } else {
                    out.extend((extra_off as u32).to_le_bytes());
                    extra.extend(data);
                    extra_off += data.len();
                }
            }
            out.extend(next.to_le_bytes());
            out.extend(extra);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::TiffBuilder;
    use super::*;

    #[test]
    fn parse_basic() {
        let data = TiffBuilder::new()
            .long(tags::IMAGE_WIDTH, &[640])
            .short(tags::IMAGE_LENGTH, &[480])
            .ascii(tags::MAKE, "FlashRaw Test Make")
            .short(tags::BITS_PER_SAMPLE, &[12, 12, 12])
            .build(0);
        let t = Tiff::parse(&data).unwrap();
        let ifd = &t.ifds[0];
        assert_eq!(ifd.width(), 640);
        assert_eq!(ifd.height(), 480);
        assert_eq!(ifd.string(tags::MAKE).unwrap(), "FlashRaw Test Make");
        assert_eq!(ifd.get(tags::BITS_PER_SAMPLE).unwrap().u32s(), vec![12, 12, 12]);
    }

    #[test]
    fn recursive_chain_terminates() {
        // next pointer points back at the first IFD
        let data = TiffBuilder::new().long(tags::IMAGE_WIDTH, &[1]).build(8);
        let t = Tiff::parse(&data).unwrap();
        assert_eq!(t.ifds.len(), 1);
    }

    #[test]
    fn self_referencing_subifd() {
        let data = TiffBuilder::new().long(tags::SUB_IFDS, &[8]).long(tags::EXIF_IFD, &[8]).build(0);
        let t = Tiff::parse(&data).unwrap();
        assert!(t.ifds[0].sub_ifds.is_empty());
        assert!(t.ifds[0].exif.is_none());
    }

    #[test]
    fn garbage_does_not_panic() {
        for len in 0..64 {
            let mut d = vec![0u8; len];
            if len >= 2 {
                d[0] = b'I';
                d[1] = b'I';
            }
            let _ = Tiff::parse(&d);
        }
        let mut d = TiffBuilder::new().long(tags::STRIP_OFFSETS, &[1, 2, 3, 4]).build(0);
        // Corrupt count to something enormous.
        d[8 + 2 + 4] = 0xFF;
        d[8 + 2 + 5] = 0xFF;
        d[8 + 2 + 6] = 0xFF;
        let t = Tiff::parse(&d).unwrap();
        let e = t.ifds[0].get(tags::STRIP_OFFSETS);
        if let Some(e) = e {
            let _ = e.u32s();
        }
    }
}
