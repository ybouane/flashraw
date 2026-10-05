//! Adobe DNG (1.0 – 1.7).
//!
//! Supports CFA, LinearRaw and monochrome images stored as strips or tiles with
//! compression 1 (uncompressed, 8–32 bit, integer or float), 7 (lossless JPEG),
//! 8/32946 (deflate, predictors 1/2/3/34892–34895), 34892 (lossy JPEG) and 52546
//! (JPEG XL, `jpegxl` feature). Tiles/strips decode in parallel straight into the
//! output buffer; region decoding touches only intersecting tiles.
//!
//! DNG anchors CFA and black-level patterns at the ActiveArea origin; FlashRaw
//! re-anchors them at the buffer origin (see `flashraw_core::types`).

use crate::common::*;
use flashraw_codecs::{deflate, ljpeg, packed};
use flashraw_core::par::{GridWriter, TileMut};
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;

/// Is this TIFF a DNG?
pub fn is_dng(tiff: &Tiff) -> bool {
    tiff.ifds.first().is_some_and(|i| i.has(tags::DNG_VERSION))
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Payload {
    Uncompressed,
    Ljpeg,
    Deflate,
    LossyJpeg,
    JpegXl,
    /// GoPro VC-5: one wavelet bitstream for the whole image.
    Vc5,
}

/// Parsed DNG raw image description shared by full/region decode.
struct DngRaw<'a> {
    data: &'a [u8],
    order: ByteOrder,
    width: u32,
    height: u32,
    cpp: u32,
    bps: u32,
    float: bool,
    /// Byte order of float samples (the TIFF's, except for one writer's quirk).
    float_order: ByteOrder,
    payload: Payload,
    predictor: u32,
    tile_w: u32,
    tile_h: u32,
    /// Units are TIFF tiles (strips otherwise).
    tiled: bool,
    /// Byte ranges per grid tile (row-major).
    units: Vec<(usize, usize)>,
    linearization: Option<Vec<u16>>,
    dng_bug16: bool,
    vc5_white: u32,
    vc5_phase: Option<flashraw_codecs::vc5::CfaPhase>,
    /// RowInterleaveFactor and ColumnInterleaveFactor: a tile stores its rows (columns)
    /// grouped by `index % factor`.
    #[cfg_attr(not(feature = "jpegxl"), allow(dead_code))]
    interleave: (usize, usize),
}

/// Stored position of row (column) `pos` of an `n`-long tile dimension whose rows
/// (columns) are grouped into `factor` fields: field `pos % factor` holds positions
/// `f, f + factor, ...` in order, fields one after another.
#[cfg_attr(not(feature = "jpegxl"), allow(dead_code))]
fn interleaved_pos(pos: usize, n: usize, factor: usize) -> usize {
    if factor <= 1 || pos >= n {
        return pos;
    }
    let f = pos % factor;
    // Lengths of the fields before `f`: ceil((n - g) / factor) for g < f.
    let before: usize = (0..f).map(|g| (n - g).div_ceil(factor)).sum();
    before + pos / factor
}

/// Open a DNG.
pub fn open<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("DNG without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let version = ifd0.get(tags::DNG_VERSION).map(|e| e.bytes()).unwrap_or(&[1, 0, 0, 0]);
    let mut v4 = [0u8; 4];
    for (i, b) in version.iter().take(4).enumerate() {
        v4[i] = *b;
    }
    meta.dng_version = Some(v4);
    meta.unique_camera_model = ifd0.string(tags::UNIQUE_CAMERA_MODEL);
    if meta.make.is_empty() || meta.model.is_empty() {
        if let Some(u) = &meta.unique_camera_model {
            let mut it = u.splitn(2, ' ');
            if meta.make.is_empty() {
                meta.make = it.next().unwrap_or("").to_string();
            }
            if meta.model.is_empty() {
                meta.model = it.next().unwrap_or("").to_string();
            }
        }
    }
    let cam = db_lookup(&mut meta, "dng");
    let _ = cam;

    let raw_ifd = find_raw_ifd(&tiff).ok_or_else(|| Error::invalid_metadata("DNG has no raw image IFD"))?;
    let (info, raw) = parse_raw(data, &tiff, ifd0, raw_ifd, v4, &meta, opts)?;
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);
    meta.dng = Some(dng_extras(ifd0, raw_ifd));
    if let Some(l) = raw_ifd.u32(tags::CFA_LAYOUT).filter(|&l| l != 1) {
        meta.set_extra("cfa_layout", l);
    }
    if let Some(sw) = &meta.software {
        meta.set_extra("software", sw.clone());
    }
    meta.set_extra("dng_version", format!("{}.{}.{}.{}", v4[0], v4[1], v4[2], v4[3]));

    let raw = std::sync::Arc::new(raw);
    let (r1, r2) = (raw.clone(), raw.clone());
    let mut dec = SimpleDecoder::new(RawFormat::Dng, meta, info, Box::new(move |ctx| r1.decode_region(ctx, None)));
    dec.region = Some(Box::new(move |ctx, rect| r2.decode_region(ctx, Some(rect))));
    let tiled = raw.tile_w < raw.width;
    dec.random_access = match raw.payload {
        Payload::Uncompressed if raw.units.len() == 1 => RandomAccess::Rows,
        _ if tiled => RandomAccess::Tiles { width: raw.tile_w, height: raw.tile_h },
        _ if raw.units.len() > 1 => RandomAccess::Strips { rows: raw.tile_h },
        _ => RandomAccess::Sequential,
    };
    dec.units = raw.unit_list();
    if raw.payload == Payload::Uncompressed
        && raw.units.len() == 1
        && !raw.float
        && raw.linearization.is_none()
        && raw.cpp == 1
    {
        let (o, l) = raw.units[0];
        let bits = raw.bps;
        let order =
            if bits == 16 && raw.order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
        // Addressed by geometry (see `unit_geometry`); a truncated payload is not complete
        // and decodes on the CPU under the corruption policy.
        let l = l.max(raw.unit_geometry(0, raw.height as usize).needed());
        if let Ok(d) = flashraw_core::bytes::slice_at_lenient(data, o, l) {
            dec.packed = Some(PackedPayload {
                data: d,
                width: raw.width,
                height: raw.height,
                bits,
                order,
                row_stride: (raw.width as usize * bits as usize).div_ceil(8),
            });
        }
    }
    Ok(Box::new(dec))
}

/// The main raw IFD: NewSubFileType 0 with CFA/LinearRaw photometric, largest area.
fn find_raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    tiff.all_ifds()
        .into_iter()
        .filter(|i| i.u32(tags::NEW_SUBFILE_TYPE).unwrap_or(0) == 0)
        .filter(|i| {
            matches!(i.u32(tags::PHOTOMETRIC), Some(32803) | Some(34892))
                || (i.u32(tags::PHOTOMETRIC) == Some(1) && i.bps() > 8)
        })
        .filter(|i| i.width() > 0 && i.height() > 0)
        .fold(None, |best: Option<&'t Ifd<'a>>, i| match best {
            // Keep the first of equally large images: multi-frame DNGs (e.g. Pentax Pixel
            // Shift) store frames in order and frame 0 is the primary image.
            Some(b) if b.width() as u64 * b.height() as u64 >= i.width() as u64 * i.height() as u64 => Some(b),
            _ => Some(i),
        })
}

/// Rendering hints an editor applies itself (BaselineExposure, profile tables, user crop...).
/// Per-image tags may sit in the raw IFD; profile tags live in IFD0.
fn dng_extras(ifd0: &Ifd, raw: &Ifd) -> flashraw_core::DngExtras {
    let get = |t: u16| raw.get(t).or_else(|| ifd0.get(t));
    let finite = |v: Option<f64>| v.filter(|x| x.is_finite());
    let scalar = |t: u16| finite(get(t).and_then(|e| e.f64(0)));
    let vec = |t: u16| get(t).map(|e| e.f64s()).filter(|v| !v.is_empty() && v.iter().all(|x| x.is_finite()));
    let dims = |t: u16| {
        let v = ifd0.get(t)?.u32s();
        (v.len() >= 3 && v[..3].iter().all(|&d| d > 0 && d < 1 << 16)).then(|| [v[0], v[1], v[2]])
    };
    let mut x = flashraw_core::DngExtras::default();
    x.baseline_exposure = scalar(tags::BASELINE_EXPOSURE);
    x.baseline_noise = scalar(tags::BASELINE_NOISE);
    x.baseline_sharpness = scalar(tags::BASELINE_SHARPNESS);
    x.linear_response_limit = scalar(tags::LINEAR_RESPONSE_LIMIT);
    x.as_shot_neutral = vec(tags::AS_SHOT_NEUTRAL);
    x.as_shot_white_xy = vec(tags::AS_SHOT_WHITE_XY).filter(|v| v.len() >= 2).map(|v| (v[0], v[1]));
    x.default_user_crop = vec(tags::DEFAULT_USER_CROP).filter(|v| v.len() >= 4).map(|v| [v[0], v[1], v[2], v[3]]);
    x.default_scale = vec(tags::DEFAULT_SCALE).filter(|v| v.len() >= 2).map(|v| (v[0], v[1]));
    x.noise_profile = vec(tags::NOISE_PROFILE);
    let p = &mut x.profile;
    p.name = ifd0.string(tags::PROFILE_NAME).filter(|s| !s.is_empty());
    p.embed_policy = ifd0.u32(tags::PROFILE_EMBED_POLICY);
    p.hue_sat_map_count =
        [tags::PROFILE_HUE_SAT_MAP_DATA1, tags::PROFILE_HUE_SAT_MAP_DATA2, tags::PROFILE_HUE_SAT_MAP_DATA3]
            .iter()
            .filter(|&&t| ifd0.has(t))
            .count() as u8;
    p.hue_sat_map_dims = dims(tags::PROFILE_HUE_SAT_MAP_DIMS).filter(|_| p.hue_sat_map_count > 0);
    p.look_table_dims = dims(tags::PROFILE_LOOK_TABLE_DIMS).filter(|_| ifd0.has(tags::PROFILE_LOOK_TABLE_DATA));
    p.tone_curve = ifd0.has(tags::PROFILE_TONE_CURVE);
    p.baseline_exposure_offset = finite(ifd0.f64(tags::BASELINE_EXPOSURE_OFFSET));
    x
}

