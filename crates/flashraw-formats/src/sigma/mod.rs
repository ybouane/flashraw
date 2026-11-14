//! Sigma / Foveon X3F reader (SD9 to sd Quattro H, DP1 to dp3 Quattro, Polaroid x530).
//!
//! An X3F file is a header, a list of sections and a directory at the end of the file that
//! locates them (all integers little-endian):
//!
//! * `SECi` image sections: raw sensor data (several codings) and thumbnails,
//! * `SECp` a list of name/value properties (camera, exposure, lens, time),
//! * `SECc` the CAMF calibration block (see [`camf`]).
//!
//! The sensor stores three samples per pixel. The entropy decoders live in
//! `flashraw_codecs::x3f`; Quattro bodies get their autofocus sites repaired and their
//! half-resolution layers replicated in [`quattro`].

#[allow(dead_code)]
pub(crate) mod camf;
mod quattro;

use crate::common::{Known, OpenOptions, SimpleDecoder, apply_db, classify_previews, db_lookup, push_jpeg};
use flashraw_codecs::x3f::{self, TrueHeader};
use flashraw_core::decoder::OpcodeLists;
use flashraw_core::integrity::check_range;
use flashraw_core::tiff::Tiff;
use flashraw_core::*;
use std::sync::Arc;

/// True when `data` starts with the X3F signature.
pub fn probe(data: &[u8]) -> bool {
    data.len() >= 16 && data.starts_with(b"FOVb")
}

/// Sigma TIFF/DNG files other than DNG are not an X3F; delegates to the generic TIFF handling.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    crate::misc::open_tiff(data, tiff, opts)
}

// ---------------------------------------------------------------------------------------------
// Container
// ---------------------------------------------------------------------------------------------

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    let b = d.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Header size of an image section.
const IMAGE_HEADER: usize = 28;
/// Directory entries read at most.
const MAX_DIRECTORY_ENTRIES: usize = 4096;
/// Properties read at most.
const MAX_PROPERTIES: usize = 4096;
/// Longest property string, in UTF-16 code units.
const MAX_PROPERTY_UNITS: usize = 1024;

/// First version that records a white-balance name in the header.
const VERSION_WB_NAME: u32 = 0x0002_0001;
/// First version of the Quattro generation.
const VERSION_QUATTRO: u32 = 0x0004_0000;

/// A directory entry whose first bytes lie inside the file.
#[derive(Debug, Clone, Copy)]
struct Section {
    offset: usize,
    size: usize,
}

impl Section {
    /// The section's bytes as far as the file holds them.
    fn available<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        let start = self.offset.min(data.len());
        let end = self.offset.saturating_add(self.size).min(data.len());
        &data[start..end]
    }

    /// Whether the declared range lies completely inside the file.
    fn is_complete(&self, data: &[u8]) -> bool {
        self.offset.checked_add(self.size).is_some_and(|e| e <= data.len())
    }

    fn tag<'a>(&self, data: &'a [u8]) -> Option<&'a [u8]> {
        data.get(self.offset..self.offset.checked_add(4)?)
    }
}

/// What an image section holds: `type << 16 | format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImageKind(u32);

impl ImageKind {
    fn new(kind: u32, format: u32) -> Self {
        ImageKind(kind.wrapping_mul(65536).wrapping_add(format))
    }
    /// JPEG thumbnail or preview.
    const JPEG: ImageKind = ImageKind(0x0002_0012);
    /// Uncompressed 24-bit RGB thumbnail.
    const RGB_THUMBNAIL: ImageKind = ImageKind(0x0002_0003);

    /// Raw sensor codings in order of preference.
    const RAW: [(ImageKind, Coding); 8] = [
        // Polaroid x530.
        (ImageKind(0x0003_0005), Coding::Huffman10),
        // SD9, SD10, early SD14.
        (ImageKind(0x0003_0006), Coding::Huffman10),
        // DP1, DP1s, DP2, later SD14/SD15.
        (ImageKind(0x0003_001e), Coding::True { quattro: false }),
        // Merrill bodies.
        (ImageKind(0x0001_001e), Coding::True { quattro: false }),
        // dp Quattro, sd Quattro, sd Quattro H; 0x29 is not in the corpus.
        (ImageKind(0x0001_0023), Coding::True { quattro: true }),
        (ImageKind(0x0001_0025), Coding::True { quattro: true }),
        (ImageKind(0x0001_0027), Coding::True { quattro: true }),
        (ImageKind(0x0001_0029), Coding::True { quattro: true }),
    ];

    /// Rank and coding when this is a raw sensor image.
    fn coding(self) -> Option<(usize, Coding)> {
        Self::RAW.iter().position(|(k, _)| *k == self).map(|i| (i, Self::RAW[i].1))
    }
}

/// How the raw sensor section is entropy coded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Coding {
    /// 10-bit mapped Huffman (or "simple" fixed-width rows): the early bodies.
    Huffman10,
    /// Difference coding of three planes; Quattro bodies have planes of different sizes.
    True { quattro: bool },
}

/// A parsed image-section header.
#[derive(Debug, Clone, Copy)]
struct Image {
    section: Section,
    kind: ImageKind,
    columns: u32,
    rows: u32,
    row_stride: u32,
}

/// The file's structure.
struct Container<'a> {
    data: &'a [u8],
    version: u32,
    images: Vec<Image>,
    properties: Option<Section>,
    calibration: Vec<Section>,
}

impl<'a> Container<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        let version = u32_at(data, 4).ok_or_else(|| Error::corrupt("X3F: no header"))?;
        let dir_at = data
            .len()
            .checked_sub(4)
            .and_then(|at| u32_at(data, at))
            .map(|v| v as usize)
            .filter(|&d| data.get(d..d.saturating_add(4)) == Some(b"SECd"))
            .ok_or_else(|| Error::corrupt("X3F: no directory"))?;
        let count = u32_at(data, dir_at + 8).ok_or_else(|| Error::corrupt("X3F: truncated directory"))? as usize;
        if count > MAX_DIRECTORY_ENTRIES {
            return Err(Error::corrupt("X3F: implausible directory size"));
        }

        let mut c = Container { data, version, images: Vec::new(), properties: None, calibration: Vec::new() };
        for i in 0..count {
            let at = dir_at + 12 + 12 * i;
            let (Some(offset), Some(size)) = (u32_at(data, at), u32_at(data, at + 4)) else { continue };
            let section = Section { offset: offset as usize, size: size as usize };
            // The head of the section must be in the file; the body may run past its end.
            if section.offset.saturating_add(section.size.min(IMAGE_HEADER)) > data.len() {
                continue;
            }
            match section.tag(data) {
                Some(b"SECi") if section.size >= IMAGE_HEADER => {
                    let field = |o: usize| u32_at(data, section.offset + o);
                    if let (Some(ty), Some(fmt), Some(columns), Some(rows), Some(row_stride)) =
                        (field(8), field(12), field(16), field(20), field(24))
                    {
                        c.images.push(Image { section, kind: ImageKind::new(ty, fmt), columns, rows, row_stride });
                    }
                }
                // Metadata is read from sections the file holds completely.
                Some(b"SECp") if section.is_complete(data) => {
                    c.properties.get_or_insert(section);
                }
                Some(b"SECc") => c.calibration.push(section),
                _ => {}
            }
        }
        Ok(c)
    }

    /// The raw sensor image: the most preferred coding, the first in the directory.
    fn raw_image(&self) -> Option<(Image, Coding)> {
        self.images
            .iter()
            .filter_map(|im| im.kind.coding().map(|(rank, coding)| (rank, *im, coding)))
            .min_by_key(|(rank, ..)| *rank)
            .map(|(_, im, coding)| (im, coding))
    }

    /// Camera rotation recorded in the header.
    fn orientation(&self) -> Orientation {
        let at = if self.version < VERSION_QUATTRO { 36 } else { 48 };
        match u32_at(self.data, at).unwrap_or(0) {
            90 => Orientation::Rotate90,
            180 => Orientation::Rotate180,
            270 => Orientation::Rotate270,
            _ => Orientation::Normal,
        }
    }

    /// White-balance preset name stored in the header of the second and third generation.
    fn header_preset_name(&self) -> Option<String> {
        if !(VERSION_WB_NAME..VERSION_QUATTRO).contains(&self.version) {
            return None;
        }
        let field = self.data.get(40..72)?;
        let name = &field[..field.iter().position(|&b| b == 0).unwrap_or(field.len())];
        let name = std::str::from_utf8(name).ok()?.trim();
        (!name.is_empty() && name.bytes().all(|b| (33..=126).contains(&b))).then(|| name.to_string())
    }
}

// ---------------------------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------------------------

/// The `(name, value)` pairs of a property section, in file order. A damaged list yields the
/// properties before the damage.
fn read_properties(section: &[u8]) -> Vec<(String, String)> {
    let count = u32_at(section, 8).unwrap_or(0) as usize;
    let chars = (24usize + 8 * count).min(section.len());
    let chars = &section[chars..];
    let string_at = |unit: u32| -> Option<String> {
        let bytes = chars.get(unit as usize * 2..).filter(|b| !b.is_empty())?;
        let mut units = Vec::new();
        for pair in bytes.chunks_exact(2).take(MAX_PROPERTY_UNITS) {
            let u = u16::from_le_bytes([pair[0], pair[1]]);
            if u == 0 {
                break;
            }
            units.push(u);
        }
        Some(String::from_utf16_lossy(&units))
    };
    let mut out = Vec::new();
    for i in 0..count.min(MAX_PROPERTIES) {
        let (Some(n), Some(v)) = (u32_at(section, 24 + 8 * i), u32_at(section, 28 + 8 * i)) else { break };
        let (Some(name), Some(value)) = (string_at(n), string_at(v)) else { break };
        out.push((name, value));
    }
    out
}

/// Read access to the properties: the last occurrence of a name wins, values are trimmed.
struct Properties(Vec<(String, String)>);

impl Properties {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.trim())
    }
    fn number(&self, name: &str) -> Option<f64> {
        self.get(name)?.parse().ok()
    }
    fn positive(&self, name: &str) -> Option<f64> {
        self.number(name).filter(|&v| v > 0.0)
    }
    fn text(&self, name: &str) -> Option<String> {
        self.get(name).filter(|v| !v.is_empty()).map(String::from)
    }
    /// Ordered pair from the first and last numeric word of a range such as "7.3 to 21.9".
    fn range(&self, name: &str) -> Option<(f32, f32)> {
        let mut words = self.get(name)?.split_whitespace().filter_map(|w| w.parse::<f32>().ok());
        let first = words.next()?;
        let last = words.next_back().unwrap_or(first);
        Some((first.min(last), first.max(last)))
    }
}

