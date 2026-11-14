//! Samsung SRW files (NX, EX and WB series) and the forwarding of the Pentax-built GX bodies.
//!
//! The maker note stores the white balance and the black level offset by per-file keys. Five payload
//! layouts exist (row-table coded V0, Huffman V1, V2, 16-bit words, packed 12-bit rows); the visible
//! area of each body family is a row of [`READOUTS`].

use crate::common::*;
use crate::nikon::conventions::*;
use crate::nikon::packed_geometry;
use flashraw_codecs::nikon_packed::{self, PackedSpec};
use flashraw_codecs::packed::{self, PackedLayout};
use flashraw_codecs::samsung as codec;
use flashraw_core::bytes::ReadAt;
use flashraw_core::tiff::{Tiff, TiffOptions, parse_ifd_at, tags};
use flashraw_core::*;

/// SRW is TIFF based and dispatched by make: never matches by signature.
pub fn probe(_data: &[u8]) -> bool {
    false
}

/// SRW files have no signature of their own: always an error, use [`open_tiff`].
pub fn open<'a>(_data: &'a [u8], _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("SRW files are TIFF-based; use `open_tiff`".into()))
}

// ---------------------------------------------------------------------------------------------
// Maker note
// ---------------------------------------------------------------------------------------------

mod mn_tag {
    /// Eleven key words used to unmask the values below.
    pub const KEYS: u16 = 0xA020;
    /// As-shot multipliers R G G B (stored key-offset).
    pub const WHITE_BALANCE: u16 = 0xA021;
    /// Black levels R G G B (stored key-offset).
    pub const BLACK_LEVEL: u16 = 0xA028;
}

/// Tag of the raw IFD that points at the table of row offsets (compression 32770).
const TAG_ROW_OFFSETS: u16 = 0xA010;

#[derive(Default)]
struct MakerNote {
    /// R G B G2 multipliers.
    cam_mul: Option<[f32; 4]>,
    /// R G G B.
    black: Option<[u32; 4]>,
}

/// `N` consecutive 4-byte integers of an entry's value bytes, whatever its declared type.
fn words<const N: usize>(bytes: &[u8], order: ByteOrder) -> Option<[u32; N]> {
    let mut out = [0u32; N];
    let chunks = bytes.get(..N * 4)?.chunks_exact(4);
    for (o, c) in out.iter_mut().zip(chunks) {
        *o = order.u32([c[0], c[1], c[2], c[3]]);
    }
    Some(out)
}

fn read_maker_note(data: &[u8], tiff: &Tiff) -> MakerNote {
    let mut mn = MakerNote::default();
    let Some(entry) = tiff.find_entry(tags::MAKER_NOTE) else { return mn };
    let Some(note) = data.get(entry.value_pos..) else { return mn };
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    let Ok(ifd) = parse_ifd_at(note, tiff.order, 0, opts) else { return mn };
    let mut key = [0u32; 11];
    // Entries are taken in directory order; a key listed after its users does not apply to them.
    for e in &ifd.entries {
        match e.tag {
            mn_tag::KEYS => {
                if let Some(k) = words::<11>(e.data, tiff.order) {
                    key = k;
                }
            }
            mn_tag::WHITE_BALANCE => {
                if let Some(v) = words::<4>(e.data, tiff.order) {
                    let m = |c: usize| v[c].wrapping_sub(key[c]) as f32;
                    mn.cam_mul = Some([m(0), m(1), m(3), m(2)]);
                }
            }
            mn_tag::BLACK_LEVEL => {
                if let Some(v) = words::<4>(e.data, tiff.order) {
                    mn.black = Some(std::array::from_fn(|c| v[c].wrapping_sub(key[c])));
                }
            }
            _ => {}
        }
    }
    mn
}

// ---------------------------------------------------------------------------------------------
// Payload
// ---------------------------------------------------------------------------------------------

enum Payload {
    /// Compression 32770 with a row offset table (offsets from the strip start).
    V0(Vec<u32>),
    /// Compression 32772.
    V1,
    /// Compression 32773 (NX1, NX500).
    V2,
    /// 16-bit words; the samples are shifted right by `shift` after unpacking.
    Words { order: ByteOrder, shift: u32 },
    /// MSB-first packed rows.
    Packed(PackedSpec),
}

