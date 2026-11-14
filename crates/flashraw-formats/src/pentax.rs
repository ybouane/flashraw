//! Pentax / Ricoh PEF files (and the Pentax-built Samsung GX bodies).
//!
//! A PEF is a TIFF whose Make tag selects this reader. The Pentax maker note carries the black
//! point, the as-shot white balance and the position of the Huffman table; the sensor payload is
//! Pentax-Huffman coded (compression 65535) or, on the first generation of bodies, plain 16-bit
//! words. The sensor buffer of each body carries dark or blank margins, cut by the per-body rules
//! of [`AREA_RULES`].

use crate::common::*;
use crate::nikon::conventions::*;
use crate::nikon::packed_geometry;
use flashraw_codecs::nikon_huffman::DirectHuff;
use flashraw_codecs::nikon_packed::{self, PackedSpec};
use flashraw_codecs::packed::{self, PackedLayout};
use flashraw_codecs::pentax_huffman;
use flashraw_core::tiff::{Entry, Tiff, TiffOptions, parse_ifd_at, tags};
use flashraw_core::*;

/// PEF is TIFF based and dispatched by make: never matches by signature.
pub fn probe(_data: &[u8]) -> bool {
    false
}

/// PEF files have no signature of their own: always an error, use [`open_tiff`].
pub fn open<'a>(_data: &'a [u8], _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("PEF files are TIFF-based; use `open_tiff`".into()))
}

// ---------------------------------------------------------------------------------------------
// Maker note
// ---------------------------------------------------------------------------------------------

/// How a maker note starts and where its directory is. Verified: `AOC` and `PENTAX` on corpus
/// files; `RICOH` has no sample file; a note with an unknown header is parsed as a bare
/// directory (defined behaviour).
#[derive(Clone, Copy)]
struct Framing {
    magic: &'static [u8],
    /// Position of the two byte-order letters after the magic (`None`: the file's order).
    order_at: Option<usize>,
    /// Directory position, from the start of the note (`rel_note`) or of the file.
    dir_at: usize,
    /// Offsets inside the note are relative to the note (else to the file).
    rel_note: bool,
}

const FRAMINGS: [Framing; 3] = [
    Framing { magic: b"PENTAX \0", order_at: Some(8), dir_at: 10, rel_note: true },
    Framing { magic: b"RICOH\0", order_at: Some(6), dir_at: 8, rel_note: true },
    Framing { magic: b"AOC\0", order_at: None, dir_at: 6, rel_note: false },
];

/// Fewest bytes a maker note must have for the framing to be examined at all.
const FRAMING_PROBE: usize = 10;

mod mn_tag {
    pub const ISO_CODE: u16 = 0x0014;
    pub const BLACK_POINT: u16 = 0x0200;
    pub const WHITE_POINT: u16 = 0x0201;
    pub const HUFFMAN_TABLE: u16 = 0x0220;
}

#[derive(Default)]
struct MakerNote {
    /// Byte order of the note, when its directory parsed.
    order: Option<ByteOrder>,
    iso: Option<u32>,
    /// R G G B.
    black: Option<[u16; 4]>,
    /// R G G B as-shot multipliers.
    wb: Option<[u16; 4]>,
    /// Absolute file offset of the Huffman table.
    huffman_at: Option<usize>,
}

fn four_u16(e: &Entry) -> Option<[u16; 4]> {
    Some([e.u16(0)?, e.u16(1)?, e.u16(2)?, e.u16(3)?])
}