fn rational_pair(ifd: &Ifd, tag: u16) -> Option<(f64, f64)> {
    let e = ifd.get(tag)?;
    Some((e.f64(0)?, e.f64(1)?))
}

/// How many of the 32-bit float samples in `s` read in `order` look like image data
/// (zero, or finite with a magnitude in [1e-8, 1e4]).
fn float_plausibility(s: &[u8], order: ByteOrder) -> usize {
    s.chunks_exact(4)
        .map(|c| f32::from_bits(order.u32([c[0], c[1], c[2], c[3]])))
        .filter(|v| *v == 0.0 || (1e-8..=1e4).contains(&v.abs()))
        .count()
}

/// One calibration of the camera profile: an XYZ -> camera matrix (`rows` x 3) and an
/// optional CameraCalibration (`rows` x `rows`) at a correlated colour temperature.
#[derive(Debug, Clone)]
struct Calibration {
    temperature: Option<f64>,
    rows: usize,
    color: Vec<f64>,
    camera: Option<Vec<f64>>,
}

/// The profile's calibrations (ColorMatrix1/2/3 with their illuminants and
/// CameraCalibration1/2/3), or the database matrices when the file has none.
fn calibrations(ifd0: &Ifd, raw: &Ifd, order: ByteOrder, cm: &ColorMatrices) -> Vec<Calibration> {
    let sets = [
        (tags::COLOR_MATRIX1, tags::CALIBRATION_ILLUMINANT1, tags::CAMERA_CALIBRATION1, tags::ILLUMINANT_DATA1),
        (tags::COLOR_MATRIX2, tags::CALIBRATION_ILLUMINANT2, tags::CAMERA_CALIBRATION2, tags::ILLUMINANT_DATA2),
        (tags::COLOR_MATRIX3, tags::CALIBRATION_ILLUMINANT3, tags::CAMERA_CALIBRATION3, tags::ILLUMINANT_DATA3),
    ];
    let mut out = Vec::new();
    for (mtag, itag, ctag, dtag) in sets {
        let Some(m) = ifd0.get(mtag).or_else(|| raw.get(mtag)) else { continue };
        let color = m.f64s();
        let rows = color.len() / 3;
        if !(3..=4).contains(&rows) || color.len() != rows * 3 || !color.iter().all(|v| v.is_finite()) {
            continue;
        }
        let code = ifd0.u32(itag).unwrap_or(0);
        let temperature = if code == 255 {
            // Illuminant described by IlluminantData; only the chromaticity form is used.
            ifd0.get(dtag).and_then(|e| illuminant_data_xy(e.bytes(), order)).and_then(|(x, y)| xy_to_cct(x, y))
        } else {
            Illuminant(code.min(u16::MAX as u32) as u16).temperature().map(f64::from)
        };
        let camera =
            ifd0.get(ctag).map(|e| e.f64s()).filter(|v| v.len() == rows * rows && v.iter().all(|x| x.is_finite()));
        out.push(Calibration { temperature, rows, color, camera });
    }
    if out.is_empty() {
        for m in &cm.color {
            let rows = m.rows as usize;
            if (3..=4).contains(&rows) && m.values.len() == rows * 3 {
                let color = m.values.iter().map(|&v| v as f64).collect();
                out.push(Calibration {
                    temperature: m.illuminant.temperature().map(f64::from),
                    rows,
                    color,
                    camera: None,
                });
            }
        }
    }
    out
}

/// IlluminantData of chromaticity type: u16 type 0, then x and y as unsigned rationals.
fn illuminant_data_xy(b: &[u8], order: ByteOrder) -> Option<(f64, f64)> {
    let u16_at = |o: usize| b.get(o..o + 2).map(|s| order.u16([s[0], s[1]]));
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| order.u32([s[0], s[1], s[2], s[3]]));
    if u16_at(0)? != 0 {
        return None;
    }
    let rat = |o: usize| -> Option<f64> {
        let (n, d) = (u32_at(o)?, u32_at(o + 4)?);
        (d != 0).then(|| n as f64 / d as f64)
    };
    Some((rat(2)?, rat(10)?))
}

/// Correlated colour temperature of a CIE xy chromaticity (Robertson's method).
fn xy_to_cct(x: f64, y: f64) -> Option<f64> {
    // Isotemperature lines: reciprocal megakelvin, CIE 1960 u, v, slope.
    const LINES: [[f64; 4]; 31] = [
        [0.0, 0.18006, 0.26352, -0.24341],
        [10.0, 0.18066, 0.26589, -0.25479],
        [20.0, 0.18133, 0.26846, -0.26876],
        [30.0, 0.18208, 0.27119, -0.28539],
        [40.0, 0.18293, 0.27407, -0.30470],
        [50.0, 0.18388, 0.27709, -0.32675],
        [60.0, 0.18494, 0.28021, -0.35156],
        [70.0, 0.18611, 0.28342, -0.37915],
        [80.0, 0.18740, 0.28668, -0.40955],
        [90.0, 0.18880, 0.28997, -0.44278],
        [100.0, 0.19032, 0.29326, -0.47888],
        [125.0, 0.19462, 0.30141, -0.58204],
        [150.0, 0.19962, 0.30921, -0.70471],
        [175.0, 0.20525, 0.31647, -0.84901],
        [200.0, 0.21142, 0.32312, -1.0182],
        [225.0, 0.21807, 0.32909, -1.2168],
        [250.0, 0.22511, 0.33439, -1.4512],
        [275.0, 0.23247, 0.33904, -1.7298],
        [300.0, 0.24010, 0.34308, -2.0637],
        [325.0, 0.24792, 0.34655, -2.4681],
        [350.0, 0.25591, 0.34951, -2.9641],
        [375.0, 0.26400, 0.35200, -3.5814],
        [400.0, 0.27218, 0.35407, -4.3633],
        [425.0, 0.28039, 0.35577, -5.3762],
        [450.0, 0.28863, 0.35714, -6.7262],
        [475.0, 0.29685, 0.35823, -8.5955],
        [500.0, 0.30505, 0.35907, -11.324],
        [525.0, 0.31320, 0.35968, -15.628],
        [550.0, 0.32129, 0.36011, -23.325],
        [575.0, 0.32931, 0.36038, -40.770],
        [600.0, 0.33724, 0.36051, -116.45],
    ];
    let d = 1.5 - x + 6.0 * y;
    if !(x.is_finite() && y.is_finite() && x > 0.0 && y > 0.0 && d > 0.0) {
        return None;
    }
    let (u, v) = (2.0 * x / d, 3.0 * y / d);
    let mut last_dt = 0.0;
    for i in 1..LINES.len() {
        let len = (1.0 + LINES[i][3] * LINES[i][3]).sqrt();
        let (du, dv) = (1.0 / len, LINES[i][3] / len);
        let dt = -(u - LINES[i][1]) * dv + (v - LINES[i][2]) * du;
        if dt <= 0.0 || i == LINES.len() - 1 {
            let dt = -dt.min(0.0);
            let f = if i == 1 { 0.0 } else { dt / (last_dt + dt) };
            let mired = LINES[i - 1][0] * f + LINES[i][0] * (1.0 - f);
            return Some(1.0e6 / mired.max(1.0e-3));
        }
        last_dt = dt;
    }
    None
}

/// Camera neutral (normalised to a maximum of 1) of the white chromaticity `x, y`:
/// AnalogBalance * CameraCalibration * ColorMatrix * XYZ(x, y), with the profile's
/// calibrations interpolated in reciprocal colour temperature between the two that
/// bracket the white's temperature (clamped at the ends), as the DNG specification
/// describes for dual-illuminant profiles.
fn white_xy_neutral(cals: &[Calibration], analog_balance: &[f32], x: f64, y: f64) -> Option<Vec<f32>> {
    if !(x > 0.0 && y > 0.0 && x + y < 1.0) {
        return None;
    }
    let mut known: Vec<&Calibration> = cals.iter().filter(|c| c.temperature.is_some_and(|t| t > 0.0)).collect();
    known.sort_by(|a, b| a.temperature.unwrap().total_cmp(&b.temperature.unwrap()));
    if cals.is_empty() {
        return None;
    }
    let pick = |c: &Calibration| (c.color.clone(), c.camera.clone());
    let (color, camera) = match (known.first(), known.last(), xy_to_cct(x, y)) {
        (Some(lo), Some(hi), Some(t)) if lo.rows == hi.rows => {
            let (tlo, thi) = (lo.temperature.unwrap(), hi.temperature.unwrap());
            if t <= tlo {
                pick(lo)
            } else if t >= thi {
                pick(hi)
            } else {
                // Bracketing pair: the warmest calibration at or above, coolest at or below.
                let a = known.iter().rev().find(|c| c.temperature.unwrap() <= t).unwrap();
                let b = known.iter().find(|c| c.temperature.unwrap() >= t).unwrap();
                let (ta, tb) = (a.temperature.unwrap(), b.temperature.unwrap());
                let g = if tb > ta { ((1.0 / t - 1.0 / tb) / (1.0 / ta - 1.0 / tb)).clamp(0.0, 1.0) } else { 1.0 };
                let mix =
                    |p: &[f64], q: &[f64]| p.iter().zip(q).map(|(p, q)| g * p + (1.0 - g) * q).collect::<Vec<_>>();
                let camera = match (&a.camera, &b.camera) {
                    (Some(p), Some(q)) => Some(mix(p, q)),
                    (p, q) => p.clone().or_else(|| q.clone()),
                };
                (mix(&a.color, &b.color), camera)
            }
        }
        _ => pick(&cals[0]),
    };
    let rows = color.len() / 3;
    let xyz = [x / y, 1.0, (1.0 - x - y) / y];
    let mut n: Vec<f64> = (0..rows).map(|r| (0..3).map(|c| color[r * 3 + c] * xyz[c]).sum()).collect();
    if let Some(cc) = camera.filter(|cc| cc.len() == rows * rows) {
        n = (0..rows).map(|r| (0..rows).map(|c| cc[r * rows + c] * n[c]).sum()).collect();
    }
    if analog_balance.len() == rows {
        for (v, ab) in n.iter_mut().zip(analog_balance) {
            *v *= *ab as f64;
        }
    }
    let max = n.iter().copied().fold(0.0, f64::max);
    (max > 0.0 && n.iter().all(|&v| v.is_finite() && v > 0.0)).then(|| n.iter().map(|&v| (v / max) as f32).collect())
}