impl Payload {
    fn compressed(&self) -> bool {
        matches!(self, Payload::V0(_) | Payload::V1 | Payload::V2)
    }
}

const COMPRESSION_NONE: u32 = 1;
const COMPRESSION_PACKED_GUARDED: u32 = 32769;
const COMPRESSION_V0: u32 = 32770;
const COMPRESSION_V1: u32 = 32772;
const COMPRESSION_V2: u32 = 32773;
/// Packed layout flag: a guard byte after every 10 samples.
const PACKED_GUARD_BYTES: u32 = 1;
/// Packed layout flags of the NX20 family: 24-bit gathering unit with swapped sample pairs.
const PACKED_NX20: u32 = 80;

fn read_row_offsets(data: &[u8], table: usize, rows: usize) -> Result<Vec<u32>> {
    let truncated = || Error::corrupt("truncated SRW row offset table");
    (0..rows)
        .map(|r| {
            let at = r.checked_mul(4).and_then(|o| o.checked_add(table)).ok_or_else(truncated)?;
            data.u32_at(ByteOrder::Little, at).map_err(|_| truncated())
        })
        .collect()
}

fn choose_payload(
    data: &[u8],
    tiff: &Tiff,
    compression: u32,
    row_table: Option<u32>,
    (w, h, bps): (u32, u32, u32),
    bytes: usize,
) -> Result<Payload> {
    match (compression, row_table) {
        (COMPRESSION_V0, Some(t)) => Ok(Payload::V0(read_row_offsets(data, t as usize, h as usize)?)),
        (COMPRESSION_V1, _) => Ok(Payload::V1),
        (COMPRESSION_V2, _) => Ok(Payload::V2),
        (COMPRESSION_PACKED_GUARDED | COMPRESSION_V0 | COMPRESSION_NONE, _) => {
            if bytes as u64 == w as u64 * h as u64 * 2 || bps > 12 {
                let shift = u32::from(compression == COMPRESSION_PACKED_GUARDED && bps == 12);
                Ok(Payload::Words { order: tiff.order, shift })
            } else if bps == 12 {
                let flags = if compression == COMPRESSION_PACKED_GUARDED { PACKED_GUARD_BYTES } else { 0 };
                Ok(Payload::Packed(PackedSpec {
                    width: w as usize,
                    height: h as usize,
                    bits: bps,
                    flags,
                    field2_offset: None,
                }))
            } else {
                Err(Error::unsupported_compression("SRW", format!("uncompressed {bps}-bit")))
            }
        }
        (c, _) => Err(Error::unsupported_compression("SRW", format!("TIFF compression {c}"))),
    }
}

// ---------------------------------------------------------------------------------------------
// Visible area
// ---------------------------------------------------------------------------------------------

/// What identifies a readout family.
#[derive(Clone, Copy)]
enum Trigger {
    Width(u32),
    Height(u32),
    Model(&'static str),
}

/// The readout families. Each is a row of [`READOUTS`] and has one arm in [`Readout::geometry`].
#[derive(Clone, Copy)]
enum Readout {
    /// NX5, NX10, NX11, NX100 (verified).
    Nx5,
    /// NX3000 (verified).
    Nx3000,
    /// NX2000, NX300, NX300M, NX30, NX3300, EK-GN120 (verified).
    Nx300,
    /// NX20, NX200, NX210, NX1000, NX1100 (verified for 12-bit; other bit depths: no sample).
    Nx20,
    /// NX mini (verified).
    NxMini,
    /// NX1, NX500 (verified).
    Nx1,
    /// EX1 (verified; the wider readout has no sample).
    Ex1,
    /// WB2000 (verified; the wider readout has no sample).
    Wb2000,
    /// EX2F (verified).
    Ex2f,
    /// WB550 (no sample file).
    Wb550,
}

/// First match wins.
const READOUTS: [(Trigger, Readout); 10] = [
    (Trigger::Width(4704), Readout::Nx5),
    (Trigger::Model("NX3000"), Readout::Nx3000),
    (Trigger::Height(3714), Readout::Nx300),
    (Trigger::Width(5632), Readout::Nx20),
    (Trigger::Width(5664), Readout::NxMini),
    (Trigger::Width(6496), Readout::Nx1),
    (Trigger::Model("EX1"), Readout::Ex1),
    (Trigger::Model("WB2000"), Readout::Wb2000),
    (Trigger::Model("EX2F"), Readout::Ex2f),
    (Trigger::Model("WB550"), Readout::Wb550),
];

struct Geometry {
    area: Rect,
    /// Significant bits of the stored samples.
    sig_bits: u32,
    /// CFA phase of the visible area, when it differs from the sensor's own.
    cfa_visible: Option<CfaPattern>,
}

impl Readout {
    fn find(model_uc: &str, w: u32, h: u32) -> Option<Readout> {
        READOUTS
            .iter()
            .find(|(t, _)| match *t {
                Trigger::Width(v) => w == v,
                Trigger::Height(v) => h == v,
                Trigger::Model(m) => model_uc == m,
            })
            .map(|&(_, r)| r)
    }

