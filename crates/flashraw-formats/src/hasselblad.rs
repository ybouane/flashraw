//! Hasselblad 3FR / FFF (H, X, CFV, CF, Ixpress backs; Imacon/Flexcolor FFF).
//!
//! TIFF containers whose CFA image (SubIFD, photometric 32803) is either Hasselblad's
//! lossless-JPEG variant ([`flashraw_codecs::hasselblad`], TIFF compression 7) or
//! plain little-endian 16-bit samples (newer X1D/X2D/CFV 3FR). Metadata: the maker
//! note gives the sensor and coating codes (0x0011/0x0016), which
//! select the sensor geometry of uncropped files and the colour-matrix family
//! ("50-15-Coated5-3FR"...); white balance is the DNG-style AsShotNeutral; black is
//! the BlackLevel tag (or measured from DNG-style MaskedAreas when present).

use crate::common::*;
use crate::medium::{cfa_at_origin, even_margins};
use flashraw_codecs::hasselblad::{HasselbladHeader, decode_par};
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;

/// Magic-signature probe (Hasselblad files are TIFF; dispatch is by Make).
pub fn probe(_data: &[u8]) -> bool {
    false
}

/// Not a magic-signature format.
pub fn open<'a>(_data: &'a [u8], _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("Hasselblad files are TIFF-based".into()))
}

/// Hasselblad maker-note values.
#[derive(Debug, Default)]
struct Maker {
    sensor_code: u32,
    coating_code: u32,
    has_color_matrix: bool,
}

fn parse_makernote(data: &[u8], tiff: &Tiff) -> Maker {
    let mut m = Maker::default();
    let Some(e) = tiff.find_entry(tags::MAKER_NOTE) else { return m };
    let opts = flashraw_core::tiff::TiffOptions { follow_sub_ifds: false, ..Default::default() };
    let Ok(ifd) = flashraw_core::tiff::parse_ifd_at(data, tiff.order, e.value_pos as u32, opts) else { return m };
    m.sensor_code = ifd.u32(0x0011).unwrap_or(0);
    m.coating_code = ifd.u32(0x0016).unwrap_or(0);
    let connector_ok =
        ifd.string(0x0015).is_some_and(|s| s.chars().all(|c| c.is_ascii_alphanumeric() || " /-".contains(c)));
    m.has_color_matrix = connector_ok
        && ifd.get(0x002a).is_some_and(|c| c.typ == flashraw_core::tiff::types::SRATIONAL && c.count == 12);
    m
}

/// Sensor family, derived from the normalised model and the maker-note sensor codes.
#[derive(Debug, Clone)]
struct Sensor {
    code: u32,
    sub_code: u32,
    /// Colour-matrix family without the container suffix (e.g. "50-15-Coated5").
    family: Option<String>,
    /// Raw dimensions of a full, uncropped sensor readout.
    uncropped: bool,
}