fn read_maker_note(data: &[u8], tiff: &Tiff) -> MakerNote {
    let mut mn = MakerNote::default();
    let Some(entry) = tiff.find_entry(tags::MAKER_NOTE) else { return mn };
    let pos = entry.value_pos;
    let Some(head) = pos.checked_add(FRAMING_PROBE).and_then(|end| data.get(pos..end)) else { return mn };
    let framing = FRAMINGS.iter().find(|f| head.starts_with(f.magic));
    let (order, base, dir_offset) = match framing {
        Some(f) => {
            let order = match f.order_at.map(|o| &head[o..o + 2]) {
                None => tiff.order,
                Some(b"II") => ByteOrder::Little,
                Some(b"MM") => ByteOrder::Big,
                Some(_) => return mn,
            };
            if f.rel_note { (order, pos, f.dir_at) } else { (order, 0, pos.saturating_add(f.dir_at)) }
        }
        None => (tiff.order, 0, pos),
    };
    let Ok(dir_offset) = u32::try_from(dir_offset) else { return mn };
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    let Ok(ifd) = parse_ifd_at(&data[base..], order, dir_offset, opts) else { return mn };
    mn.order = Some(order);
    // Later entries win.
    for e in &ifd.entries {
        match e.tag {
            mn_tag::ISO_CODE => mn.iso = e.u32(0).map(iso_from_code).filter(|&i| i > 0),
            mn_tag::BLACK_POINT => mn.black = four_u16(e),
            mn_tag::WHITE_POINT => mn.wb = four_u16(e),
            mn_tag::HUFFMAN_TABLE => mn.huffman_at = Some(base.saturating_add(e.value_pos)),
            _ => {}
        }
    }
    mn
}

/// Maker-note ISO codes and their speeds (public tag documentation). Codes 6, 8, 9, 12 and 15
/// agree with the EXIF ISO of sample files; the others are unverified.
const ISO_CODES: [(u32, u32); 14] = [
    (3, 50),
    (4, 64),
    (5, 80),
    (6, 100),
    (7, 125),
    (8, 160),
    (9, 200),
    (10, 250),
    (11, 320),
    (12, 400),
    (15, 800),
    (18, 1600),
    (21, 3200),
    (24, 6400),
];

/// Codes from here on are taken to be the speed itself (unverified).
const ISO_PASSTHROUGH_FROM: u32 = 50;

fn iso_from_code(code: u32) -> u32 {
    match ISO_CODES.iter().find(|&&(c, _)| c == code) {
        Some(&(_, iso)) => iso,
        None if code >= ISO_PASSTHROUGH_FROM => code,
        None => 0,
    }
}

// ---------------------------------------------------------------------------------------------
// Visible area
// ---------------------------------------------------------------------------------------------

/// Which bodies a rule applies to (compared with the upper-cased model name).
#[derive(Clone, Copy)]
enum Body {
    Any,
    OneOf(&'static [&'static str]),
    StartsWith(&'static str),
}

/// Which buffer sizes a rule applies to.
#[derive(Clone, Copy)]
enum Buffer {
    Any,
    Width(u32),
    WidthAtLeast(u32),
    Size(u32, u32),
}

/// What a rule does to one coordinate of the visible area.
#[derive(Clone, Copy)]
enum Edit {
    Keep,
    Set(u32),
    /// Buffer extent minus a margin (saturating).
    FromEnd(u32),
    /// Current value minus a margin (saturating).
    Minus(u32),
}

impl Edit {
    fn apply(self, current: u32, buffer: u32) -> u32 {
        match self {
            Edit::Keep => current,
            Edit::Set(v) => v,
            Edit::FromEnd(m) => buffer.saturating_sub(m),
            Edit::Minus(m) => current.saturating_sub(m),
        }
    }
}

/// One entry of the visible-area table. Rules apply in table order and edit only the fields
/// they name. Flags (from the corpus): `edge` = image statistics place a border there;
/// `sample` = a corpus file reproduces the values; `none` = no sample.
struct AreaRule {
    body: Body,
    buffer: Buffer,
    /// New x, y, width and height of the visible area.
    edits: [Edit; 4],
    /// CFA phase of the visible area, when it differs from the sensor's own.
    cfa: Option<CfaPattern>,
}

use Buffer::{Any as AnySize, Size, Width, WidthAtLeast};
use Edit::{FromEnd, Keep, Minus, Set};

const fn rule(body: Body, buffer: Buffer, edits: [Edit; 4], cfa: Option<CfaPattern>) -> AreaRule {
    AreaRule { body, buffer, edits, cfa }
}

