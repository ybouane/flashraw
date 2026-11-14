//! Hasselblad (and Imacon) 3FR / FFF reader.
//!
//! Both containers are TIFF, in either byte order. IFD0 is a small RGB thumbnail with the
//! EXIF sub-IFD (maker note included); the raw image is a SubIFD with photometric 32803, one
//! strip and 16 bits per sample, stored either as Hasselblad's lossless JPEG stream
//! (compression 7 or 99) or as plain 16-bit samples (compression 1). The sensor is
//! identified from the maker note's sensor and coating codes, or from the buffer size when
//! those do not match; the identity selects the visible area, the black and white level and
//! the colour matrix key (`<family>-3FR` / `<family>-FFF`). Bodies sold under the
//! Hasselblad name that write Sony ARW files are handed to the Sony reader.

use crate::common::*;
use crate::medium::{cfa_at_origin, decode_u16_rows, even_margins};
use flashraw_codecs::hasselblad::{HasselbladHeader, decode_par};
use flashraw_core::tiff::{Ifd, Tiff, TiffOptions, parse_ifd_at, tags, types};
use flashraw_core::*;

// ---------------------------------------------------------------------------------------------
// Camera facts
// ---------------------------------------------------------------------------------------------

/// Raw IFD photometric value (CFA) and the TIFF compressions it may use.
const PHOTOMETRIC_CFA: u32 = 32803;
const COMPRESSION_NONE: u32 = 1;
const COMPRESSION_LJPEG: [u32; 2] = [7, 99];
/// Compressions of Sony-built bodies (ARW).
const COMPRESSION_SONY: [u32; 3] = [32767, 32769, 32770];

/// Hasselblad's embedded-Exif blob; its presence pins the white level to full scale.
const TAG_EMBEDDED_EXIF: u16 = 0xC51B;

/// Maker note tags (IFD at the note's data, no header).
const NOTE_SENSOR_CODE: u16 = 0x0011;
const NOTE_MODEL_NAME: u16 = 0x0015;
const NOTE_COATING_CODE: u16 = 0x0016;
const NOTE_CCD_PROFILE: u16 = 0x002A;
/// The CCD profile is 12 signed rationals.
const NOTE_CCD_PROFILE_COUNT: u32 = 12;

/// Sensor generation for a (sensor code, coating code) pair of the maker note.
struct FamilyRow {
    sensor: u32,
    coatings: &'static [u32],
    family: &'static str,
}

/// Family names are the colour matrix key stems of the camera database.
const FAMILY_BY_CODES: [FamilyRow; 13] = [
    FamilyRow { sensor: 4, coatings: &[0, 1], family: "16-Uncoated" },
    FamilyRow { sensor: 6, coatings: &[0, 1], family: "22-Uncoated" },
    FamilyRow { sensor: 8, coatings: &[1], family: "31-Uncoated" },
    FamilyRow { sensor: 9, coatings: &[0, 1], family: "39-Uncoated" },
    FamilyRow { sensor: 9, coatings: &[4], family: "39-Coated" },
    FamilyRow { sensor: 13, coatings: &[4], family: "40-Coated" },
    FamilyRow { sensor: 13, coatings: &[5], family: "40-Coated5" },
    FamilyRow { sensor: 11, coatings: &[4], family: "50-Coated" },
    FamilyRow { sensor: 11, coatings: &[5], family: "50-Coated5" },
    FamilyRow { sensor: 15, coatings: &[5], family: "50-15-Coated5" },
    FamilyRow { sensor: 12, coatings: &[4], family: "60-Coated" },
    FamilyRow { sensor: 17, coatings: &[5], family: "100-17-Coated5" },
    FamilyRow { sensor: 20, coatings: &[6], family: "100-20-Coated6" },
];

/// Family of the second generation 50 MP sensor (X1D, X1D II, CFV II 50C bodies).
const FAMILY_50MP: &str = "50-15-Coated5";
const FAMILY_50MP_II: &str = "50-15-Coated5-II";
/// Sensor code of the 50 MP sensor and the 3FR width of the X1D.
const CODE_50MP: u32 = 15;
const WIDTH_X1D: u32 = 8384;

