//! Canon TIFF-based raws: CR2 (all generations, incl. sRAW/mRAW) and the TIFF raws of
//! the EOS-1D / 1Ds / D2000.
//!
//! ## Payload
//! The sensor data is one lossless-JPEG stream, usually in IFD3, cut into vertical
//! slices described by tag `0xC640` (see [`flashraw_codecs::canon_cr2`]). The JPEG
//! frame dimensions do not match the image: a frame of `W` columns and `C`
//! components covers `W * C` sensor columns, and very wide frames carry two sensor
//! rows per JPEG row. EOS-1D/1Ds TIFFs point at the stream from maker note tag
//! `0x0081`.
//!
//! sRAW/mRAW frames are subsampled YCbCr (component 0 sampling 2x1 or 2x2) and are
//! converted to linear RGB (three samples per pixel) by
//! [`flashraw_codecs::canon_sraw`].
//!
//! ## Calibration
//! Margins come from the per-sensor layout table ([`super::geometry`]) or, for
//! sensors missing from it, from the maker note SensorInfo. The black level is
//! measured from the left/right optical-black columns (per CFA position); when the
//! layout is unknown the maker-note per-channel black levels are used instead. The
//! back to the full bit depth. As-shot white balance comes from ColorData.

use super::geometry::{self, Geometry};
use super::makernote::CanonMakernote;
use crate::common::*;
use flashraw_codecs::canon_cr2::{self, Slices};
use flashraw_codecs::canon_sraw::{self, SrawConversion, SrawParams};
use flashraw_codecs::ljpeg::{LjpegHeader, LjpegOptions};
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;

const TAG_CR2_SLICE: u16 = 0xC640;
const TAG_GRAY_RESPONSE_CURVE: u16 = 0x0123;
const TAG_KODAK_IFD: u16 = 0x8290;

/// Canon model IDs (maker note tag 0x0010) that need special handling.
mod ids {
    pub const EOS_40D: u32 = 0x8000_0190;
    pub const EOS_5D_MARK_II: u32 = 0x8000_0218;
    pub const EOS_7D: u32 = 0x8000_0250;
    pub const EOS_50D: u32 = 0x8000_0261;
    pub const EOS_1D_MARK_IV: u32 = 0x8000_0281;
    pub const EOS_60D: u32 = 0x8000_0287;
}

/// The located raw stream.
struct RawStream<'a> {
    data: &'a [u8],
    offset: usize,
    hdr: LjpegHeader,
    slices: Slices,
    /// Linearisation curve of the raw IFD (tag 0x0123, Kodak-built EOS D2000/D6000).
    curve: Option<Vec<u16>>,
    /// TIFF-EP CFA pattern of the raw IFD, if present.
    cfa: Option<CfaPattern>,
    /// Located through the EOS-1D/1Ds maker-note pointer.
    from_makernote: bool,
}

/// Locate the maker note IFD (EXIF tag 0x927C; offsets relative to the TIFF base).
pub(super) fn makernote_ifd<'a>(tiff: &Tiff<'a>) -> Option<Ifd<'a>> {
    let e = tiff.all_ifds().into_iter().find_map(|i| i.exif.as_ref().and_then(|x| x.get(tags::MAKER_NOTE).copied()))?;
    tiff.sub_ifd_at(e.value_field).ok()
}

/// interleaved components widen the frame, and frames more than 4x wider than tall
/// carry two sensor rows per JPEG row.
fn jpeg_raw_dims(h: &LjpegHeader) -> (u32, u32) {
    let (mut w, mut hgt) = (h.width, h.height);
    if h.sraw == 0 && h.clrs & 1 == 0 {
        w = w.saturating_mul(h.clrs);
    }
    if w > hgt.saturating_mul(4) && h.clrs & 1 == 0 {
        w /= 2;
        hgt = hgt.saturating_mul(2);
    }
    (w, hgt)
}

fn parse_stream(data: &[u8], offset: usize, len: Option<usize>) -> Option<(&[u8], LjpegHeader)> {
    let s = match len {
        Some(l) if l > 0 => bytes::slice_at_lenient(data, offset, l).ok()?,
        _ => data.get(offset..)?,
    };
    if s.len() < 4 || s[0] != 0xFF || s[1] != 0xD8 {
        return None;
    }
    let hdr = LjpegHeader::parse(s, LjpegOptions::default()).ok()?;
    Some((s, hdr))
}

