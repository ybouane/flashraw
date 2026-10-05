//! Helpers shared by format decoders: camera-database application, TIFF payload
//! access, preview discovery, and a generic decoder implementation for formats whose
//! payload is fully described by a closure.

use flashraw_camera_db::Camera;
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;

/// Options for opening a file.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenOptions {
    pub limits: Limits,
}

/// Look up the camera database for the file's make/model and decoder mode, and fill
/// canonical names into `meta`.
pub fn db_lookup(meta: &mut RawMetadata, mode: &str) -> Option<&'static Camera> {
    let cam = flashraw_camera_db::find(&meta.make, &meta.model, mode).or_else(|| {
        // Some files pad/abbreviate the make; retry with the model alone under the canonical make.
        flashraw_camera_db::find_any(&meta.make, &meta.model)
    });
    if let Some(c) = cam {
        meta.canonical_make = c.canonical_make.to_string();
        meta.canonical_model = c.canonical_model_for(&meta.model).to_string();
    } else {
        if meta.canonical_make.is_empty() {
            meta.canonical_make = flashraw_camera_db::canonical_make(&meta.make).to_string();
            if meta.canonical_make.is_empty() {
                meta.canonical_make = meta.make.clone();
            }
        }
        if meta.canonical_model.is_empty() {
            meta.canonical_model = tidy_model(&strip_make_prefix(&meta.model, &meta.canonical_make));
        }
    }
    if !mode.is_empty() {
        meta.set_extra("mode", mode);
    }
    cam
}

/// "NIKON D850" → "D850" when the make is Nikon, etc. The make must end at a word
/// boundary: whitespace, or a hyphen/underscore followed by a letter ("LG-H815" → "H815",
/// "SAMSUNG-SM-G930A" → "SM-G930A"; "OM-1" keeps its make-like prefix, "LGE" never
/// matches "LG-...").
pub fn strip_make_prefix(model: &str, make: &str) -> String {
    let m = model.trim();
    for prefix in [make, make.split_whitespace().next().unwrap_or(make)] {
        let (Some(head), Some(rest)) = (m.get(..prefix.len()), m.get(prefix.len()..)) else { continue };
        if prefix.is_empty() || rest.is_empty() || !head.eq_ignore_ascii_case(prefix) {
            continue;
        }
        let mut chars = rest.chars();
        let boundary = match chars.next() {
            Some(c) if c.is_whitespace() => true,
            Some('-' | '_') => chars.next().is_some_and(|c| c.is_ascii_alphabetic()),
            _ => false,
        };
        if boundary {
            let rest = rest.trim_start();
            return rest.strip_prefix(['-', '_']).unwrap_or(rest).trim().to_string();
        }
    }
    m.to_string()
}

/// Model name for display when the camera database has no entry: whitespace runs
/// collapsed and a trailing firmware signature ("DCS460B         FILE VERSION 3") cut.
pub fn tidy_model(model: &str) -> String {
    let words: Vec<&str> = model.split_whitespace().collect();
    let n = words.len();
    let keep = if n >= 3
        && words[n - 3] == "FILE"
        && words[n - 2] == "VERSION"
        && words[n - 1].bytes().all(|b| b.is_ascii_digit())
    {
        n - 3
    } else {
        n
    };
    if keep == 0 { words.join(" ") } else { words[..keep].join(" ") }
}

/// Which `FrameInfo` fields the file itself already provided (the database only fills gaps).
#[derive(Debug, Clone, Copy, Default)]
pub struct Known {
    pub cfa: bool,
    pub crop: bool,
    pub active_area: bool,
    pub black: bool,
    pub white: bool,
    pub color: bool,
    pub masked: bool,
}

