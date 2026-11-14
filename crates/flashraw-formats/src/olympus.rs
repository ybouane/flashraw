//! Olympus / OM System ORF and ORI files.
//!
//! The container is TIFF (magics `IIRO`, `IIRS`, `MMOR`, or a plain header). Calibration lives in
//! the maker note: the black levels and white balance in the root directory (older bodies) or in
//! the ImageProcessing sub-directory, the coder parameters of the lossless codec and two embedded
//! JPEGs. Six payload layouts exist, chosen by [`LAYOUT_RULES`]; the readout columns without image
//! are cut by [`WIDTH_TRIMS`] and [`MODEL_TRIMS`].

use crate::common::*;
use crate::nikon::conventions::file_orientation;
use crate::sony::{cfa_black, table};
use flashraw_codecs::olympus::{self as codec, OlympusParams};
use flashraw_codecs::packed::{self, PackedLayout};
use flashraw_core::bits::{BitPumpMsb32, BitRead};
use flashraw_core::tiff::{Ifd, Tiff, TiffOptions, parse_ifd_at, tags, types};
use flashraw_core::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// The ORF TIFF magics `IIRO`, `IIRS`, `MMOR`.
pub fn probe(data: &[u8]) -> bool {
    matches!(data.get(..4), Some(b"IIRO" | b"IIRS" | b"MMOR"))
}

/// Parse and open an ORF whose magic [`probe`] accepted.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let tiff = Tiff::parse(data)?;
    open_tiff(data, tiff, opts)
}

// ---------------------------------------------------------------------------------------------
// Maker note
// ---------------------------------------------------------------------------------------------

/// How a maker note starts and where its root directory is. The two newer framings keep the
/// directory offsets relative to the note; the oldest one keeps them relative to the file.
struct Framing {
    magic: &'static [u8],
    /// The note must be longer than this many bytes.
    longer_than: usize,
    /// Position of the two byte-order letters (`None`: the file's order).
    order_at: Option<usize>,
    /// Directory position relative to the note start.
    dir_at: usize,
    /// Offsets are relative to the note start (else to the file start).
    rel_note: bool,
}

const FRAMINGS: [Framing; 3] = [
    Framing { magic: b"OLYMPUS\0", longer_than: 12, order_at: Some(8), dir_at: 12, rel_note: true },
    Framing { magic: b"OM SYSTEM\0", longer_than: 16, order_at: Some(12), dir_at: 16, rel_note: true },
    Framing { magic: b"OLYMP\0", longer_than: 8, order_at: None, dir_at: 8, rel_note: false },
];

/// Root-directory tags (older bodies keep their calibration here).
mod root_tag {
    pub const THUMBNAIL: u16 = 0x0100;
    pub const BLACK: u16 = 0x1012;
    pub const WB_RED: u16 = 0x1017;
    pub const WB_BLUE: u16 = 0x1018;
    pub const VALID_BITS: u16 = 0x102C;
    pub const EQUIPMENT: u16 = 0x2010;
    pub const CAMERA_SETTINGS: u16 = 0x2020;
    pub const IMAGE_PROCESSING: u16 = 0x2040;
}

/// Tags of the CameraSettings directory.
mod settings_tag {
    pub const PREVIEW_OFFSET: u16 = 0x0101;
    pub const PREVIEW_LENGTH: u16 = 0x0102;
}

/// Tags of the ImageProcessing directory.
mod proc_tag {
    pub const WB_LEVELS: u16 = 0x0100;
    pub const BLACK: u16 = 0x0600;
    pub const VALID_BITS: u16 = 0x0611;
    /// Left, top, width, height of the default crop.
    pub const CROP: [u16; 4] = [0x0612, 0x0613, 0x0614, 0x0615];
    /// Parameters of the lossless codec, in the order the codec takes them.
    pub const CODER: [u16; 14] = [
        0x0640, 0x0641, 0x0642, 0x0643, 0x0644, 0x0645, 0x0646, 0x0647, 0x0648, 0x0649, 0x0650, 0x0651, 0x0652, 0x0653,
    ];
}

#[derive(Default)]
struct MakerNote {
    /// R G G B black levels.
    black: Option<[u32; 4]>,
    /// Red and blue levels (256 = 1.0).
    wb_rb: [Option<u32>; 2],
    valid_bits: Option<u32>,
    /// Left, top, width, height.
    crop: [Option<u32>; 4],
    coder: [Option<u32>; 14],
    /// Embedded JPEGs `(offset, length)` in file order: thumbnail, then the preview.
    jpegs: Vec<(usize, usize)>,
}

fn first_values<const N: usize>(ifd: &Ifd, tag: u16) -> Option<[u32; N]> {
    let e = ifd.get(tag)?;
    let mut v = [0u32; N];
    for (i, slot) in v.iter_mut().enumerate() {
        *slot = e.u32(i)?;
    }
    Some(v)
}