/// Pick the raw stream: the largest lossless-JPEG strip (by samples x bits, first wins
/// ties), or the EOS-1D maker-note pointer.
fn find_stream<'a>(data: &'a [u8], tiff: &Tiff<'a>, mn: &CanonMakernote) -> Option<RawStream<'a>> {
    let mut best: Option<(u64, RawStream<'a>)> = None;
    let mut consider = |src: Option<(&'a [u8], LjpegHeader)>, offset: usize, ifd: Option<&Ifd>| {
        let Some((s, hdr)) = src else { return };
        if hdr.clrs == 3 && hdr.sraw == 0 {
            return; // three-component JPEGs are RGB previews, not sensor data
        }
        let (w, h) = jpeg_raw_dims(&hdr);
        let score = w as u64 * h as u64 * hdr.bits as u64;
        if best.as_ref().is_none_or(|(b, _)| score > *b) {
            let slices =
                ifd.and_then(|i| i.get(TAG_CR2_SLICE)).map(|e| Slices::from_tag(&e.u32s())).unwrap_or_default();
            let curve = ifd
                .and_then(|i| i.get(TAG_GRAY_RESPONSE_CURVE))
                .map(|e| e.u32s().into_iter().take(0x10000).map(|v| v.min(65535) as u16).collect::<Vec<_>>())
                .filter(|c| !c.is_empty());
            let cfa = ifd.and_then(tiff_ep_cfa);
            best = Some((score, RawStream { data: s, offset, hdr, slices, curve, cfa, from_makernote: ifd.is_none() }));
        }
    };
    for ifd in tiff.all_ifds() {
        let st = strips(ifd);
        if st.len() != 1 {
            continue;
        }
        let (o, l) = st[0];
        consider(parse_stream(data, o, Some(l)), o, Some(ifd));
    }
    if let Some(o) = mn.raw_1d {
        consider(parse_stream(data, o as usize, None), o as usize, None);
    }
    best.map(|(_, s)| s)
}

/// Calibration stored in the Kodak IFD (tag 0x8290) of the Kodak-built EOS D2000/D6000:
/// black level (average of the top/bottom optical-black values, tags 0x3EF/0x3F0) and
/// the as-shot white balance (software WB tag 0x3FD, or the preset table selected by
/// tag 0x3FC).
fn kodak_calibration(tiff: &Tiff) -> (Option<f32>, Option<[f32; 4]>) {
    let Some(off) = tiff.ifds.first().and_then(|i| i.u32(TAG_KODAK_IFD)) else { return (None, None) };
    let Ok(k) = tiff.sub_ifd_at(off) else { return (None, None) };
    let mut black: Option<u32> = None;
    // Preset slot selected by tag 0x3FC: Daylight, Tungsten, Fluorescent, Flash, Custom, Auto
    // (tags 0x848..=0x84D); `Some(None)` marks a software WB that disables presets.
    let mut wbi: Option<Option<u16>> = None;
    let mut wb = None;
    for e in &k.entries {
        match e.tag {
            0x03ef => black = e.u32(0),
            0x03f0 => {
                if let Some(v) = e.u32(0) {
                    black = Some(match black {
                        Some(b) if b != 0 => (b + v) / 2,
                        _ => v,
                    });
                }
            }
            0x03fc => wbi = e.u32(0).filter(|&v| v < 6).map(|v| Some(v as u16)),
            0x03fd if e.count == 72 => {
                let w = super::makernote::entry_words(e);
                if let Some(m) = w.get(20..23) {
                    let f = |v: u16| 2048.0 / (v.max(1) as f32);
                    wb = Some([f(m[0]), f(m[1]), f(m[2]), f(m[1])]);
                    wbi = Some(None);
                }
            }
            t @ 0x0848..=0x084d => {
                let slot = (t - 0x0848) as u16;
                if t == 0x084d && wbi.is_none() {
                    wbi = Some(Some(5));
                }
                if wbi == Some(Some(slot)) {
                    let num = |i: usize| e.f64(i).map(|v| v as f32).filter(|v| *v > 0.001).unwrap_or(1.0);
                    let (r, g, b) = (num(0), num(1), num(2));
                    let g2 = g * g;
                    let (ri, gi, bi) = ((g2 / r) as i32 as f32, g as i32 as f32, (g2 / b) as i32 as f32);
                    wb = Some([ri, gi, bi, gi]);
                }
            }
            _ => {}
        }
    }
    (black.map(|b| b as f32), wb)
}

fn short_model(model: &str) -> &str {
    let m = model.trim();
    m.strip_prefix("Canon ").unwrap_or(m)
}

fn cfa_from_filters(f: u8) -> CfaPattern {
    let c = |i: u8| CfaColor::from_index((f >> (2 * i)) & 3);
    let colors = [c(0), c(1), c(2), c(3)].map(|x| if x as u8 == 3 { CfaColor::Green } else { x });
    CfaPattern::new(2, 2, &colors).unwrap_or(CfaPattern::RGGB)
}

/// Black-level strategy for the Bayer path.
enum Black {
    /// Measure from these masked rectangles (fallback value if they are unusable).
    Masked(Vec<Rect>, f32),
    /// Per-channel values in file order (R, G1, G2, B) from the maker note.
    Channels([u16; 4]),
    Uniform(f32),
}

pub fn open<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("CR2 without IFD0"))?;
    let mut meta = RawMetadata::default();
    exif::read_ifd0(ifd0, &mut meta);
    let mn = makernote_ifd(&tiff).map(|i| CanonMakernote::parse(&i)).unwrap_or_default();
    fill_makernote_metadata(&mut meta, &mn);

    let stream = find_stream(data, &tiff, &mn).ok_or_else(|| Error::CameraUnsupported {
        make: meta.make.clone(),
        model: meta.model.clone(),
        reason: "no lossless-JPEG raw stream found".into(),
    })?;
    let orientation = orientation(&tiff, &mn);
    let mut previews = tiff_previews(data, 0, &tiff, orientation);
    previews.retain(|p| p.offset != stream.offset as u64);
    classify_previews(&mut previews);
    meta.previews = previews;

    if stream.hdr.sraw != 0 {
        open_sraw(data, stream, meta, mn, orientation, opts)
    } else {
        open_bayer(&tiff, stream, meta, mn, orientation, opts)
    }
}