/// A raw buffer size; `h == None` matches any height.
#[derive(Clone, Copy)]
struct Size {
    w: u32,
    h: Option<u32>,
}

const fn size(w: u32, h: u32) -> Size {
    Size { w, h: Some(h) }
}

impl Size {
    fn matches(&self, w: u32, h: u32) -> bool {
        self.w == w && self.h.is_none_or(|sh| sh == h)
    }
}

/// Buffer sizes (full and cropped readouts) of a sensor, with the provisional sensor code.
struct SizeGroup {
    sizes: &'static [Size],
    family: &'static str,
    code: u32,
}

const SIZE_GROUPS: [SizeGroup; 10] = [
    SizeGroup {
        sizes: &[Size { w: 4090, h: None }, size(4096, 4096), size(4088, 4088), size(4080, 4080)],
        family: "16-Uncoated",
        code: 4,
    },
    SizeGroup { sizes: &[size(4096, 5456), size(4088, 5448), size(4080, 5440)], family: "22-Uncoated", code: 6 },
    SizeGroup { sizes: &[size(6542, 4916), size(6504, 4880), size(6496, 4872)], family: "31-Uncoated", code: 8 },
    SizeGroup {
        sizes: &[size(7262, 5456), size(7224, 5420), size(7216, 5412), size(7212, 5412)],
        family: "39-Uncoated",
        code: 9,
    },
    SizeGroup { sizes: &[size(7410, 5586), size(7312, 5486), size(7304, 5478)], family: "40-Coated5", code: 13 },
    SizeGroup { sizes: &[size(8282, 6240), size(8184, 6140), size(8176, 6132)], family: "50-Coated", code: 11 },
    SizeGroup {
        sizes: &[size(8374, 6304), size(8384, 6304), size(8280, 6208), size(8272, 6200)],
        family: FAMILY_50MP,
        code: CODE_50MP,
    },
    SizeGroup { sizes: &[size(9044, 6732), size(8964, 6716), size(8956, 6708)], family: "60-Coated", code: 12 },
    SizeGroup { sizes: &[size(12000, 8816), size(11608, 8708), size(11600, 8700)], family: "100-17-Coated5", code: 17 },
    SizeGroup { sizes: &[size(11904, 8842), size(11664, 8750), size(11656, 8742)], family: "100-20-Coated6", code: 20 },
];

/// Size ranges (half-open on `w * h`) that identify a family when no exact size does.
struct AreaRange {
    from: u64,
    to: u64,
    family: &'static str,
    code: u32,
}

const AREA_RANGES: [AreaRange; 2] = [
    AreaRange { from: 7212 * 5412, to: 7304 * 5478, family: "39-Uncoated", code: 9 },
    // No matrix row in the database: such files take the file's ColorMatrix1; the code stays 0.
    AreaRange { from: 10320 * 7752, to: 10520 * 8000, family: "80-Coated", code: 0 },
];

/// Full sensor readouts, which carry the vendor margins of [`VISIBLE`].
const UNCROPPED_SIZES: [Size; 14] = [
    Size { w: 4090, h: None },
    size(4096, 4096),
    size(5568, 3648),
    size(4096, 5456),
    size(6542, 4916),
    size(7262, 5456),
    size(7410, 5586),
    size(8282, 6240),
    size(8374, 6304),
    size(8384, 6304),
    size(9044, 6732),
    size(10320, 7752),
    size(12000, 8816),
    size(11904, 8842),
];

/// An extent of the visible area: fixed, or the buffer dimension minus a margin (saturating).
#[derive(Clone, Copy)]
enum Extent {
    Fixed(u32),
    Trim(u32),
}

impl Extent {
    fn of(self, full: u32) -> u32 {
        match self {
            Extent::Fixed(v) => v,
            Extent::Trim(t) => full.saturating_sub(t),
        }
    }
}

/// Colour at the visible area's first sample (all Hasselblad readouts are Bayer).
#[derive(Clone, Copy)]
enum Phase {
    Rggb,
    Grbg,
}

impl Phase {
    fn pattern(self) -> CfaPattern {
        match self {
            Phase::Rggb => CfaPattern::RGGB,
            Phase::Grbg => CfaPattern::GRBG,
        }
    }
}