fn classify(mn: &Maker, w: u32, h: u32, csi: &str) -> Sensor {
    let (mut code, coat) = (mn.sensor_code, mn.coating_code);
    let mut sub = 0;
    let csi_ii = |s: &str| s.starts_with("X1D") || s.starts_with("CFV II");
    let by_code = match (code, coat) {
        (4, c) if c < 2 => Some("16-Uncoated"),
        (6, c) if c < 2 => Some("22-Uncoated"),
        (8, 1) => Some("31-Uncoated"),
        (9, c) if c < 2 => Some("39-Uncoated"),
        (9, 4) => Some("39-Coated"),
        (13, 4) => Some("40-Coated"),
        (13, 5) => Some("40-Coated5"),
        (11, 4) => Some("50-Coated"),
        (11, 5) => Some("50-Coated5"),
        (15, 5) => Some("50-15-Coated5"),
        (12, 4) => Some("60-Coated"),
        (17, 5) => Some("100-17-Coated5"),
        (20, 6) => Some("100-20-Coated6"),
        _ => None,
    };
    let mut family = by_code.map(str::to_string);
    if family.as_deref() == Some("50-15-Coated5") && csi_ii(csi) {
        sub = 2;
        if csi.starts_with("CFV II 50C") || csi.to_ascii_uppercase().starts_with("X1D II 50C") {
            family = Some("50-15-Coated5-II".into());
        }
    }
    if by_code.is_none() {
        let np = w as u64 * h as u64;
        let (fam, c): (Option<&str>, u32) = match (w, h) {
            (4090, _) | (4096, 4096) | (4088, 4088) | (4080, 4080) => (Some("16-Uncoated"), 4),
            (4096, 5456) | (4088, 5448) | (4080, 5440) => (Some("22-Uncoated"), 6),
            (6542, 4916) | (6504, 4880) | (6496, 4872) => (Some("31-Uncoated"), 8),
            (7262, 5456) | (7224, 5420) | (7216, 5412) | (7212, 5412) => (Some("39-Uncoated"), 9),
            _ if (7212 * 5412..7304 * 5478).contains(&np) => (Some("39-Uncoated"), 9),
            (7410, 5586) | (7312, 5486) | (7304, 5478) => (Some("40-Coated5"), 13),
            (8282, 6240) | (8184, 6140) | (8176, 6132) => (Some("50-Coated"), 11),
            (8374, 6304) | (8384, 6304) | (8280, 6208) | (8272, 6200) => (Some("50-15-Coated5"), 15),
            (9044, 6732) | (8964, 6716) | (8956, 6708) => (Some("60-Coated"), 12),
            (12000, 8816) | (11608, 8708) | (11600, 8700) => (Some("100-17-Coated5"), 17),
            (11904, 8842) | (11664, 8750) | (11656, 8742) => (Some("100-20-Coated6"), 20),
            _ if (10320 * 7752..10520 * 8000).contains(&np) => (Some("80-Coated"), 0),
            _ => (None, 0),
        };
        family = fam.map(str::to_string);
        if code == 0 {
            code = c;
        }
        if code == 15 && (w == 8384 || csi_ii(csi)) {
            sub = 2;
            if csi.contains(" II ") {
                family = Some("50-15-Coated5-II".into());
            }
        }
    }
    let uncropped = matches!(
        (w, h),
        (4090, _)
            | (4096, 4096)
            | (5568, 3648)
            | (4096, 5456)
            | (6542, 4916)
            | (7262, 5456)
            | (7410, 5586)
            | (8282, 6240)
            | (8374, 6304)
            | (8384, 6304)
            | (9044, 6732)
            | (10320, 7752)
            | (12000, 8816)
            | (11904, 8842)
    );
    Sensor { code, sub_code: sub, family, uncropped }
}

fn raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    tiff.all_ifds()
        .into_iter()
        .filter(|i| i.u32(tags::PHOTOMETRIC) == Some(32803) && i.width() > 0 && i.height() > 0)
        .max_by_key(|i| i.width() as u64 * i.height() as u64)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Payload {
    Ljpeg { base: i32 },
    Raw16,
}