/// Apply camera-database knowledge to `info` for fields not marked as `known`.
pub fn apply_db(info: &mut FrameInfo, meta: &RawMetadata, cam: Option<&'static Camera>, known: Known) {
    if let Some(c) = cam {
        if !known.cfa {
            if let Some(p) = c.cfa.and_then(parse_db_cfa) {
                info.cfa = Some(p);
                info.layout = layout_for(&p);
            }
        }
        if !known.crop || !known.active_area {
            if let Some((x, y, w, h)) = c.resolve_crop(info.width, info.height) {
                let r = Rect::new(x, y, w, h);
                if !known.crop {
                    info.crop = r;
                }
                if !known.active_area {
                    info.active_area = r;
                }
            }
        }
        if !known.masked && info.masked_areas.is_empty() {
            for &[x, w] in c.black_areas_v {
                info.masked_areas.push(Rect::new(x, 0, w, info.height));
            }
            for &[y, h] in c.black_areas_h {
                info.masked_areas.push(Rect::new(0, y, info.width, h));
            }
        }
        let iso = meta.exposure.iso.unwrap_or(0);
        if let Some(s) = c.sensor(iso) {
            if !known.black {
                if s.black_colors.len() >= 4 {
                    info.black_level = BlackLevel::pattern2x2([
                        s.black_colors[0] as f32,
                        s.black_colors[1] as f32,
                        s.black_colors[2] as f32,
                        s.black_colors[3] as f32,
                    ]);
                } else if s.black >= 0 {
                    info.black_level = BlackLevel::uniform(s.black as f32);
                } else if !info.masked_areas.is_empty() {
                    info.black_from_masked = true;
                }
            }
            if !known.white && s.white > 0 {
                info.white_level = WhiteLevel::uniform(s.white as u32);
            }
        }
    }
    if !known.color && info.color_matrices.color.is_empty() {
        info.color_matrices = db_color(meta, cam);
    }
}

/// Colour matrix from the database (exact entry first, fallback colour table second).
pub fn db_color(meta: &RawMetadata, cam: Option<&'static Camera>) -> ColorMatrices {
    let m: Option<&[i32]> = cam.map(|c| c.color_matrix).filter(|m| !m.is_empty()).or_else(|| {
        flashraw_camera_db::find_matrix(&meta.canonical_make, &meta.canonical_model).map(|m| m.color_matrix)
    });
    let mut out = ColorMatrices::default();
    if let Some(m) = m {
        let rows = m.len() / 3;
        if let Some(cm) = ColorMatrix::new(Illuminant::D65, rows, m.iter().map(|&v| v as f32 / 10000.0).collect()) {
            out.color.push(cm);
            out.source = ColorSource::CameraDb;
        }
    }
    out
}

pub fn parse_db_cfa(s: &str) -> Option<CfaPattern> {
    let rows: Vec<&str> = s.split('/').collect();
    if rows.len() > 1 { CfaPattern::parse_dims(s, rows[0].len(), rows.len()) } else { CfaPattern::parse(s) }
}

pub fn layout_for(p: &CfaPattern) -> SensorLayout {
    if p.is_bayer() {
        SensorLayout::Bayer
    } else if p.is_xtrans() {
        SensorLayout::XTrans
    } else if p.width() == 4 && p.height() == 4 && {
        let c = p.colors();
        c[0] == c[1] && c[0] == c[4] && c[0] == c[5]
    } {
        SensorLayout::QuadBayer
    } else {
        SensorLayout::OtherCfa
    }
}