/// What a geometry row does to the black level.
#[derive(Clone, Copy)]
enum BlackRule {
    Keep,
    Add(f32),
}

/// Only rows of this black level range apply.
#[derive(Clone, Copy)]
enum BlackCondition {
    Always,
    Above(f32),
    AtMost(f32),
}

/// Visible area, black and white level of a full readout, per sensor code and generation.
struct Visible {
    codes: &'static [u32],
    /// Second generation flag (`None`: any).
    sub: Option<u32>,
    when: BlackCondition,
    x: u32,
    y: u32,
    w: Extent,
    h: Extent,
    phase: Phase,
    black: BlackRule,
    white: Option<u32>,
    /// Offset the lossless JPEG decoder adds to every sample.
    ljpeg_base: i32,
}

const fn visible(codes: &'static [u32], x: u32, y: u32, w: Extent, h: Extent, phase: Phase) -> Visible {
    Visible {
        codes,
        sub: None,
        when: BlackCondition::Always,
        x,
        y,
        w,
        h,
        phase,
        black: BlackRule::Keep,
        white: None,
        ljpeg_base: 0,
    }
}

/// First matching row wins; codes without a row use the whole buffer.
const VISIBLE: [Visible; 8] = [
    visible(&[9], 7, 4, Extent::Fixed(7248), Extent::Fixed(5444), Phase::Grbg),
    visible(&[11, 13], 41, 4, Extent::Trim(82), Extent::Trim(84), Phase::Grbg),
    // Levels as the file states them: the H5D-50c sample's BlackLevel tag is 256, the X1D,
    // X1D II and 907X samples state 256 / 65535 (the X1D sample clips at 65535).
    Visible { sub: Some(0), ..visible(&[15], 52, 100, Extent::Fixed(8272), Extent::Fixed(6200), Phase::Rggb) },
    Visible { sub: Some(2), ..visible(&[15], 48, 96, Extent::Trim(106), Extent::Trim(96), Phase::Rggb) },
    // 60 MP back: the black level tells the two firmware conventions apart.
    Visible {
        when: BlackCondition::Above(500.0),
        ..visible(&[12], 44, 12, Extent::Fixed(8956), Extent::Fixed(6708), Phase::Rggb)
    },
    // The decoder adds 256 to every sample of this convention. Measured on the 60 MP sample
    // (A0000902.3FR, BlackLevel tag 0, WhiteLevel 32767): optical-black columns have a median of
    // 255-256 and highlights clip at 33025 (90 image pixels).
    Visible {
        when: BlackCondition::AtMost(500.0),
        black: BlackRule::Add(256.0),
        white: Some(33025),
        ljpeg_base: 256,
        ..visible(&[12], 40, 8, Extent::Fixed(8964), Extent::Fixed(6716), Phase::Rggb)
    },
    visible(&[17], 64, 108, Extent::Fixed(11608), Extent::Trim(108), Phase::Rggb),
    visible(&[20, 22], 124, 92, Extent::Fixed(11664), Extent::Trim(92), Phase::Rggb),
];

/// The 16 MP "V96C" back (code 4, 4090 wide): applies whatever the size table says.
const WIDTH_V96C: u32 = 4090;
const V96C: Visible = visible(&[4], 3, 6, Extent::Trim(10), Extent::Trim(6), Phase::Grbg);

/// Defaults when neither the raw IFD nor a row says otherwise.
const DEFAULT_WHITE: u32 = 65535;

// ---------------------------------------------------------------------------------------------
// Maker note
// ---------------------------------------------------------------------------------------------

/// Values of the maker note that identify the sensor and the container flavour.
#[derive(Default)]
struct MakerNote {
    sensor_code: u32,
    coating_code: u32,
    /// The model name string is sane and the CCD colour profile is present.
    has_ccd_profile: bool,
    /// The last nine values of the CCD colour profile: the sensor's XYZ to camera matrix
    /// (the first three are 1). Empty without a profile.
    ccd_matrix: Vec<f32>,
}

fn name_is_plain(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'/' | b'-'))
}