/// The directory a root entry designates: embedded (UNDEFINED type) or by offset.
fn sub_directory<'a>(note: &'a [u8], order: ByteOrder, root: &Ifd, tag: u16) -> Option<Ifd<'a>> {
    let e = root.get(tag)?;
    let at = if e.typ == types::UNDEFINED { e.value_pos } else { e.u32(0)? as usize };
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    parse_ifd_at(note, order, u32::try_from(at).ok()?, opts).ok()
}

fn read_maker_note(data: &[u8], tiff: &Tiff) -> MakerNote {
    let mut mn = MakerNote::default();
    let Some(entry) = tiff.find_entry(tags::MAKER_NOTE) else { return mn };
    let start = entry.value_pos;
    let Some(blob) = start.checked_add(entry.data.len()).and_then(|end| data.get(start..end)) else { return mn };
    let Some(f) = FRAMINGS.iter().find(|f| blob.starts_with(f.magic) && blob.len() > f.longer_than) else { return mn };
    let order = match f.order_at.map(|o| &blob[o..o + 2]) {
        Some(b"II") => ByteOrder::Little,
        Some(b"MM") => ByteOrder::Big,
        _ => tiff.order,
    };
    let (base, dir_at) = if f.rel_note { (start, f.dir_at) } else { (0, start.saturating_add(f.dir_at)) };
    let note = &data[base..];
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    let Ok(root) =
        u32::try_from(dir_at).map_err(|_| ()).and_then(|at| parse_ifd_at(note, order, at, opts).map_err(|_| ()))
    else {
        return mn;
    };

    if let Some(v) = first_values::<4>(&root, root_tag::BLACK) {
        mn.black = Some(v);
    }
    mn.wb_rb = [root.u32(root_tag::WB_RED), root.u32(root_tag::WB_BLUE)];
    if let Some(v) = root.u32(root_tag::VALID_BITS) {
        mn.valid_bits = Some(v);
    }
    if let Some(t) = root.get(root_tag::THUMBNAIL).filter(|e| e.typ == types::UNDEFINED) {
        mn.jpegs.push((base.saturating_add(t.value_pos), t.data.len()));
    }
    // Present sub-directories, in this order. The equipment directory carries nothing used.
    let _equipment = sub_directory(note, order, &root, root_tag::EQUIPMENT);
    if let Some(settings) = sub_directory(note, order, &root, root_tag::CAMERA_SETTINGS) {
        if let (Some(o), Some(l)) =
            (settings.u32(settings_tag::PREVIEW_OFFSET), settings.u32(settings_tag::PREVIEW_LENGTH))
        {
            mn.jpegs.push((base.saturating_add(o as usize), l as usize));
        }
    }
    if let Some(p) = sub_directory(note, order, &root, root_tag::IMAGE_PROCESSING) {
        if let Some(v) = first_values::<2>(&p, proc_tag::WB_LEVELS) {
            mn.wb_rb = v.map(Some);
        }
        if let Some(v) = first_values::<4>(&p, proc_tag::BLACK) {
            mn.black = Some(v);
        }
        if let Some(v) = p.u32(proc_tag::VALID_BITS) {
            mn.valid_bits = Some(v);
        }
        mn.crop = proc_tag::CROP.map(|t| p.u32(t));
        mn.coder = proc_tag::CODER.map(|t| p.u32(t));
    }
    mn
}

// ---------------------------------------------------------------------------------------------
// Payload layouts
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layout {
    /// 12-bit LSB-first with one pad byte after every 10 samples.
    Padded12,
    /// 12-bit MSB-first, even rows first, then the odd rows from the next 2048-byte boundary.
    Interlaced12,
    /// 12-bit MSB-first inside 32-bit little-endian words.
    Words12,
    /// 12-bit MSB-first bytes.
    Msb12,
    /// The lossless codec.
    Compressed,
    /// 16-bit words, 12 or 10 valid bits at the top.
    Unpacked,
}

impl Layout {
    /// The "mode" the camera database is asked for.
    fn mode(self) -> &'static str {
        match self {
            Layout::Compressed => "adaptive-rice",
            Layout::Words12 | Layout::Msb12 => "12bit-packed",
            Layout::Interlaced12 => "12bit-packed-interlaced",
            Layout::Padded12 => "12bit-packed-padded",
            Layout::Unpacked => "uncompressed",
        }
    }
    fn is_packed12(self) -> bool {
        matches!(self, Layout::Padded12 | Layout::Interlaced12 | Layout::Words12 | Layout::Msb12)
    }
}

/// A condition on the strip size against the pixel count: `bytes * num` compared with `area * den`.
#[derive(Clone, Copy)]
enum Size {
    Any,
    Equals(u64, u64),
    Exceeds(u64, u64),
}