fn parse_raw<'a>(
    data: &'a [u8],
    tiff: &Tiff<'a>,
    ifd0: &Ifd<'a>,
    raw: &Ifd<'a>,
    version: [u8; 4],
    meta: &RawMetadata,
    opts: &OpenOptions,
) -> Result<(FrameInfo, DngRaw<'a>)> {
    let width = raw.width();
    let height = raw.height();
    let cpp = raw.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
    let bps = raw.bps();
    opts.limits.check_dims(width, height, cpp)?;
    if cpp == 0 || cpp > 4 {
        return Err(Error::unsupported_compression("DNG", format!("{cpp} samples per pixel")));
    }
    if raw.u32(tags::PLANAR_CONFIG).unwrap_or(1) != 1 && cpp > 1 {
        return Err(Error::unsupported_compression("DNG", "planar configuration 2"));
    }
    let float = raw.u32(tags::SAMPLE_FORMAT) == Some(3);
    let compression = raw.compression();
    let photometric = raw.u32(tags::PHOTOMETRIC).unwrap_or(32803);
    let (payload, comp_kind) = match compression {
        1 => (Payload::Uncompressed, Compression::Uncompressed),
        7 => (Payload::Ljpeg, Compression::LosslessJpeg),
        8 | 32946 => (Payload::Deflate, Compression::Deflate),
        34892 => (Payload::LossyJpeg, Compression::LossyJpeg),
        52546 => (Payload::JpegXl, Compression::JpegXl),
        9 => (Payload::Vc5, Compression::Vc5),
        c => return Err(Error::unsupported_compression("DNG", format!("TIFF compression {c}"))),
    };
    if payload == Payload::Uncompressed && !float && !(1..=16).contains(&bps) && bps != 32 {
        return Err(Error::unsupported_compression("DNG", format!("{bps}-bit uncompressed")));
    }
    if float && !matches!(bps, 16 | 24 | 32) {
        return Err(Error::unsupported_compression("DNG", format!("{bps}-bit float")));
    }
    // Grid: tiles, or strips as full-width tiles.
    let (tile_w, tile_h, units) = if raw.has(tags::TILE_OFFSETS) {
        let tw = raw.req_u32(tags::TILE_WIDTH)?;
        let th = raw.req_u32(tags::TILE_LENGTH)?;
        (tw, th, tiles(raw))
    } else {
        let rps = raw.u32(tags::ROWS_PER_STRIP).unwrap_or(height).clamp(1, height.max(1));
        (width, rps, strips(raw))
    };
    if tile_w == 0 || tile_h == 0 {
        return Err(Error::invalid_metadata("DNG tile size is zero"));
    }
    let expected = width.div_ceil(tile_w) as usize * height.div_ceil(tile_h) as usize;
    if units.len() < expected {
        // Single-strip files sometimes claim RowsPerStrip < height with one strip; treat as one unit.
        if units.len() != 1 || raw.has(tags::TILE_OFFSETS) {
            return Err(Error::corrupt(format!("DNG has {} tiles/strips, expected {expected}", units.len())));
        }
    }
    let (tile_h, units) = if units.len() == 1 && expected > 1 && !raw.has(tags::TILE_OFFSETS) {
        (height, units)
    } else {
        (tile_h, units)
    };

    let mut info = FrameInfo::new(width, height, bps.min(16) as u8);
    info.cpp = cpp;
    info.sample_format = if float { SampleFormat::F32 } else { SampleFormat::U16 };
    info.compression = comp_kind;
    info.orientation = Orientation::from_exif(ifd0.u32(tags::ORIENTATION).unwrap_or(1));
    let lists = [tags::OPCODE_LIST1, tags::OPCODE_LIST2, tags::OPCODE_LIST3]
        .map(|t| raw.get(t).or_else(|| ifd0.get(t)).map(|e| e.bytes().to_vec()).unwrap_or_default());
    let opcodes = flashraw_core::decoder::OpcodeLists { lists };
    if !opcodes.is_empty() {
        info.opcodes = Some(std::sync::Arc::new(opcodes));
    }

    // Active area & crop.
    let active = raw
        .get(tags::ACTIVE_AREA)
        .and_then(|e| Some((e.u32(0)?, e.u32(1)?, e.u32(2)?, e.u32(3)?)))
        .and_then(|(t, l, b, r)| Rect::from_ltrb(l, t, r, b))
        .filter(|r| !r.is_empty() && r.right() <= width && r.bottom() <= height)
        .unwrap_or(Rect::full(width, height));
    info.active_area = active;
    info.crop = match (rational_pair(raw, tags::DEFAULT_CROP_ORIGIN), rational_pair(raw, tags::DEFAULT_CROP_SIZE)) {
        (Some((ox, oy)), Some((w, h))) if w >= 1.0 && h >= 1.0 && ox >= 0.0 && oy >= 0.0 => {
            Rect::new(active.x + ox.round() as u32, active.y + oy.round() as u32, w.round() as u32, h.round() as u32)
                .intersect(&active)
                .unwrap_or(active)
        }
        (None, Some((w, h))) if w >= 1.0 && h >= 1.0 => {
            Rect::new(active.x, active.y, w as u32, h as u32).intersect(&active).unwrap_or(active)
        }
        _ => active,
    };
    if let Some(m) = raw.get(tags::MASKED_AREAS) {
        let v = m.u32s();
        for c in v.chunks_exact(4) {
            if let Some(r) = Rect::from_ltrb(c[1], c[0], c[3], c[2]) {
                info.masked_areas.push(r);
            }
        }
    }

    // Layout & CFA.
    if photometric == 32803 {
        // Staggered layouts A-D (2-5: even rows or columns offset by half a photosite, e.g.
        // converted SuperCCD files) are returned as stored with their stored-grid CFA, like
        // native SuperCCD buffers; undoing the stagger interpolates and is left to
        // post-processing (DefaultScale gives the pixel aspect).
        let layout = raw.u32(tags::CFA_LAYOUT).unwrap_or(1);
        if !(1..=5).contains(&layout) {
            return Err(Error::CameraUnsupported {
                make: meta.make.clone(),
                model: meta.model.clone(),
                reason: format!("DNG CFALayout {layout}"),
            });
        }
        let dim = raw
            .get(tags::CFA_REPEAT_PATTERN_DIM)
            .ok_or_else(|| Error::invalid_metadata("DNG CFA without CFARepeatPatternDim"))?;
        let (ph, pw) = (dim.u32(0).unwrap_or(2) as usize, dim.u32(1).unwrap_or(2) as usize);
        let pat = raw.get(tags::CFA_PATTERN_EP).ok_or_else(|| Error::invalid_metadata("DNG CFA without CFAPattern"))?;
        let plane: Vec<u8> =
            raw.get(tags::CFA_PLANE_COLOR).map(|e| e.bytes().to_vec()).unwrap_or_else(|| vec![0, 1, 2]);
        let colors: Option<Vec<CfaColor>> = pat
            .bytes()
            .iter()
            .take(pw * ph)
            .map(|&i| plane.get(i as usize).copied().and_then(CfaColor::from_dng))
            .collect();
        let colors =
            colors.filter(|c| c.len() == pw * ph).ok_or_else(|| Error::invalid_metadata("invalid DNG CFAPattern"))?;
        let p =
            CfaPattern::new(pw, ph, &colors).ok_or_else(|| Error::invalid_metadata("invalid DNG CFA dimensions"))?;
        // Re-anchor from ActiveArea origin to buffer origin.
        let p = p.shifted(pw - active.x as usize % pw, ph - active.y as usize % ph);
        info.layout = if layout == 1 { layout_for(&p) } else { SensorLayout::OtherCfa };
        info.cfa = Some(p);
    } else if cpp == 1 {
        info.layout = SensorLayout::Monochrome;
    } else {
        info.layout = SensorLayout::Linear;
    }

    // Linearization.
    let linearization = raw
        .get(tags::LINEARIZATION_TABLE)
        .map(|e| e.u32s().into_iter().map(|v| v.min(65535) as u16).collect::<Vec<_>>())
        .filter(|t| !t.is_empty());

    // Black level (pattern anchored at the active area).
    let (br, bc) = raw
        .get(tags::BLACK_LEVEL_REPEAT_DIM)
        .map(|e| (e.u32(0).unwrap_or(1) as usize, e.u32(1).unwrap_or(1) as usize))
        .unwrap_or((1, 1));
    let (br, bc) = (br.clamp(1, 8), bc.clamp(1, 8));
    if let Some(e) = raw.get(tags::BLACK_LEVEL) {
        let vals = e.f32s();
        let need = br * bc * cpp as usize;
        let values: Vec<f32> = if vals.len() >= need {
            vals[..need].to_vec()
        } else if !vals.is_empty() {
            (0..need).map(|i| vals[i % vals.len()]).collect()
        } else {
            vec![0.0; need]
        };
        let mut b = BlackLevel {
            rows: br as u8,
            cols: bc as u8,
            samples: cpp as u8,
            values,
            row_deltas: vec![],
            col_deltas: vec![],
        };
        b = if active.x as usize % bc != 0 || active.y as usize % br != 0 {
            b.shifted(bc - active.x as usize % bc, br - active.y as usize % br)
        } else {
            b
        };
        if let Some(dh) = raw.get(tags::BLACK_LEVEL_DELTA_H) {
            let d = dh.f32s();
            if d.iter().any(|&v| v != 0.0) {
                let mut cols = vec![0.0f32; width as usize];
                for (i, v) in d.iter().enumerate().take(active.width as usize) {
                    cols[active.x as usize + i] = *v;
                }
                b.col_deltas = cols;
            }
        }
        if let Some(dv) = raw.get(tags::BLACK_LEVEL_DELTA_V) {
            let d = dv.f32s();
            if d.iter().any(|&v| v != 0.0) {
                let mut rows = vec![0.0f32; height as usize];
                for (i, v) in d.iter().enumerate().take(active.height as usize) {
                    rows[active.y as usize + i] = *v;
                }
                b.row_deltas = rows;
            }
        }
        info.black_level = b;
    }
    // Writer quirk: a big-endian TIFF whose 32-bit float samples are little-endian, with
    // WhiteLevel written as a LONG holding the bit pattern of 1.0f. Confirmed against the
    // samples themselves so a file that is only odd in its WhiteLevel keeps its order.
    let float_order = if float
        && tiff.order == ByteOrder::Big
        && bps == 32
        && payload == Payload::Uncompressed
        && raw.get(tags::WHITE_LEVEL).is_some_and(|e| e.typ == 4 && e.u32s() == [0x3F80_0000])
        && units
            .first()
            .and_then(|&(o, l)| data.get(o..o.saturating_add(l.min(1 << 16))))
            .is_some_and(|s| float_plausibility(s, ByteOrder::Little) > float_plausibility(s, ByteOrder::Big))
    {
        ByteOrder::Little
    } else {
        tiff.order
    };
    // White level.
    info.white_level = match raw.get(tags::WHITE_LEVEL) {
        _ if float_order != tiff.order => WhiteLevel::uniform(1),
        Some(e) if !e.is_empty() => WhiteLevel { values: e.u32s() },
        _ => {
            let max = if float {
                1
            } else if let Some(t) = &linearization {
                t.iter().copied().max().unwrap_or(65535) as u32
            } else if bps >= 16 {
                65535
            } else {
                (1u32 << bps) - 1
            };
            WhiteLevel::uniform(max)
        }
    };

    // Colour.
    let colors =
        if photometric == 32803 { info.cfa.map(|p| p.distinct_colors().len()).unwrap_or(3) } else { cpp as usize };
    let rows = colors.max(3);
    let mut cm = ColorMatrices { source: ColorSource::File, ..Default::default() };
    let ill = |tag: u16| Illuminant(ifd0.u32(tag).unwrap_or(0) as u16);
    for (mtag, itag) in [
        (tags::COLOR_MATRIX1, tags::CALIBRATION_ILLUMINANT1),
        (tags::COLOR_MATRIX2, tags::CALIBRATION_ILLUMINANT2),
        (tags::COLOR_MATRIX3, tags::CALIBRATION_ILLUMINANT3),
    ] {
        if let Some(m) = ifd0.get(mtag).or_else(|| raw.get(mtag)) {
            let v = m.f32s();
            if let Some(x) =
                ColorMatrix::new(ill(itag), v.len() / 3, v.clone()).filter(|x| x.rows as usize == rows || v.len() == 9)
            {
                cm.color.push(x);
            }
        }
    }
    for (mtag, itag) in [
        (tags::FORWARD_MATRIX1, tags::CALIBRATION_ILLUMINANT1),
        (tags::FORWARD_MATRIX2, tags::CALIBRATION_ILLUMINANT2),
        (tags::FORWARD_MATRIX3, tags::CALIBRATION_ILLUMINANT3),
    ] {
        if let Some(m) = ifd0.get(mtag) {
            let v = m.f32s();
            // Forward matrices are 3 x N (XYZ rows); store as-is with rows = 3.
            if v.len() >= 9 {
                cm.forward.push(ColorMatrix { illuminant: ill(itag), rows: 3, values: v });
            }
        }
    }
    for (mtag, itag) in [
        (tags::CAMERA_CALIBRATION1, tags::CALIBRATION_ILLUMINANT1),
        (tags::CAMERA_CALIBRATION2, tags::CALIBRATION_ILLUMINANT2),
        (tags::CAMERA_CALIBRATION3, tags::CALIBRATION_ILLUMINANT3),
    ] {
        if let Some(m) = ifd0.get(mtag) {
            let v = m.f32s();
            let n = (v.len() as f64).sqrt() as usize;
            if n * n == v.len() && n >= 3 {
                cm.calibration.push(ColorMatrix { illuminant: ill(itag), rows: n as u8, values: v });
            }
        }
    }
    if let Some(ab) = ifd0.get(tags::ANALOG_BALANCE) {
        cm.analog_balance = ab.f32s();
    }
    if cm.color.is_empty() {
        cm = db_color(meta, None);
    }
    info.color_matrices = cm;
    info.white_balance =
        ifd0.get(tags::AS_SHOT_NEUTRAL).and_then(|n| WhiteBalance::from_neutral(&n.f32s())).or_else(|| {
            // No AsShotNeutral: the camera neutral of the AsShotWhiteXY chromaticity.
            let (x, y) = rational_pair(ifd0, tags::AS_SHOT_WHITE_XY)?;
            let cals = calibrations(ifd0, raw, tiff.order, &info.color_matrices);
            WhiteBalance::from_neutral(&white_xy_neutral(&cals, &info.color_matrices.analog_balance, x, y)?)
        });

    let order = tiff.order;
    // Field interleaving (DNG 1.7, used with JPEG XL tiles); other payloads store rows in order.
    let factor = |t: u16| raw.u32(t).map(|v| v.clamp(1, 16) as usize).unwrap_or(1);
    let interleave = if payload == Payload::JpegXl {
        (factor(tags::ROW_INTERLEAVE_FACTOR), factor(tags::COLUMN_INTERLEAVE_FACTOR))
    } else {
        (1, 1)
    };
    let dng_bug16 = version >= [1, 1, 0, 0];
    let tile_w = tile_w.min(width.max(1)).max(if raw.has(tags::TILE_OFFSETS) { tile_w } else { 1 });
    let raw_desc = DngRaw {
        data,
        order,
        width,
        height,
        cpp,
        bps,
        float,
        float_order,
        payload,
        predictor: raw.u32(tags::PREDICTOR).unwrap_or(1),
        tile_w,
        tile_h,
        tiled: raw.has(tags::TILE_OFFSETS),
        units,
        linearization,
        dng_bug16,
        vc5_white: info.white_level.max(),
        vc5_phase: info.cfa.as_ref().and_then(flashraw_codecs::vc5::CfaPhase::from_pattern),
        interleave,
    };
    Ok((info, raw_desc))
}