fn read_maker_note(tiff: &Tiff) -> MakerNote {
    let mut note = MakerNote::default();
    let Some(entry) = tiff.find_entry(tags::MAKER_NOTE) else { return note };
    let Ok(start) = u32::try_from(entry.value_pos) else { return note };
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    let Ok(ifd) = parse_ifd_at(tiff.data, tiff.order, start, opts) else { return note };
    note.sensor_code = ifd.u32(NOTE_SENSOR_CODE).unwrap_or(0);
    note.coating_code = ifd.u32(NOTE_COATING_CODE).unwrap_or(0);
    let name_ok = ifd.string(NOTE_MODEL_NAME).is_some_and(|s| name_is_plain(&s));
    let profile = ifd.get(NOTE_CCD_PROFILE).filter(|e| e.typ == types::SRATIONAL && e.count == NOTE_CCD_PROFILE_COUNT);
    note.has_ccd_profile = name_ok && profile.is_some();
    if let Some(e) = profile {
        let v = e.f32s();
        if v.len() == NOTE_CCD_PROFILE_COUNT as usize && v.iter().all(|x| x.is_finite()) {
            note.ccd_matrix = v[3..].to_vec();
        }
    }
    note
}

// ---------------------------------------------------------------------------------------------
// Sensor identification and geometry
// ---------------------------------------------------------------------------------------------

/// What the maker note, the buffer size and the body name say about the sensor.
#[derive(Debug, PartialEq)]
struct Identity {
    /// Final sensor code (0 when unknown).
    code: u32,
    /// 2 for the second generation 50 MP sensor, else 0.
    sub: u32,
    family: Option<&'static str>,
}

/// Model name without the maker, and the body (the part before the first '/').
fn body_name(model: &str) -> String {
    let mut name = model.trim();
    while let Some(rest) = name.strip_prefix("Hasselblad") {
        name = rest.trim_start();
    }
    let name = name.trim();
    name.split('/').next().unwrap_or(name).to_string()
}

fn is_x1d_or_cfv_ii(body: &str) -> bool {
    body.starts_with("X1D") || body.starts_with("CFV II")
}

fn size_family(w: u32, h: u32) -> Option<(&'static str, u32)> {
    let exact = SIZE_GROUPS.iter().find(|g| g.sizes.iter().any(|s| s.matches(w, h))).map(|g| (g.family, g.code));
    exact.or_else(|| {
        let area = w as u64 * h as u64;
        AREA_RANGES.iter().find(|r| (r.from..r.to).contains(&area)).map(|r| (r.family, r.code))
    })
}

fn identify(note: &MakerNote, w: u32, h: u32, body: &str) -> Identity {
    let by_codes = FAMILY_BY_CODES
        .iter()
        .find(|r| r.sensor == note.sensor_code && r.coatings.contains(&note.coating_code))
        .map(|r| r.family);
    if let Some(mut family) = by_codes {
        let mut sub = 0;
        if family == FAMILY_50MP && is_x1d_or_cfv_ii(body) {
            sub = 2;
            let second = body.starts_with("CFV II 50C")
                || body.as_bytes().get(..10).is_some_and(|b| b.eq_ignore_ascii_case(b"X1D II 50C"));
            if second {
                family = FAMILY_50MP_II;
            }
        }
        return Identity { code: note.sensor_code, sub, family: Some(family) };
    }
    let (mut family, by_size) = match size_family(w, h) {
        Some((f, c)) => (Some(f), c),
        None => (None, 0),
    };
    let code = if note.sensor_code == 0 { by_size } else { note.sensor_code };
    let mut sub = 0;
    if code == CODE_50MP && (w == WIDTH_X1D || is_x1d_or_cfv_ii(body)) {
        sub = 2;
        if body.contains(" II ") {
            family = Some(FAMILY_50MP_II);
        }
    }
    Identity { code, sub, family }
}

fn is_uncropped(w: u32, h: u32) -> bool {
    UNCROPPED_SIZES.iter().any(|s| s.matches(w, h))
}

/// Visible area and levels of the raw buffer.
#[derive(Debug, PartialEq)]
struct Geometry {
    visible: Rect,
    phase: CfaPattern,
    black: f32,
    white: u32,
    ljpeg_base: i32,
}