#[derive(Clone, Copy)]
enum Bits {
    Any,
    Exactly(u32),
    OneOf(&'static [u32]),
}

/// One row of [`LAYOUT_RULES`]: all conditions must hold.
struct LayoutRule {
    layout: Layout,
    bits: Bits,
    /// PhotometricInterpretation 2 (RGB) is required.
    photometric_rgb: bool,
    size: Size,
}

/// First match wins. Corpus bodies: Padded12 E-300/E-330/E-500; Interlaced12 C-5050Z..C-8080WZ,
/// SP-350/500/510/565/570; Words12 SH-2, XZ-2, XZ-10; Msb12 none (unverified); Compressed all
/// bodies from the E-3 on; Unpacked E-1, E-10, E-20, E-400.
const LAYOUT_RULES: [LayoutRule; 6] = [
    LayoutRule { layout: Layout::Padded12, bits: Bits::Any, photometric_rgb: false, size: Size::Equals(5, 8) },
    LayoutRule { layout: Layout::Interlaced12, bits: Bits::Exactly(12), photometric_rgb: true, size: Size::Any },
    LayoutRule { layout: Layout::Words12, bits: Bits::Exactly(12), photometric_rgb: false, size: Size::Equals(2, 3) },
    LayoutRule { layout: Layout::Msb12, bits: Bits::Exactly(12), photometric_rgb: false, size: Size::Any },
    LayoutRule {
        layout: Layout::Compressed,
        bits: Bits::Exactly(16),
        photometric_rgb: false,
        size: Size::Exceeds(7, 1),
    },
    LayoutRule { layout: Layout::Unpacked, bits: Bits::OneOf(&[14, 16]), photometric_rgb: false, size: Size::Any },
];

impl LayoutRule {
    fn matches(&self, bps: u32, photometric: Option<u32>, bytes: u64, area: u64) -> bool {
        let bits = match self.bits {
            Bits::Any => true,
            Bits::Exactly(b) => bps == b,
            Bits::OneOf(bs) => bs.contains(&bps),
        };
        let size = match self.size {
            Size::Any => true,
            Size::Equals(n, d) => bytes.saturating_mul(n) == area.saturating_mul(d),
            Size::Exceeds(n, d) => bytes.saturating_mul(n) > area.saturating_mul(d),
        };
        bits && size && (!self.photometric_rgb || photometric == Some(2))
    }
}

// ---------------------------------------------------------------------------------------------
// Geometry and level tables
// ---------------------------------------------------------------------------------------------

/// Readout columns (and rows) without image, by raw width. Verified by sample files for 4100, 4080
/// and 10400; 8200, 8180 and 9280 have no sample. `(raw width, columns cut, rows cut)`.
const WIDTH_TRIMS: [(u32, u32, u32); 6] =
    [(4100, 4, 0), (4080, 24, 0), (10400, 12, 0), (8200, 30, 0), (8180, 10, 0), (9280, 6, 6)];

/// How a model name is matched (case-sensitive, on the trimmed model).
#[derive(Clone, Copy)]
enum Model {
    OneOf(&'static [&'static str]),
    StartsWith(&'static str),
}

impl Model {
    fn matches(self, model: &str) -> bool {
        match self {
            Model::OneOf(names) => names.contains(&model),
            Model::StartsWith(p) => model.starts_with(p),
        }
    }
}

/// Columns cut by model, after the width trims. Verified for E-300/E-500, E-330, TG-4, TG-5/TG-6
/// and STYLUS1; TG-7 has a sample in the OM System set.
const MODEL_TRIMS: [(Model, u32); 5] = [
    (Model::OneOf(&["E-300", "E-500"]), 20),
    (Model::OneOf(&["E-330"]), 30),
    (Model::OneOf(&["TG-4"]), 16),
    (Model::OneOf(&["TG-5", "TG-6", "TG-7"]), 26),
    (Model::StartsWith("STYLUS1"), 16),
];

fn visible_size(model: &str, raw_w: u32, buf_h: u32) -> (u32, u32) {
    let (mut w, mut h) = (raw_w, buf_h);
    if let Some(&(_, cut_w, cut_h)) = WIDTH_TRIMS.iter().find(|t| t.0 == raw_w) {
        w = w.saturating_sub(cut_w);
        h = h.saturating_sub(cut_h);
    }
    if let Some(&(_, cut)) = MODEL_TRIMS.iter().find(|(m, _)| m.matches(model)) {
        w = w.saturating_sub(cut);
    }
    (w.min(raw_w), h)
}

// ---------------------------------------------------------------------------------------------
// Open
// ---------------------------------------------------------------------------------------------

/// The raw image: the largest uncompressed non-preview image; on a tie the last one.
fn find_raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    tiff.all_ifds()
        .into_iter()
        .filter(|i| {
            i.width() > 0 && i.height() > 0 && i.has(tags::STRIP_OFFSETS) && i.compression() == 1 && !is_preview_ifd(i)
        })
        .max_by_key(|i| u64::from(i.width()) * u64::from(i.height()))
}