/// `YYYY-MM-DDTHH:MM:SS` for seconds since 1970-01-01T00:00:00 (the camera clock is taken as UTC).
fn format_time(seconds: i64) -> String {
    let (days, secs) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Proleptic Gregorian `(year, month, day)` of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

fn apply_properties(meta: &mut RawMetadata, p: &Properties) {
    meta.make = p.get("CAMMANUF").unwrap_or_default().to_string();
    meta.model = p.get("CAMMODEL").unwrap_or_default().to_string();
    meta.serial = p.text("CAMSERIAL");
    meta.firmware = p.text("FIRMVERS");
    meta.exposure.iso = p.positive("ISO").map(|v| v as u32);
    meta.exposure.exposure_time = p.positive("SHUTTER");
    meta.exposure.f_number = p.positive("APERTURE").map(|v| v as f32);
    meta.exposure.focal_length = p.positive("FLENGTH").map(|v| v as f32);
    meta.exposure.focal_length_35mm = p.positive("FLEQ35MM").map(|v| v as u32);
    meta.exposure.exposure_bias = p.number("EXPCOMP").map(|v| v as f32);
    meta.lens.model = p.text("LENSMODEL");
    if let Some(r) = p.range("LENSFRANGE") {
        meta.lens.focal_range = Some(r);
    }
    if let Some(r) = p.range("LENSARANGE") {
        meta.lens.aperture_range = Some(r);
    }
    if let Some(t) = p.get("TIME").and_then(|t| t.parse::<i64>().ok()).filter(|&t| t > 0) {
        meta.capture_time = Some(format_time(t));
    }
    if let Some(wb) = p.get("WB_DESC") {
        meta.set_extra("white_balance", wb);
    }
}

// ---------------------------------------------------------------------------------------------
// Quattro model names
// ---------------------------------------------------------------------------------------------

/// Raw widths of the Quattro bodies (full and half resolution).
const QUATTRO_WIDTHS: [u32; 6] = [5888, 2944, 6656, 3328, 5504, 2752];
/// Raw widths of the sd Quattro H family.
const QUATTRO_H_WIDTHS: [u32; 4] = [6656, 3328, 5504, 2752];
/// Raw heights of the sd Quattro.
const QUATTRO_SD_HEIGHTS: [u32; 2] = [3776, 1888];

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Quattro files have no property list; the model is in the header text or follows from the size.
fn quattro_model(data: &[u8], width: u32, height: u32) -> Option<String> {
    if !QUATTRO_WIDTHS.contains(&width) {
        return None;
    }
    let head = &data[..data.len().min(2048)];
    if let Some(at) = find(head, b"SIGMA dp") {
        let digit = head.get(at + 8).copied().filter(u8::is_ascii_digit).unwrap_or(b'2');
        return Some(format!("dp{} Quattro", digit as char));
    }
    if let Some(at) = find(head, b"sd Quatt") {
        let window = &head[at..head.len().min(at + 20)];
        if find(window, b"sd Quattro H").is_some() {
            return Some("sd Quattro H".to_string());
        }
        let text = &head[at..];
        let text = &text[..text.iter().position(|&b| b == 0).unwrap_or(text.len())];
        return Some(String::from_utf8_lossy(text).trim().to_string());
    }
    Some(
        if QUATTRO_H_WIDTHS.contains(&width) {
            "sd Quattro H"
        } else if QUATTRO_SD_HEIGHTS.contains(&height) {
            "sd Quattro"
        } else {
            "dp2 Quattro"
        }
        .to_string(),
    )
}

// ---------------------------------------------------------------------------------------------
// Open
// ---------------------------------------------------------------------------------------------

/// Open an X3F file.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if !probe(data) {
        return Err(Error::UnsupportedFormat("not an X3F file".into()));
    }
    let container = Container::parse(data)?;
    let (image, coding) =
        container.raw_image().ok_or_else(|| Error::UnsupportedFormat("X3F: no raw image section".into()))?;
    let (w, h) = (image.columns, image.rows);
    if w == 0 || h == 0 || w > 65535 || h > 65535 {
        return Err(Error::corrupt(format!("X3F: raw image size {w}x{h}")));
    }
    opts.limits.check_dims(w, h, 3)?;
    let orientation = container.orientation();

    // Metadata.
    let mut meta = RawMetadata::default();
    if let Some(section) = container.properties {
        apply_properties(&mut meta, &Properties(read_properties(section.available(data))));
    }
    if meta.make.is_empty() && meta.model.is_empty() {
        meta.make = "SIGMA".to_string();
        meta.model = quattro_model(data, w, h).unwrap_or_default();
    }
    meta.set_extra("x3f_version", format!("{}.{}", container.version >> 16, container.version & 0xffff));
    meta.set_extra("x3f_raw_type", format!("{:x}/{:x}", image.kind.0 >> 16, image.kind.0 & 0xffff));
    collect_previews(&container, orientation, &mut meta);

    // Frame description from the database.
    let mode = format!("x3f-{w}x{h}");
    let cam = db_lookup(&mut meta, &mode).filter(|c| c.mode == mode);
    let mut info = FrameInfo::new(w, h, 12);
    info.cpp = 3;
    info.layout = SensorLayout::Foveon;
    info.compression = Compression::SigmaX3f;
    info.orientation = orientation;
    info.pixel_aspect = if u64::from(h) * 2 < u64::from(w) {
        0.5
    } else if h > w {
        2.0
    } else {
        1.0
    };
    let fallback = flashraw_camera_db::find_matrix(&meta.canonical_make, &meta.canonical_model);
    info.black_level = BlackLevel::uniform(fallback.map_or(0, |f| f.black.max(0)) as f32);
    info.white_level = WhiteLevel::uniform(fallback.filter(|f| f.white > 0).map_or(16383, |f| f.white as u32));
    apply_db(&mut info, &meta, cam, Known::default());
    info.crop = info.active_area;
    // The autofocus repair measures from the black level before calibration changes anything.
    let black_for_af = info.black_level.values.first().copied().unwrap_or(0.0).max(0.0) as u32;
    let swap_rb = meta.make.eq_ignore_ascii_case("Polaroid") && meta.model.eq_ignore_ascii_case("x530");
    let active = info.active_area;

    // Calibration block.
    let camf_section = container.calibration.iter().find(|s| s.is_complete(data));
    if let Some(mut camf) = camf_section.and_then(|s| camf::Camf::from_section(s.available(data))) {
        calibrate(&mut camf, &container, &mut meta, &mut info, coding, swap_rb);
    }

    let job = Job { data, image, coding, swap_rb, black_for_af, active };
    let decode = move |ctx: &DecodeContext| job.decode(ctx);
    Ok(Box::new(SimpleDecoder::new(RawFormat::X3f, meta, info, Box::new(decode))))
}

/// Offer every thumbnail-like image section as a preview.
fn collect_previews(container: &Container, orientation: Orientation, meta: &mut RawMetadata) {
    let data = container.data;
    for im in &container.images {
        let body = im.section.size - IMAGE_HEADER;
        let start = im.section.offset + IMAGE_HEADER;
        if im.kind == ImageKind::JPEG {
            push_jpeg(data, start, body, orientation, &mut meta.previews);
        } else if im.kind == ImageKind::RGB_THUMBNAIL {
            let (w, h) = (im.columns as usize, im.rows as usize);
            let bytes = w.saturating_mul(h).saturating_mul(3);
            if w > 0 && h > 0 && im.row_stride as usize == 3 * w && im.section.is_complete(data) && bytes <= body {
                meta.previews.push(PreviewInfo::new(
                    start as u64,
                    bytes as u64,
                    im.columns,
                    im.rows,
                    PreviewEncoding::Rgb8,
                    orientation,
                    PreviewKind::Thumbnail,
                ));
            }
        }
    }
    classify_previews(&mut meta.previews);
}

/// Everything the calibration block contributes: optical-black areas, white balance and colour
/// matrix, digital-ISO scaling and lens-shading maps.
fn calibrate(
    camf: &mut camf::Camf,
    container: &Container,
    meta: &mut RawMetadata,
    info: &mut FrameInfo,
    coding: Coding,
    swap_rb: bool,
) {
    let (w, h) = (info.width, info.height);
    let quattro = coding == Coding::True { quattro: true };

    // Black areas from the shielded rows (the Quattro keep the database's).
    if !quattro {
        let active = camf.rect("ActiveImageArea").unwrap_or(info.active_area);
        let buffer = Rect::full(w, h);
        info.masked_areas = ["DarkShieldTop", "DarkShieldBottom"]
            .into_iter()
            .filter_map(|name| camf.rect(name))
            .filter_map(|r| r.intersect(&buffer))
            .filter(|r| r.intersect(&active).is_none() && r.intersect(&info.active_area).is_none())
            .collect();
        info.black_from_masked = !info.masked_areas.is_empty();
    }

    // White-balance preset in effect.
    let preset: Option<String> = if container.version >= VERSION_QUATTRO {
        camf.white_balance_name().map(String::from)
    } else {
        meta.extra("white_balance").map(String::from).or_else(|| container.header_preset_name())
    };
    let preset = preset.as_deref().unwrap_or("Auto");

    if let Some(white) = stated_white(camf, coding) {
        info.white_level = WhiteLevel::uniform(white);
    }
    apply_digital_iso(camf, meta, info);

    let calibration = camf::calibration(camf, preset);

    if !quattro {
        let shading_preset = calibration.as_ref().map_or("Auto", |c| c.preset.as_str());
        if let Some(mut grids) = camf::shading(camf, shading_preset, w, h) {
            if swap_rb {
                grids.swap(0, 2);
            }
            let list2 = gain_map_opcodes(&grids, info);
            info.opcodes = Some(Arc::new(OpcodeLists { lists: [Vec::new(), list2, Vec::new()] }));
            meta.set_extra("lens_shading", "CAMF");
        }
    }

    // White balance and colour matrix.
    if let Some(cal) = calibration {
        if let Some(mut matrix) = cal.color_matrix() {
            let mut gains = cal.gains;
            if swap_rb {
                gains.swap(0, 2);
                for c in 0..3 {
                    matrix.values.swap(c, 6 + c);
                }
            }
            let wb = WhiteBalance::from_multipliers(gains[0] as f32, gains[1] as f32, gains[2] as f32, gains[1] as f32);
            if let Some(wb) = wb {
                info.white_balance = Some(wb);
                info.color_matrices =
                    ColorMatrices { color: vec![matrix], source: ColorSource::File, ..Default::default() };
                meta.set_extra("white_balance", &cal.preset);
            }
        }
    }
}