    /// The visible area of this readout; may also adjust how the payload is read.
    fn geometry(self, (w, h, bps): (u32, u32, u32), payload: &mut Payload) -> Geometry {
        let mut g = Geometry { area: Rect::new(0, 0, w, h), sig_bits: bps, cfa_visible: None };
        match self {
            Readout::Nx5 => g.area = Rect::new(8, 8, w.saturating_sub(16), h.saturating_sub(8)),
            Readout::Nx3000 => {
                g.area = Rect::new(92, 38, 5456, 3634);
                g.cfa_visible = Some(CfaPattern::GRBG);
            }
            Readout::Nx300 => {
                g.area = if w == 5600 {
                    Rect::new(w.saturating_sub(5536), 18, 5536, h.saturating_sub(18))
                } else {
                    Rect::new(0, 0, 5536, h.saturating_sub(18))
                };
                g.cfa_visible = Some(CfaPattern::GRBG);
            }
            Readout::Nx20 => {
                let left = 32 + bps.min(32);
                g.area = Rect::new(left, 2, 5574u32.saturating_sub(left), 3694);
                match payload {
                    Payload::Packed(spec) if bps == 12 => spec.flags = PACKED_NX20,
                    Payload::Words { order, .. } => {
                        *order = ByteOrder::Little;
                        g.sig_bits = 12;
                    }
                    _ => {}
                }
            }
            Readout::NxMini => {
                g.area = Rect::new(96, 17, 5544, h.saturating_sub(17));
                g.cfa_visible = Some(CfaPattern::GBRG);
            }
            Readout::Nx1 => g.cfa_visible = Some(CfaPattern::GRBG),
            Readout::Ex1 => {
                g.area = compact_camera_area(w, h, (6, 20, 2), 3682, (46, 10, 8));
                *payload = Payload::Words { order: ByteOrder::Little, shift: 0 };
            }
            Readout::Wb2000 => {
                g.area = compact_camera_area(w, h, (10, 3, 2), 3718, (56, 28, 8));
                *payload = Payload::Words { order: ByteOrder::Little, shift: 0 };
            }
            Readout::Ex2f => {
                g.area = Rect::new(24, 15, 4040, 3030);
                g.cfa_visible = Some(CfaPattern::GBRG);
                let shift = match payload {
                    Payload::Words { shift, .. } => *shift,
                    _ => 1,
                };
                *payload = Payload::Words { order: ByteOrder::Little, shift };
            }
            Readout::Wb550 => {
                if let Payload::Words { order, .. } = payload {
                    *order = ByteOrder::Little;
                }
            }
        }
        g
    }
}

/// Area of the compact-camera readouts: `(trim_w, trim_h, top)` of the narrow readout; when the
/// trimmed width exceeds `wide_above` the `(extra_w, extra_h, top)` of the wide readout apply.
fn compact_camera_area(
    w: u32,
    h: u32,
    (trim_w, trim_h, top): (u32, u32, u32),
    wide_above: u32,
    (extra_w, extra_h, wide_top): (u32, u32, u32),
) -> Rect {
    let (mut aw, mut ah, mut top) = (w.saturating_sub(trim_w), h.saturating_sub(trim_h), top);
    if aw > wide_above {
        ah = ah.saturating_sub(extra_h);
        aw = aw.saturating_sub(extra_w);
        top = wide_top;
    }
    Rect::new(0, top, aw, ah)
}

// ---------------------------------------------------------------------------------------------
// Optical black of the first NX bodies
// ---------------------------------------------------------------------------------------------

/// Mean black per buffer-parity class, measured in the left and right masked strips of a 12-bit
/// MSB-first packed frame (stride `w*3/2`). `None` when the data does not cover the strips, a
/// class is empty, or a quarter or more of the samples are zero (the share is defined behaviour).
fn measure_masked_black(src: &[u8], w: u32, strips: &[Rect; 2]) -> Option<[f32; 4]> {
    let stride = w as usize * 3 / 2;
    let (mut sum, mut count) = ([0u64; 4], [0u64; 4]);
    let (mut zeros, mut total) = (0u64, 0u64);
    for r in strips {
        for y in r.y as usize..r.bottom() as usize {
            let row = src.get(y.checked_mul(stride)?..)?;
            for x in r.x as usize..r.right() as usize {
                let b = row.get(x / 2 * 3..x / 2 * 3 + 3)?;
                let v = if x % 2 == 0 {
                    (u32::from(b[0]) << 4) | u32::from(b[1] >> 4)
                } else {
                    (u32::from(b[1] & 15) << 8) | u32::from(b[2])
                };
                let class = (y % 2) * 2 + x % 2;
                sum[class] += u64::from(v);
                count[class] += 1;
                zeros += u64::from(v == 0);
                total += 1;
            }
        }
    }
    if count.contains(&0) || zeros >= total / 4 {
        return None;
    }
    Some(std::array::from_fn(|i| (sum[i] / count[i]) as f32))
}

// ---------------------------------------------------------------------------------------------
// Open
// ---------------------------------------------------------------------------------------------

fn word_order(order: ByteOrder) -> PackOrder {
    if order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes }
}