impl DngRaw<'_> {
    fn tiles_x(&self) -> u32 {
        self.width.div_ceil(self.tile_w)
    }

    fn unit_list(&self) -> Vec<DecodeUnit> {
        let tx = self.tiles_x();
        self.units
            .iter()
            .enumerate()
            .map(|(i, &(o, l))| {
                let (x, y) = ((i as u32 % tx) * self.tile_w, (i as u32 / tx) * self.tile_h);
                DecodeUnit {
                    rect: Rect::new(x, y, self.tile_w, self.tile_h).clamp_to(self.width, self.height),
                    offset: o as u64,
                    length: l as u64,
                }
            })
            .collect()
    }

    fn decode_region(&self, ctx: &DecodeContext, region: Option<Rect>) -> Result<RawImage> {
        let full = Rect::full(self.width, self.height);
        let region = match region {
            Some(r) => r.intersect(&full).ok_or_else(|| Error::InvalidArgument(format!("region {r} outside image")))?,
            None => full,
        };
        let mut img = if self.float {
            RawImage::new_f32(region.width, region.height, self.cpp, &ctx.limits)?
        } else {
            RawImage::new_u16(region.width, region.height, self.cpp, &ctx.limits)?
        };
        if self.payload == Payload::Vc5 {
            // One bitstream covers the whole image: decode fully, then crop.
            let mut full_img = RawImage::new_u16(self.width, self.height, 1, &ctx.limits)?;
            let phase =
                self.vc5_phase.ok_or_else(|| Error::unsupported_compression("DNG", "VC-5 without a Bayer CFA"))?;
            let src = self.compressed_bytes(ctx, 0)?;
            let res = match &src.truncation {
                Some(t) => Err(Error::truncated(t.clone())),
                None => ctx.profiler.time(Stage::EntropyDecode, || {
                    flashraw_codecs::vc5::decode(
                        src.bytes,
                        self.width as usize,
                        self.height as usize,
                        self.vc5_white,
                        phase,
                        full_img.u16_mut(),
                        ctx.parallel,
                    )
                }),
            };
            if let Err(e) = res {
                // One unit: recovering leaves the whole image zero.
                let samples = full_img.u16().len() as u64;
                self.recover_unit(ctx, 0, src.truncation, e, samples)?;
                full_img.u16_mut().fill(0);
            }
            return if region == full { Ok(full_img) } else { full_img.crop(region) };
        }
        if self.payload == Payload::Uncompressed && self.units.len() == 1 && !self.float && self.bps < 32 {
            ctx.profiler.time(Stage::Unpack, || self.unpack_single_strip(ctx, region, img.u16_mut()))?;
            return Ok(img);
        }
        let stage = match self.payload {
            Payload::Uncompressed => Stage::Unpack,
            _ => Stage::EntropyDecode,
        };
        let (tx, ty) = (self.tiles_x(), self.height.div_ceil(self.tile_h));
        let n = (tx * ty) as usize;
        let errors = std::sync::Mutex::new(Vec::<(usize, Error)>::new());
        let mut float_max = None;
        // Parallelise across units when there are several; a single unit parallelises inside (uncompressed rows).
        let par_units = ctx.parallel && n > 1;
        ctx.profiler.time(stage, || match &mut img.data {
            PixelData::U16(buf) => {
                let g = GridWriter::new(
                    buf,
                    region.width,
                    region.height,
                    self.cpp,
                    (region.x, region.y),
                    self.tile_w,
                    self.tile_h,
                    self.width,
                    self.height,
                )
                .ok_or_else(|| Error::corrupt("invalid DNG tile grid"))?;
                par::for_each_index(n, par_units, |i| {
                    let (x, y) = (i as u32 % tx, i as u32 / tx);
                    if let Some(t) = g.claim(x, y) {
                        if let Err(e) = self.decode_unit_u16(ctx, i, t, ctx.parallel && !par_units) {
                            errors.lock().unwrap().push((i, e));
                        }
                    }
                });
                Ok::<(), Error>(())
            }
            PixelData::F32(buf) => {
                let g = GridWriter::new(
                    buf,
                    region.width,
                    region.height,
                    self.cpp,
                    (region.x, region.y),
                    self.tile_w,
                    self.tile_h,
                    self.width,
                    self.height,
                )
                .ok_or_else(|| Error::corrupt("invalid DNG tile grid"))?;
                let max = std::sync::atomic::AtomicU32::new(0);
                par::for_each_index(n, par_units, |i| {
                    let (x, y) = (i as u32 % tx, i as u32 / tx);
                    if let Some(t) = g.claim(x, y) {
                        match self.decode_unit_f32(ctx, i, t) {
                            Ok(m) => {
                                max.fetch_max(m, std::sync::atomic::Ordering::Relaxed);
                            }
                            Err(e) => errors.lock().unwrap().push((i, e)),
                        }
                    }
                });
                float_max = Some(f32::from_bits(max.into_inner()));
                Ok(())
            }
        })?;
        if let Some(e) = errors.into_inner().unwrap().into_iter().min_by_key(|(i, _)| *i).map(|(_, e)| e) {
            return Err(e);
        }
        img.float_max = float_max;
        Ok(img)
    }

    /// Uncompressed single-strip images: unpack rows in parallel directly into `out`.
    fn unpack_single_strip(&self, ctx: &DecodeContext, region: Rect, out: &mut [u16]) -> Result<()> {
        let src = self.uncompressed_bytes(ctx, 0, region.bottom() as usize)?;
        let truncation = src.truncation;
        let src = src.bytes;
        let filled = std::sync::atomic::AtomicU64::new(0);
        let cpp = self.cpp as usize;
        let bits = self.bps;
        let order =
            if bits == 16 && self.order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
        let full_rl = self.width as usize * cpp;
        let row_bytes = (full_rl * bits as usize).div_ceil(8);
        let out_rl = region.width as usize * cpp;
        let x0 = region.x as usize * cpp;
        let lin = self.linearization.as_deref();
        let simd = ctx.simd;
        par::for_each_row_chunk_bounded(out, out_rl, par::memory_bound_workers(), ctx.parallel, |first, chunk| {
            let mut tmp = if x0 == 0 && out_rl == full_rl { Vec::new() } else { vec![0u16; full_rl] };
            for (i, row) in chunk.chunks_mut(out_rl).enumerate() {
                let y = region.y as usize + first + i;
                let s = src.get(y * row_bytes..).unwrap_or(&[]);
                let s = &s[..row_bytes.min(s.len())];
                let n = if s.len() == row_bytes {
                    if tmp.is_empty() {
                        packed::unpack_row(s, row, bits, order, simd);
                    } else {
                        packed::unpack_row(s, &mut tmp, bits, order, simd);
                        row.copy_from_slice(&tmp[x0..x0 + out_rl]);
                    }
                    out_rl
                } else {
                    // Short final row: its samples within the region are complete unless
                    // recovering truncated data (the rest stay zero).
                    let (n, lost) = unpack_short_row(s, row, &mut tmp, x0, bits, order, simd);
                    filled.fetch_add(lost, std::sync::atomic::Ordering::Relaxed);
                    n
                };
                if let Some(l) = lin {
                    let last = *l.last().unwrap();
                    for v in row[..n].iter_mut() {
                        *v = l.get(*v as usize).copied().unwrap_or(last);
                    }
                }
            }
        });
        if let Some(t) = truncation {
            ctx.recovered(CorruptReason::Truncated(t), filled.into_inner());
        }
        Ok(())
    }

    fn data_unit(&self, i: usize) -> DataUnit {
        if self.tiled { DataUnit::Tile(i as u32) } else { DataUnit::Strip(i as u32) }
    }

    /// Byte layout of uncompressed unit `i` up to its row `rows` (exclusive): rows hold
    /// `tile_w` samples, but only the columns and rows inside the image are needed (edge
    /// tiles and the last strip may stop after their last needed sample).
    fn unit_geometry(&self, i: usize, rows: usize) -> RowGeometry {
        let tx = self.tiles_x() as usize;
        let (x0, y0) = ((i % tx) * self.tile_w as usize, (i / tx) * self.tile_h as usize);
        let cols = (self.width as usize).saturating_sub(x0).min(self.tile_w as usize);
        let rows = (self.height as usize).saturating_sub(y0).min(self.tile_h as usize).min(rows);
        let cpp = self.cpp as usize;
        let stride = (self.tile_row_len() * self.bps as usize).div_ceil(8);
        RowGeometry::packed(rows, cols * cpp, self.bps, stride)
    }

    /// Uncompressed unit `i`, rows `0..rows`. Some writers (e.g. DJI) understate
    /// StripByteCounts; uncompressed data is addressed by geometry, so the geometry's
    /// bytes are read when the file has them.
    fn uncompressed_bytes(&self, ctx: &DecodeContext, i: usize, rows: usize) -> Result<SensorBytes<'_>> {
        let (o, _) = *self.units.get(i).ok_or_else(|| Error::corrupt(format!("missing DNG tile {i}")))?;
        ctx.sensor_bytes(self.data, self.data_unit(i), o, self.unit_geometry(i, rows).needed())
    }

    /// Compressed unit `i`: its declared byte range.
    fn compressed_bytes(&self, ctx: &DecodeContext, i: usize) -> Result<SensorBytes<'_>> {
        let (o, l) = *self.units.get(i).ok_or_else(|| Error::corrupt(format!("missing DNG tile {i}")))?;
        ctx.sensor_bytes(self.data, self.data_unit(i), o, l)
    }

    /// Recover policy: a compressed unit that is truncated or failed to decode is
    /// reported (its `samples` are left zero by the caller). Fails otherwise.
    fn recover_unit(
        &self,
        ctx: &DecodeContext,
        i: usize,
        truncation: Option<Truncation>,
        e: Error,
        samples: u64,
    ) -> Result<()> {
        if !ctx.recover || !matches!(e, Error::CorruptFile { .. }) {
            return Err(e);
        }
        let reason = match truncation {
            Some(t) => CorruptReason::Truncated(t),
            None => CorruptReason::InvalidUnit { unit: self.data_unit(i), message: e.to_string() },
        };
        ctx.recovered(reason, samples);
        Ok(())
    }

    /// Samples per tile row.
    fn tile_row_len(&self) -> usize {
        self.tile_w as usize * self.cpp as usize
    }

    fn decode_unit_u16(
        &self,
        ctx: &DecodeContext,
        i: usize,
        mut t: TileMut<'_, '_, u16>,
        parallel_rows: bool,
    ) -> Result<()> {
        if self.payload == Payload::Uncompressed {
            return self.unpack_unit_u16(ctx, i, t);
        }
        let src = self.compressed_bytes(ctx, i)?;
        let res = self.decode_compressed_u16(src.bytes, &mut t, parallel_rows);
        match (res, src.truncation) {
            (Ok(()), None) => Ok(()),
            // Recovering: the available bytes held the whole stream.
            (Ok(()), Some(tr)) => self.recover_unit(ctx, i, Some(tr.clone()), Error::truncated(tr), 0),
            (Err(e), tr) => {
                let (w, h) = t.size();
                self.recover_unit(ctx, i, tr, e, (w * h * self.cpp as usize) as u64)?;
                (0..h).for_each(|y| t.row(y).fill(0));
                Ok(())
            }
        }
    }

    /// Uncompressed unit: rows straight from the file (missing data only when recovering).
    fn unpack_unit_u16(&self, ctx: &DecodeContext, i: usize, mut t: TileMut<'_, '_, u16>) -> Result<()> {
        let bits = self.bps;
        if bits == 32 {
            // 32-bit integer samples: keep the high 16 bits is lossy; reject explicitly.
            return Err(Error::unsupported_compression("DNG", "32-bit integer samples"));
        }
        let (w, h) = t.size();
        let cpp = self.cpp as usize;
        let (sx, sy) = (t.skip_x as usize * cpp, t.skip_y as usize);
        let src = self.uncompressed_bytes(ctx, i, sy + h)?;
        let trl = self.tile_row_len();
        let order =
            if bits == 16 && self.order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
        let row_bytes = (trl * bits as usize).div_ceil(8);
        let mut tmp = vec![0u16; trl];
        let mut filled = 0u64;
        for y in 0..h {
            let off = (sy + y) * row_bytes;
            let s = src.bytes.get(off..).unwrap_or(&[]);
            let s = &s[..row_bytes.min(s.len())];
            let row = t.row(y);
            let n = if s.len() == row_bytes {
                if sx == 0 && w * cpp == trl {
                    packed::unpack_row(s, row, bits, order, ctx.simd);
                } else {
                    packed::unpack_row(s, &mut tmp, bits, order, ctx.simd);
                    row.copy_from_slice(&tmp[sx..sx + w * cpp]);
                }
                w * cpp
            } else {
                let (n, lost) = unpack_short_row(s, row, &mut tmp, sx, bits, order, ctx.simd);
                filled += lost;
                n
            };
            self.linearize(&mut row[..n]);
        }
        if let Some(tr) = src.truncation {
            ctx.recovered(CorruptReason::Truncated(tr), filled);
        }
        Ok(())
    }

    fn linearize(&self, row: &mut [u16]) {
        if let Some(l) = self.linearization.as_deref() {
            let last = *l.last().unwrap();
            for v in row.iter_mut() {
                *v = l.get(*v as usize).copied().unwrap_or(last);
            }
        }
    }

    fn decode_compressed_u16(&self, src: &[u8], t: &mut TileMut<'_, '_, u16>, parallel_rows: bool) -> Result<()> {
        let (w, h) = t.size();
        let cpp = self.cpp as usize;
        let (sx, sy) = (t.skip_x as usize * cpp, t.skip_y as usize);
        let lin = self.linearization.as_deref();
        let finish = |row: &mut [u16]| {
            if let Some(l) = lin {
                let last = *l.last().unwrap();
                for v in row.iter_mut() {
                    *v = l.get(*v as usize).copied().unwrap_or(last);
                }
            }
        };
        let trl = self.tile_row_len();
        match self.payload {
            Payload::Uncompressed => Err(Error::corrupt("uncompressed DNG unit decoded as compressed")),
            Payload::Ljpeg => {
                let hdr = match ljpeg::LjpegHeader::parse(src, ljpeg::LjpegOptions { dng_bug16: self.dng_bug16 }) {
                    Ok(h) => h,
                    Err(Error::UnsupportedCompression { .. }) if cpp == 1 => {
                        return self.decode_dct12_tile(src, t, sx, sy, finish);
                    }
                    Err(e) => return Err(e),
                };
                let (mut row, mut col) = (0usize, 0usize);
                let (cx0, cx1) = (sx, sx + w * cpp);
                ljpeg::decode_rows_par(src, &hdr, parallel_rows, |_, mut s| {
                    while !s.is_empty() && row < sy + h {
                        let take = (trl - col).min(s.len());
                        if row >= sy {
                            let (a, b) = (col.max(cx0), (col + take).min(cx1));
                            if a < b {
                                let r = t.row(row - sy);
                                let dst = &mut r[a - cx0..b - cx0];
                                dst.copy_from_slice(&s[a - col..b - col]);
                                finish(dst);
                            }
                        }
                        s = &s[take..];
                        col += take;
                        if col == trl {
                            col = 0;
                            row += 1;
                        }
                    }
                    Ok(())
                })
            }
            Payload::Deflate => {
                let bytes_ps = self.bps.div_ceil(8) as usize;
                let row_bytes = trl * bytes_ps;
                let raw = deflate::inflate_zlib(src, row_bytes * self.tile_h as usize)?;
                if raw.len() < (sy + h) * row_bytes {
                    return Err(Error::corrupt(format!(
                        "deflate data decodes to {} bytes, {} rows of {row_bytes} needed",
                        raw.len(),
                        sy + h
                    )));
                }
                for y in 0..h {
                    let off = (sy + y) * row_bytes;
                    let Some(rb) = raw.get(off..off + row_bytes) else { break };
                    let mut vals: Vec<u16> = match bytes_ps {
                        1 => rb.iter().map(|&b| b as u16).collect(),
                        _ => rb
                            .chunks_exact(2)
                            .map(|c| {
                                if self.order == ByteOrder::Little {
                                    u16::from_le_bytes([c[0], c[1]])
                                } else {
                                    u16::from_be_bytes([c[0], c[1]])
                                }
                            })
                            .collect(),
                    };
                    match self.predictor {
                        2 => deflate::undo_horizontal_u16(&mut vals, cpp, 1),
                        34892 => deflate::undo_horizontal_u16(&mut vals, cpp, 2),
                        34893 => deflate::undo_horizontal_u16(&mut vals, cpp, 4),
                        _ => {}
                    }
                    let r = t.row(y);
                    r.copy_from_slice(&vals[sx..sx + w * cpp]);
                    finish(r);
                }
                Ok(())
            }
            Payload::LossyJpeg => {
                let img = flashraw_codecs::jpeg::decode(src)?;
                let ccount = img.components;
                for y in 0..h {
                    let r = t.row(y);
                    let yy = sy + y;
                    if yy >= img.height {
                        break;
                    }
                    for (xi, v) in r.iter_mut().enumerate() {
                        let x = (sx + xi) / cpp;
                        let c = (sx + xi) % cpp;
                        if x < img.width {
                            *v = img.data[(yy * img.width + x) * ccount + c.min(ccount - 1)] as u16;
                        }
                    }
                    finish(r);
                }
                Ok(())
            }
            Payload::JpegXl => decode_jxl_u16(self, src, t, sx, sy, finish),
            Payload::Vc5 => Err(Error::corrupt("VC-5 is decoded as a whole image")),
        }
    }

    /// Blackmagic 12-bit DCT tiles: the JPEG frame is twice the tile width; block
    /// columns in the left half fill even tile rows, the right half odd rows, and each
    /// 8x8 block covers 16 tile rows (every other row).
    fn decode_dct12_tile(
        &self,
        src: &[u8],
        t: &mut TileMut<'_, '_, u16>,
        sx: usize,
        sy: usize,
        finish: impl Fn(&mut [u16]),
    ) -> Result<()> {
        let d = flashraw_codecs::dct12::Dct12::parse(src, self.dng_bug16)?;
        let tw = self.tile_w as usize;
        let (w, h) = t.size();
        d.decode_blocks(src, |jrow, jcol, block| {
            let (jrow, jcol) = (jrow as usize, jcol as usize);
            let row0 = jcol / tw + jrow * 2;
            let col0 = jcol % tw;
            for bi in 0..8 {
                let r = row0 + bi * 2;
                if r < sy || r - sy >= h {
                    continue;
                }
                let line = t.row(r - sy);
                for j in 0..8 {
                    let c = col0 + j;
                    if c >= sx && c - sx < w {
                        line[c - sx] = block[bi * 8 + j];
                    }
                }
            }
        })?;
        for y in 0..h {
            finish(t.row(y));
        }
        Ok(())
    }

    /// Decode one float tile; returns the largest non-negative sample written (as bits).
    fn decode_unit_f32(&self, ctx: &DecodeContext, i: usize, mut t: TileMut<'_, '_, f32>) -> Result<u32> {
        let (w, h) = t.size();
        let cpp = self.cpp as usize;
        let sy = t.skip_y as usize;
        let src = match self.payload {
            Payload::Uncompressed => self.uncompressed_bytes(ctx, i, sy + h)?,
            _ => self.compressed_bytes(ctx, i)?,
        };
        let truncation = src.truncation.clone();
        match self.decode_f32_rows(src.bytes, &mut t, truncation.is_some()) {
            Ok((max, filled)) => {
                if let Some(tr) = truncation {
                    if self.payload == Payload::Uncompressed {
                        ctx.recovered(CorruptReason::Truncated(tr), filled);
                    } else {
                        self.recover_unit(ctx, i, Some(tr.clone()), Error::truncated(tr), filled)?;
                    }
                }
                Ok(max)
            }
            Err(e) => {
                self.recover_unit(ctx, i, truncation, e, (w * h * cpp) as u64)?;
                (0..h).for_each(|y| t.row(y).fill(0.0));
                Ok(0)
            }
        }
    }

    /// Float rows of one unit. Returns the largest non-negative sample written (as bits)
    /// and, when `partial` (recovering truncated data), the samples left zero.
    fn decode_f32_rows(&self, src: &[u8], t: &mut TileMut<'_, '_, f32>, partial: bool) -> Result<(u32, u64)> {
        let (w, h) = t.size();
        let cpp = self.cpp as usize;
        let (sx, sy) = (t.skip_x as usize * cpp, t.skip_y as usize);
        let trl = self.tile_row_len();
        let bytes_ps = (self.bps / 8) as usize;
        let row_bytes = trl * bytes_ps;
        thread_local! {
            static SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        let mut scratch = SCRATCH.with(|c| std::mem::take(&mut *c.borrow_mut()));
        let raw: &[u8] = match self.payload {
            Payload::Uncompressed => src,
            Payload::Deflate => {
                let need = row_bytes * self.tile_h as usize;
                if scratch.len() < need {
                    scratch.resize(need, 0);
                }
                let n = deflate::inflate_zlib_into(src, &mut scratch[..need])?;
                &scratch[..n]
            }
            _ => return Err(Error::unsupported_compression("DNG", "float samples with this compression")),
        };
        let factor = match (self.payload, self.predictor) {
            (Payload::Deflate, 3) => cpp,
            (Payload::Deflate, 34894) => cpp * 2,
            (Payload::Deflate, 34895) => cpp * 4,
            _ => 0,
        };
        let little = self.float_order == ByteOrder::Little;
        let mut rowbuf = vec![0u8; row_bytes];
        let mut vals = vec![0f32; if sx == 0 && w * cpp == trl { 0 } else { trl }];
        let mut max = 0u32;
        let mut filled = 0u64;
        for y in 0..h {
            let off = (sy + y) * row_bytes;
            let rb = raw.get(off..).unwrap_or(&[]);
            let rb = &rb[..row_bytes.min(rb.len())];
            if rb.len() < row_bytes {
                // Short final row. Complete unless recovering: decode it whole (the
                // predictors run over full rows) from zero-padded bytes, then zero the
                // samples the data did not cover.
                let n = rb.len() / bytes_ps;
                if !partial && n < sx + w * cpp {
                    return Err(Error::corrupt(format!("DNG float row {} truncated", sy + y)));
                }
                rowbuf.fill(0);
                rowbuf[..rb.len()].copy_from_slice(rb);
                if vals.is_empty() {
                    vals = vec![0f32; trl];
                }
                deflate::decode_float_row(&mut rowbuf, trl, bytes_ps, factor, little, &mut vals);
                let row = t.row(y);
                row.copy_from_slice(&vals[sx..sx + w * cpp]);
                let keep = n.saturating_sub(sx).min(w * cpp);
                row[keep..].fill(0.0);
                filled += (w * cpp - keep) as u64;
                max = max.max(max_nonneg_bits(&row[..keep]));
                (y + 1..h).for_each(|yy| t.row(yy).fill(0.0));
                filled += ((h - y - 1) * w * cpp) as u64;
                break;
            }
            rowbuf.copy_from_slice(rb);
            if vals.is_empty() {
                deflate::decode_float_row(&mut rowbuf, trl, bytes_ps, factor, little, t.row(y));
            } else {
                deflate::decode_float_row(&mut rowbuf, trl, bytes_ps, factor, little, &mut vals);
                t.row(y).copy_from_slice(&vals[sx..sx + w * cpp]);
            }
            // While the row is in cache: its maximum for a later float -> integer conversion.
            max = max.max(max_nonneg_bits(t.row(y)));
        }
        // Return the per-thread scratch buffer for the next tile.
        SCRATCH.with(|c| *c.borrow_mut() = scratch);
        Ok((max, filled))
    }
}