/// White level the calibration block states, in the domain of the decoded samples.
///
/// * Merrill and Quattro bodies record the sample depth, `ImageDepth` (12 and 14 bits): their
///   samples saturate at the top of that range.
/// * The difference-coded bodies before them (DP1, DP1s, DP2, DP2x, SD15, ...) record per-layer
///   `DarkLevel` and `MaxOutputLevel` in the sensor's output domain, and store their samples
///   with the dark level already subtracted (at base ISO the dark-shield rows read about 1).
///   Their white is `MaxOutputLevel - DarkLevel`, which DP2x and SD15 state again as
///   `RawSaturationLevel`. The lowest layer's value is taken: clipped samples of every layer
///   lie above it (by 5 to 20 % in sample files).
/// * The 10-bit Huffman bodies (SD9, SD10, SD14, Polaroid x530) keep a pedestal in their
///   samples and scale them differently; their levels come from the database.
fn stated_white(camf: &mut camf::Camf, coding: Coding) -> Option<u32> {
    if let Some(depth) = camf.scalar("ImageDepth").filter(|d| (8.0..=16.0).contains(d) && d.fract() == 0.0) {
        return Some((1u32 << depth as u32) - 1);
    }
    if coding != (Coding::True { quattro: false }) {
        return None;
    }
    let dark = camf.values("DarkLevel", 3)?;
    let max = camf.values("MaxOutputLevel", 3)?;
    let white = dark.iter().zip(&max).map(|(d, m)| m - d).fold(f64::INFINITY, f64::min);
    (256.0..=65535.0).contains(&white).then_some(white as u32)
}

/// Above the sensor's base sensitivity the camera amplifies digitally: the raw samples stay at
/// base gain and saturate at `1/gain` of the range. Lower the white level accordingly.
fn apply_digital_iso(camf: &mut camf::Camf, meta: &mut RawMetadata, info: &mut FrameInfo) {
    let (Some(capture), Some(sensor)) = (camf.scalar("CaptureISO"), camf.scalar("SensorISO")) else { return };
    let gain = capture / sensor;
    let black = f64::from(info.black_level.values.first().copied().unwrap_or(0.0).max(0.0));
    let white = f64::from(info.white_level.max());
    if gain.is_finite() && gain > 1.05 && gain <= 1024.0 && white > black {
        let scaled = (black + (white - black) / gain).round().max(black + 1.0);
        info.white_level = WhiteLevel::uniform(scaled as u32);
        meta.set_extra("digital_iso_gain", format!("{gain:.3}"));
    }
}

/// DNG OpcodeList2 (big-endian) with one `GainMap` per layer. The grids' corner points lie on
/// the corners of the whole buffer; GainMap positions are relative to the active area.
fn gain_map_opcodes(grids: &[camf::GainGrid; 3], info: &FrameInfo) -> Vec<u8> {
    const GAIN_MAP: u32 = 9;
    const DNG_VERSION_1_3: u32 = 0x0103_0000;
    const OPTIONAL: u32 = 1;
    let active = info.active_area;
    let (aw, ah) = (active.width, active.height);
    let mut out = Vec::new();
    out.extend_from_slice(&3u32.to_be_bytes());
    for (plane, g) in grids.iter().enumerate() {
        let (rows, cols) = (g.rows as u32, g.cols as u32);
        let mut words = |ws: &[u32]| ws.iter().for_each(|v| out.extend_from_slice(&v.to_be_bytes()));
        words(&[GAIN_MAP, DNG_VERSION_1_3, OPTIONAL, 76 + 4 * rows * cols]);
        words(&[0, 0, ah, aw, plane as u32, 1, 1, 1, rows, cols]);
        let spacing_v = f64::from(info.height - 1) / f64::from(rows.saturating_sub(1).max(1)) / f64::from(ah.max(1));
        let spacing_h = f64::from(info.width - 1) / f64::from(cols.saturating_sub(1).max(1)) / f64::from(aw.max(1));
        let origin_v = -f64::from(active.y) / f64::from(ah.max(1));
        let origin_h = -f64::from(active.x) / f64::from(aw.max(1));
        for v in [spacing_v, spacing_h, origin_v, origin_h] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&1u32.to_be_bytes());
        for gain in &g.gains {
            out.extend_from_slice(&gain.to_be_bytes());
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------------------------

/// Everything the decode closure needs.
struct Job<'a> {
    data: &'a [u8],
    image: Image,
    coding: Coding,
    swap_rb: bool,
    black_for_af: u32,
    /// Active area of the frame (after the database), which bounds the autofocus repair.
    active: Rect,
}

/// Where a plane's samples are stored.
#[derive(Debug, Clone, Copy)]
struct PlaneShape {
    /// Columns and rows the plane codes.
    cols: usize,
    rows: usize,
    /// Columns kept (the stored row length).
    kept: usize,
}

/// How the three planes combine into the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Assembly {
    /// Every plane is a full layer: sample `k` of pixel `(x, y)` is plane `k` at `(y, x)`.
    Dense,
    /// Quattro half-resolution layout: the first two planes cover every second pixel in each direction.
    Half,
}

impl Job<'_> {
    fn decode(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let sec = self.image.section;
        check_range(self.data.len(), DataUnit::Payload, sec.offset, sec.size).map_err(Error::truncated)?;
        let (w, h) = (self.image.columns, self.image.rows);
        let mut img = RawImage::new_u16(w, h, 3, &ctx.limits)?;
        let payload = &self.data[sec.offset + IMAGE_HEADER..sec.offset + sec.size];
        let (wu, hu) = (w as usize, h as usize);
        match self.coding {
            Coding::Huffman10 => {
                ctx.profiler.time(Stage::EntropyDecode, || self.decode_huffman10(payload, ctx, img.u16_mut()))?;
                if self.swap_rb {
                    for px in img.u16_mut().chunks_exact_mut(3) {
                        px.swap(0, 2);
                    }
                }
            }
            Coding::True { quattro } => {
                let assembly = self.decode_true(payload, quattro, ctx, img.u16_mut())?;
                if quattro {
                    ctx.profiler.time(Stage::Calibration, || {
                        quattro::repair_af_sites(img.u16_mut(), wu, hu, self.active, self.black_for_af);
                        if assembly == Assembly::Half {
                            quattro::replicate_half_layers(img.u16_mut(), wu, hu, ctx.parallel);
                        }
                    });
                }
            }
        }
        Ok(img)
    }

    fn decode_huffman10(&self, payload: &[u8], ctx: &DecodeContext, out: &mut [u16]) -> Result<()> {
        let (w, h) = (self.image.columns as usize, self.image.rows as usize);
        let mapping_bytes = payload.get(..2048).ok_or_else(|| Error::corrupt("X3F: truncated mapping table"))?;
        let mapping: Vec<u16> = mapping_bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        let rest = &payload[2048..];
        if self.image.row_stride != 0 {
            return x3f::decode_simple(rest, self.image.row_stride as usize, &mapping, 10, w, h, out, ctx.parallel);
        }
        let table_bytes = rest.get(..4096).ok_or_else(|| Error::corrupt("X3F: truncated code table"))?;
        let table: Vec<u32> =
            table_bytes.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        let body = &rest[4096..];
        let offsets_len = h * 4;
        if body.len() <= offsets_len {
            return Err(Error::corrupt("X3F: no room for the row offsets"));
        }
        let (coded, offset_bytes) = body.split_at(body.len() - offsets_len);
        let offsets: Vec<u32> =
            offset_bytes.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        let code = x3f::huffman_code(&table, &mapping)?;
        x3f::decode_huffman(&code, coded, &offsets, w, h, out, ctx.parallel)
    }

    /// Shapes of the three planes for this header, and how they assemble.
    fn plane_shapes(
        &self,
        header: &TrueHeader,
        quattro: bool,
        ctx: &DecodeContext,
    ) -> Result<([PlaneShape; 3], Assembly)> {
        let (w, h) = (self.image.columns as usize, self.image.rows as usize);
        let Some(planes) = header.planes.filter(|_| quattro) else {
            let dense = PlaneShape { cols: w, rows: h, kept: w };
            return Ok(([dense; 3], Assembly::Dense));
        };
        let coded = planes.map(|(c, r)| (c as usize, r as usize));
        let (c0, r0) = coded[0];
        let (kept, assembly) = if r0 == h / 2 {
            if coded[1].1 != r0 {
                return Err(Error::corrupt("X3F: Quattro planes of different heights"));
            }
            ([c0, c0, coded[2].0], Assembly::Half)
        } else if r0 == h {
            if coded.iter().any(|&(_, r)| r != h) {
                return Err(Error::corrupt("X3F: Quattro planes of different heights"));
            }
            ([w; 3], Assembly::Dense)
        } else {
            return Err(Error::corrupt("X3F: unexpected Quattro plane geometry"));
        };
        let shapes: [PlaneShape; 3] =
            std::array::from_fn(|i| PlaneShape { cols: coded[i].0, rows: coded[i].1, kept: kept[i] });
        for s in &shapes {
            if s.cols < s.kept {
                return Err(Error::corrupt("X3F: Quattro plane narrower than its stored width"));
            }
            ctx.limits.check_dims(s.kept as u32, s.rows as u32, 1)?;
        }
        Ok((shapes, assembly))
    }

    /// Decode the three planes of a difference-coded image into `out`.
    fn decode_true(&self, payload: &[u8], quattro: bool, ctx: &DecodeContext, out: &mut [u16]) -> Result<Assembly> {
        let header = TrueHeader::parse(payload, quattro)?;
        let code = header.table()?;
        let body = payload.get(header.data_offset..).ok_or_else(|| Error::corrupt("X3F: header beyond the section"))?;
        let ranges = header.plane_ranges(body.len())?;
        let (shapes, assembly) = self.plane_shapes(&header, quattro, ctx)?;

        let decoded = ctx.profiler.time(Stage::EntropyDecode, || {
            par::map(vec![0usize, 1, 2], ctx.parallel, |i| -> Result<Vec<u16>> {
                let s = shapes[i];
                let mut plane = alloc_zeroed::<u16>(s.kept.saturating_mul(s.rows))?;
                let (start, end) = ranges[i];
                let coded = body.get(start..end).unwrap_or(&[]);
                x3f::decode_true_plane(&code, coded, header.seeds[i], s.cols, s.rows, s.kept, &mut plane)?;
                Ok(plane)
            })
        });
        let mut planes = Vec::with_capacity(3);
        for p in decoded {
            planes.push(p?);
        }
        let (w, h) = (self.image.columns as usize, self.image.rows as usize);
        ctx.profiler.time(Stage::Calibration, || match assembly {
            Assembly::Dense => interleave_dense(out, w, h, &planes, &shapes, ctx.parallel),
            Assembly::Half => interleave_half(out, w, h, &planes, &shapes, ctx.parallel),
        });
        Ok(assembly)
    }
}