/// The white balance from the red and blue levels (256 = 1.0), when plausible.
fn white_balance(rb: [Option<u32>; 2]) -> Option<WhiteBalance> {
    let [Some(r), Some(b)] = rb else { return None };
    if r == 0 || b == 0 {
        return None;
    }
    let (r, b) = (r as f32 / 256.0, b as f32 / 256.0);
    let (max, min) = (r.max(b).max(1.0), r.min(b).min(1.0));
    (min > 0.001 && max / min <= 100.0).then(|| WhiteBalance::from_multipliers(r, 1.0, b, 1.0)).flatten()
}

/// Open an ORF given as an already parsed TIFF (files with a plain TIFF header and an Olympus make).
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let mut meta = RawMetadata::default();
    for ifd in &tiff.ifds {
        flashraw_core::exif::read_ifd0(ifd, &mut meta);
    }
    let raw =
        find_raw_ifd(&tiff).ok_or_else(|| no_raw_ifd(&tiff, "ORF", Error::invalid_metadata("ORF: no raw image")))?;
    let (raw_w, raw_h, bps) = (raw.width(), raw.height(), raw.bps());
    let (offset, declared) = *strips(raw).first().ok_or_else(|| Error::invalid_metadata("ORF: no strip offsets"))?;
    let area = u64::from(raw_w) * u64::from(raw_h);
    let model = meta.model.trim().to_string();

    let mn = read_maker_note(data, &tiff);
    let valid_bits = mn.valid_bits.filter(|b| (8..=16).contains(b));

    let photometric = raw.u32(tags::PHOTOMETRIC);
    let layout = LAYOUT_RULES
        .iter()
        .find(|r| r.matches(bps, photometric, declared as u64, area))
        .map(|r| r.layout)
        .ok_or_else(|| Error::unsupported_compression("ORF", format!("{bps}-bit uncompressed layout")))?;
    let bits = if layout.is_packed12() { 12 } else { valid_bits.unwrap_or(12) };

    // Geometry: an odd number of rows is read as one more.
    let buf_h = raw_h.saturating_add(raw_h & 1);
    opts.limits.check_dims(raw_w, buf_h, 1)?;
    let (vis_w, vis_h) = visible_size(&model, raw_w, buf_h);
    let active = Rect::new(0, 0, vis_w, vis_h);
    let crop = match mn.crop {
        [Some(x), Some(y), Some(w), Some(h)] if w > 0 && h > 0 => Rect::new(x, y, w, h).intersect(&active),
        _ => None,
    }
    .unwrap_or(active);

    // Levels.
    let mut cblack = [0u32; 4];
    if let Some([r, g1, g2, b]) = mn.black {
        cblack = [r, g1, b, g2];
    }
    if bits == 14 {
        cblack = cblack.map(|v| v.wrapping_mul(4));
    }
    // No per-body white overrides: the E-300/E-330/E-500 samples are all 12bit-packed-padded and clip
    // at 4095 (E-330: 504781 pixels at 4095, E-500: 882), the Stylus 1 sample is 12-bit; the bit-depth
    // maximum or the camera table applies.
    let mut white = None;
    let norm = flashraw_camera_db::base_model("Olympus", &model);
    let entry = table::lookup("Olympus", norm).or_else(|| table::lookup("OM System", norm));
    let mut black = 0u32;
    table::apply_levels(entry, &mut black, &mut cblack, &mut white);
    db_lookup(&mut meta, layout.mode());

    let cfa =
        tiff.all_ifds().into_iter().find_map(|i| tiff_ep_cfa(i).filter(|c| c.is_bayer())).unwrap_or(CfaPattern::RGGB);
    let mut info = FrameInfo::new(raw_w, buf_h, bits as u8);
    info.layout = layout_for(&cfa);
    info.cfa = Some(cfa);
    info.black_level = cfa_black(&cfa, black, &cblack);
    info.white_level = WhiteLevel::uniform(white.unwrap_or((1u32 << bits.min(16)) - 1));
    info.white_balance = white_balance(mn.wb_rb);
    info.orientation = file_orientation(&tiff);
    info.color_matrices = table::color(entry, &meta);
    info.compression =
        if layout == Layout::Compressed { Compression::OlympusCompressed } else { Compression::Uncompressed };
    info.active_area = active;
    info.crop = crop;

    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);
    for &(o, l) in &mn.jpegs {
        push_jpeg(data, o, l, info.orientation, &mut meta.previews);
    }
    classify_previews(&mut meta.previews);

    // The payload runs to the end of the file: the extra row of an odd-height frame comes from
    // the bytes after the strip.
    let src = data.get(offset..).unwrap_or(&[]);
    let decoder = Decoder {
        data,
        src,
        offset,
        declared,
        layout,
        w: raw_w as usize,
        buf_h: buf_h as usize,
        coded_rows: raw_h as usize,
        big_endian: tiff.order == ByteOrder::Big,
        shift: if valid_bits == Some(10) { 6 } else { 4 },
        params: OlympusParams::from_tags(&mn.coder),
    };
    let mut dec =
        SimpleDecoder::new(RawFormat::Orf, meta, info, Box::new(move |ctx: &DecodeContext| decoder.decode(ctx)));
    if layout != Layout::Compressed {
        dec.random_access = RandomAccess::Rows;
    }
    if matches!(layout, Layout::Words12 | Layout::Msb12) {
        let stride = (raw_w as usize).saturating_mul(3) / 2;
        let need = stride.saturating_mul(raw_h as usize);
        if let Some(bytes) = src.get(..need) {
            dec.packed = Some(PackedPayload {
                data: bytes,
                width: raw_w,
                height: raw_h,
                bits: 12,
                order: if layout == Layout::Words12 { PackOrder::Msb32 } else { PackOrder::MsbBytes },
                row_stride: stride,
            });
        }
    }
    Ok(Box::new(dec))
}