/// Unpack a row whose source `s` is shorter than a full row: `out` receives the samples
/// `x0..x0 + out.len()` of the row (`tmp` is a full-row scratch buffer). Samples not
/// entirely within `s` are zero. Returns how many leading samples of `out` were decoded
/// and how many were left zero.
fn unpack_short_row(
    s: &[u8],
    out: &mut [u16],
    tmp: &mut Vec<u16>,
    x0: usize,
    bits: u32,
    order: PackOrder,
    simd: bool,
) -> (usize, u64) {
    let need = x0 + out.len();
    if tmp.len() < need {
        tmp.resize(need, 0);
    }
    let complete = packed::unpack_row_partial(s, &mut tmp[..need], bits, order, simd);
    out.copy_from_slice(&tmp[x0..need]);
    let n = complete.saturating_sub(x0).min(out.len());
    (n, (out.len() - n) as u64)
}

#[cfg(feature = "jpegxl")]
fn decode_jxl_u16(
    raw: &DngRaw,
    src: &[u8],
    t: &mut TileMut<'_, '_, u16>,
    sx: usize,
    sy: usize,
    finish: impl Fn(&mut [u16]),
) -> Result<()> {
    // Samples are scaled to the DNG's BitsPerSample, not the codestream's bit depth.
    let img = flashraw_codecs::jxl::decode(src, raw.bps)?;
    let cpp = raw.cpp as usize;
    let (rif, cif) = raw.interleave;
    let (w, h) = t.size();
    for y in 0..h {
        let yy = sy + y;
        if yy >= img.height {
            break;
        }
        let sr = interleaved_pos(yy, img.height, rif);
        let r = t.row(y);
        for (xi, v) in r.iter_mut().enumerate().take(w * cpp) {
            let x = (sx + xi) / cpp;
            let c = (sx + xi) % cpp;
            if x < img.width {
                let sc = interleaved_pos(x, img.width, cif);
                *v = img.data[(sr * img.width + sc) * img.channels + c.min(img.channels - 1)];
            }
        }
        finish(r);
    }
    Ok(())
}