fn fill_makernote_metadata(meta: &mut RawMetadata, mn: &CanonMakernote) {
    if meta.lens.model.is_none() {
        meta.lens.model = mn.lens_model.clone();
    }
    if meta.lens.focal_range.is_none() {
        meta.lens.focal_range = mn.focal_range;
    }
    if meta.serial.is_none() {
        meta.serial = mn.serial.map(|s| s.to_string());
    }
    if meta.firmware.is_none() {
        meta.firmware = mn.firmware.clone();
    }
    if let Some(id) = mn.model_id {
        meta.set_extra("canon_model_id", format!("{id:#010x}"));
    }
}

/// Orientation: the first IFD with a non-default orientation tag.
fn orientation(tiff: &Tiff, _mn: &CanonMakernote) -> Orientation {
    tiff.ifds
        .iter()
        .filter_map(|i| i.u32(tags::ORIENTATION))
        .map(Orientation::from_exif)
        .find(|o| *o != Orientation::Normal)
        .unwrap_or(Orientation::Normal)
}

fn white_level(meta: &RawMetadata, cam: Option<&'static flashraw_camera_db::Camera>, bits: u32) -> u32 {
    let model = cam.map(|c| c.canonical_model).unwrap_or(meta.canonical_model.as_str());
    flashraw_camera_db::find_matrix("Canon", model)
        .map(|m| m.white)
        .filter(|&w| w > 0)
        .map(|w| w as u32)
        .unwrap_or_else(|| (1u32 << bits.clamp(1, 16)) - 1)
}