/// TIFF-EP / EXIF CFA pattern (0x828D/0x828E or 0xA302) from an IFD.
pub fn tiff_ep_cfa(ifd: &Ifd) -> Option<CfaPattern> {
    if let (Some(dim), Some(pat)) = (ifd.get(tags::CFA_REPEAT_PATTERN_DIM), ifd.get(tags::CFA_PATTERN_EP)) {
        let (h, w) = (dim.u32(0)? as usize, dim.u32(1)? as usize);
        let cols: Option<Vec<CfaColor>> = pat.bytes().iter().take(w * h).map(|&b| CfaColor::from_dng(b)).collect();
        return CfaPattern::new(w, h, &cols?);
    }
    if let Some(p) = ifd.get(tags::CFA_PATTERN) {
        // EXIF CFAPattern: u16 width, u16 height (byte order ambiguous), then colours.
        let b = p.bytes();
        if b.len() >= 8 {
            let (mut w, mut h) = (u16::from_le_bytes([b[0], b[1]]) as usize, u16::from_le_bytes([b[2], b[3]]) as usize);
            if w > 8 || h > 8 {
                w = u16::from_be_bytes([b[0], b[1]]) as usize;
                h = u16::from_be_bytes([b[2], b[3]]) as usize;
            }
            let cols: Option<Vec<CfaColor>> = b.get(4..4 + w * h)?.iter().map(|&c| CfaColor::from_dng(c)).collect();
            return CfaPattern::new(w, h, &cols?);
        }
    }
    None
}

/// `(offset, length)` pairs of an IFD's strips.
pub fn strips(ifd: &Ifd) -> Vec<(usize, usize)> {
    let offs = ifd.get(tags::STRIP_OFFSETS).map(|e| e.u32s()).unwrap_or_default();
    let lens = ifd.get(tags::STRIP_BYTE_COUNTS).map(|e| e.u32s()).unwrap_or_default();
    offs.iter().zip(lens.iter().chain(std::iter::repeat(&0))).map(|(&o, &l)| (o as usize, l as usize)).collect()
}

/// `(offset, length)` pairs of an IFD's tiles.
pub fn tiles(ifd: &Ifd) -> Vec<(usize, usize)> {
    let offs = ifd.get(tags::TILE_OFFSETS).map(|e| e.u32s()).unwrap_or_default();
    let lens = ifd.get(tags::TILE_BYTE_COUNTS).map(|e| e.u32s()).unwrap_or_default();
    offs.iter().zip(lens.iter().chain(std::iter::repeat(&0))).map(|(&o, &l)| (o as usize, l as usize)).collect()
}

/// An IFD's payload as one byte range (strips are contiguous in the common case): from
/// the first strip offset to the end of the last strip. Resolved leniently when the file
/// is opened; decoders check the bytes they need with [`PayloadRange::require`] or
/// [`PayloadRange::rows`], so a truncated payload fails (or is recovered) when it is
/// decoded, not when the file is parsed.
#[derive(Debug, Clone, Copy)]
pub struct PayloadRange<'a> {
    /// File offset of the first strip.
    pub offset: usize,
    /// Declared length (to the end of the last strip; to the end of the file when the
    /// strips declare no length).
    pub declared: usize,
    /// The declared range as far as the file holds it (empty when `offset` is past EOF).
    pub bytes: &'a [u8],
    /// `Strip(0)` for a single strip, [`DataUnit::Payload`] for several.
    pub unit: DataUnit,
}

impl<'a> PayloadRange<'a> {
    /// The first `needed` bytes of the payload in `data` (the whole file), under the
    /// context's corruption policy (see [`DecodeContext::sensor_bytes`]). The geometry
    /// decides what is needed: a declared length beyond it may run past the end of the
    /// file, and one short of it is read on when the file holds the bytes.
    pub fn require(&self, data: &'a [u8], ctx: &DecodeContext, needed: usize) -> Result<SensorBytes<'a>> {
        ctx.sensor_bytes(data, self.unit, self.offset, needed)
    }

    /// [`PayloadRange::require`] for rows; a recovered truncation is reported with the
    /// samples the available bytes do not cover.
    pub fn rows(&self, data: &'a [u8], ctx: &DecodeContext, geometry: RowGeometry) -> Result<&'a [u8]> {
        ctx.sensor_rows(data, self.unit, self.offset, geometry)
    }
}

/// The payload range of an IFD's strips (see [`PayloadRange`]). Fails only when the
/// IFD has no strip offsets.
pub fn contiguous_payload<'a>(data: &'a [u8], ifd: &Ifd) -> Result<PayloadRange<'a>> {
    let s = strips(ifd);
    let (first, _) = *s.first().ok_or_else(|| Error::invalid_metadata("image without strip offsets"))?;
    let end = s.iter().map(|&(o, l)| o.saturating_add(l)).max().unwrap_or(first);
    let declared = if end <= first { data.len().saturating_sub(first) } else { end - first };
    let start = first.min(data.len());
    let stop = first.saturating_add(declared).min(data.len()).max(start);
    let unit = if s.len() == 1 { DataUnit::Strip(0) } else { DataUnit::Payload };
    Ok(PayloadRange { offset: first, declared, bytes: &data[start..stop], unit })
}