/// Open an SRW (or forward a Pentax-built GX file to the Pentax reader).
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    // GX-1S/GX-1L/GX10/GX20 write Pentax PEF files.
    if let Some(e) = tiff.find_entry(tags::MAKER_NOTE) {
        if data.get(e.value_pos..e.value_pos.saturating_add(4)) == Some(b"AOC\0") {
            return crate::pentax::open_tiff(data, tiff, opts);
        }
    }
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("SRW without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let model = strip_make_prefix(&meta.model, "SAMSUNG");
    let model_uc = model.to_ascii_uppercase();
    let mn = read_maker_note(data, &tiff);

    let raw = select_raw_ifd(&tiff, false)
        .ok_or_else(|| no_raw_ifd(&tiff, "SRW", Error::invalid_metadata("SRW has no raw image IFD")))?;
    let (w, h) = (raw.width(), raw.height());
    opts.limits.check_dims(w, h, 1)?;
    let bps = raw.bps();
    let (offset, declared) =
        *strips(raw).first().ok_or_else(|| Error::invalid_metadata("SRW raw IFD without strips"))?;
    let src = data.get(offset..).unwrap_or(&[]);
    let mut payload = choose_payload(data, &tiff, raw.compression(), raw.u32(TAG_ROW_OFFSETS), (w, h, bps), declared)?;

    // Visible area and payload adjustments of the readout family.
    let geometry = match Readout::find(&model_uc, w, h) {
        Some(r) => r.geometry((w, h, bps), &mut payload),
        None => Geometry { area: Rect::new(0, 0, w, h), sig_bits: bps, cfa_visible: None },
    };
    let area = geometry.area.clamp_to(w, h);
    let cfa_override = geometry.cfa_visible.map(|c| cfa_at_buffer_origin(c, area.x, area.y));
    let area = even_margins(area);
    let sig_bits = geometry.sig_bits;

    // Levels.
    let cam = db_lookup(&mut meta, if payload.compressed() { "entropy-coded" } else { "" });
    let table = flashraw_camera_db::find_matrix("Samsung", &meta.canonical_model)
        .or_else(|| flashraw_camera_db::find_matrix("Samsung", &model));
    let mut base_black = 0u32;
    let mut cblack = mn.black.unwrap_or([0; 4]);
    let mut maximum = 0u32;
    apply_table_levels(table, &mut base_black, &mut cblack, &mut maximum);
    if maximum == 0 {
        maximum = (1u32 << sig_bits.clamp(1, 16)) - 1;
    }

    let mut info = FrameInfo::new(w, h, sig_bits.min(16) as u8);
    info.compression = match payload {
        Payload::V0(_) => Compression::SamsungV0,
        Payload::V1 => Compression::SamsungV1,
        Payload::V2 => Compression::SamsungV2,
        _ => Compression::Uncompressed,
    };
    info.active_area = area;
    info.crop = area;
    info.white_level = WhiteLevel::uniform(maximum);
    info.orientation = file_orientation(&tiff);
    let cfa = cfa_override
        .or_else(|| tiff_ep_cfa(raw))
        .or_else(|| cam.and_then(|c| c.cfa).and_then(parse_db_cfa))
        .unwrap_or(CfaPattern::RGGB);
    info.layout = layout_for(&cfa);
    info.cfa = Some(cfa);
    info.black_level = black_for_cfa(&cfa, base_black, cblack);
    info.white_balance = mn.cam_mul.and_then(wb_from_cam_mul);
    info.color_matrices = color_from_table(table, &meta, cam);
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);

    if w == 4704 && matches!(payload, Payload::Packed(_)) {
        let (left, right) = (area.x, area.right());
        let strips =
            [Rect::new(0, area.y, left, area.height), Rect::new(right, area.y, w.saturating_sub(right), area.height)];
        info.masked_areas = strips.to_vec();
        if let Some(b) = measure_masked_black(src, w, &strips) {
            info.black_level = BlackLevel::pattern2x2(b);
        }
    }

    // Row layout and random access.
    let (width, height) = (w as usize, h as usize);
    let row_layout = match &payload {
        Payload::Packed(spec) => spec.fast_layout().map(|layout| RowLayout { layout, shift: 0 }),
        Payload::Words { order, shift } => {
            Some(RowLayout { layout: PackedLayout::tight(width, height, 16, word_order(*order)), shift: *shift })
        }
        _ => None,
    };
    let packed = match (&payload, row_layout) {
        (Payload::Packed(spec), Some(rl)) => Some(PackedPayload {
            data: src,
            width: w,
            height: h,
            bits: spec.bits,
            order: rl.layout.order,
            row_stride: rl.layout.row_stride,
        }),
        _ => None,
    };

    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
        let out = img.u16_mut();
        let unit = DataUnit::Strip(0);
        let len = data.len();
        match &payload {
            Payload::V0(row_offsets) => {
                ctx.profiler.time(Stage::EntropyDecode, || {
                    codec::decode_v0_with(src, row_offsets, width, height, out, ctx.parallel)
                })?;
                let first_missing = first_unfinished_row(row_offsets, src.len(), declared);
                finish_truncated_stream(ctx, len, unit, offset, declared, out, width, height, first_missing)?;
            }
            Payload::V1 => {
                let dead_row = ctx.profiler.time(Stage::EntropyDecode, || codec::decode_v1(src, width, height, out))?;
                finish_truncated_stream(ctx, len, unit, offset, declared, out, width, height, dead_row)?;
            }
            Payload::V2 => {
                ctx.profiler
                    .time(Stage::EntropyDecode, || codec::decode_v2(src, width, height, out))
                    .map_err(|e| e.or_truncated(len, unit, offset, declared))?;
            }
            Payload::Packed(spec) => {
                let rows = ctx.sensor_rows(data, unit, offset, packed_geometry(spec))?;
                ctx.profiler.time(Stage::Unpack, || {
                    nikon_packed::decode(rows, spec, out, width, ctx.parallel, ctx.simd, ctx.recover)
                })?;
            }
            Payload::Words { order, shift } => {
                let layout = PackedLayout::tight(width, height, 16, word_order(*order));
                let rows = ctx.sensor_rows(data, unit, offset, row_geometry(&layout, height))?;
                ctx.profiler.time(Stage::Unpack, || {
                    packed::unpack(rows, layout, out, width, ctx.parallel, ctx.simd, ctx.recover)
                })?;
                if *shift > 0 {
                    let s = *shift;
                    par::for_each_row_chunk(out, width, 256, ctx.parallel, |_, c| c.iter_mut().for_each(|v| *v >>= s));
                }
            }
        }
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(RawFormat::Srw, meta, info, Box::new(decode));
    dec.units = vec![DecodeUnit { rect: Rect::full(w, h), offset: offset as u64, length: declared as u64 }];
    dec.packed = packed;
    if let Some(rl) = row_layout {
        dec.random_access = RandomAccess::Rows;
        dec.region =
            Some(Box::new(move |ctx, rect| decode_rows_region(data, offset, DataUnit::Strip(0), rl, rect, ctx)));
    }
    Ok(Box::new(dec))
}