/// The visible-area table, in application order.
const AREA_RULES: [AreaRule; 14] = [
    // K10D, K200D, K-m, K2000. edge.
    rule(Body::Any, Size(3936, 2624), [Set(0), Set(0), Set(3896), Set(2616)], None),
    // K20D. sample.
    rule(Body::Any, Size(4864, 3136), [Set(0), Set(0), Set(4688), Set(3124)], Some(CfaPattern::BGGR)),
    // K-r, K-x. edge.
    rule(Body::OneOf(&["K-R", "K-X"]), Width(4352), [Keep, Keep, Set(4309), Keep], Some(CfaPattern::BGGR)),
    // K-5 family. edge.
    rule(
        Body::OneOf(&["K-5", "K-5 II", "K-5 II S"]),
        WidthAtLeast(4960),
        [Set(10), Keep, Set(4950), Keep],
        Some(CfaPattern::BGGR),
    ),
    // K-70. sample.
    rule(Body::OneOf(&["K-70"]), Width(6080), [Set(60), Set(32), Set(6020), Set(4016)], None),
    // K-7. sample.
    rule(Body::OneOf(&["K-7"]), Width(4736), [Set(0), Set(2), Set(4684), Set(3122)], Some(CfaPattern::BGGR)),
    // KF. sample.
    rule(Body::OneOf(&["KF"]), Width(6080), [Set(56), Set(28), Set(6024), Set(4024)], None),
    // K-3, K-3 II. sample.
    rule(Body::OneOf(&["K-3 II", "K-3"]), Width(6080), [Set(4), Keep, Set(6040), Keep], None),
    // K-3 Mark III and its monochrome version. sample.
    rule(Body::StartsWith("K-3 MARK III"), Width(6304), [Set(26), Set(34), Set(6224), Set(4160)], None),
    // KP. sample.
    rule(Body::OneOf(&["KP"]), Width(6112), [Set(54), Set(28), Set(6028), FromEnd(28)], None),
    // 645D. sample.
    rule(Body::OneOf(&["645D"]), Width(7424), [Set(48), Set(29), Set(7328), Set(5502)], Some(CfaPattern::GRBG)),
    // K-1, K-1 Mark II, any buffer size. sample.
    rule(Body::OneOf(&["K-1", "K-1 MARK II"]), AnySize, [Keep, Set(18), Keep, FromEnd(18)], None),
    // K-1 family, full-size buffer. sample.
    rule(Body::OneOf(&["K-1", "K-1 MARK II"]), Width(7392), [Set(6), Keep, Set(7376), Keep], None),
    // *ist DS: two rows of the buffer carry no image. sample.
    rule(Body::OneOf(&["*IST DS"]), AnySize, [Keep, Keep, Keep, Minus(2)], None),
];

impl AreaRule {
    fn applies(&self, model_uc: &str, w: u32, h: u32) -> bool {
        let body = match self.body {
            Body::Any => true,
            Body::OneOf(names) => names.contains(&model_uc),
            Body::StartsWith(p) => model_uc.starts_with(p),
        };
        let buffer = match self.buffer {
            Buffer::Any => true,
            Buffer::Width(v) => w == v,
            Buffer::WidthAtLeast(v) => w >= v,
            Buffer::Size(bw, bh) => (w, h) == (bw, bh),
        };
        body && buffer
    }
}

/// The visible area of a `w x h` buffer and the CFA phase of that area (if it differs).
fn visible_area(model_uc: &str, w: u32, h: u32) -> (Rect, Option<CfaPattern>) {
    let mut area = Rect::new(0, 0, w, h);
    let mut cfa = None;
    for r in AREA_RULES.iter().filter(|r| r.applies(model_uc, w, h)) {
        let [x, y, rw, rh] = r.edits;
        area = Rect::new(x.apply(area.x, w), y.apply(area.y, h), rw.apply(area.width, w), rh.apply(area.height, h));
        cfa = r.cfa.or(cfa);
    }
    (area, cfa)
}

// ---------------------------------------------------------------------------------------------
// Payload
// ---------------------------------------------------------------------------------------------

/// How the sensor samples are stored.
enum Payload {
    /// Compression 65535: Pentax Huffman with the table from the maker note or the default one.
    Huffman(DirectHuff),
    /// 16-bit words in the file's byte order (12 or 14 significant bits in the low bits).
    Words(ByteOrder),
    /// Plain MSB-first packed rows (no sample file).
    Packed(PackedSpec),
}