/// Decode a compressed strip or tile (`(offset, length)` in `data`) under the context's
/// corruption policy. Strict: a range past the end of the file fails with a structured
/// truncation before decoding. Recover: `decode` runs on the bytes present; a unit that
/// is truncated or fails with [`Error::CorruptFile`] is zero-filled (`clear`) and
/// reported with its `samples`; a truncated unit whose stream still decodes is reported
/// with none filled.
pub fn compressed_unit(
    data: &[u8],
    ctx: &DecodeContext,
    unit: DataUnit,
    (offset, len): (usize, usize),
    samples: u64,
    decode: impl FnOnce(&[u8]) -> Result<()>,
    clear: impl FnOnce(),
) -> Result<()> {
    let src = ctx.sensor_bytes(data, unit, offset, len)?;
    let res = decode(src.bytes);
    let reason = match (res, src.truncation) {
        (Ok(()), None) => return Ok(()),
        (Ok(()), Some(t)) => {
            ctx.recovered(CorruptReason::Truncated(t), 0);
            return Ok(());
        }
        (Err(e), _) if !matches!(e, Error::CorruptFile { .. }) => return Err(e),
        (Err(_), Some(t)) => CorruptReason::Truncated(t),
        (Err(e), None) if ctx.recover => CorruptReason::InvalidUnit { unit, message: e.to_string() },
        (Err(e), None) => return Err(e),
    };
    clear();
    ctx.recovered(reason, samples);
    Ok(())
}

/// Discover embedded JPEG / RGB previews in a TIFF structure.
pub fn tiff_previews(data: &[u8], base: usize, tiff: &Tiff, orientation: Orientation) -> Vec<PreviewInfo> {
    let mut out = Vec::new();
    for ifd in tiff.all_ifds() {
        // JPEGInterchangeFormat thumbnails.
        if let (Some(o), Some(l)) =
            (ifd.u32(tags::JPEG_INTERCHANGE_FORMAT), ifd.u32(tags::JPEG_INTERCHANGE_FORMAT_LENGTH))
        {
            push_jpeg(data, base + o as usize, l as usize, orientation, &mut out);
            continue;
        }
        let comp = ifd.compression();
        let photometric = ifd.u32(tags::PHOTOMETRIC).unwrap_or(0);
        let s = strips(ifd);
        if s.len() == 1 && (comp == 6 || comp == 7 || comp == 99) && photometric != 32803 && photometric != 34892 {
            let (o, l) = s[0];
            push_jpeg(data, base + o, l, orientation, &mut out);
        } else if comp == 1
            && photometric == 2
            && ifd.bps() == 8
            && ifd.u32(tags::SAMPLES_PER_PIXEL) == Some(3)
            && s.len() == 1
        {
            let (o, l) = s[0];
            let (w, h) = (ifd.width(), ifd.height());
            let need = (w as u64) * (h as u64) * 3;
            let end = base.checked_add(o).and_then(|v| v.checked_add(l));
            if w > 0 && h > 0 && (l as u64) >= need && end.is_some_and(|e| e <= data.len()) {
                out.push(PreviewInfo::new(
                    (base + o) as u64,
                    need,
                    w,
                    h,
                    PreviewEncoding::Rgb8,
                    orientation,
                    PreviewKind::Thumbnail,
                ));
            }
        }
    }
    classify_previews(&mut out);
    out
}