fn geometry(id: &Identity, w: u32, h: u32, black: f32, white: u32) -> Geometry {
    let mut g = Geometry { visible: Rect::full(w, h), phase: CfaPattern::RGGB, black, white, ljpeg_base: 0 };
    let applies = |r: &Visible| {
        r.codes.contains(&id.code)
            && r.sub.is_none_or(|s| s == id.sub)
            && match r.when {
                BlackCondition::Always => true,
                BlackCondition::Above(b) => black > b,
                BlackCondition::AtMost(b) => black <= b,
            }
    };
    let mut apply = |r: &Visible| {
        g.visible = Rect::new(r.x, r.y, r.w.of(w), r.h.of(h));
        g.phase = r.phase.pattern();
        match r.black {
            BlackRule::Keep => {}
            BlackRule::Add(v) => g.black += v,
        }
        if let Some(v) = r.white {
            g.white = v;
        }
        g.ljpeg_base = r.ljpeg_base;
    };
    if is_uncropped(w, h) {
        if let Some(row) = VISIBLE.iter().find(|r| applies(r)) {
            apply(row);
        }
    }
    if id.code == 4 && w == WIDTH_V96C {
        apply(&V96C);
    }
    g.visible = g.visible.clamp_to(w, h);
    g
}

// ---------------------------------------------------------------------------------------------
// Container flavour, colour, masks
// ---------------------------------------------------------------------------------------------

/// True when some IFD has `tag` with a declared count of 9 or 12.
fn has_matrix_tag(tiff: &Tiff, tag: u16) -> bool {
    tiff.all_ifds().into_iter().any(|i| i.get(tag).is_some_and(|e| e.count == 9 || e.count == 12))
}

/// 3FR files carry a single ColorMatrix1; FFF files written by the desktop software have a
/// ColorMatrix2 and/or the maker note's CCD profile.
fn is_3fr(tiff: &Tiff, note: &MakerNote) -> bool {
    has_matrix_tag(tiff, tags::COLOR_MATRIX1) && !has_matrix_tag(tiff, tags::COLOR_MATRIX2) && !note.has_ccd_profile
}

/// The database matrix of a family, else the maker note's CCD profile, else the file's
/// ColorMatrix1. The family rows of FFF files were taken from that same profile (the
/// H5D-50c's "50-15-Coated5" row is its file's profile), so it reads the same way.
fn colour(tiff: &Tiff, note: &MakerNote, key: Option<&str>) -> ColorMatrices {
    let mut out = ColorMatrices::default();
    if let Some(m) = key.and_then(|k| flashraw_camera_db::find_matrix("Hasselblad", k)) {
        let values = m.color_matrix.iter().map(|&v| v as f32 / 10000.0).collect();
        if let Some(cm) = ColorMatrix::new(Illuminant::D65, m.color_matrix.len() / 3, values) {
            out.color.push(cm);
            out.source = ColorSource::CameraDb;
            return out;
        }
    }
    if let Some(cm) = ColorMatrix::new(Illuminant::D65, 3, note.ccd_matrix.clone()) {
        out.color.push(cm);
        out.source = ColorSource::File;
        return out;
    }
    if let Some(e) = tiff.find_entry(tags::COLOR_MATRIX1).filter(|e| e.count == 9) {
        if let Some(cm) = ColorMatrix::new(Illuminant::D65, 3, e.f32s()) {
            out.color.push(cm);
            out.source = ColorSource::File;
        }
    }
    out
}

/// Optical-black rectangles of a DNG MaskedAreas tag: groups of top, left, bottom, right.
fn masked_areas(raw: &Ifd) -> Vec<Rect> {
    let Some(e) = raw.get(tags::MASKED_AREAS) else { return Vec::new() };
    e.u32s().chunks_exact(4).filter_map(|g| Rect::from_ltrb(g[1], g[0], g[3], g[2])).collect()
}

/// Black level of the raw IFD (0 when absent or not a sane number).
fn file_black(raw: &Ifd) -> f32 {
    raw.get(tags::BLACK_LEVEL).and_then(|e| e.f64(0)).filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(0.0) as f32
}