#[cfg(not(feature = "jpegxl"))]
fn decode_jxl_u16(
    _raw: &DngRaw,
    _src: &[u8],
    _t: &mut TileMut<'_, '_, u16>,
    _sx: usize,
    _sy: usize,
    _finish: impl Fn(&mut [u16]),
) -> Result<()> {
    Err(Error::unsupported_compression("DNG", "JPEG XL (build with the `jpegxl` feature)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Single-IFD DNG writer in either byte order; the strip follows the IFD.
    struct Dng {
        big: bool,
        entries: Vec<(u16, u16, u32, Vec<u8>)>,
        payload: Vec<u8>,
    }

    impl Dng {
        /// A `w` x `h` RGGB CFA DNG with one uncompressed strip.
        fn new(big: bool, w: u32, h: u32, bps: u16, payload: Vec<u8>) -> Self {
            let mut d = Dng { big, entries: vec![], payload };
            d.longs(tags::NEW_SUBFILE_TYPE, &[0]);
            d.longs(tags::IMAGE_WIDTH, &[w]);
            d.longs(tags::IMAGE_LENGTH, &[h]);
            d.shorts(tags::BITS_PER_SAMPLE, &[bps]);
            d.shorts(tags::COMPRESSION, &[1]);
            d.shorts(tags::PHOTOMETRIC, &[32803]);
            d.raw(tags::MAKE, 2, b"Test\0".to_vec());
            d.raw(tags::MODEL, 2, b"Model\0".to_vec());
            d.shorts(tags::SAMPLES_PER_PIXEL, &[1]);
            d.longs(tags::ROWS_PER_STRIP, &[h]);
            d.shorts(tags::CFA_REPEAT_PATTERN_DIM, &[2, 2]);
            d.raw(tags::CFA_PATTERN_EP, 1, vec![0, 1, 1, 2]);
            d.raw(tags::DNG_VERSION, 1, vec![1, 4, 0, 0]);
            d
        }
        fn u16b(&self, v: u16) -> [u8; 2] {
            if self.big { v.to_be_bytes() } else { v.to_le_bytes() }
        }
        fn u32b(&self, v: u32) -> [u8; 4] {
            if self.big { v.to_be_bytes() } else { v.to_le_bytes() }
        }
        fn raw(&mut self, tag: u16, typ: u16, data: Vec<u8>) {
            let size = match typ {
                3 | 8 => 2,
                4 | 9 | 11 => 4,
                5 | 10 | 12 => 8,
                _ => 1,
            };
            self.entries.retain(|e| e.0 != tag);
            self.entries.push((tag, typ, (data.len() / size) as u32, data));
        }
        fn shorts(&mut self, tag: u16, v: &[u16]) {
            let b = v.iter().flat_map(|&x| self.u16b(x)).collect();
            self.raw(tag, 3, b);
        }
        fn longs(&mut self, tag: u16, v: &[u32]) {
            let b = v.iter().flat_map(|&x| self.u32b(x)).collect();
            self.raw(tag, 4, b);
        }
        /// Signed rationals with denominator 10^6.
        fn srationals(&mut self, tag: u16, v: &[f64]) {
            let b = v
                .iter()
                .flat_map(|&x| [self.u32b((x * 1e6).round() as i32 as u32), self.u32b(1_000_000)].concat())
                .collect();
            self.raw(tag, 10, b);
        }
        fn rationals(&mut self, tag: u16, v: &[f64]) {
            let b =
                v.iter().flat_map(|&x| [self.u32b((x * 1e6).round() as u32), self.u32b(1_000_000)].concat()).collect();
            self.raw(tag, 5, b);
        }
        fn build(&self) -> Vec<u8> {
            let mut entries = self.entries.clone();
            entries.retain(|e| e.0 != tags::STRIP_OFFSETS && e.0 != tags::STRIP_BYTE_COUNTS);
            let n = entries.len() + 2;
            let extra_base = 8 + 2 + n * 12 + 4;
            let extra_len: usize =
                entries.iter().filter(|e| e.3.len() > 4).map(|e| e.3.len().next_multiple_of(2)).sum();
            let payload_off = (extra_base + extra_len) as u32;
            entries.push((tags::STRIP_OFFSETS, 4, 1, self.u32b(payload_off).to_vec()));
            entries.push((tags::STRIP_BYTE_COUNTS, 4, 1, self.u32b(self.payload.len() as u32).to_vec()));
            entries.sort_by_key(|e| e.0);
            let mut out = if self.big { b"MM\0*".to_vec() } else { b"II*\0".to_vec() };
            out.extend(self.u32b(8));
            out.extend(self.u16b(entries.len() as u16));
            let mut extra = Vec::new();
            for (tag, typ, count, data) in &entries {
                out.extend(self.u16b(*tag));
                out.extend(self.u16b(*typ));
                out.extend(self.u32b(*count));
                if data.len() <= 4 {
                    let mut d = data.clone();
                    d.resize(4, 0);
                    out.extend(d);
                } else {
                    out.extend(self.u32b((extra_base + extra.len()) as u32));
                    extra.extend(data);
                    if data.len() % 2 == 1 {
                        extra.push(0);
                    }
                }
            }
            out.extend(self.u32b(0));
            out.extend(extra);
            assert_eq!(out.len(), payload_off as usize);
            out.extend(&self.payload);
            out
        }
    }

    fn open_dng(data: &[u8]) -> Result<Box<dyn RawDecoder + '_>> {
        crate::open(data, &OpenOptions::default())
    }

    fn u16_dng(big: bool) -> Dng {
        let s: Vec<u8> = (0..8 * 6).flat_map(|i: u16| (i * 300).to_le_bytes()).collect();
        let mut d = Dng::new(big, 8, 6, 16, s);
        d.longs(tags::WHITE_LEVEL, &[16383]);
        d
    }

    const IDENTITY: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

    #[test]
    fn robertson_cct() {
        // D65, illuminant A and D50 chromaticities.
        let d65 = xy_to_cct(0.31271, 0.32902).unwrap();
        assert!((d65 - 6504.0).abs() < 10.0, "{d65}");
        let a = xy_to_cct(0.44757, 0.40745).unwrap();
        assert!((a - 2856.0).abs() < 5.0, "{a}");
        let d50 = xy_to_cct(0.34567, 0.35850).unwrap();
        assert!((d50 - 5003.0).abs() < 10.0, "{d50}");
        assert!(xy_to_cct(0.0, 0.3).is_none());
        assert!(xy_to_cct(f64::NAN, 0.3).is_none());
    }

    #[test]
    fn illuminant_data_chromaticity() {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let mut b = vec![0u8, 0];
            for v in [4334u32, 10000, 4244, 10000] {
                b.extend(if order == ByteOrder::Little { v.to_le_bytes() } else { v.to_be_bytes() });
            }
            let (x, y) = illuminant_data_xy(&b, order).unwrap();
            assert!((x - 0.4334).abs() < 1e-9 && (y - 0.4244).abs() < 1e-9);
            assert!(illuminant_data_xy(&b[..12], order).is_none());
            // Spectral data (type 1) is not a chromaticity.
            b[if order == ByteOrder::Little { 0 } else { 1 }] = 1;
            assert!(illuminant_data_xy(&b, order).is_none());
        }
    }

    fn cal(temperature: Option<f64>, color: &[f64], camera: Option<&[f64]>) -> Calibration {
        Calibration { temperature, rows: color.len() / 3, color: color.to_vec(), camera: camera.map(|c| c.to_vec()) }
    }

    fn close(a: &[f32], b: &[f64]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(a, b)| (*a as f64 - b).abs() < 1e-5)
    }

    /// Normalised camera neutral of the white `x, y` through the XYZ -> camera matrix `m`.
    fn neutral(m: &[f64], x: f64, y: f64) -> Vec<f64> {
        let xyz = [x / y, 1.0, (1.0 - x - y) / y];
        let v: Vec<f64> = (0..3).map(|r| (0..3).map(|c| m[r * 3 + c] * xyz[c]).sum()).collect();
        let mx = v.iter().copied().fold(0.0, f64::max);
        v.into_iter().map(|e| e / mx).collect()
    }

    #[test]
    fn white_xy_neutral_single_matrix() {
        let (x, y) = (0.3127, 0.3290);
        let n = white_xy_neutral(&[cal(None, &IDENTITY, None)], &[], x, y).unwrap();
        assert!(close(&n, &neutral(&IDENTITY, x, y)), "{n:?}");
        // CameraCalibration and AnalogBalance scale the channels.
        let cc = [2.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let n2 = white_xy_neutral(&[cal(Some(6500.0), &IDENTITY, Some(&cc))], &[1.0, 1.0, 0.5], x, y).unwrap();
        let both = [2.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.5];
        assert!(close(&n2, &neutral(&both, x, y)), "{n2:?}");
        // Invalid whites, empty profiles and non-positive neutrals give nothing.
        assert!(white_xy_neutral(&[], &[], x, y).is_none());
        assert!(white_xy_neutral(&[cal(None, &IDENTITY, None)], &[], 0.0, 0.3).is_none());
        let negative = [-1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        assert!(white_xy_neutral(&[cal(None, &negative, None)], &[], x, y).is_none());
    }

    #[test]
    fn white_xy_neutral_interpolates_in_mired() {
        let warm = cal(Some(2856.0), &IDENTITY, None);
        let cool_m = [1.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0];
        let cool = cal(Some(6500.0), &cool_m, None);
        // Whites beyond either end use that end's matrix, in either tag order.
        for cals in [[warm.clone(), cool.clone()], [cool.clone(), warm.clone()]] {
            let (x, y) = (0.25, 0.26); // far above 10000 K
            assert!(close(&white_xy_neutral(&cals, &[], x, y).unwrap(), &neutral(&cool_m, x, y)));
            let (x, y) = (0.50, 0.41); // below 2856 K
            assert!(close(&white_xy_neutral(&cals, &[], x, y).unwrap(), &neutral(&IDENTITY, x, y)));
            // In between: weight on the warm matrix (1/T - 1/T2) / (1/T1 - 1/T2).
            let (x, y) = (0.3805, 0.3768);
            let t = xy_to_cct(x, y).unwrap();
            assert!((3800.0..4200.0).contains(&t), "{t}");
            let g = (1.0 / t - 1.0 / 6500.0) / (1.0 / 2856.0 - 1.0 / 6500.0);
            let mixed: Vec<f64> = IDENTITY.iter().zip(&cool_m).map(|(a, b)| g * a + (1.0 - g) * b).collect();
            assert!(close(&white_xy_neutral(&cals, &[], x, y).unwrap(), &neutral(&mixed, x, y)));
        }
        // A third calibration at the white's own temperature is used as is.
        let (x, y) = (0.4334, 0.4244);
        let own_m = [0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 3.0];
        let own = cal(xy_to_cct(x, y), &own_m, None);
        let n = white_xy_neutral(&[warm, cool, own], &[], x, y).unwrap();
        assert!(close(&n, &neutral(&own_m, x, y)), "{n:?}");
    }

    fn decode_dng(data: &[u8]) -> Result<RawImage> {
        let p = Profiler::new();
        open_dng(data)?.decode(&DecodeContext::new(&p))
    }

    fn float_samples() -> Vec<f32> {
        (0..8 * 6).map(|i| i as f32 / 64.0).collect()
    }

    /// Big-endian 32-bit float DNG with the samples written in `samples_big` order.
    fn float_dng(samples_big: bool, white: u32) -> Dng {
        let s =
            float_samples().iter().flat_map(|v| if samples_big { v.to_be_bytes() } else { v.to_le_bytes() }).collect();
        let mut d = Dng::new(true, 8, 6, 32, s);
        d.shorts(tags::SAMPLE_FORMAT, &[3]);
        d.longs(tags::WHITE_LEVEL, &[white]);
        d
    }

    #[test]
    fn big_endian_tiff_with_little_endian_floats() {
        // WhiteLevel holds the bits of 1.0f and the samples are little-endian.
        let f = float_dng(false, 0x3F80_0000).build();
        let d = open_dng(&f).unwrap();
        assert_eq!(d.frame_info().white_level.values, vec![1]);
        let img = decode_dng(&f).unwrap();
        assert_eq!(img.data.as_f32().unwrap(), &float_samples()[..]);
        // The same odd WhiteLevel with genuinely big-endian samples keeps the TIFF order.
        let f = float_dng(true, 0x3F80_0000).build();
        assert_eq!(decode_dng(&f).unwrap().data.as_f32().unwrap(), &float_samples()[..]);
        // Ordinary big-endian float DNGs are untouched.
        let f = float_dng(true, 1).build();
        assert_eq!(open_dng(&f).unwrap().frame_info().white_level.values, vec![1]);
        assert_eq!(decode_dng(&f).unwrap().data.as_f32().unwrap(), &float_samples()[..]);
    }

    #[test]
    fn interleave_fields() {
        // Factor 2 over 6 rows: even rows first, then odd rows.
        let map: Vec<usize> = (0..6).map(|y| interleaved_pos(y, 6, 2)).collect();
        assert_eq!(map, [0, 3, 1, 4, 2, 5]);
        // Uneven fields: 7 rows in 3 fields of 3, 2 and 2 rows.
        let map: Vec<usize> = (0..7).map(|y| interleaved_pos(y, 7, 3)).collect();
        assert_eq!(map, [0, 3, 5, 1, 4, 6, 2]);
        // Always a permutation; factor 1 (or out-of-range positions) is the identity.
        for (n, f) in [(1, 2), (5, 2), (4224, 2), (17, 4), (9, 16)] {
            let mut seen: Vec<usize> = (0..n).map(|p| interleaved_pos(p, n, f)).collect();
            seen.sort_unstable();
            assert!(seen.iter().copied().eq(0..n), "{n} {f}");
        }
        assert_eq!(interleaved_pos(5, 8, 1), 5);
        assert_eq!(interleaved_pos(9, 8, 2), 9);
    }

    #[test]
    fn staggered_cfa_layouts_decode_as_stored() {
        let mut d = u16_dng(false);
        d.shorts(tags::CFA_REPEAT_PATTERN_DIM, &[2, 4]);
        d.raw(tags::CFA_PATTERN_EP, 1, vec![0, 1, 2, 1, 2, 1, 0, 1]);
        d.shorts(tags::CFA_LAYOUT, &[2]);
        let f = d.build();
        let dec = open_dng(&f).unwrap();
        let info = dec.frame_info();
        assert_eq!(info.layout, SensorLayout::OtherCfa);
        let p = info.cfa.unwrap();
        assert_eq!((p.width(), p.height()), (4, 2));
        assert_eq!(p.color_at(0, 0), CfaColor::Red);
        assert_eq!(p.color_at(2, 0), CfaColor::Blue);
        assert_eq!(p.color_at(0, 1), CfaColor::Blue);
        assert_eq!(p.color_at(1, 1), CfaColor::Green);
        assert_eq!(dec.metadata().extra("cfa_layout"), Some("2"));
        let expected: Vec<u16> = (0..8 * 6).map(|i: u16| i * 300).collect();
        assert_eq!(decode_dng(&f).unwrap().u16(), &expected[..]);
        // Layouts beyond staggered A-D are a clear "unsupported mode", not a compression error.
        d.shorts(tags::CFA_LAYOUT, &[6]);
        let f = d.build();
        assert!(matches!(open_dng(&f).err(), Some(Error::CameraUnsupported { .. })));
    }

    #[test]
    fn as_shot_white_xy_sets_white_balance() {
        let mut d = u16_dng(false);
        d.srationals(tags::COLOR_MATRIX1, &IDENTITY);
        d.shorts(tags::CALIBRATION_ILLUMINANT1, &[21]);
        d.rationals(tags::AS_SHOT_WHITE_XY, &[0.3127, 0.3290]);
        let f = d.build();
        let wb = open_dng(&f).unwrap().frame_info().white_balance.unwrap();
        let n = neutral(&IDENTITY, 0.3127, 0.3290);
        // Multipliers are the reciprocal neutral relative to green.
        assert!((wb.coeffs[0] as f64 - n[1] / n[0]).abs() < 1e-4, "{wb:?}");
        assert!((wb.coeffs[2] as f64 - n[1] / n[2]).abs() < 1e-4, "{wb:?}");
        // AsShotNeutral wins when both are present.
        d.rationals(tags::AS_SHOT_NEUTRAL, &[0.5, 1.0, 0.25]);
        let f = d.build();
        let wb = open_dng(&f).unwrap().frame_info().white_balance.unwrap();
        assert!((wb.coeffs[0] - 2.0).abs() < 1e-5 && (wb.coeffs[2] - 4.0).abs() < 1e-5, "{wb:?}");
    }
}