fn open_bayer<'a>(
    tiff: &Tiff<'a>,
    stream: RawStream<'a>,
    mut meta: RawMetadata,
    mn: CanonMakernote,
    orientation: Orientation,
    opts: &OpenOptions,
) -> Result<Box<dyn RawDecoder + 'a>> {
    let (raw_w, raw_h) = jpeg_raw_dims(&stream.hdr);
    opts.limits.check_dims(raw_w, raw_h, 1)?;
    let cam = db_lookup(&mut meta, "");
    let model = short_model(&meta.model).to_string();

    // Geometry and black-level strategy (see module docs).
    let mut g = Geometry { left: 0, top: 0, width: raw_w, height: raw_h };
    let mut insets = [0u32; 4];
    let mut filters = if stream.from_makernote { 0x61u8 } else { 0x94u8 };
    let layout = geometry::find(raw_w, raw_h);
    if let Some(l) = layout {
        g = l.geometry();
        insets = [l.mask_left_start, l.mask_left_end_inset, l.mask_right_start, l.mask_right_end_inset];
        if l.filters != 0 {
            filters = l.filters;
        }
    }
    // PowerShot G11 / S90 (and relatives) share a layout but use different margins.
    if mn.model_id.is_some_and(|id| id | 0x20000 == 0x0272_0000) {
        g.left = 8;
        g.top = 16;
    }
    let mut black = None;
    let avg_black = mn.color.map(|c| c.average_black()).unwrap_or(0);
    if layout.is_none() && avg_black > 0 {
        if let Some(b) = mn.color.and_then(|c| c.black) {
            black = Some(Black::Channels(b));
        }
        if let Some(s) = mn.sensor.filter(|s| s.width as u32 == raw_w && s.height as u32 == raw_h) {
            let left = (s.crop.left as u32 + 1) & !1;
            let top = (s.crop.top as u32 + 1) & !1;
            g = Geometry {
                left,
                top,
                width: (s.crop.right as u32).saturating_sub(left),
                height: (s.crop.bottom as u32).saturating_sub(top),
            };
        }
    }
    // Model-specific layouts that override the table.
    let mut explicit_mask: Option<Rect> = None;
    match model.as_str() {
        "PowerShot G16" => {
            g.top = 29;
            g.left = 120;
            g.width = raw_w.saturating_sub(120 + 48);
            g.height = raw_h.saturating_sub(29 + 14);
            explicit_mask = Some(Rect::new(0, 0, 16, 80));
        }
        "PowerShot S120" if raw_w == 4192 && raw_h == 3062 => {
            g = Geometry { left: 120, top: 31, width: 4022, height: 3016 };
            explicit_mask = Some(Rect::new(23, 31, 72 - 23, 3016));
        }
        "PowerShot SX50 HS" => g.top = 17,
        _ => {}
    }
    // The CFA is defined relative to the visible origin; anchor it at the buffer origin
    // before the margins are evened out below.
    let vis = stream.cfa.unwrap_or_else(|| cfa_from_filters(filters));
    let (sx, sy) = ((2 - g.left as usize % 2) % 2, (2 - g.top as usize % 2) % 2);
    let buf_cfa = vis.shifted(sx, sy);
    // visible image is stable): odd margins grow by one pixel.
    if g.top % 2 == 1 {
        g.top += 1;
        g.height = g.height.saturating_sub(1);
    }
    if g.left % 2 == 1 {
        g.left += 1;
        g.width = g.width.saturating_sub(1);
    }
    let fallback_black = flashraw_camera_db::find_matrix("Canon", cam.map(|c| c.canonical_model).unwrap_or(&model))
        .map(|m| m.black.max(0) as f32)
        .unwrap_or(0.0);
    let black = black.unwrap_or_else(|| {
        let mut rects = Vec::new();
        if let Some(r) = explicit_mask {
            rects.push(r);
        } else {
            for (x0, x1) in geometry::masked_columns(&g, raw_w, insets) {
                if x1 > x0 && g.height > 0 {
                    rects.push(Rect::new(x0, g.top, x1 - x0, g.height));
                }
            }
        }
        if rects.is_empty() { Black::Uniform(fallback_black) } else { Black::Masked(rects, fallback_black) }
    });

    // Kodak-built bodies (EOS D2000/D6000) carry a linearisation curve, their own CFA
    // tags and Kodak-IFD calibration.
    let (kodak_black, kodak_wb) = kodak_calibration(tiff);
    let black = match kodak_black {
        Some(b) => Black::Uniform(b),
        None => black,
    };
    let bits = stream.hdr.bits;
    let mut info = FrameInfo::new(raw_w, raw_h, bits as u8);
    info.compression = Compression::LosslessJpeg;
    info.orientation = orientation;
    info.active_area = Rect::new(g.left, g.top, g.width, g.height).clamp_to(raw_w, raw_h);
    info.crop = info.active_area;
    info.cfa = Some(buf_cfa);
    info.layout = SensorLayout::Bayer;
    match black {
        Black::Masked(rects, fb) => {
            info.masked_areas = rects;
            info.black_level = BlackLevel::uniform(fb);
            info.black_from_masked = true;
        }
        Black::Channels(b) => {
            // File order R, G1, G2, B → positions of the visible CFA, re-anchored at (0, 0).
            let mut vals = [0f32; 4];
            let mut greens = [b[1], b[2]].into_iter();
            for (i, v) in vals.iter_mut().enumerate() {
                let (x, y) = (i % 2, i / 2);
                *v = match vis.color_at(x, y) {
                    CfaColor::Red => b[0],
                    CfaColor::Blue => b[3],
                    _ => greens.next().unwrap_or(b[1]),
                } as f32;
            }
            info.black_level = BlackLevel::pattern2x2(vals).shifted(sx, sy);
        }
        Black::Uniform(v) => info.black_level = BlackLevel::uniform(v),
    }
    info.white_level = WhiteLevel::uniform(match &stream.curve {
        Some(c) => c[(c.len() - 1).min(0xfff)] as u32,
        None => white_level(&meta, cam, bits),
    });
    if let Some([r, g0, b, g2]) = mn.as_shot_wb().or(kodak_wb) {
        info.white_balance = WhiteBalance::from_multipliers(r, g0, b, g2);
    }
    info.color_matrices = db_color(&meta, cam);
    if stream.slices.is_sliced() {
        meta.set_extra(
            "cr2_slices",
            format!("{}x{}+{}", stream.slices.count, stream.slices.width, stream.slices.last_width),
        );
    }

    let shift = raw_w == 3984;
    let (offset, len) = (stream.offset as u64, stream.data.len() as u64);
    let RawStream { data: sdata, hdr, slices, curve, .. } = stream;
    let mut dec = SimpleDecoder::new(
        RawFormat::Cr2,
        meta,
        info,
        Box::new(move |ctx: &DecodeContext| {
            let mut img = RawImage::new_u16(raw_w, raw_h, 1, &ctx.limits)?;
            ctx.profiler.time(Stage::EntropyDecode, || {
                canon_cr2::decode(sdata, &hdr, raw_w as usize, raw_h as usize, slices, shift, img.u16_mut())
            })?;
            if let Some(c) = &curve {
                let mut full = c.clone();
                let last = *full.last().unwrap_or(&0);
                full.resize(0x10000, last);
                ctx.profiler.time(Stage::Calibration, || apply_curve(img.u16_mut(), &full, ctx.parallel));
            }
            Ok(img)
        }),
    );
    dec.units = vec![DecodeUnit { rect: Rect::full(raw_w, raw_h), offset, length: len }];
    Ok(Box::new(dec))
}