// ---------------------------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------------------------

/// Everything the decode needs, copied out of the parsed file.
struct Decoder<'a> {
    data: &'a [u8],
    /// The file from the strip start to its end.
    src: &'a [u8],
    offset: usize,
    declared: usize,
    layout: Layout,
    w: usize,
    /// Buffer height (the coded rows rounded up to even).
    buf_h: usize,
    coded_rows: usize,
    big_endian: bool,
    /// Right shift of the unpacked 16-bit words.
    shift: u32,
    params: OlympusParams,
}

/// Samples the data did not provide, counted for the coded rows only.
struct Missing {
    w: usize,
    coded_rows: usize,
    filled: AtomicU64,
}

impl Missing {
    /// Row `y` got only `n` of its samples.
    fn row(&self, y: usize, n: usize) {
        if y < self.coded_rows && n < self.w {
            self.filled.fetch_add((self.w - n) as u64, Ordering::Relaxed);
        }
    }
}

/// Run `f(y, row)` over all rows in parallel chunks.
fn for_rows(out: &mut [u16], w: usize, ctx: &DecodeContext, f: impl Fn(usize, &mut [u16]) + Sync + Send) {
    par::for_each_row_chunk_bounded(out, w, par::memory_bound_workers(), ctx.parallel, |first, chunk| {
        for (i, row) in chunk.chunks_mut(w).enumerate() {
            f(first + i, row);
        }
    });
}