/// White level: the raw IFD's, or full scale; full scale too when the embedded-Exif blob exists.
fn file_white(tiff: &Tiff, raw: &Ifd) -> u32 {
    let tagged = raw.u32(tags::WHITE_LEVEL).filter(|&v| v > 0).unwrap_or(DEFAULT_WHITE);
    if tiff.find_entry(TAG_EMBEDDED_EXIF).is_some() { DEFAULT_WHITE } else { tagged }
}

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

/// Hasselblad files are TIFF; they are dispatched by Make, never by signature.
pub fn probe(_data: &[u8]) -> bool {
    false
}

/// Not a magic-signature format.
pub fn open<'a>(_data: &'a [u8], _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("Hasselblad files are TIFF-based".into()))
}

/// How the sensor samples are stored.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Payload {
    LosslessJpeg,
    Raw16,
}

/// Open a Hasselblad / Imacon TIFF raw.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("3FR without IFD0"))?;
    let raw = tiff
        .all_ifds()
        .into_iter()
        .filter(|i| i.u32(tags::PHOTOMETRIC) == Some(PHOTOMETRIC_CFA) && i.width() > 0 && i.height() > 0)
        .max_by_key(|i| i.width() as u64 * i.height() as u64)
        .ok_or_else(|| no_raw_ifd(&tiff, "Hasselblad", Error::invalid_metadata("Hasselblad: no CFA image")))?;
    if COMPRESSION_SONY.contains(&raw.compression()) {
        return crate::sony::open_tiff(data, tiff, opts);
    }
    let (w, h) = (raw.width(), raw.height());
    opts.limits.check_dims(w, h, 1)?;
    if raw.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1) != 1 {
        return Err(Error::unsupported_compression("Hasselblad", "multi-shot capture"));
    }
    let (compression, bits) = (raw.compression(), raw.bps());
    let payload = match (compression, bits) {
        (c, _) if COMPRESSION_LJPEG.contains(&c) => Payload::LosslessJpeg,
        (COMPRESSION_NONE, 16) => Payload::Raw16,
        _ => {
            return Err(Error::unsupported_compression(
                "Hasselblad",
                format!("compression {compression}, {bits} bits"),
            ));
        }
    };

    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let body = body_name(&meta.model);
    let note = read_maker_note(&tiff);
    let id = identify(&note, w, h, &body);
    let container_3fr = is_3fr(&tiff, &note);
    meta.set_extra("hasselblad_container", if container_3fr { "3FR" } else { "FFF" });
    meta.set_extra("hasselblad_sensor_code", id.code);
    let cam = db_lookup(&mut meta, "");
    let key = id.family.map(|f| format!("{f}-{}", if container_3fr { "3FR" } else { "FFF" }));
    if let Some(k) = &key {
        meta.set_extra("hasselblad_sensor", k);
    }

    let geo = geometry(&id, w, h, file_black(raw), file_white(&tiff, raw));
    let mut info = FrameInfo::new(w, h, 16);
    info.layout = SensorLayout::Bayer;
    info.cfa = Some(cfa_at_origin(geo.phase, geo.visible.x, geo.visible.y));
    let area = even_margins(geo.visible);
    info.active_area = area;
    info.crop = area;
    info.white_level = WhiteLevel::uniform(geo.white);
    info.black_level = BlackLevel::uniform(geo.black);
    info.orientation = Orientation::from_exif(ifd0.u32(tags::ORIENTATION).unwrap_or(1));
    info.compression =
        if payload == Payload::LosslessJpeg { Compression::HasselbladLjpeg } else { Compression::Uncompressed };
    info.white_balance = tiff.find_entry(tags::AS_SHOT_NEUTRAL).and_then(|e| WhiteBalance::from_neutral(&e.f32s()));
    info.color_matrices = colour(&tiff, &note, key.as_deref());
    info.masked_areas = masked_areas(raw);
    info.black_from_masked = !info.masked_areas.is_empty();
    apply_db(
        &mut info,
        &meta,
        cam,
        Known { cfa: true, crop: true, active_area: true, black: true, white: true, masked: true, color: false },
    );
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);

    let (offset, declared) = *strips(raw).first().ok_or_else(|| Error::invalid_metadata("Hasselblad: no strips"))?;
    let format = if container_3fr { RawFormat::ThreeFr } else { RawFormat::Fff };
    let order = tiff.order;
    let (width, height) = (w as usize, h as usize);
    let base = geo.ljpeg_base;
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
        match payload {
            Payload::LosslessJpeg => {
                let stream = data.get(offset..).unwrap_or(&[]);
                let truncated = |e: Error| e.or_truncated(data.len(), DataUnit::Strip(0), offset, declared);
                ctx.profiler.time(Stage::EntropyDecode, || {
                    let header = HasselbladHeader::parse(stream).map_err(truncated)?;
                    decode_par(stream, &header, img.u16_mut(), width, height, base, ctx.parallel).map_err(truncated)
                })?;
            }
            Payload::Raw16 => {
                ctx.profiler.time(Stage::Unpack, || {
                    decode_u16_rows(ctx, data, DataUnit::Strip(0), offset, order, width, width * 2, img.u16_mut())
                })?;
            }
        }
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(format, meta, info, Box::new(decode));
    if payload == Payload::Raw16 {
        let wanted = declared.max(width.saturating_mul(height).saturating_mul(2));
        let bytes =
            if offset < data.len() { &data[offset..data.len().min(offset.saturating_add(wanted))] } else { &[] };
        dec.random_access = RandomAccess::Rows;
        dec.packed = Some(PackedPayload {
            data: bytes,
            width: w,
            height: h,
            bits: 16,
            order: if order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes },
            row_stride: width.saturating_mul(2),
        });
    }
    Ok(Box::new(dec))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(sensor_code: u32, coating_code: u32) -> MakerNote {
        MakerNote { sensor_code, coating_code, has_ccd_profile: false, ccd_matrix: Vec::new() }
    }

    #[test]
    fn the_ccd_profile_is_the_matrix_without_a_family_row() {
        // A minimal TIFF: IFD0 with one entry and no colour tags.
        let data = [b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0, 1, 4, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        let tiff = Tiff::parse(&data).unwrap();
        let mut n = note(13, 4);
        n.ccd_matrix = vec![0.69, -0.2, -0.03, -0.44, 1.23, 0.23, -0.17, 0.32, 0.68];
        // The 40 MP coated sensor has a 3FR row only; its FFF files carry their own profile.
        let c = colour(&tiff, &n, Some("40-Coated-FFF"));
        assert_eq!((c.source, c.color[0].values.clone()), (ColorSource::File, n.ccd_matrix.clone()));
        assert_eq!(colour(&tiff, &n, Some("40-Coated-3FR")).source, ColorSource::CameraDb);
        assert!(colour(&tiff, &note(13, 4), Some("40-Coated-FFF")).color.is_empty());
    }

    #[test]
    fn identification_by_codes() {
        let id = identify(&note(15, 5), 8384, 6304, "X1D II 50C");
        assert_eq!(id, Identity { code: 15, sub: 2, family: Some("50-15-Coated5-II") });
        let id = identify(&note(15, 5), 8384, 6304, "x1d ii 50c");
        assert_eq!(id, Identity { code: 15, sub: 0, family: Some("50-15-Coated5") });
        let id = identify(&note(15, 5), 8384, 6304, "X1D");
        assert_eq!(id, Identity { code: 15, sub: 2, family: Some("50-15-Coated5") });
        let id = identify(&note(15, 5), 8374, 6304, "CFV II 50C");
        assert_eq!((id.sub, id.family), (2, Some("50-15-Coated5-II")));
        let id = identify(&note(15, 5), 8374, 6304, "CFV-50c");
        assert_eq!((id.sub, id.family), (0, Some("50-15-Coated5")));
        assert_eq!(identify(&note(20, 6), 11904, 8842, "X2D 100C").family, Some("100-20-Coated6"));
        // Coating 0 and 1 are equivalent for the uncoated sensors.
        assert_eq!(identify(&note(9, 0), 7262, 5456, "H3D").family, Some("39-Uncoated"));
        assert_eq!(identify(&note(9, 1), 7262, 5456, "H3D").family, Some("39-Uncoated"));
        assert_eq!(identify(&note(9, 4), 7262, 5456, "H3D").family, Some("39-Coated"));
    }

    #[test]
    fn identification_by_size() {
        // No maker note: code 0 at 7262x5456 is the 39 MP sensor.
        let id = identify(&note(0, 0), 7262, 5456, "H3D");
        assert_eq!(id, Identity { code: 9, sub: 0, family: Some("39-Uncoated") });
        // An unknown code keeps the code but takes the family from the size.
        let id = identify(&note(99, 0), 7262, 5456, "H3D");
        assert_eq!(id, Identity { code: 99, sub: 0, family: Some("39-Uncoated") });
        // The " II " rule of the size path.
        let id = identify(&note(0, 9), 8384, 6304, "Foo II 50C");
        assert_eq!(id, Identity { code: 15, sub: 2, family: Some("50-15-Coated5-II") });
        let id = identify(&note(0, 0), 8374, 6304, "X1D 50C");
        assert_eq!(id, Identity { code: 15, sub: 2, family: Some("50-15-Coated5") });
        // Area ranges.
        assert_eq!(size_family(7250, 5430), Some(("39-Uncoated", 9)));
        assert_eq!(size_family(10400, 7800), Some(("80-Coated", 0)));
        assert_eq!(size_family(100, 100), None);
        assert_eq!(size_family(4090, 12345), Some(("16-Uncoated", 4)));
    }

    #[test]
    fn body_names() {
        assert_eq!(body_name("Hasselblad H3D"), "H3D");
        assert_eq!(body_name(" Hasselblad  Hasselblad CFV-50c/Flash Sync "), "CFV-50c");
        assert_eq!(body_name("X1D II 50C"), "X1D II 50C");
        assert_eq!(body_name(""), "");
    }

    #[test]
    fn geometry_and_cfa_phase() {
        // H3D: 7,4,7248,5444 with GRBG -> active 8,4,7247,5444, RGGB at the buffer origin.
        let id = identify(&note(9, 4), 7262, 5456, "H3D");
        let g = geometry(&id, 7262, 5456, 0.0, 65535);
        assert_eq!(g.visible, Rect::new(7, 4, 7248, 5444));
        assert_eq!(even_margins(g.visible), Rect::new(8, 4, 7247, 5444));
        assert_eq!(cfa_at_origin(g.phase, g.visible.x, g.visible.y), CfaPattern::RGGB);
        // 60 MP back: both black conventions.
        let id = identify(&note(12, 4), 9044, 6732, "H4D-60");
        let low = geometry(&id, 9044, 6732, 0.0, 65535);
        assert_eq!(
            (low.visible, low.black, low.white, low.ljpeg_base),
            (Rect::new(40, 8, 8964, 6716), 256.0, 33025, 256)
        );
        let high = geometry(&id, 9044, 6732, 600.0, 65535);
        assert_eq!(
            (high.visible, high.black, high.white, high.ljpeg_base),
            (Rect::new(44, 12, 8956, 6708), 600.0, 65535, 0)
        );
        // X1D II sub 2: the file's levels are kept.
        let id = identify(&note(15, 5), 8384, 6304, "X1D II 50C");
        let g = geometry(&id, 8384, 6304, 256.0, 1000);
        assert_eq!((g.visible, g.black, g.white), (Rect::new(48, 96, 8278, 6208), 256.0, 1000));
        // Cropped sizes keep the whole buffer.
        let id = identify(&note(11, 4), 8184, 6140, "H4D-50");
        assert_eq!(geometry(&id, 8184, 6140, 0.0, 65535).visible, Rect::full(8184, 6140));
        // V96C overrides, and everything is clamped to the buffer.
        let id = identify(&note(4, 0), 4090, 4090, "V96C");
        let g = geometry(&id, 4090, 4090, 0.0, 65535);
        assert_eq!(g.visible, Rect::new(3, 6, 4080, 4084));
        assert_eq!(g.phase, CfaPattern::GRBG);
        assert_eq!(geometry(&id, 4090, 3, 0.0, 65535).visible, Rect::default());
    }
}