/// Open a Hasselblad / Imacon TIFF raw.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("3FR without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    // Sony-made bodies sold as Hasselblad (Lunar, Stellar, HV) write ordinary ARW files.
    if raw_ifd(&tiff).is_some_and(|r| matches!(r.compression(), 32767 | 32769 | 32770)) {
        return crate::sony::open_tiff(data, tiff, opts);
    }
    let raw = raw_ifd(&tiff)
        .ok_or_else(|| no_raw_ifd(&tiff, "Hasselblad", Error::invalid_metadata("Hasselblad: no CFA image")))?;
    let (w, h) = (raw.width(), raw.height());
    opts.limits.check_dims(w, h, 1)?;
    if raw.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1) != 1 {
        return Err(Error::unsupported_compression("Hasselblad", "multi-shot capture"));
    }
    let mn = parse_makernote(data, &tiff);
    // Strip the make prefix and the "/initiator" suffix of the model tag.
    let model = meta.model.trim().trim_start_matches("Hasselblad").trim().to_string();
    let csi = model.split('/').next().unwrap_or("").to_string();
    let sensor = classify(&mn, w, h, &csi);
    let has_cm1 = tiff.all_ifds().iter().any(|i| i.get(tags::COLOR_MATRIX1).is_some_and(|e| matches!(e.count, 9 | 12)));
    let has_cm2 = tiff.all_ifds().iter().any(|i| i.get(tags::COLOR_MATRIX2).is_some_and(|e| matches!(e.count, 9 | 12)));
    let is_3fr = has_cm1 && !has_cm2 && !mn.has_color_matrix;
    meta.set_extra("hasselblad_container", if is_3fr { "3FR" } else { "FFF" });
    meta.set_extra("hasselblad_sensor_code", sensor.code);
    let cam = db_lookup(&mut meta, "");

    // Black / white from DNG-style tags in the raw IFD.
    let mut black = raw.f64(tags::BLACK_LEVEL).filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(0.0);
    let mut white = raw.u32(tags::WHITE_LEVEL).filter(|&v| v > 0).unwrap_or(0xffff);
    if tiff.find_entry(0xC51B).is_some() {
        white = 0xffff;
    }
    let comp = raw.compression();
    let mut payload = match (comp, raw.bps()) {
        (7, _) | (99, _) => Payload::Ljpeg { base: 0 },
        (1, 16) => Payload::Raw16,
        (c, b) => return Err(Error::unsupported_compression("Hasselblad", format!("compression {c}, {b} bits"))),
    };

    // Geometry: full readouts of known sensors carry fixed borders.
    let mut vis = Rect::full(w, h);
    let mut cfa_vis = CfaPattern::RGGB;
    if sensor.uncropped {
        match (sensor.code, sensor.sub_code) {
            (9, _) => {
                vis = Rect::new(7, 4, 7248, 5444);
                cfa_vis = CfaPattern::GRBG;
            }
            (13, _) | (11, _) => {
                vis = Rect::new(41, 4, w.saturating_sub(82), h.saturating_sub(84));
                cfa_vis = CfaPattern::GRBG;
            }
            (15, 0) => {
                vis = Rect::new(52, 100, 8272, 6200);
                black = 256.0;
            }
            (15, 2) => {
                vis = Rect::new(48, 96, w.saturating_sub(106), h.saturating_sub(96));
                white = 0xffff;
            }
            (12, _) => {
                if black > 500.0 {
                    vis = Rect::new(44, 12, 8956, 6708);
                    black = 512.0;
                } else {
                    vis = Rect::new(40, 8, 8964, 6716);
                    black += 256.0;
                    white = 0x8101;
                    if let Payload::Ljpeg { .. } = payload {
                        payload = Payload::Ljpeg { base: 256 };
                    }
                }
            }
            (17, _) => vis = Rect::new(64, 108, 11608, h.saturating_sub(108)),
            (20, _) | (22, _) => vis = Rect::new(124, 92, 11664, h.saturating_sub(92)),
            _ => {}
        }
    }
    if sensor.code == 4 && w == 4090 {
        // V96C
        vis = Rect::new(3, 6, w.saturating_sub(10), h.saturating_sub(6));
        cfa_vis = CfaPattern::GRBG;
    }
    let vis = vis.clamp_to(w, h);
    let mut info = FrameInfo::new(w, h, 16);
    info.white_level = WhiteLevel::uniform(white);
    info.black_level = BlackLevel::uniform(black as f32);
    info.active_area = even_margins(vis);
    info.crop = info.active_area;
    let cfa = cfa_at_origin(cfa_vis, vis.x, vis.y);
    info.cfa = Some(cfa);
    info.layout = SensorLayout::Bayer;
    info.orientation = Orientation::from_exif(ifd0.u32(tags::ORIENTATION).unwrap_or(1));
    info.compression = match payload {
        Payload::Ljpeg { .. } => Compression::HasselbladLjpeg,
        Payload::Raw16 => Compression::Uncompressed,
    };
    if let Some(m) = raw.get(tags::MASKED_AREAS) {
        for c in m.u32s().chunks_exact(4) {
            if let Some(r) = Rect::from_ltrb(c[1], c[0], c[3], c[2]) {
                info.masked_areas.push(r);
            }
        }
        info.black_from_masked = !info.masked_areas.is_empty();
    }
    if let Some(n) = tiff.find_entry(tags::AS_SHOT_NEUTRAL) {
        info.white_balance = WhiteBalance::from_neutral(&n.f32s());
    }
    // Colour: the per-sensor-family fallback table, then the file's ColorMatrix1.
    let key = sensor.family.as_ref().map(|f| format!("{f}-{}", if is_3fr { "3FR" } else { "FFF" }));
    if let Some(k) = &key {
        meta.set_extra("hasselblad_sensor", k.clone());
        if let Some(m) = flashraw_camera_db::find_matrix("Hasselblad", k) {
            let v: Vec<f32> = m.color_matrix.iter().map(|&x| x as f32 / 10000.0).collect();
            if let Some(cm) = ColorMatrix::new(Illuminant::D65, v.len() / 3, v) {
                info.color_matrices.color.push(cm);
                info.color_matrices.source = ColorSource::CameraDb;
            }
        }
    }
    if info.color_matrices.color.is_empty() {
        if let Some(e) = tiff.find_entry(tags::COLOR_MATRIX1) {
            let v = e.f32s();
            if let Some(cm) = ColorMatrix::new(Illuminant::D65, v.len() / 3, v).filter(|_| e.count == 9) {
                info.color_matrices.color.push(cm);
                info.color_matrices.source = ColorSource::File;
            }
        }
    }
    apply_db(
        &mut info,
        &meta,
        cam,
        Known { cfa: true, crop: true, active_area: true, black: true, white: true, masked: true, color: false },
    );
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);

    let (off, len) = *strips(raw).first().ok_or_else(|| Error::invalid_metadata("Hasselblad: no strips"))?;
    let order = tiff.order;
    let (wu, hu) = (w as usize, h as usize);
    // A payload past the end of the file fails when decoded (metadata and previews stay
    // available). Raw16 is addressed by geometry; the lossless JPEG stream may run past
    // its declared length and sees the rest of the file.
    let src = match payload {
        Payload::Raw16 => bytes::slice_at_lenient(data, off, len.max(wu * hu * 2)).unwrap_or(&[]),
        Payload::Ljpeg { .. } => data.get(off..).unwrap_or(&[]),
    };
    let decode_fn = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
        let unit = DataUnit::Strip(0);
        match payload {
            // One entropy-coded stream: fails under either policy; a failure over a
            // declared range past the end of the file is reported as truncation.
            Payload::Ljpeg { base } => ctx
                .profiler
                .time(Stage::EntropyDecode, || {
                    let hdr = HasselbladHeader::parse(src)?;
                    decode_par(src, &hdr, img.u16_mut(), wu, hu, base, ctx.parallel)
                })
                .map_err(|e| e.or_truncated(data.len(), unit, off, len))?,
            Payload::Raw16 => ctx.profiler.time(Stage::Unpack, || {
                crate::medium::decode_u16_rows(ctx, data, unit, off, order, wu, wu * 2, img.u16_mut())
            })?,
        }
        Ok(img)
    };
    let format = if is_3fr { RawFormat::ThreeFr } else { RawFormat::Fff };
    let mut dec = SimpleDecoder::new(format, meta, info, Box::new(decode_fn));
    if payload == Payload::Raw16 {
        dec.random_access = RandomAccess::Rows;
        dec.packed = Some(PackedPayload {
            data: src,
            width: w,
            height: h,
            bits: 16,
            order: if order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes },
            row_stride: wu * 2,
        });
    }
    Ok(Box::new(dec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensor_families() {
        let mn = |c, k| Maker { sensor_code: c, coating_code: k, has_color_matrix: false };
        let s = classify(&mn(15, 5), 8384, 6304, "X1D II 50C");
        assert_eq!((s.code, s.sub_code, s.uncropped), (15, 2, true));
        assert_eq!(s.family.as_deref(), Some("50-15-Coated5-II"));
        let s = classify(&mn(15, 5), 8374, 6304, "CFV-50c");
        assert_eq!((s.sub_code, s.family.as_deref()), (0, Some("50-15-Coated5")));
        let s = classify(&mn(0, 0), 7262, 5456, "H3D");
        assert_eq!((s.code, s.family.as_deref()), (9, Some("39-Uncoated")));
        let s = classify(&mn(20, 6), 11904, 8842, "X2D 100C");
        assert_eq!(s.family.as_deref(), Some("100-20-Coated6"));
    }
}