/// The first row (in row order) whose bit stream is not wholly inside the `available` bytes of
/// the source. Rows are independent streams that each end where the next one starts (the
/// last ends at the declared end of the strip); a final block can spill into the following
/// row's bytes, so a row counts as lost when the stream *after* its successor is cut.
fn first_unfinished_row(row_offsets: &[u32], available: usize, declared: usize) -> Option<usize> {
    let mut starts: Vec<usize> = row_offsets.iter().map(|&o| o as usize).collect();
    starts.sort_unstable();
    let end_of = |s: usize| match starts.get(starts.partition_point(|&v| v <= s)) {
        Some(&next) => next,
        None => declared.max(s),
    };
    row_offsets.iter().position(|&o| end_of(end_of(o as usize)) > available)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{self, TiffWriter, ascii, long, short};

    #[test]
    fn masked_black_measurement() {
        let strips = [Rect::new(0, 0, 2, 2), Rect::new(2, 0, 0, 2)];
        // 4 pixels wide: stride 6; every sample 0x101.
        let row = [0x10u8, 0x11, 0x01, 0x10, 0x11, 0x01];
        let src: Vec<u8> = row.iter().chain(&row).copied().collect();
        assert_eq!(measure_masked_black(&src, 4, &strips), Some([257.0; 4]));
        assert_eq!(measure_masked_black(&[], 4, &strips), None);
        // Mostly zero: rejected.
        assert_eq!(measure_masked_black(&[0u8; 12], 4, &strips), None);
        // A class without any pixel.
        assert_eq!(measure_masked_black(&src, 4, &[Rect::new(0, 0, 1, 1), Rect::new(0, 0, 0, 0)]), None);
    }

    #[test]
    fn unfinished_rows_follow_the_next_stream() {
        // Four rows of 10 bytes each; the strip is declared 40 bytes.
        let offsets = [0u32, 10, 20, 30];
        assert_eq!(first_unfinished_row(&offsets, 40, 40), None);
        // 25 bytes present: row 1 ends at 20 but its successor ends at 30 > 25: lost from row 1 on.
        assert_eq!(first_unfinished_row(&offsets, 25, 40), Some(1));
        assert_eq!(first_unfinished_row(&offsets, 0, 40), Some(0));
        // Row order, not sorted order.
        assert_eq!(first_unfinished_row(&[30, 20, 10, 0], 25, 40), Some(0));
    }

    #[test]
    fn readout_families() {
        assert!(matches!(Readout::find("NX10", 4704, 3156), Some(Readout::Nx5)));
        assert!(matches!(Readout::find("NX3000", 4704, 10), Some(Readout::Nx5)));
        assert!(matches!(Readout::find("NX3000", 5000, 10), Some(Readout::Nx3000)));
        assert!(matches!(Readout::find("NX300", 5600, 3714), Some(Readout::Nx300)));
        assert!(matches!(Readout::find("NX1", 6496, 4000), Some(Readout::Nx1)));
        assert!(matches!(Readout::find("EX2F", 4100, 3100), Some(Readout::Ex2f)));
        assert!(Readout::find("NX2", 1, 1).is_none());
    }

    #[test]
    fn compact_camera_areas() {
        // EX1 narrow and wide readouts.
        assert_eq!(compact_camera_area(3688, 2760, (6, 20, 2), 3682, (46, 10, 8)), Rect::new(0, 2, 3682, 2740));
        assert_eq!(compact_camera_area(3800, 2760, (6, 20, 2), 3682, (46, 10, 8)), Rect::new(0, 8, 3748, 2730));
        // Saturates on tiny buffers.
        assert_eq!(compact_camera_area(2, 2, (6, 20, 2), 3682, (46, 10, 8)), Rect::new(0, 2, 0, 0));
    }

    fn nx200_words(h: u32, samples: &[u16]) -> Vec<u8> {
        let payload: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut t = TiffWriter::new();
        t.buf.extend(&payload);
        let ifd = t.ifd(vec![
            ascii(0x010F, "SAMSUNG"),
            ascii(0x0110, "NX200"),
            long(0x0100, &[5632]),
            long(0x0101, &[h]),
            short(0x0102, &[14]),
            short(0x0103, &[1]),
            long(0x0111, &[8]),
            long(0x0117, &[payload.len() as u32]),
        ]);
        t.finish(ifd)
    }

    #[test]
    fn nx200_family_word_frame() {
        let h = 8;
        let samples: Vec<u16> = (0..5632 * h).map(|i| ((i * 31) % 4096) as u16).collect();
        let file = nx200_words(h, &samples);
        let dec = crate::open(&file, &OpenOptions::default()).unwrap();
        assert_eq!(dec.format(), RawFormat::Srw);
        assert_eq!(dec.frame_info().white_level.values[0], 4095);
        assert_eq!(dec.frame_info().bits_per_sample, 12);
        let img = crate::nikon::conventions::robustness::open_decode(&file).unwrap();
        assert_eq!(img.u16(), &samples[..]);
        assert!(crate::nikon::conventions::robustness::region_matches_full(&file));
    }

    #[test]
    fn row_table_past_the_end_is_an_error() {
        let mut t = TiffWriter::new();
        let ifd = t.ifd(vec![
            ascii(0x010F, "SAMSUNG"),
            ascii(0x0110, "NX300"),
            long(0x0100, &[64]),
            long(0x0101, &[64]),
            short(0x0102, &[12]),
            short(0x0103, &[32770]),
            long(0x0111, &[8]),
            long(0x0117, &[10]),
            long(0xA010, &[100_000]),
        ]);
        let file = t.finish(ifd);
        let err = crate::open(&file, &OpenOptions::default()).err().unwrap();
        assert!(err.to_string().contains("row offset table"), "{err}");
    }

    #[test]
    fn mutated_real_files_do_not_panic() {
        for rel in ["Samsung/NX10/SAM_3630.SRW", "Samsung/NX300/nx300.SRW", "Samsung/NX3000/_3184416.SRW"] {
            if let Some(d) = testkit::corpus(rel) {
                crate::nikon::conventions::robustness::mutate(&d, 11);
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
        ("Samsung/EK-GN120/20231015_094741.srw", "e6904534a16e9d11"),
        ("Samsung/EX1/SAM_5548.SRW", "4c33e5a671a1ebc8"),
        ("Samsung/EX2F/RAW_SAMSUNG_EX2F.SRW", "45ae15ea6a97f219"),
        ("Samsung/NX mini/SAM_0295.SRW", "96c338a0adaa3e9f"),
        ("Samsung/NX1/2016-07-23-142101_sam_9364.srw", "dde8ad90f9bd7d22"),
        ("Samsung/NX10/SAM_3630.SRW", "13e785bee8998c54"),
        ("Samsung/NX100/RAW_SAMSUNG_NX100.SRW", "e4a4551e4b5f9ce2"),
        ("Samsung/NX1000/_5169011.SRW", "acbadd9c3db7db33"),
        ("Samsung/NX11/SAM_5935.SRW", "366b1d3fc762e50e"),
        ("Samsung/NX1100/Sam20_50mm.srw", "12626c65e257a0a0"),
        ("Samsung/NX20/03194133.SRW", "6ba55b7cfeee4ad7"),
        ("Samsung/NX200/2013-05-08-194524__sam6589.srw", "afb70667f35e697f"),
        ("Samsung/NX2000/_3240014.SRW", "06f2f5e2715b3800"),
        ("Samsung/NX210/nx210.SRW", "fdba845dd4649951"),
        ("Samsung/NX30/2015-03-07-163604_sam_7204.srw", "885c30ae03ff8b0d"),
        ("Samsung/NX300/nx300.SRW", "7385c7733241b309"),
        ("Samsung/NX3000/_3184416.SRW", "3f2b7d21ade0c02e"),
        ("Samsung/NX300M/RAW_SAMSUNG_NX300M.SRW", "5a520e032dd390ad"),
        ("Samsung/NX3300/SAM_4260.SRW", "c18180a17283c7c8"),
        ("Samsung/NX5/SAM_3213.SRW", "343b9ca45bb2cf92"),
        ("Samsung/NX500/SAM_2922.SRW", "5079300a16e31653"),
        ("Samsung/NX500/SAM_2927.SRW", "45b31ee63426e674"),
        ("Samsung/WB2000/SAM_4727.SRW", "701397756f989d0d"),
    ];
}