impl Decoder<'_> {
    /// The truncation of the strip's first `need` bytes, per the corruption policy.
    fn truncation(&self, ctx: &DecodeContext, need: usize) -> Result<Option<Truncation>> {
        Ok(ctx.sensor_bytes(self.data, DataUnit::Strip(0), self.offset, need)?.truncation)
    }

    fn decode(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let mut img = RawImage::new_u16(self.w as u32, self.buf_h as u32, 1, &ctx.limits)?;
        let out = img.u16_mut();
        let missing = Missing { w: self.w, coded_rows: self.coded_rows, filled: AtomicU64::new(0) };
        let truncation = match self.layout {
            Layout::Compressed => {
                ctx.profiler
                    .time(Stage::EntropyDecode, || {
                        codec::decode_with(self.src, &self.params, out, self.w, self.buf_h, ctx.parallel)
                    })
                    .map_err(|e| e.or_truncated(self.data.len(), DataUnit::Strip(0), self.offset, self.declared))?;
                return Ok(img);
            }
            Layout::Words12 | Layout::Msb12 => self.decode_words12(ctx, out, &missing)?,
            Layout::Interlaced12 => self.decode_interlaced(ctx, out, &missing)?,
            Layout::Padded12 => self.decode_padded(ctx, out, &missing)?,
            Layout::Unpacked => self.decode_unpacked(ctx, out, &missing)?,
        };
        if let Some(t) = truncation {
            ctx.recovered(CorruptReason::Truncated(t), missing.filled.into_inner());
        }
        Ok(img)
    }

    fn decode_words12(&self, ctx: &DecodeContext, out: &mut [u16], missing: &Missing) -> Result<Option<Truncation>> {
        let w = self.w;
        if w % 2 != 0 {
            return Err(Error::unsupported_compression("ORF", "12-bit packed rows with an odd width"));
        }
        let stride = w * 3 / 2;
        let truncation = self.truncation(ctx, stride.saturating_mul(self.coded_rows))?;
        let src = self.src;
        if self.layout == Layout::Words12 && stride % 4 != 0 {
            // Rows are not word aligned: one continuous stream of 12-bit samples.
            ctx.profiler.time(Stage::Unpack, || {
                let mut pump = BitPumpMsb32::new(src);
                for v in out.iter_mut() {
                    *v = pump.get(12) as u16;
                }
            });
            if truncation.is_some() {
                let complete = packed::complete_samples(src.len(), 12, PackOrder::Msb32).min(out.len());
                out[complete..].fill(0);
                missing.filled.store((w * self.coded_rows).saturating_sub(complete) as u64, Ordering::Relaxed);
            }
        } else {
            let order = if self.layout == Layout::Words12 { PackOrder::Msb32 } else { PackOrder::MsbBytes };
            let layout = PackedLayout { width: w, height: self.buf_h, bits: 12, order, row_stride: stride };
            // Rows past the strip read the bytes that follow it: always recover.
            ctx.profiler.time(Stage::Unpack, || packed::unpack(src, layout, out, w, ctx.parallel, ctx.simd, true))?;
            if truncation.is_some() {
                let g = RowGeometry::packed(self.coded_rows, w, 12, stride);
                missing.filled.store(g.missing_samples(src.len()), Ordering::Relaxed);
            }
        }
        Ok(truncation)
    }

    fn decode_interlaced(&self, ctx: &DecodeContext, out: &mut [u16], missing: &Missing) -> Result<Option<Truncation>> {
        const FIELD_ALIGN: usize = 2048;
        let w = self.w;
        let stride = w * 12 / 8;
        let half = self.buf_h.div_ceil(2);
        let second = half.saturating_mul(stride).div_ceil(FIELD_ALIGN).saturating_mul(FIELD_ALIGN);
        let need = second.saturating_add(stride.saturating_mul(self.coded_rows / 2));
        let truncation = self.truncation(ctx, need)?;
        let (src, simd, truncated) = (self.src, ctx.simd, truncation.is_some());
        ctx.profiler.time(Stage::Unpack, || {
            for_rows(out, w, ctx, |y, row| {
                let at = if y % 2 == 0 { (y / 2) * stride } else { second.saturating_add((y / 2) * stride) };
                let s = src.get(at..).unwrap_or(&[]);
                if let Some(full) = s.get(..stride) {
                    packed::unpack_row(full, row, 12, PackOrder::MsbBytes, simd);
                } else if truncated {
                    let n = packed::unpack_row_partial(s, row, 12, PackOrder::MsbBytes, simd);
                    missing.row(y, n);
                } else if !s.is_empty() {
                    packed::unpack_row(s, row, 12, PackOrder::MsbBytes, simd);
                }
            });
        });
        Ok(truncation)
    }

    fn decode_padded(&self, ctx: &DecodeContext, out: &mut [u16], missing: &Missing) -> Result<Option<Truncation>> {
        /// Data bytes and total bytes of a group: 10 samples, then one pad byte.
        const GROUP_DATA: usize = 15;
        const GROUP: usize = 16;
        let w = self.w;
        if w % 10 != 0 {
            return Err(Error::unsupported_compression("ORF", "padded 12-bit rows not a multiple of 10 pixels"));
        }
        let stride = w / 10 * GROUP;
        let geometry = RowGeometry { rows: self.coded_rows, stride, row_bytes: stride.saturating_sub(1), samples: w };
        let truncation = self.truncation(ctx, geometry.needed())?;
        let (src, coded_rows, truncated) = (self.src, self.coded_rows, truncation.is_some());
        ctx.profiler.time(Stage::Unpack, || {
            for_rows(out, w, ctx, |y, row| {
                let mut done = 0;
                for (g, px) in row.chunks_mut(10).enumerate() {
                    let at = y.saturating_mul(stride).saturating_add(g * GROUP);
                    let s = src.get(at..).unwrap_or(&[]);
                    let avail = s.len().min(GROUP);
                    if avail == GROUP || (avail == GROUP_DATA && y < coded_rows) {
                        packed::unpack_row(&s[..GROUP_DATA], px, 12, PackOrder::LsbBytes, false);
                        done += px.len();
                    } else {
                        if truncated {
                            done += packed::unpack_row_partial(&s[..avail], px, 12, PackOrder::LsbBytes, false);
                        }
                        break;
                    }
                }
                if truncated {
                    missing.row(y, done);
                }
            });
        });
        Ok(truncation)
    }

    fn decode_unpacked(&self, ctx: &DecodeContext, out: &mut [u16], missing: &Missing) -> Result<Option<Truncation>> {
        let w = self.w;
        let truncation = self.truncation(ctx, w.saturating_mul(2).saturating_mul(self.coded_rows))?;
        let order = if self.big_endian { PackOrder::MsbBytes } else { PackOrder::LsbBytes };
        let (src, simd, shift, truncated) = (self.src, ctx.simd, self.shift, truncation.is_some());
        ctx.profiler.time(Stage::Unpack, || {
            for_rows(out, w, ctx, |y, row| {
                let s = src.get(y.saturating_mul(w * 2)..).unwrap_or(&[]);
                if let Some(full) = s.get(..w * 2) {
                    packed::unpack_row(full, row, 16, order, simd);
                } else if truncated {
                    let n = packed::unpack_row_partial(s, row, 16, order, simd);
                    missing.row(y, n);
                } else {
                    return;
                }
                row.iter_mut().for_each(|v| *v >>= shift);
            });
        });
        Ok(truncation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{self, TiffWriter, ascii, long, short};

    #[test]
    fn probe_magics() {
        assert!(probe(b"IIRO\x08\0\0\0"));
        assert!(probe(b"IIRS"));
        assert!(probe(b"MMOR"));
        assert!(!probe(b"IIU\0"));
        assert!(!probe(b"II"));
        assert!(!probe(b""));
    }

    #[test]
    fn visible_widths() {
        assert_eq!(visible_size("E-M1", 4100, 3000), (4096, 3000));
        assert_eq!(visible_size("E-M5", 10400, 7000), (10388, 7000));
        assert_eq!(visible_size("X", 9280, 7000), (9274, 6994));
        assert_eq!(visible_size("TG-6", 4000, 3000), (3974, 3000));
        assert_eq!(visible_size("STYLUS1s", 4100, 3000), (4080, 3000));
        assert_eq!(visible_size("E-300", 5, 5), (0, 5));
        assert_eq!(visible_size("tg-6", 4000, 3000), (4000, 3000));
    }

    #[test]
    fn layout_choice() {
        let pick = |bps, photo, bytes, area| {
            LAYOUT_RULES.iter().find(|r| r.matches(bps, photo, bytes, area)).map(|r| r.layout)
        };
        assert_eq!(pick(12, None, 8, 5), Some(Layout::Padded12));
        assert_eq!(pick(12, Some(2), 1, 1), Some(Layout::Interlaced12));
        assert_eq!(pick(12, None, 3, 2), Some(Layout::Words12));
        assert_eq!(pick(12, None, 5, 2), Some(Layout::Msb12));
        assert_eq!(pick(16, None, 100, 100), Some(Layout::Compressed));
        assert_eq!(pick(16, None, 100, 1000), Some(Layout::Unpacked));
        assert_eq!(pick(14, None, 100, 100), Some(Layout::Unpacked));
        assert_eq!(pick(8, None, 100, 100), None);
    }

    #[test]
    fn white_balance_plausibility() {
        assert!(white_balance([Some(512), Some(256)]).is_some());
        assert!(white_balance([Some(0), Some(256)]).is_none());
        assert!(white_balance([None, Some(256)]).is_none());
        assert!(white_balance([Some(256 * 200), Some(256)]).is_none());
    }

    /// An ORF whose payload follows the directory.
    fn orf(model: &str, w: u32, h: u32, bps: u16, payload: &[u8], strip_len: u32) -> Vec<u8> {
        let tags = |off: u32| {
            vec![
                ascii(0x010F, "OLYMPUS"),
                ascii(0x0110, model),
                long(0x0100, &[w]),
                long(0x0101, &[h]),
                short(0x0102, &[bps]),
                short(0x0103, &[1]),
                long(0x0111, &[off]),
                long(0x0117, &[strip_len]),
            ]
        };
        let mut measure = TiffWriter::new();
        measure.ifd(tags(0));
        let mut t = TiffWriter::new();
        let ifd = t.ifd(tags(measure.buf.len() as u32));
        t.buf.extend(payload);
        t.finish(ifd)
    }

    #[test]
    fn unpacked_odd_height_reads_the_extra_row_after_the_strip() {
        let (w, h) = (8usize, 3usize);
        let coded: Vec<u16> = (0..w * h).map(|i| ((i as u16) * 16 + 16) & 0xFFF0).collect();
        let extra: Vec<u16> = (0..w).map(|i| (i as u16 + 1) << 4).collect();
        let payload: Vec<u8> = coded.iter().chain(&extra).flat_map(|v| v.to_le_bytes()).collect();
        let file = orf("E-1", w as u32, h as u32, 14, &payload, (w * h * 2) as u32);
        let img = crate::nikon::conventions::robustness::open_decode(&file).unwrap();
        assert_eq!((img.width as usize, img.height as usize), (w, h + 1));
        let expect: Vec<u16> = coded.iter().chain(&extra).map(|v| v >> 4).collect();
        assert_eq!(img.u16(), &expect[..]);
        // Without the bytes after the strip the extra row stays zero.
        let file = orf("E-1", w as u32, h as u32, 14, &payload[..w * h * 2], (w * h * 2) as u32);
        let img = crate::nikon::conventions::robustness::open_decode(&file).unwrap();
        assert!(img.u16()[w * h..].iter().all(|&v| v == 0));
    }

    /// 12-bit LSB-first samples, 10 per 15 bytes plus a pad byte.
    fn pack_padded(samples: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        for group in samples.chunks(10) {
            let mut bits = 0u128;
            for (i, &s) in group.iter().enumerate() {
                bits |= u128::from(s) << (12 * i);
            }
            out.extend(&bits.to_le_bytes()[..15]);
            out.push(0);
        }
        out
    }

    #[test]
    fn padded_rows_and_their_truncation() {
        let (w, h) = (20usize, 4usize);
        let samples: Vec<u16> = (0..w * h).map(|i| ((i * 53) % 4096) as u16).collect();
        let payload = pack_padded(&samples);
        let file = orf("E-500", w as u32, h as u32, 12, &payload, payload.len() as u32);
        let img = crate::nikon::conventions::robustness::open_decode(&file).unwrap();
        assert_eq!(img.u16(), &samples[..]);

        let cut = &file[..file.len() - 40];
        let dec = crate::open(cut, &OpenOptions::default()).unwrap();
        let prof = Profiler::new();
        let mut ctx = DecodeContext::new(&prof);
        ctx.recover = false;
        assert!(dec.decode(&ctx).is_err_and(|e| e.truncation().is_some()));
        let mut ctx = DecodeContext::new(&prof);
        ctx.recover = true;
        let img = dec.decode(&ctx).unwrap();
        assert_eq!(&img.u16()[..2 * w], &samples[..2 * w]);
        let warnings = ctx.take_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].samples_filled > 0);
    }

    #[test]
    fn mutated_real_files_do_not_panic() {
        for rel in [
            "Olympus/E-500/_1010010.ORF",
            "Olympus/E-1/E_1__C106743_gredos.ORF",
            "Olympus/SP500UZ/RAW_OLYMPUS_SP500UZ.ORF",
        ] {
            if let Some(d) = testkit::corpus(rel) {
                crate::nikon::conventions::robustness::mutate(&d, 13);
            }
        }
    }

    /// Fingerprints of what the reader reports for the corpus files (see `testkit::check_golden`).
    #[test]
    #[ignore = "needs the corpus"]
    fn golden_metadata_of_corpus_files() {
        testkit::check_golden(GOLDEN);
    }

    const GOLDEN: &[(&str, &str)] = &[
        ("OLYMPUS/STYLUS1,1s/P4155261.ORF", "d4533d57de665e55"),
        ("OM System/OM-1/OM_Systems_OM1_RAW_image_HiRes_80mpx_P1010027.ORI", "56b69de31739e59f"),
        ("OM System/TG-7/UWAG0002.ORF", "e940b65bdaa8cba9"),
        ("Olympus/C5050Z/RAW_OLYMPUS_C5050Z.ORF", "91d6984ce3247769"),
        ("Olympus/C5060WZ/C5060WZ_5_7_22_8mm__27_110mm_equiv__F_2_8_8P4141018_cabo_de_gata.ORF", "d4da5e9b35f8cc63"),
        ("Olympus/C7070WZ/PC270085.ORF", "dff28522fd7f9a5a"),
        ("Olympus/C8080WZ/RAW_OLYMPUS_C8080.ORF", "9f9bef7c680ba425"),
        ("Olympus/E-1/E_1__C106743_gredos.ORF", "5b363c1c910166b6"),
        ("Olympus/E-10/P7198607.ORF", "c253fec3a28b579f"),
        ("Olympus/E-20/RAW_OLYMPUS_E20.ORF", "71ea5dac4a6476c1"),
        ("Olympus/E-3/E_3__3099164_casco_antiguo_petrer.ORF", "6833ebbc8450c495"),
        ("Olympus/E-30/RAW_OLYMPUS_E30.ORF", "1af257a97e39a0d9"),
        ("Olympus/E-300/P1252148.ORF", "4debda2ce8cdbd04"),
        ("Olympus/E-330/P3307182.ORF", "5f8968d4502f741a"),
        ("Olympus/E-400/_A270187.ORF", "4640f8b036fffbe1"),
        ("Olympus/E-500/_1010010.ORF", "c4b36d28dd99dbf7"),
        ("Olympus/E-M10 Mark IV/PA040002.ORF", "56024faa113154b3"),
        ("Olympus/E-M1MarkII/Olympus_EM1mk2_Standard_20MP.ORF", "f5a374d8b0bd8988"),
        ("Olympus/E-M5 Mark III/PB240241.ORF", "0005b76e18549e0a"),
        ("Olympus/E-P3/RAW_OLYMPUS_EP3.ORF", "75789fd254d46b26"),
        ("Olympus/E-P5/OLYMPUS_PEN_E-P5_ISO200_IT8COLOR.ORF", "3a6a766fb76f4123"),
        ("Olympus/SH-2/PC260009.ORF", "c736c2fccf50542f"),
        ("Olympus/SP500UZ/RAW_OLYMPUS_SP500UZ.ORF", "1732072933e96c8f"),
        ("Olympus/SP565UZ/2019-09-26--10.03.53001.ORF", "bf7baa93ee8561a9"),
        ("Olympus/Stylus1/OLYMPUS-STYLUS1.ORF", "f9794743bd81d2f6"),
        ("Olympus/TG-4/P1170373.ORF", "bacb818c81b8177c"),
        ("Olympus/TG-5/P9100007.ORF", "46d15ad1e2c4e897"),
        ("Olympus/TG-6/_6100003.ORF", "81caae22655eb566"),
        ("Olympus/XZ-1/p1319978.orf", "311daef2eccc3d10"),
        ("Olympus/XZ-10/P1240016.ORF", "cdea07bc40c550da"),
        ("Olympus/XZ-2/p7166537.orf", "b930a09bc9c7ced3"),
    ];
}