/// Add a JPEG preview if the range is valid and starts with SOI.
pub fn push_jpeg(data: &[u8], offset: usize, len: usize, orientation: Orientation, out: &mut Vec<PreviewInfo>) {
    if len < 4 || offset.checked_add(len).is_none_or(|e| e > data.len()) {
        return;
    }
    let d = &data[offset..offset + len];
    if d[0] != 0xFF || d[1] != 0xD8 {
        return;
    }
    if out.iter().any(|p| p.offset == offset as u64) {
        return;
    }
    let (w, h) = flashraw_codecs::jpeg::dimensions(d).unwrap_or((0, 0));
    out.push(PreviewInfo::new(
        offset as u64,
        len as u64,
        w,
        h,
        PreviewEncoding::Jpeg,
        orientation,
        PreviewKind::Preview,
    ));
}

/// Label previews: the smallest is the thumbnail; previews >= 1/2 of the largest dimension are "full size".
pub fn classify_previews(p: &mut [PreviewInfo]) {
    if p.is_empty() {
        return;
    }
    let max = p.iter().map(|x| x.width.max(x.height)).max().unwrap_or(0);
    let min_area = p.iter().map(|x| x.width as u64 * x.height as u64).min().unwrap_or(0);
    for x in p.iter_mut() {
        x.kind = if p_len_area(x) == min_area && x.width.max(x.height) <= 640 {
            PreviewKind::Thumbnail
        } else if x.width.max(x.height) >= 1600 && x.width.max(x.height) * 2 >= max {
            PreviewKind::FullSize
        } else {
            PreviewKind::Preview
        };
    }
}

fn p_len_area(x: &PreviewInfo) -> u64 {
    x.width as u64 * x.height as u64
}

/// Fill derived metadata fields from frame info.
pub fn finish_metadata(meta: &mut RawMetadata, info: &FrameInfo, format: RawFormat) {
    meta.format = Some(format);
    meta.compression = Some(info.compression);
    meta.bits_per_sample = info.bits_per_sample;
    meta.raw_width = info.width;
    meta.raw_height = info.height;
    meta.width = info.crop.width;
    meta.height = info.crop.height;
    if info.orientation != Orientation::Normal || meta.orientation == Orientation::Normal {
        meta.orientation = info.orientation;
    }
    // Formats that resolve vendor orientation records fill `orientation_info` themselves;
    // otherwise only the container's standard field is known.
    if meta.orientation_info.resolved != meta.orientation {
        meta.orientation_info = OrientationMetadata::from_container(meta.orientation);
    }
}

type DecodeFn<'a> = dyn Fn(&DecodeContext) -> Result<RawImage> + Send + Sync + 'a;
type RegionFn<'a> = dyn Fn(&DecodeContext, Rect) -> Result<RawImage> + Send + Sync + 'a;

/// A [`RawDecoder`] built from parsed state plus a decode closure. Most formats use
/// this instead of hand-writing the trait.
pub struct SimpleDecoder<'a> {
    pub format: RawFormat,
    pub meta: RawMetadata,
    pub info: FrameInfo,
    pub decode: Box<DecodeFn<'a>>,
    pub region: Option<Box<RegionFn<'a>>>,
    pub random_access: RandomAccess,
    pub units: Vec<DecodeUnit>,
    pub packed: Option<PackedPayload<'a>>,
}

impl<'a> SimpleDecoder<'a> {
    pub fn new(format: RawFormat, mut meta: RawMetadata, mut info: FrameInfo, decode: Box<DecodeFn<'a>>) -> Self {
        info.sanitize();
        finish_metadata(&mut meta, &info, format);
        SimpleDecoder {
            format,
            meta,
            info,
            decode,
            region: None,
            random_access: RandomAccess::Sequential,
            units: Vec::new(),
            packed: None,
        }
    }
}