fn open_sraw<'a>(
    _data: &'a [u8],
    stream: RawStream<'a>,
    mut meta: RawMetadata,
    mn: CanonMakernote,
    orientation: Orientation,
    opts: &OpenOptions,
) -> Result<Box<dyn RawDecoder + 'a>> {
    let hdr = &stream.hdr;
    let mode = if hdr.sraw >= 3 { "sRaw1" } else { "sRaw2" };
    let cam = db_lookup(&mut meta, mode);
    let (mut raw_w, mut raw_h) = jpeg_raw_dims(hdr);
    let (mut left, mut top, mut width, mut height) = (0u32, 0u32, raw_w, raw_h);
    if let Some(l) = geometry::find(raw_w, raw_h) {
        let g = l.geometry();
        (left, top, width, height) = (g.left, g.top, g.width, g.height);
    }
    // Known sRAW width quirks, portrait-stored frames and the 5DS sRAW1 layout.
    width = match width {
        3344 => 3272,
        3872 => 3866,
        w => w,
    };
    if height > width {
        std::mem::swap(&mut width, &mut height);
        std::mem::swap(&mut raw_w, &mut raw_h);
    }
    if width == 7200 && height == 3888 {
        (raw_w, width, raw_h, height) = (6480, 6480, 4320, 4320);
    }
    // Frames whose aspect ratio is implausible take their geometry from SensorInfo.
    let mut full_frame = false;
    let ratio = raw_h as f32 / raw_w.max(1) as f32;
    if let Some(s) = mn.sensor.filter(|s| (ratio < 0.57 || ratio > 0.75) && s.width > 1 && s.height > 1) {
        raw_w = s.width as u32;
        raw_h = s.height as u32;
        left = s.crop.left as u32;
        top = s.crop.top as u32;
        width = s.crop.width();
        height = s.crop.height();
        full_frame = true;
    } else if raw_w == 4032 && raw_h == 3402 && meta.model.eq_ignore_ascii_case("Canon EOS 80D") {
        (raw_w, left, width, raw_h, top, height) = (4536, 28, 4536 - 28, 3024, 8, 3024 - 8);
        full_frame = true;
    }
    let (out_w, out_h) = if full_frame { (raw_w, raw_h) } else { (width, height) };
    opts.limits.check_dims(out_w, out_h, 3)?;

    let id = mn.model_id.unwrap_or(0);
    let conversion =
        if matches!(id, ids::EOS_5D_MARK_II | ids::EOS_7D | ids::EOS_50D | ids::EOS_1D_MARK_IV | ids::EOS_60D) {
            SrawConversion::Matrix
        } else {
            SrawConversion::Original { subtract_512: id < ids::EOS_5D_MARK_II }
        };
    let fw = mn.firmware_number().unwrap_or(0);
    let newer = id >= ids::EOS_1D_MARK_IV || (id == ids::EOS_5D_MARK_II && fw > 1_000_006);
    let mul = mn.color.and_then(|c| c.sraw_mul).map(|m| [m[0], m[1], m[2]]).unwrap_or([0; 3]);
    let params = SrawParams {
        width: out_w as usize,
        height: out_h as usize,
        raw_width: raw_w as usize,
        slices: [stream.slices.count as u16, stream.slices.width as u16, stream.slices.last_width as u16],
        conversion,
        hue: SrawParams::default_hue(hdr.sraw, newer),
        mul,
    };
    let _ = ids::EOS_40D;

    let mut info = FrameInfo::new(out_w, out_h, 15);
    info.cpp = 3;
    info.layout = SensorLayout::Linear;
    info.cfa = None;
    info.compression = Compression::CanonSraw;
    info.orientation = orientation;
    info.active_area = if full_frame { Rect::new(left, top, width, height) } else { Rect::full(out_w, out_h) };
    info.crop = info.active_area;
    info.white_level = WhiteLevel::uniform(0x3fff);
    info.black_level = BlackLevel::uniform(0.0);
    if let Some([r, g0, b, g2]) = mn.as_shot_wb() {
        info.white_balance = WhiteBalance::from_multipliers(r, g0, b, g2);
    }
    info.color_matrices = db_color(&meta, cam);
    meta.set_extra("sraw_subsampling", if hdr.sraw >= 3 { "4:2:0" } else { "4:2:2" });

    let (offset, len) = (stream.offset as u64, stream.data.len() as u64);
    let RawStream { data: sdata, hdr, .. } = stream;
    let mut dec = SimpleDecoder::new(
        RawFormat::Cr2,
        meta,
        info,
        Box::new(move |ctx: &DecodeContext| {
            let mut img = RawImage::new_u16(out_w, out_h, 3, &ctx.limits)?;
            ctx.profiler.time(Stage::EntropyDecode, || canon_sraw::decode(sdata, &hdr, &params, img.u16_mut()))?;
            Ok(img)
        }),
    );
    dec.units = vec![DecodeUnit { rect: Rect::full(out_w, out_h), offset, length: len }];
    Ok(Box::new(dec))
}