/// Interleave full-size planes: sample `k` of pixel `(x, y)` is plane `k` at row `y`, column `x`.
fn interleave_dense(
    out: &mut [u16],
    w: usize,
    h: usize,
    planes: &[Vec<u16>],
    shapes: &[PlaneShape; 3],
    parallel: bool,
) {
    let stride = w * 3;
    par::for_each_row_chunk(&mut out[..w * h * 3], stride, 32, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_exact_mut(stride).enumerate() {
            let y = first + i;
            for (k, (plane, shape)) in planes.iter().zip(shapes).enumerate() {
                if y >= shape.rows {
                    continue;
                }
                let src = &plane[y * shape.kept..(y + 1) * shape.kept];
                for (px, &v) in row.chunks_exact_mut(3).zip(src) {
                    px[k] = v;
                }
            }
        }
    });
}

/// Assemble the Quattro half-resolution layout: the bottom and middle layers land on the
/// even rows and columns, the top layer at full resolution.
fn interleave_half(out: &mut [u16], w: usize, h: usize, planes: &[Vec<u16>], shapes: &[PlaneShape; 3], parallel: bool) {
    let stride = w * 3;
    let (low, mid, top) = (&planes[0], &planes[1], &planes[2]);
    let (low_shape, top_shape) = (shapes[0], shapes[2]);
    let low_rows = low_shape.rows.min(h / 2);
    let low_cols = low_shape.kept.min(w / 2);
    par::for_each_row_chunk(&mut out[..w * h * 3], stride, 32, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_exact_mut(stride).enumerate() {
            let y = first + i;
            if y < top_shape.rows {
                let src = &top[y * top_shape.kept..(y + 1) * top_shape.kept];
                for (px, &v) in row.chunks_exact_mut(3).zip(src) {
                    px[2] = v;
                }
            }
            if y % 2 == 0 && y / 2 < low_rows {
                let base = (y / 2) * low_shape.kept;
                let (lo, mi) = (&low[base..base + low_cols], &mid[base..base + low_cols]);
                for ((px, &a), &b) in row.chunks_exact_mut(6).zip(lo).zip(mi) {
                    px[0] = a;
                    px[1] = b;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::camf::tests::Block;
    use super::*;
    use crate::testkit::{Lcg, check_golden, corpus, fnv};

    const PLAIN: Coding = Coding::True { quattro: false };
    const QUATTRO: Coding = Coding::True { quattro: true };

    // -----------------------------------------------------------------------------------------
    // A small X3F writer
    // -----------------------------------------------------------------------------------------

    fn image_section(kind: u32, format: u32, cols: u32, rows: u32, stride: u32, payload: &[u8]) -> Vec<u8> {
        let mut s = b"SECi".to_vec();
        for v in [0x0002_0003, kind, format, cols, rows, stride] {
            s.extend_from_slice(&u32::to_le_bytes(v));
        }
        s.extend_from_slice(payload);
        s
    }

    fn property_section(props: &[(&str, &str)]) -> Vec<u8> {
        let mut chars: Vec<u16> = Vec::new();
        let mut at = |s: &str| {
            let o = chars.len() as u32;
            chars.extend(s.encode_utf16());
            chars.push(0);
            o
        };
        let pairs: Vec<(u32, u32)> = props.iter().map(|(n, v)| (at(n), at(v))).collect();
        let mut s = b"SECp".to_vec();
        for v in [0x0002_0000, props.len() as u32, 0, 0, chars.len() as u32] {
            s.extend_from_slice(&v.to_le_bytes());
        }
        for (n, v) in pairs {
            s.extend_from_slice(&n.to_le_bytes());
            s.extend_from_slice(&v.to_le_bytes());
        }
        s.extend(chars.iter().flat_map(|c| c.to_le_bytes()));
        s
    }

    /// Header, sections and a directory at the end.
    fn x3f_file(version: u32, sections: &[Vec<u8>]) -> Vec<u8> {
        let mut f = b"FOVb".to_vec();
        f.extend_from_slice(&version.to_le_bytes());
        f.resize(128, 0);
        let mut entries = Vec::new();
        for s in sections {
            entries.push((f.len() as u32, s.len() as u32, [s[0], s[1], s[2], s[3]]));
            f.extend_from_slice(s);
        }
        let dir = f.len() as u32;
        f.extend_from_slice(b"SECd");
        f.extend_from_slice(&0x0002_0000u32.to_le_bytes());
        f.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (o, s, name) in entries {
            f.extend_from_slice(&o.to_le_bytes());
            f.extend_from_slice(&s.to_le_bytes());
            f.extend_from_slice(&name);
        }
        f.extend_from_slice(&dir.to_le_bytes());
        f
    }

    /// A 4x3 image in the uncompressed 3/5 coding: identity index mapping, three 10-bit indices per word.
    fn polaroid_file(props: &[(&str, &str)]) -> Vec<u8> {
        let mut payload: Vec<u8> = (0..1024u16).flat_map(|v| v.to_le_bytes()).collect();
        for _row in 0..3 {
            for i in 0..4u32 {
                payload.extend_from_slice(&((i % 4) | ((i % 3) << 10) | (1 << 20)).to_le_bytes());
            }
        }
        x3f_file(0x0002_0001, &[image_section(3, 5, 4, 3, 16, &payload), property_section(props)])
    }

    fn decode(d: &dyn RawDecoder, recover: bool) -> Result<RawImage> {
        let p = Profiler::default();
        let mut ctx = DecodeContext::new(&p);
        ctx.recover = recover;
        d.decode(&ctx)
    }

    fn open_bytes(data: &[u8]) -> Result<Box<dyn RawDecoder + '_>> {
        open(data, &OpenOptions::default())
    }

    // -----------------------------------------------------------------------------------------
    // Container and metadata
    // -----------------------------------------------------------------------------------------

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19000), (2022, 1, 8));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11016), (2000, 2, 29));
        assert_eq!(format_time(1_161_358_478), "2006-10-20T15:34:38");
        assert_eq!(format_time(1), "1970-01-01T00:00:01");
        // Far beyond the 4-digit years: no overflow.
        assert!(format_time(i64::MAX).starts_with("292277026596-"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(!probe(b"FOVb"));
        assert!(probe(&[b"FOVb".as_slice(), &[0; 12]].concat()));
        let e = open_bytes(&[b"FOVb".as_slice(), &[0; 60]].concat()).err().unwrap();
        assert_eq!(e.kind(), "CorruptFile");
        let mut pointing_outside = [b"FOVb".as_slice(), &[0; 60]].concat();
        pointing_outside.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(open_bytes(&pointing_outside).err().unwrap().kind(), "CorruptFile");
        assert_eq!(open_bytes(b"IIII not an x3f").err().unwrap().kind(), "UnsupportedFormat");
        // No raw image among the sections.
        let only_props = x3f_file(0x0002_0001, &[property_section(&[("ISO", "100")])]);
        assert_eq!(open_bytes(&only_props).err().unwrap().kind(), "UnsupportedFormat");
    }

    #[test]
    fn uncompressed_polaroid_swaps_the_outer_layers() {
        let file = polaroid_file(&[("CAMMANUF", "Polaroid"), ("CAMMODEL", "x530"), ("ISO", "100")]);
        let d = open_bytes(&file).unwrap();
        let (m, f) = (d.metadata(), d.frame_info());
        assert_eq!((m.make.as_str(), m.model.as_str(), m.exposure.iso), ("Polaroid", "x530", Some(100)));
        assert_eq!((f.width, f.height, f.cpp, f.layout), (4, 3, 3, SensorLayout::Foveon));
        assert_eq!(f.compression, Compression::SigmaX3f);
        assert_eq!(m.extra("x3f_version"), Some("2.1"));
        assert_eq!(m.extra("x3f_raw_type"), Some("3/5"));
        let img = decode(d.as_ref(), false).unwrap();
        let px = img.u16();
        // Running sums per layer along row 0: layer 0 steps (0,1,2,3), layer 1 (0,1,2,0), layer 2 (1,1,1,1); the
        // Polaroid stores them in reverse order.
        assert_eq!(&px[0..3], &[1, 0, 0]);
        assert_eq!(&px[3..6], &[2, 1, 1]);
        assert_eq!(&px[9..12], &[4, 3, 6]);
        // Another make is not swapped.
        let other = polaroid_file(&[("CAMMANUF", "SIGMA"), ("CAMMODEL", "SD9")]);
        let img = decode(open_bytes(&other).unwrap().as_ref(), false).unwrap();
        assert_eq!(&img.u16()[3..6], &[1, 1, 2]);
    }

    #[test]
    fn properties_become_metadata() {
        let p = |v: &[(&str, &str)]| {
            let mut m = RawMetadata::default();
            apply_properties(&mut m, &Properties(v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()));
            m
        };
        let m = p(&[
            ("CAMMANUF", " SIGMA "),
            ("CAMMODEL", "SIGMA DP2 Merrill"),
            ("CAMSERIAL", ""),
            ("FIRMVERS", "1.02"),
            ("ISO", "200.9"),
            ("SHUTTER", "0.008"),
            ("APERTURE", "2.8"),
            ("FLENGTH", "30"),
            ("FLEQ35MM", "45.7"),
            ("EXPCOMP", "-0.3"),
            ("LENSMODEL", "30mm F2.8"),
            ("LENSFRANGE", "21.9 to 7.3"),
            ("LENSARANGE", "19"),
            ("TIME", "1161358478"),
            ("WB_DESC", " Auto "),
        ]);
        assert_eq!((m.make.as_str(), m.model.as_str()), ("SIGMA", "SIGMA DP2 Merrill"));
        assert_eq!((m.serial.as_deref(), m.firmware.as_deref()), (None, Some("1.02")));
        assert_eq!(m.exposure.iso, Some(200));
        assert_eq!(m.exposure.exposure_time, Some(0.008));
        assert_eq!((m.exposure.f_number, m.exposure.focal_length), (Some(2.8), Some(30.0)));
        assert_eq!((m.exposure.focal_length_35mm, m.exposure.exposure_bias), (Some(45), Some(-0.3)));
        assert_eq!(m.lens.model.as_deref(), Some("30mm F2.8"));
        assert_eq!((m.lens.focal_range, m.lens.aperture_range), (Some((7.3, 21.9)), Some((19.0, 19.0))));
        assert_eq!(m.capture_time.as_deref(), Some("2006-10-20T15:34:38"));
        assert_eq!(m.extra("white_balance"), Some("Auto"));
        // Invalid or non-positive numbers leave the field absent; a repeated name: the last one wins.
        let m = p(&[("ISO", "fast"), ("SHUTTER", "-1"), ("APERTURE", "0"), ("TIME", "-5"), ("EXPCOMP", "0")]);
        assert_eq!((m.exposure.iso, m.exposure.exposure_time, m.exposure.f_number), (None, None, None));
        assert_eq!((m.capture_time, m.exposure.exposure_bias), (None, Some(0.0)));
        let m = p(&[("ISO", "100"), ("ISO", "400"), ("LENSFRANGE", "no numbers")]);
        assert_eq!((m.exposure.iso, m.lens.focal_range), (Some(400), None));
    }

    #[test]
    fn property_lists_are_read_defensively() {
        let section = property_section(&[("A", "1"), ("B", "2"), ("C", "3")]);
        assert_eq!(read_properties(&section).len(), 3);
        // Cut inside the character data: the pairs before the damage survive.
        let cut = read_properties(&section[..section.len() - 5]);
        assert!(cut.len() <= 3);
        assert_eq!(cut.first(), Some(&("A".to_string(), "1".to_string())));
        // Offsets outside the character data end the list; huge counts are bounded.
        let mut bad = section.clone();
        bad[24 + 8..24 + 12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(read_properties(&bad).len(), 1);
        let mut many = section.clone();
        many[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        let _ = read_properties(&many);
        assert!(read_properties(&section[..10]).is_empty());
    }

    #[test]
    fn quattro_model_names() {
        let with = |text: &[u8], at: usize| {
            let mut d = vec![0u8; 3000];
            d[at..at + text.len()].copy_from_slice(text);
            d
        };
        assert_eq!(quattro_model(&with(b"SIGMA dp0 Quattro", 1446), 5888, 3672).as_deref(), Some("dp0 Quattro"));
        assert_eq!(quattro_model(&with(b"SIGMA dpX", 100), 5888, 3672).as_deref(), Some("dp2 Quattro"));
        assert_eq!(quattro_model(&with(b"sd Quattro H", 500), 5888, 3776).as_deref(), Some("sd Quattro H"));
        assert_eq!(quattro_model(&with(b"sd Quattro  ", 500), 5888, 3776).as_deref(), Some("sd Quattro"));
        // Past the first 2048 bytes: by size.
        let none = vec![0u8; 3000];
        assert_eq!(quattro_model(&none, 6656, 4480).as_deref(), Some("sd Quattro H"));
        assert_eq!(quattro_model(&none, 5888, 3776).as_deref(), Some("sd Quattro"));
        assert_eq!(quattro_model(&none, 5888, 3672).as_deref(), Some("dp2 Quattro"));
        assert_eq!(quattro_model(&none, 4928, 3264), None);
    }

    #[test]
    fn truncated_raw_section_opens_but_does_not_decode() {
        let file = polaroid_file(&[("CAMMANUF", "Polaroid"), ("CAMMODEL", "x530")]);
        // Declare more raw data than the file has.
        let dir = u32::from_le_bytes(file[file.len() - 4..].try_into().unwrap()) as usize;
        let mut long = file.clone();
        let size_at = dir + 12 + 4;
        let declared = u32::from_le_bytes(long[size_at..size_at + 4].try_into().unwrap()) + 100_000;
        long[size_at..size_at + 4].copy_from_slice(&declared.to_le_bytes());
        let d = open_bytes(&long).unwrap();
        assert_eq!(d.metadata().make, "Polaroid");
        for recover in [false, true] {
            let e = decode(d.as_ref(), recover).err().unwrap();
            assert_eq!(e.kind(), "CorruptFile");
            let t = e.truncation().unwrap();
            assert_eq!(t.expected, u64::from(declared));
        }
    }

    #[test]
    fn files_cut_anywhere_never_panic() {
        let file = polaroid_file(&[("CAMMANUF", "Polaroid"), ("CAMMODEL", "x530"), ("ISO", "100")]);
        for len in 0..file.len() {
            if let Ok(d) = open_bytes(&file[..len]) {
                let _ = decode(d.as_ref(), true);
            }
        }
        let mut rng = Lcg(7);
        for _ in 0..300 {
            let mut f = file.clone();
            for _ in 0..3 {
                let at = rng.below(f.len() as u64) as usize;
                f[at] = rng.next() as u8;
            }
            if let Ok(d) = open_bytes(&f) {
                let _ = decode(d.as_ref(), false);
            }
        }
    }

    #[test]
    fn rgb_thumbnail_and_jpeg_previews() {
        let rgb = image_section(2, 3, 2, 2, 6, &[7; 12]);
        let mut jpeg_body = vec![0xFF, 0xD8, 0xFF, 0xD9];
        jpeg_body.resize(40, 0);
        let jpeg = image_section(2, 0x12, 10, 10, 0, &jpeg_body);
        let mut payload: Vec<u8> = (0..1024u16).flat_map(|v| v.to_le_bytes()).collect();
        payload.extend_from_slice(&[0; 16]);
        let raw = image_section(3, 5, 4, 1, 16, &payload);
        let file = x3f_file(0x0002_0001, &[raw, rgb, jpeg]);
        let d = open_bytes(&file).unwrap();
        let previews = &d.metadata().previews;
        assert_eq!(previews.len(), 2);
        let thumb = previews.iter().find(|p| p.encoding == PreviewEncoding::Rgb8).unwrap();
        assert_eq!((thumb.width, thumb.height, thumb.length), (2, 2, 12));
        assert_eq!(&file[thumb.offset as usize..][..12], &[7; 12]);
        assert!(previews.iter().any(|p| p.encoding == PreviewEncoding::Jpeg));
        // A stride that is not 3 x width rejects the thumbnail.
        let skewed = image_section(2, 3, 2, 2, 8, &[7; 12]);
        let file = x3f_file(0x0002_0001, &[image_section(3, 5, 4, 1, 16, &payload), skewed]);
        assert!(open_bytes(&file).unwrap().metadata().previews.is_empty());
    }

    #[test]
    fn header_rotation_and_preset_name() {
        let mut file = polaroid_file(&[("CAMMANUF", "SIGMA")]);
        file[36..40].copy_from_slice(&90u32.to_le_bytes());
        assert_eq!(open_bytes(&file).unwrap().frame_info().orientation, Orientation::Rotate90);
        file[36..40].copy_from_slice(&45u32.to_le_bytes());
        assert_eq!(open_bytes(&file).unwrap().frame_info().orientation, Orientation::Normal);
        // The Quattro generation keeps the rotation elsewhere.
        file[4..8].copy_from_slice(&0x0004_0001u32.to_le_bytes());
        file[36..40].copy_from_slice(&90u32.to_le_bytes());
        file[48..52].copy_from_slice(&180u32.to_le_bytes());
        assert_eq!(open_bytes(&file).unwrap().frame_info().orientation, Orientation::Rotate180);

        let mut named = file.clone();
        named[40..72].fill(0);
        named[40..46].copy_from_slice(b"Shade\0");
        let header = |data: &[u8], version| {
            Container { data, version, images: vec![], properties: None, calibration: vec![] }.header_preset_name()
        };
        assert_eq!(header(&named, 0x0002_0003).as_deref(), Some("Shade"));
        // Only graphic ASCII characters qualify, and only the second and third generation have the field.
        named[44] = b' ';
        named[45] = b'x';
        assert_eq!(header(&named, 0x0002_0003), None);
        assert_eq!(header(&named, 0x0002_0000), None);
    }

    // -----------------------------------------------------------------------------------------
    // Calibration block
    // -----------------------------------------------------------------------------------------

    fn blank_container(version: u32) -> Container<'static> {
        Container { data: &[], version, images: vec![], properties: None, calibration: vec![] }
    }

    fn base_info(w: u32, h: u32) -> FrameInfo {
        let mut info = FrameInfo::new(w, h, 12);
        info.cpp = 3;
        info.black_level = BlackLevel::uniform(31.0);
        info.white_level = WhiteLevel::uniform(3900);
        info.active_area = Rect::new(12, 0, w - 20, h - 40);
        info
    }

    const IDENTITY: [f32; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

    fn classic_block() -> Block {
        Block::default()
            .property("WhiteBalanceIlluminants", &[("Auto", "AutoIll"), ("Shade", "ShadeIll")])
            .matrix("AutoIll", &[9], &IDENTITY)
            .matrix("ShadeIll", &[9], &[2.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.5])
    }

    #[test]
    fn white_balance_and_matrix_from_the_block() {
        let run = |wb_desc: &str, swap: bool| {
            let mut camf = camf::Camf::parse(classic_block().0);
            let mut meta = RawMetadata::default();
            meta.set_extra("white_balance", wb_desc);
            let mut info = base_info(400, 300);
            calibrate(&mut camf, &blank_container(0x0002_0001), &mut meta, &mut info, PLAIN, swap);
            (meta, info)
        };
        let (m, i) = run("Shade", false);
        assert_eq!(m.extra("white_balance"), Some("Shade"));
        assert_eq!(i.color_matrices.source, ColorSource::File);
        let matrix = i.color_matrices.color[0].values.clone();
        assert_eq!((matrix[0], matrix[4], matrix[8]), (0.5, 1.0, 2.0));
        let wb = i.white_balance.unwrap();
        // The neutral is D65 white through the illuminant matrix (2, 1, 0.5): gains are its reciprocals.
        assert!((wb.coeffs[0] - 2.0 / 0.95047).abs() < 1e-3 && (wb.coeffs[2] - 0.5 / 1.08883).abs() < 1e-3);
        // An unknown preset falls back to Auto.
        let (m, _) = run("Bulb", false);
        assert_eq!(m.extra("white_balance"), Some("Auto"));
        // The Polaroid stores red and blue the other way round: matrix rows and gains are exchanged.
        let (_, s) = run("Shade", true);
        let swapped = &s.color_matrices.color[0].values;
        assert_eq!(&swapped[0..3], &matrix[6..9]);
        assert_eq!(&swapped[6..9], &matrix[0..3]);
        let sw = s.white_balance.unwrap();
        assert_eq!((sw.coeffs[0], sw.coeffs[2]), (wb.coeffs[2], wb.coeffs[0]));
    }

    #[test]
    fn a_corrupt_block_leaves_the_database_colour_data() {
        let mut bytes = classic_block().0;
        bytes[8] = 9; // an unknown flag byte of the first entry
        let mut camf = camf::Camf::parse(bytes);
        let mut meta = RawMetadata::default();
        let mut info = base_info(400, 300);
        info.color_matrices.source = ColorSource::CameraDb;
        calibrate(&mut camf, &blank_container(0x0002_0001), &mut meta, &mut info, PLAIN, false);
        assert!(info.white_balance.is_none());
        assert_eq!(info.color_matrices.source, ColorSource::CameraDb);
        assert_eq!(meta.extra("white_balance"), None);
    }

    #[test]
    fn white_level_from_the_block() {
        let white = |block: Block, coding: Coding| stated_white(&mut camf::Camf::parse(block.0), coding);
        let depth = |d: f32| Block::default().matrix("ImageDepth", &[1], &[d]);
        assert_eq!(white(depth(12.0), PLAIN), Some(4095));
        assert_eq!(white(depth(14.0), QUATTRO), Some(16383));
        for bad in [0.0, 7.0, 12.5, 17.0, f32::NAN] {
            assert_eq!(white(depth(bad), PLAIN), None, "{bad}");
        }
        // Dark-subtracted samples: the smallest MaxOutputLevel - DarkLevel of the layers.
        let levels = |dark: [f32; 3], max: [f32; 3]| {
            Block::default().matrix("DarkLevel", &[3], &dark).matrix("MaxOutputLevel", &[3], &max)
        };
        let sd15 = || levels([479.0, 694.0, 690.0], [4057.0, 4074.0, 4074.0]);
        assert_eq!(white(sd15(), PLAIN), Some(3380));
        // The 10-bit Huffman bodies keep a pedestal: their levels are not in the samples' domain.
        assert_eq!(white(sd15(), Coding::Huffman10), None);
        assert_eq!(white(levels([1000.0; 3], [1100.0; 3]), PLAIN), None);
        assert_eq!(white(Block::default().matrix("DarkLevel", &[3], &[479.0, 694.0, 690.0]), PLAIN), None);

        // The stated white replaces the database's, before the digital-ISO scaling.
        let mut camf =
            camf::Camf::parse(depth(12.0).matrix("CaptureISO", &[1], &[200.0]).matrix("SensorISO", &[1], &[100.0]).0);
        let (mut meta, mut info) = (RawMetadata::default(), base_info(400, 300));
        calibrate(&mut camf, &blank_container(0x0002_0001), &mut meta, &mut info, PLAIN, false);
        // 31 + (4095 - 31) / 2
        assert_eq!(info.white_level.max(), 2063);
    }

    #[test]
    fn digital_iso_lowers_the_white_level() {
        let run = |capture: f32, sensor: f32| {
            let mut camf = camf::Camf::parse(
                Block::default().matrix("CaptureISO", &[1], &[capture]).matrix("SensorISO", &[1], &[sensor]).0,
            );
            let mut meta = RawMetadata::default();
            let mut info = base_info(400, 300);
            apply_digital_iso(&mut camf, &mut meta, &mut info);
            (info.white_level.max(), meta.extra("digital_iso_gain").map(String::from))
        };
        // gain 1.999: 31 + 3869 / 1.999 = 1966.47
        assert_eq!(run(199.9, 100.0), (1966, Some("1.999".to_string())));
        assert_eq!(run(200.0, 100.0), (1966, Some("2.000".to_string())));
        assert_eq!(run(400.0, 100.0), (998, Some("4.000".to_string())));
        for (c, s) in [(100.0, 100.0), (105.0, 100.0), (50.0, 100.0), (100.0, 0.0), (1e9, 1.0), (0.0, 0.0)] {
            assert_eq!(run(c, s), (3900, None), "{c} / {s}");
        }
        assert_eq!(run(106.0, 100.0), (3681, Some("1.060".to_string())));
    }

    #[test]
    fn dark_shield_rectangles_replace_the_masked_areas() {
        let block = Block::default()
            .matrix("ActiveImageArea", &[4], &[12.0, 0.0, 359.0, 259.0])
            .matrix("DarkShieldTop", &[4], &[64.0, 2.0, 300.0, 3.0])
            .matrix("DarkShieldBottom", &[4], &[64.0, 270.0, 300.0, 280.0]);
        let mut meta = RawMetadata::default();
        let mut info = base_info(400, 300);
        info.masked_areas = vec![Rect::new(0, 0, 5, 5)];
        let mut camf = camf::Camf::parse(block.0.clone());
        calibrate(&mut camf, &blank_container(0x0002_0001), &mut meta, &mut info, PLAIN, false);
        // The top strip lies inside the active area and is dropped; the bottom one stays.
        assert_eq!(info.masked_areas, vec![Rect::new(64, 270, 237, 11)]);
        assert!(info.black_from_masked);
        // Quattro frames keep the database's areas.
        let mut info = base_info(400, 300);
        info.masked_areas = vec![Rect::new(0, 0, 5, 5)];
        let mut camf = camf::Camf::parse(block.0);
        calibrate(&mut camf, &blank_container(0x0004_0001), &mut meta, &mut info, QUATTRO, false);
        assert_eq!(info.masked_areas, vec![Rect::new(0, 0, 5, 5)]);
    }

    #[test]
    fn lens_shading_is_an_opcode_list() {
        let gains: Vec<f32> = (0..18).map(|i| 1.0 + i as f32 / 100.0).collect();
        let block = classic_block().matrix("SpatialGain", &[2, 3, 3], &gains);
        let mut camf = camf::Camf::parse(block.0);
        let mut meta = RawMetadata::default();
        let mut info = base_info(400, 300);
        calibrate(&mut camf, &blank_container(0x0002_0001), &mut meta, &mut info, PLAIN, false);
        assert_eq!(meta.extra("lens_shading"), Some("CAMF"));
        let list = &info.opcodes.as_ref().unwrap().lists[1];
        let ops = flashraw_codecs::dng_opcodes::parse(list).unwrap();
        assert_eq!(ops.len(), 3);
        for (plane, op) in ops.iter().enumerate() {
            let flashraw_codecs::dng_opcodes::Opcode::GainMap {
                region,
                points_v,
                points_h,
                spacing_v,
                spacing_h,
                origin_v,
                origin_h,
                map_planes,
                gains,
            } = op
            else {
                panic!("not a GainMap: {op:?}")
            };
            assert_eq!((region.top, region.left, region.bottom, region.right), (0, 0, 260, 380));
            assert_eq!((region.plane, region.planes, region.row_pitch, region.col_pitch), (plane as u32, 1, 1, 1));
            assert_eq!((*points_v, *points_h, *map_planes, gains.len()), (2, 3, 1, 6));
            assert_eq!(*spacing_v, 299.0 / 1.0 / 260.0);
            assert_eq!(*spacing_h, 399.0 / 2.0 / 380.0);
            assert_eq!((*origin_v, *origin_h), (-0.0, -12.0 / 380.0));
        }
        // Quattro frames get none.
        let mut camf = camf::Camf::parse(classic_block().matrix("SpatialGain", &[2, 3, 3], &gains).0);
        let mut info = base_info(400, 300);
        calibrate(&mut camf, &blank_container(0x0004_0001), &mut meta, &mut info, QUATTRO, false);
        assert!(info.opcodes.is_none());
    }

    // -----------------------------------------------------------------------------------------
    // TRUE-coded planes, assembly
    // -----------------------------------------------------------------------------------------

    #[test]
    fn dense_and_half_assembly() {
        let (w, h) = (6, 4);
        let shape = |cols, rows, kept| PlaneShape { cols, rows, kept };
        let plane = |n: usize, base: u16| (0..n).map(|i| base + i as u16).collect::<Vec<u16>>();
        // Dense: layer k of pixel (x, y) is plane k at (y, x).
        let planes = vec![plane(24, 100), plane(24, 200), plane(24, 300)];
        let shapes = [shape(6, 4, 6); 3];
        let mut out = vec![0u16; 72];
        interleave_dense(&mut out, w, h, &planes, &shapes, false);
        assert_eq!(&out[(2 * 6 + 3) * 3..][..3], &[115, 215, 315]);
        // Half: planes 0 and 1 are 3x2, the top plane is 8 columns wide of which 6 are used.
        let planes = vec![plane(6, 100), plane(6, 200), plane(32, 300)];
        let shapes = [shape(3, 2, 3), shape(3, 2, 3), shape(8, 4, 8)];
        let mut out = vec![0u16; 72];
        interleave_half(&mut out, w, h, &planes, &shapes, true);
        for y in 0..4 {
            for x in 0..6 {
                let px = &out[(y * 6 + x) * 3..][..3];
                assert_eq!(px[2], 300 + (y * 8 + x) as u16);
                if y % 2 == 0 && x % 2 == 0 {
                    assert_eq!((px[0], px[1]), (100 + (y / 2 * 3 + x / 2) as u16, 200 + (y / 2 * 3 + x / 2) as u16));
                } else {
                    assert_eq!((px[0], px[1]), (0, 0));
                }
            }
        }
    }

    #[test]
    fn quattro_plane_geometry_is_checked() {
        let job = |w: u32, h: u32| Job {
            data: &[],
            image: Image {
                section: Section { offset: 0, size: 0 },
                kind: ImageKind::new(1, 0x23),
                columns: w,
                rows: h,
                row_stride: 0,
            },
            coding: Coding::True { quattro: true },
            swap_rb: false,
            black_for_af: 0,
            active: Rect::default(),
        };
        let header = |planes: [(u32, u32); 3]| TrueHeader {
            planes: Some(planes),
            seeds: [0; 3],
            table: vec![],
            plane_sizes: [0; 3],
            data_offset: 0,
        };
        let p = Profiler::default();
        let ctx = DecodeContext::new(&p);
        let j = job(5888, 3672);
        let half = j.plane_shapes(&header([(2944, 1836), (2944, 1836), (6272, 3672)]), true, &ctx).unwrap();
        assert_eq!(half.1, Assembly::Half);
        assert_eq!([half.0[0].kept, half.0[1].kept, half.0[2].kept], [2944, 2944, 6272]);
        let full = j.plane_shapes(&header([(5888, 3672), (5900, 3672), (5888, 3672)]), true, &ctx).unwrap();
        assert_eq!((full.1, full.0[1].kept), (Assembly::Dense, 5888));
        for bad in [
            [(2944, 1836), (2944, 1835), (6272, 3672)],
            [(2944, 1000), (2944, 1000), (6272, 3672)],
            [(5888, 3672), (5888, 3000), (5888, 3672)],
            [(5000, 3672), (5888, 3672), (5888, 3672)],
        ] {
            assert_eq!(j.plane_shapes(&header(bad), true, &ctx).err().unwrap().kind(), "CorruptFile", "{bad:?}");
        }
        // The limits apply to each plane.
        let mut tight = DecodeContext::new(&p);
        tight.limits = Limits::default().with_max_samples(1000);
        let e = j.plane_shapes(&header([(2944, 1836), (2944, 1836), (6272, 3672)]), true, &tight).err().unwrap();
        assert_eq!(e.kind(), "LimitExceeded");
    }

    // -----------------------------------------------------------------------------------------
    // Synthetic difference-coded files
    // -----------------------------------------------------------------------------------------

    /// Most-significant-bit-first bit writer.
    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        used: u32,
    }

    impl BitWriter {
        fn put(&mut self, value: u32, count: u32) {
            for i in (0..count).rev() {
                if self.used == 0 {
                    self.bytes.push(0);
                }
                if (value >> i) & 1 != 0 {
                    *self.bytes.last_mut().unwrap() |= 0x80 >> self.used;
                }
                self.used = (self.used + 1) & 7;
            }
        }
    }

    /// One plane in the TRUE coding with the fixed 4-bit code table of [`true_payload`]: each
    /// sample is predicted from the one two columns back (the first two of a row from the row
    /// two above), the code gives the bit length of the difference followed by its bits.
    fn encode_plane(values: &[u16], cols: usize, rows: usize, seed: u16) -> Vec<u8> {
        let mut out = BitWriter::default();
        let mut start = [[i32::from(seed); 2]; 2];
        for r in 0..rows {
            let mut prev = [0i32; 2];
            for x in 0..cols {
                let v = i32::from(values[r * cols + x]);
                let pred = if x < 2 { start[r & 1][x & 1] } else { prev[x & 1] };
                let d = v - pred;
                let len = if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() };
                assert!(len <= 15);
                out.put(len, 4);
                if len > 0 {
                    out.put(if d > 0 { d as u32 } else { (d + (1 << len) - 1) as u32 }, len);
                }
                if x < 2 {
                    start[r & 1][x & 1] = v;
                }
                prev[x & 1] = v;
            }
        }
        out.bytes
    }

    /// The payload of a TRUE image section: optional Quattro plane sizes, seeds, the code table,
    /// the plane byte counts and the planes (padded to 16 bytes). `dims` are the planes' coded sizes.
    fn true_payload(planes: Option<[(u32, u32); 3]>, dims: [(u32, u32); 3], values: &[Vec<u16>; 3]) -> Vec<u8> {
        let coded: Vec<Vec<u8>> = (0..3)
            .map(|i| {
                let (c, r) = dims[i];
                encode_plane(&values[i], c as usize, r as usize, 512)
            })
            .collect();
        let mut out = Vec::new();
        if planes.is_some() {
            for (c, r) in dims {
                out.extend_from_slice(&(c as u16).to_le_bytes());
                out.extend_from_slice(&(r as u16).to_le_bytes());
            }
        }
        for _ in 0..3 {
            out.extend_from_slice(&512u16.to_le_bytes());
        }
        out.extend_from_slice(&0u16.to_le_bytes());
        for i in 0..16u8 {
            out.extend_from_slice(&[4, i << 4]);
        }
        out.extend_from_slice(&[0, 0]);
        if planes.is_some() {
            out.extend_from_slice(&0u32.to_le_bytes());
        }
        for c in &coded {
            out.extend_from_slice(&(c.len() as u32).to_le_bytes());
        }
        for c in &coded {
            out.extend_from_slice(c);
            out.resize(out.len() + (16 - c.len() % 16) % 16, 0);
        }
        out
    }

    /// Smooth content with dark patches, so that autofocus repairs meet values near black.
    fn plane_content(cols: usize, rows: usize, black: u32, rng: &mut Lcg) -> Vec<u16> {
        let mut v = Vec::with_capacity(cols * rows);
        for y in 0..rows {
            for x in 0..cols {
                let dark = ((x / 48) * 7 + (y / 48) * 3) % 9 == 0;
                let val = if dark {
                    black.saturating_sub(8) + (rng.next() % 28) as u32
                } else {
                    black + ((x * 5 + y * 3) % 7000) as u32 + (rng.next() % 90) as u32
                };
                v.push(val.min(16383) as u16);
            }
        }
        v
    }

    fn quattro_file(kind: u32, w: u32, h: u32, planes: [(u32, u32); 3], values: &[Vec<u16>; 3]) -> Vec<u8> {
        let mut file =
            x3f_file(0x0004_0001, &[image_section(1, kind, w, h, 0, &true_payload(Some(planes), planes, values))]);
        // The model name sits in the header area of the real files.
        file[64..64 + 17].copy_from_slice(b"SIGMA dp0 Quattro");
        file
    }

    fn content(planes: [(u32, u32); 3], black: u32, seed: u64) -> [Vec<u16>; 3] {
        let mut rng = Lcg(seed);
        planes.map(|(c, r)| plane_content(c as usize, r as usize, black, &mut rng))
    }

    #[test]
    fn quattro_half_layout_assembles_and_replicates() {
        // A size without autofocus sites: assembly and replication only.
        let (w, h) = (64usize, 48usize);
        let planes = [(32, 24), (32, 24), (64, 48)];
        let values = content(planes, 2047, 1);
        let file = quattro_file(0x23, w as u32, h as u32, planes, &values);
        let d = open_bytes(&file).unwrap();
        // Only the Quattro raw widths get a model name from the header text.
        assert_eq!((d.metadata().model.as_str(), d.metadata().extra("x3f_raw_type")), ("", Some("1/23")));
        for (recover, parallel) in [(false, false), (true, true)] {
            let p = Profiler::default();
            let mut ctx = DecodeContext::new(&p);
            (ctx.recover, ctx.parallel) = (recover, parallel);
            let img = d.decode(&ctx).unwrap();
            let got = img.u16();
            let (bw, bh) = (w / 2, h / 2);
            for y in 0..h {
                for x in 0..w {
                    let (bx, by) = (x / 2, y / 2);
                    let replicated = bx < bw - 4 && (2..bh - 2).contains(&by);
                    for k in 0..3 {
                        let want = if k == 2 {
                            values[2][y * w + x]
                        } else if replicated || (x % 2 == 0 && y % 2 == 0) {
                            values[k][by * bw + bx]
                        } else {
                            0 // the border blocks keep zeros at their odd sites
                        };
                        assert_eq!(got[(y * w + x) * 3 + k], want, "({x},{y},{k})");
                    }
                }
            }
        }
    }

    #[test]
    fn quattro_full_layout_discards_extra_columns() {
        let (w, h) = (40usize, 30usize);
        let planes = [(56, 30); 3];
        let values = content(planes, 2047, 2);
        let file = quattro_file(0x25, w as u32, h as u32, planes, &values);
        let img = decode(open_bytes(&file).unwrap().as_ref(), false).unwrap();
        for y in 0..h {
            for x in 0..w {
                for k in 0..3 {
                    assert_eq!(img.u16()[(y * w + x) * 3 + k], values[k][y * 56 + x], "({x},{y},{k})");
                }
            }
        }
    }

    #[test]
    fn plane_geometry_errors_are_corrupt_file() {
        let (w, h) = (64u32, 48u32);
        for planes in [
            [(32, 24), (32, 23), (64, 48)], // middle plane of a different height
            [(32, 20), (32, 20), (64, 48)], // neither half nor full height
            [(32, 24), (28, 24), (64, 48)], // a plane narrower than the stored width
        ] {
            let values = content(planes, 2047, 3);
            let file = quattro_file(0x23, w, h, planes, &values);
            let d = open_bytes(&file).unwrap();
            assert_eq!(decode(d.as_ref(), false).err().unwrap().kind(), "CorruptFile", "{planes:?}");
        }
        // A plane wider than the stored width is fine: the extra columns are discarded.
        let planes = [(30, 24), (32, 24), (64, 48)];
        let values = content(planes, 2047, 3);
        let file = quattro_file(0x23, w, h, planes, &values);
        assert!(decode(open_bytes(&file).unwrap().as_ref(), false).is_ok());
        // Cut-off plane data fails in every policy.
        let planes = [(32, 24), (32, 24), (64, 48)];
        let values = content(planes, 2047, 3);
        let mut file = quattro_file(0x23, w, h, planes, &values);
        let dir = u32::from_le_bytes(file[file.len() - 4..].try_into().unwrap()) as usize;
        let size_at = dir + 12 + 4;
        let size = u32::from_le_bytes(file[size_at..size_at + 4].try_into().unwrap());
        file[size_at..size_at + 4].copy_from_slice(&(size - 64).to_le_bytes());
        let d = open_bytes(&file).unwrap();
        for recover in [false, true] {
            assert_eq!(decode(d.as_ref(), recover).err().unwrap().kind(), "CorruptFile");
        }
    }

    #[test]
    fn plain_true_planes_are_interleaved() {
        let (w, h) = (20usize, 10usize);
        let values = content([(20, 10); 3], 300, 4);
        let payload = true_payload(None, [(20, 10); 3], &values);
        let file = x3f_file(0x0002_0003, &[image_section(3, 0x1e, w as u32, h as u32, 0, &payload)]);
        let d = open_bytes(&file).unwrap();
        assert_eq!(d.metadata().extra("x3f_raw_type"), Some("3/1e"));
        let img = decode(d.as_ref(), false).unwrap();
        for i in 0..w * h {
            assert_eq!(&img.u16()[i * 3..][..3], &[values[0][i], values[1][i], values[2][i]]);
        }
    }

    /// A size with autofocus sites, end to end: the decoded frame equals the assembled planes
    /// run through the repair and the replication (which the unit tests of `quattro` pin).
    #[test]
    fn quattro_with_autofocus_sites_is_repaired_after_assembly() {
        let (w, h) = (2752usize, 1840usize);
        let planes = [(1376, 920), (1376, 920), (2752, 1840)];
        let values = content(planes, 2047, 5);
        let file = quattro_file(0x27, w as u32, h as u32, planes, &values);
        let d = open_bytes(&file).unwrap();
        let info = d.frame_info();
        let black = info.black_level.values[0].max(0.0) as u32;
        let img = decode(d.as_ref(), false).unwrap();
        let mut want = vec![0u16; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                want[(y * w + x) * 3 + 2] = values[2][y * w + x];
                if x % 2 == 0 && y % 2 == 0 {
                    want[(y * w + x) * 3] = values[0][(y / 2) * 1376 + x / 2];
                    want[(y * w + x) * 3 + 1] = values[1][(y / 2) * 1376 + x / 2];
                }
            }
        }
        quattro::repair_af_sites(&mut want, w, h, info.active_area, black);
        quattro::replicate_half_layers(&mut want, w, h, false);
        assert!(img.u16() == &want[..], "decoded frame differs from the reference assembly");
    }

    // -----------------------------------------------------------------------------------------
    // Real files (need the corpus; skipped without it)
    // -----------------------------------------------------------------------------------------

    /// Fingerprints (format, metadata, frame description) of the 18 real files, pinned when the
    /// reader matched the previous one on all of them.
    const GOLDEN_FINGERPRINTS: [(&str, &str); 18] = [
        ("Polaroid/Polaroid x530/RAW_POLAROID_X530.X3F", "6187f2d2d1aaa615"),
        ("Sigma/DP0 Quattro/_SDI0263.X3F", "f00dfe9861c3c8c0"),
        ("Sigma/DP1 Merrill/SDIM0555.X3F", "5b3a0ae0034417de"),
        ("Sigma/DP1/RAW_SIGMA_DP1.X3F", "9875a99d5770b722"),
        ("Sigma/DP3 Merrill/_P3M9383.X3F", "b1230fe9d4e1cb11"),
        ("Sigma/SIGMA DP2 Merrill/DP2M1726.X3F", "08308bc907285c78"),
        ("Sigma/SIGMA DP2 Merrill/DP2M5265.X3F", "a2c4e66cdcf4c899"),
        ("Sigma/SIGMA DP2 Merrill/SDIM0076.X3F", "718967ca5e3a8c65"),
        ("Sigma/SIGMA DP2 Merrill/SDIM1223.X3F", "7f371345d42ac9ac"),
        ("Sigma/SIGMA SD1 Merrill/SDIM0042.X3F", "1e5e0298b41db042"),
        ("Sigma/SIGMA SD14/20120818-SDIM4579.X3F", "0986f2f2cc9003ae"),
        ("Sigma/SIGMA SD15/_SDI5651.X3F", "1bed16828c003736"),
        ("Sigma/SIGMA dp2 Quattro/SDIM4388.X3F", "934954cb879e63b5"),
        ("Sigma/Sigma DP1s/RAW_SIGMA_DP1S.X3F", "4e03744ee46f541d"),
        ("Sigma/Sigma SD10/RAW_SIGMA_SD10.X3F", "be63a95dd6fb1d11"),
        ("Sigma/Sigma SD9/RAW_SIGMA_SD9_SRGB.X3F", "82bab5f2cac6d768"),
        ("Sigma/sd Quattro H/SDIM0061.X3F", "7b64b7a2931e720a"),
        ("Sigma/sd Quattro/sample3.X3F", "2e33406cf77dff92"),
    ];

    /// The 18 X3F files of the corpus (all of them).
    const REAL_FILES: [&str; 18] = [
        "Polaroid/Polaroid x530/RAW_POLAROID_X530.X3F",
        "Sigma/DP0 Quattro/_SDI0263.X3F",
        "Sigma/DP1 Merrill/SDIM0555.X3F",
        "Sigma/DP1/RAW_SIGMA_DP1.X3F",
        "Sigma/DP3 Merrill/_P3M9383.X3F",
        "Sigma/SIGMA DP2 Merrill/DP2M1726.X3F",
        "Sigma/SIGMA DP2 Merrill/DP2M5265.X3F",
        "Sigma/SIGMA DP2 Merrill/SDIM0076.X3F",
        "Sigma/SIGMA DP2 Merrill/SDIM1223.X3F",
        "Sigma/SIGMA SD1 Merrill/SDIM0042.X3F",
        "Sigma/SIGMA SD14/20120818-SDIM4579.X3F",
        "Sigma/SIGMA SD15/_SDI5651.X3F",
        "Sigma/SIGMA dp2 Quattro/SDIM4388.X3F",
        "Sigma/Sigma DP1s/RAW_SIGMA_DP1S.X3F",
        "Sigma/Sigma SD10/RAW_SIGMA_SD10.X3F",
        "Sigma/Sigma SD9/RAW_SIGMA_SD9_SRGB.X3F",
        "Sigma/sd Quattro H/SDIM0061.X3F",
        "Sigma/sd Quattro/sample3.X3F",
    ];

    /// FNV-1a of the little-endian bytes of the decoded buffer; the same values as the regression
    /// rows (`tests/regression/sigma.tsv`). The Quattro files exercise the autofocus repair and the
    /// replication, the others the three codings and the Polaroid layer swap.
    const GOLDEN_BUFFERS: [(&str, &str); 18] = [
        ("Polaroid/Polaroid x530/RAW_POLAROID_X530.X3F", "51b06135f72bb249"),
        ("Sigma/DP0 Quattro/_SDI0263.X3F", "5cdfe3dd19ee9337"),
        ("Sigma/DP1 Merrill/SDIM0555.X3F", "a98afe008d83f5ef"),
        ("Sigma/DP1/RAW_SIGMA_DP1.X3F", "251cf6bd9ad0b920"),
        ("Sigma/DP3 Merrill/_P3M9383.X3F", "8e9a8aed58c589b1"),
        ("Sigma/SIGMA DP2 Merrill/DP2M1726.X3F", "baba53315ee5b615"),
        ("Sigma/SIGMA DP2 Merrill/DP2M5265.X3F", "84fbad166abf2def"),
        ("Sigma/SIGMA DP2 Merrill/SDIM0076.X3F", "3f9ac0649f05ed2e"),
        ("Sigma/SIGMA DP2 Merrill/SDIM1223.X3F", "7032a5ab4ce57bd5"),
        ("Sigma/SIGMA SD1 Merrill/SDIM0042.X3F", "f7ad9b7138fdcbdc"),
        ("Sigma/SIGMA SD14/20120818-SDIM4579.X3F", "40b4e7c15d16c1c0"),
        ("Sigma/SIGMA SD15/_SDI5651.X3F", "82f0163c605af6ea"),
        ("Sigma/SIGMA dp2 Quattro/SDIM4388.X3F", "8dd868b56a29e52b"),
        ("Sigma/Sigma DP1s/RAW_SIGMA_DP1S.X3F", "9fef2cee158ce20a"),
        ("Sigma/Sigma SD10/RAW_SIGMA_SD10.X3F", "4a1f162a4e088286"),
        ("Sigma/Sigma SD9/RAW_SIGMA_SD9_SRGB.X3F", "9831b2628e677be0"),
        ("Sigma/sd Quattro H/SDIM0061.X3F", "855fb8bca03dd159"),
        ("Sigma/sd Quattro/sample3.X3F", "fa1fb657002bd3e4"),
    ];

    #[test]
    #[ignore = "needs the corpus"]
    fn golden_fingerprints() {
        if std::env::var_os("FLASHRAW_PRINT_GOLDEN").is_some() {
            check_golden(&REAL_FILES.map(|f| (f, "")));
        }
        check_golden(&GOLDEN_FINGERPRINTS);
    }

    #[test]
    #[ignore = "needs the corpus"]
    fn decoded_buffers_of_real_files() {
        let print = std::env::var_os("FLASHRAW_PRINT_GOLDEN").is_some();
        let mut seen = 0;
        let all = REAL_FILES.map(|f| (f, ""));
        for (rel, expected) in if print { &all[..] } else { &GOLDEN_BUFFERS[..] } {
            let Some(data) = corpus(rel) else { continue };
            seen += 1;
            let d = open_bytes(&data).unwrap();
            let img = decode(d.as_ref(), false).unwrap();
            let bytes: Vec<u8> = img.u16().iter().flat_map(|v| v.to_le_bytes()).collect();
            let got = format!("{:016x}", fnv(&bytes));
            if print {
                println!("(\"{rel}\", \"{got}\"),");
            } else {
                assert_eq!(&got, expected, "{rel}");
            }
        }
        eprintln!("{seen} real files checked");
    }

    /// Every real file reports a white balance and a calibration matrix from its CAMF block, and
    /// optical-black areas for the bodies whose database entry has none.
    #[test]
    #[ignore = "needs the corpus"]
    fn real_files_report_calibration() {
        for rel in REAL_FILES {
            let Some(data) = corpus(rel) else { continue };
            let d = open_bytes(&data).unwrap();
            let f = d.frame_info();
            assert!(f.white_balance.is_some(), "{rel}");
            assert_eq!(f.color_matrices.source, ColorSource::File, "{rel}");
            let quattro = d.metadata().extra("x3f_raw_type").is_some_and(|t| t.starts_with("1/2"));
            assert_eq!(f.black_from_masked, !quattro, "{rel}");
        }
    }
}