const COMPRESSION_HUFFMAN: u32 = 65535;
const COMPRESSION_NONE: u32 = 1;
/// A PackBits label that Pentax uses for plain packed data.
const COMPRESSION_PACKBITS_LABEL: u32 = 32773;

fn choose_payload(
    data: &[u8],
    tiff: &Tiff,
    mn: &MakerNote,
    compression: u32,
    (w, h, bps): (u32, u32, u32),
    bytes: usize,
) -> Result<Payload> {
    match compression {
        COMPRESSION_HUFFMAN => {
            let order = mn.order.unwrap_or(tiff.order);
            let table = match mn.huffman_at {
                Some(at) => pentax_huffman::table_from_makernote(data.get(at..).unwrap_or(&[]), order)?,
                None => pentax_huffman::default_table()?,
            };
            Ok(Payload::Huffman(table))
        }
        COMPRESSION_NONE | COMPRESSION_PACKBITS_LABEL => {
            if bytes as u64 == w as u64 * h as u64 * 2 || matches!(bps, 14 | 16) {
                Ok(Payload::Words(tiff.order))
            } else if (1..=16).contains(&bps) {
                Ok(Payload::Packed(PackedSpec {
                    width: w as usize,
                    height: h as usize,
                    bits: bps,
                    flags: 0,
                    field2_offset: None,
                }))
            } else {
                Err(Error::unsupported_compression("PEF", format!("TIFF compression {compression} ({bps} bps)")))
            }
        }
        c => Err(Error::unsupported_compression("PEF", format!("TIFF compression {c} ({bps} bps)"))),
    }
}

fn pack_order(order: ByteOrder) -> PackOrder {
    if order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes }
}

// ---------------------------------------------------------------------------------------------
// Open
// ---------------------------------------------------------------------------------------------

const PHOTOMETRIC_LINEAR_RAW: u32 = 34892;