impl RawDecoder for SimpleDecoder<'_> {
    fn format(&self) -> RawFormat {
        self.format
    }
    fn metadata(&self) -> &RawMetadata {
        &self.meta
    }
    fn frame_info(&self) -> &FrameInfo {
        &self.info
    }
    fn decode(&self, ctx: &DecodeContext) -> Result<RawImage> {
        (self.decode)(ctx)
    }
    fn random_access(&self) -> RandomAccess {
        self.random_access
    }
    fn units(&self) -> Vec<DecodeUnit> {
        self.units.clone()
    }
    fn decode_region(&self, ctx: &DecodeContext, region: Rect) -> Result<RawImage> {
        match &self.region {
            Some(f) => f(ctx, region),
            None => self.decode(ctx)?.crop(region),
        }
    }
    fn packed_payload(&self) -> Option<PackedPayload<'_>> {
        self.packed
    }
}

/// Apply a 16-bit lookup curve in place (parallel when large).
pub fn apply_curve(img: &mut [u16], curve: &[u16], parallel: bool) {
    if curve.is_empty() {
        return;
    }
    let last = *curve.last().unwrap();
    let n = curve.len();
    flashraw_core::par::for_each_row_chunk_bounded(img, 1 << 16, par::memory_bound_workers(), parallel, |_, c| {
        for v in c.iter_mut() {
            let i = *v as usize;
            *v = if i < n { curve[i] } else { last };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_prefix() {
        assert_eq!(strip_make_prefix("NIKON D850", "Nikon"), "D850");
        assert_eq!(strip_make_prefix("Canon EOS R5", "Canon"), "EOS R5");
        assert_eq!(strip_make_prefix("ILCE-7RM5", "Sony"), "ILCE-7RM5");
        assert_eq!(strip_make_prefix("NIKON D1 ", "NIKON"), "D1");
        // Word boundaries only.
        assert_eq!(strip_make_prefix("LG-H815", "LG"), "H815");
        assert_eq!(strip_make_prefix("LG-H815", "LGE"), "LG-H815");
        assert_eq!(strip_make_prefix("SAMSUNG-SM-G930A", "samsung"), "SM-G930A");
        assert_eq!(strip_make_prefix("ASUS_I01WD", "asus"), "I01WD");
        assert_eq!(strip_make_prefix("OM-1", "OM System"), "OM-1");
        assert_eq!(strip_make_prefix("OM-1MarkII", "OM"), "OM-1MarkII");
        assert_eq!(strip_make_prefix("PENTAXK-5", "PENTAX"), "PENTAXK-5");
        assert_eq!(strip_make_prefix("Canon", "Canon"), "Canon");
        assert_eq!(strip_make_prefix("LG-", "LG"), "LG-");
        // Multi-byte text next to the prefix never splits a character.
        assert_eq!(strip_make_prefix("Ünï X", "U"), "Ünï X");
    }

    #[test]
    fn tidy_models() {
        assert_eq!(tidy_model("DCS460B         FILE VERSION 3"), "DCS460B");
        assert_eq!(tidy_model("Kodak Digital Science DC50 Zoom "), "Kodak Digital Science DC50 Zoom");
        assert_eq!(tidy_model(" CFV-50c/Flash Sync"), "CFV-50c/Flash Sync");
        assert_eq!(tidy_model("E-M10 Mark III  "), "E-M10 Mark III");
        assert_eq!(tidy_model("FILE VERSION 3"), "FILE VERSION 3");
        assert_eq!(tidy_model("X FILE VERSION 3b"), "X FILE VERSION 3b");
        assert_eq!(tidy_model(""), "");
    }

    #[test]
    fn db_cfa_strings() {
        assert_eq!(parse_db_cfa("RGGB"), Some(CfaPattern::RGGB));
        let x = parse_db_cfa("GGRGGB/GGBGGR/BRGRBG/GGBGGR/GGRGGB/RBGBRG").unwrap();
        assert!(x.is_xtrans());
        assert_eq!(layout_for(&x), SensorLayout::XTrans);
    }
}