/// Open a PEF (or a Samsung-branded Pentax file forwarded by the Samsung reader).
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("PEF without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let model = strip_make_prefix(&strip_make_prefix(&meta.model, "PENTAX"), "RICOH");
    let model_uc = model.to_ascii_uppercase();

    let mn = read_maker_note(data, &tiff);
    if meta.exposure.iso.is_none() {
        meta.exposure.iso = mn.iso;
    }

    let raw = select_raw_ifd(&tiff, false)
        .ok_or_else(|| no_raw_ifd(&tiff, "PEF", Error::invalid_metadata("PEF has no raw image IFD")))?;
    let (w, h) = (raw.width(), raw.height());
    opts.limits.check_dims(w, h, 1)?;
    let bps = raw.bps();
    let (offset, declared) =
        *strips(raw).first().ok_or_else(|| Error::invalid_metadata("PEF raw IFD without strips"))?;
    let src = data.get(offset..).unwrap_or(&[]);
    let payload = choose_payload(data, &tiff, &mn, raw.compression(), (w, h, bps), declared)?;
    let huffman = matches!(payload, Payload::Huffman(_));

    // Geometry.
    let monochrome = raw.u32(tags::PHOTOMETRIC) == Some(PHOTOMETRIC_LINEAR_RAW) || model_uc.contains("MONOCHROME");
    let (area, cfa_visible) = visible_area(&model_uc, w, h);
    let cfa_override = cfa_visible.map(|c| cfa_at_buffer_origin(c, area.x, area.y));
    let area = even_margins(area);

    // Levels.
    let cam = db_lookup(&mut meta, if huffman { "huffman" } else { "" });
    let table_model = if model_uc == "K2000" { "K-m" } else { meta.canonical_model.as_str() };
    let table = flashraw_camera_db::find_matrix("Pentax", table_model)
        .or_else(|| flashraw_camera_db::find_matrix("Pentax", &model));
    let mut base_black = 0u32;
    let mut cblack = mn.black.map_or([0u32; 4], |b| b.map(u32::from));
    let mut maximum = 0u32;
    apply_table_levels(table, &mut base_black, &mut cblack, &mut maximum);
    if maximum == 0 {
        maximum = (1u32 << bps.clamp(1, 16)) - 1;
    }

    let mut info = FrameInfo::new(w, h, bps.min(16) as u8);
    info.compression = if huffman { Compression::PentaxHuffman } else { Compression::Uncompressed };
    info.active_area = area;
    info.crop = area;
    info.white_level = WhiteLevel::uniform(maximum);
    info.orientation = file_orientation(&tiff);
    if monochrome {
        info.layout = SensorLayout::Monochrome;
        info.cfa = None;
        info.black_level = BlackLevel::uniform((base_black + cblack[0]) as f32);
    } else {
        let cfa = cfa_override
            .or_else(|| tiff_ep_cfa(raw))
            .or_else(|| cam.and_then(|c| c.cfa).and_then(parse_db_cfa))
            .unwrap_or(CfaPattern::RGGB);
        info.layout = layout_for(&cfa);
        info.cfa = Some(cfa);
        info.black_level = black_for_cfa(&cfa, base_black, cblack);
        info.white_balance = mn.wb.and_then(|m| wb_from_cam_mul([m[0] as f32, m[1] as f32, m[3] as f32, m[2] as f32]));
    }
    info.color_matrices = color_from_table(table, &meta, cam);
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);

    // The row layout, when the payload has one.
    let row_layout = match &payload {
        Payload::Packed(spec) => spec.fast_layout(),
        Payload::Words(order) => Some(PackedLayout::tight(w as usize, h as usize, 16, pack_order(*order))),
        Payload::Huffman(_) => None,
    };
    let packed = match (&payload, row_layout) {
        (Payload::Packed(spec), Some(l)) => Some(PackedPayload {
            data: src,
            width: w,
            height: h,
            bits: spec.bits,
            order: l.order,
            row_stride: l.row_stride,
        }),
        _ => None,
    };

    let (width, height) = (w as usize, h as usize);
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
        let out = img.u16_mut();
        let unit = DataUnit::Strip(0);
        match &payload {
            Payload::Huffman(table) => {
                let dead_row = ctx
                    .profiler
                    .time(Stage::EntropyDecode, || {
                        pentax_huffman::decode(src, table, width, height, out, width, ctx.parallel, declared)
                    })
                    .map_err(|e| e.or_truncated(data.len(), unit, offset, declared))?;
                finish_truncated_stream(ctx, data.len(), unit, offset, declared, out, width, height, dead_row)?;
            }
            Payload::Packed(spec) => {
                let rows = ctx.sensor_rows(data, unit, offset, packed_geometry(spec))?;
                ctx.profiler.time(Stage::Unpack, || {
                    nikon_packed::decode(rows, spec, out, width, ctx.parallel, ctx.simd, ctx.recover)
                })?;
            }
            Payload::Words(order) => {
                let layout = PackedLayout::tight(width, height, 16, pack_order(*order));
                let rows = ctx.sensor_rows(data, unit, offset, row_geometry(&layout, height))?;
                ctx.profiler.time(Stage::Unpack, || {
                    packed::unpack(rows, layout, out, width, ctx.parallel, ctx.simd, ctx.recover)
                })?;
            }
        }
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(RawFormat::Pef, meta, info, Box::new(decode));
    dec.units = vec![DecodeUnit { rect: Rect::full(w, h), offset: offset as u64, length: declared as u64 }];
    dec.packed = packed;
    if let Some(layout) = row_layout {
        dec.random_access = RandomAccess::Rows;
        dec.region = Some(Box::new(move |ctx, rect| {
            decode_rows_region(data, offset, DataUnit::Strip(0), RowLayout { layout, shift: 0 }, rect, ctx)
        }));
    }
    Ok(Box::new(dec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{self, TiffWriter, ascii, long, short};

    #[test]
    fn iso_codes() {
        assert_eq!(iso_from_code(6), 100);
        assert_eq!(iso_from_code(18), 1600);
        assert_eq!(iso_from_code(400), 400);
        assert_eq!(iso_from_code(1), 0);
        assert_eq!(iso_from_code(49), 0);
        assert_eq!(iso_from_code(3), 50);
    }

    #[test]
    fn no_signature_of_its_own() {
        assert!(!probe(b"AOC\0MM\0\0\0\0"));
        assert!(matches!(open(&[], &OpenOptions::default()), Err(Error::UnsupportedFormat(_))));
    }

    #[test]
    fn visible_area_rules() {
        let r = Rect::new;
        let cases: &[(&str, u32, u32, Rect, Option<CfaPattern>)] = &[
            ("K10D", 3936, 2624, r(0, 0, 3896, 2616), None),
            ("K20D", 4864, 3136, r(0, 0, 4688, 3124), Some(CfaPattern::BGGR)),
            ("K-R", 4352, 2868, r(0, 0, 4309, 2868), Some(CfaPattern::BGGR)),
            ("K-X", 4000, 2868, r(0, 0, 4000, 2868), None),
            ("K-5 II", 4992, 3284, r(10, 0, 4950, 3284), Some(CfaPattern::BGGR)),
            ("K-5", 4000, 3000, r(0, 0, 4000, 3000), None),
            ("K-7", 4736, 3144, r(0, 2, 4684, 3122), Some(CfaPattern::BGGR)),
            ("K-70", 6080, 4044, r(60, 32, 6020, 4016), None),
            ("KF", 6080, 4044, r(56, 28, 6024, 4024), None),
            ("K-3", 6080, 4044, r(4, 0, 6040, 4044), None),
            ("K-3 MARK III MONOCHROME", 6304, 4200, r(26, 34, 6224, 4160), None),
            ("KP", 6112, 4100, r(54, 28, 6028, 4072), None),
            ("645D", 7424, 5560, r(48, 29, 7328, 5502), Some(CfaPattern::GRBG)),
            ("K-1", 7392, 4950, r(6, 18, 7376, 4932), None),
            ("K-1 MARK II", 4832, 3238, r(0, 18, 4832, 3220), None),
            ("*IST DS", 3008, 2008, r(0, 0, 3008, 2006), None),
            ("*IST DS", 3008, 1, r(0, 0, 3008, 0), None),
            ("OTHER", 3936, 2625, r(0, 0, 3936, 2625), None),
        ];
        for &(model, w, h, area, cfa) in cases {
            assert_eq!(visible_area(model, w, h), (area, cfa), "{model} {w}x{h}");
        }
    }

    /// A little-endian PEF of 16-bit words, `w x h`, the payload right after the header.
    fn words_pef(model: &str, w: u32, h: u32, payload: &[u8]) -> Vec<u8> {
        let mut t = TiffWriter::new();
        t.buf.extend(payload);
        let ifd = t.ifd(vec![
            ascii(0x010F, "PENTAX Corporation"),
            ascii(0x0110, model),
            long(0x0100, &[w]),
            long(0x0101, &[h]),
            short(0x0102, &[16]),
            short(0x0103, &[1]),
            long(0x0111, &[8]),
            long(0x0117, &[payload.len() as u32]),
        ]);
        t.finish(ifd)
    }

    #[test]
    fn word_payload_decodes_and_regions_match() {
        let (w, h) = (24u32, 10u32);
        let samples: Vec<u16> = (0..w * h).map(|i| ((i * 37) % 4096) as u16).collect();
        let payload: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        let file = words_pef("PENTAX K100D", w, h, &payload);
        let img = crate::nikon::conventions::robustness::open_decode(&file).unwrap();
        assert_eq!(img.u16(), &samples[..]);
        assert!(crate::nikon::conventions::robustness::region_matches_full(&file));
        let dec = crate::open(&file, &OpenOptions::default()).unwrap();
        assert_eq!(dec.format(), RawFormat::Pef);
        assert_eq!(dec.frame_info().white_level.values[0], 65535);
        assert_eq!(dec.units().len(), 1);
    }

    #[test]
    fn truncated_words_are_recovered_or_rejected() {
        let (w, h) = (24u32, 10u32);
        let payload = vec![7u8; (w * h * 2) as usize];
        let file = words_pef("PENTAX K100D", w, h, &payload);
        let cut = &file[..file.len() - 100];
        let dec = crate::open(&file[..], &OpenOptions::default()).unwrap();
        drop(dec);
        let prof = Profiler::new();
        let tiff_cut = crate::open(cut, &OpenOptions::default());
        // The strip tables sit after the payload: cutting may remove the directory itself.
        if let Ok(d) = tiff_cut {
            let mut ctx = DecodeContext::new(&prof);
            ctx.recover = false;
            assert!(d.decode(&ctx).is_err());
        }
    }

    #[test]
    fn unsupported_compression_and_missing_strips() {
        let mut t = TiffWriter::new();
        let ifd = t.ifd(vec![
            ascii(0x010F, "PENTAX"),
            ascii(0x0110, "PENTAX K-5"),
            long(0x0100, &[16]),
            long(0x0101, &[16]),
            short(0x0102, &[12]),
            short(0x0103, &[8]),
            long(0x0111, &[8]),
            long(0x0117, &[100]),
        ]);
        let file = t.finish(ifd);
        let err = crate::open(&file, &OpenOptions::default()).err().unwrap();
        assert_eq!(err.kind(), "UnsupportedCompression");
    }

    #[test]
    fn mutated_real_files_do_not_panic() {
        for rel in ["Pentax/K-1/IMGP8550.PEF", "Pentax/K100D/IMGP1754.PEF", "Pentax/K10D/_IGP7284.PEF"] {
            if let Some(d) = testkit::corpus(rel) {
                crate::nikon::conventions::robustness::mutate(&d, 7);
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
        ("PENTAX/PENTAX K-r/IMGP4424.PEF", "aa8672a6761b2345"),
        ("Pentax/645D/645D2840.PEF", "9b0994dd0925db6d"),
        ("Pentax/645Z/IMGP2837.PEF", "efe9cf45565ec15b"),
        ("Pentax/K-1 Mark II/_K1I9785.PEF", "7675f10a250500a4"),
        ("Pentax/K-1/IMGP8550.PEF", "472173772d3400ba"),
        ("Pentax/K-3 II/IMGP0679.PEF", "1731e85f990fc129"),
        ("Pentax/K-3 Mark III Monochrome/K3III.PEF", "f4a3faeea0d8b824"),
        ("Pentax/K-3 Mark III/IMGP9854.PEF", "f4dd20e76641eaf7"),
        ("Pentax/K-3/IMGP6917.PEF", "1f7351ebc74fa8ff"),
        ("Pentax/K-5 II s/_JAK3781.PEF", "0211f5d7a328f3fd"),
        ("Pentax/K-5 II/IMGP2033.PEF", "a3bcd97ac17dc786"),
        ("Pentax/K-5/_MAR0544.PEF", "0cc82c16ebf6d5fb"),
        ("Pentax/K-7/IMGP6298.PEF", "d6305e866376049d"),
        ("Pentax/K-70/IMGP6854.PEF", "672f4df2a7ca6d44"),
        ("Pentax/K-S1/20210215_0110.PEF", "b0e6b6807d46e2dc"),
        ("Pentax/K-S2/IMGP3369.PEF", "73914f4934ca47e0"),
        ("Pentax/K-m/RAW_PENTAX_K-m.PEF", "1b06c3404973cd69"),
        ("Pentax/K-x/IMGP6432.PEF", "833599ef358c34d3"),
        ("Pentax/K100D Super/IMGP8038.PEF", "841e519bbe64983b"),
        ("Pentax/K100D/IMGP1754.PEF", "09c6d309580bc7f7"),
        ("Pentax/K10D/_IGP7284.PEF", "f60e56006e97973d"),
        ("Pentax/K110D/2009.08.15-imgp3224.pef", "8e0e5ea9a24d0c5c"),
        ("Pentax/K2000/IMGP0251.PEF", "94af64d1c913487f"),
        ("Pentax/K200D/IMGP2293.PEF", "6ba6aac3ce21f978"),
        ("Pentax/K20D/IMGP3335.PEF", "99e9b3454d05e5e5"),
        ("Pentax/KF/IMGP0063.PEF", "62f2753bf809f9ec"),
        ("Pentax/KP/IMGP0237.PEF", "817a31256087eeab"),
        ("Pentax/ist D/pentax.PEF", "9709bc57fc5ec912"),
        ("Pentax/ist DL/IMGP0370.PEF", "52edca6a78953254"),
        ("Pentax/ist DL2/RAW_PENTAX_STAR_DL2.PEF", "b051afe33c36f160"),
        ("Pentax/ist DS/IMGP2333.pef", "53867d57a7792d22"),
    ];
}
