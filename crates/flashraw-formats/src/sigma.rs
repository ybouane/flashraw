//! Sigma / Foveon X3F (SD9 … sd Quattro H, DP1 … dp3 Quattro, Polaroid x530).
//!
//! ## Container
//! `FOVb` header (version 2.x/3.x: identifier, rotation, white-balance name, …;
//! version 4.x for Quattro bodies), a directory (`SECd`) at the offset stored in the
//! last four bytes, and sections: `SECp` (UTF-16 property list: make, model, ISO,
//! exposure, lens), `SECi` (images: raw, JPEG/RGB/Huffman thumbnails) and `SECc`
//! (CAMF calibration blob; not needed for the decoded buffer and not parsed).
//!
//! ## Raw image types
//! | type/format | cameras | coding |
//! |---|---|---|
//! | 3/5, 3/6 | Polaroid x530, SD9, SD10, SD14 | 10-bit Huffman (or uncompressed words) |
//! | 3/1e | DP1, DP1s, DP2, SD15 ("TRUE I/II") | TRUE |
//! | 1/1e | Merrill bodies | TRUE |
//! | 1/23, 1/25, 1/27, 1/29 | dp Quattro, sd Quattro (H) | TRUE with per-plane geometry |
//!
//! ## Output representation
//! The buffer is `raw_width x raw_height x 3` (top/middle/bottom layer samples per
//! pixel, [`SensorLayout::Foveon`]):
//! - Older/Merrill bodies: the three full-resolution layers interleaved.
//! - Quattro ("1:1:4") bodies store the bottom and middle layers at half resolution
//!   and the top layer at full resolution. The top layer fills channel 2
//!   at full resolution and the lower layers are placed on the even 2x2 sites of
//!   channels 0/1, then (a) the phase-detect AF pixel rows are repaired by
//!   neighbour interpolation and (b) the lower layers are replicated over each 2x2
//!   block (except for a 2-sample border).
//! - Polaroid x530 stores channels in reverse order; they are swapped to R,G,B order.
//!
//! Geometry (margins), black and white levels come from the camera database (they
//! are not stored in the file in a usable form); the colour matrix comes from
//! the database's fallback colour table. X3F has no usable as-shot white balance,
//! so none is reported.

use crate::common::*;
use flashraw_codecs::x3f;
use flashraw_core::tiff::Tiff;
use flashraw_core::*;

const SEC_PROP: &[u8; 4] = b"SECp";
const SEC_IMAGE: &[u8; 4] = b"SECi";

const RAW_X530: u32 = 0x0003_0005;
const RAW_HUFFMAN_10BIT: u32 = 0x0003_0006;
const RAW_TRUE: u32 = 0x0003_001e;
const RAW_MERRILL: u32 = 0x0001_001e;
const RAW_QUATTRO: u32 = 0x0001_0023;
const RAW_SDQ: u32 = 0x0001_0025;
const RAW_SDQH: u32 = 0x0001_0027;
const RAW_SDQH2: u32 = 0x0001_0029;
const THUMB_PLAIN: u32 = 0x0002_0003;
const THUMB_JPEG: u32 = 0x0002_0012;

/// Raw image types in order of preference.
const RAW_TYPES: [u32; 8] =
    [RAW_X530, RAW_HUFFMAN_10BIT, RAW_TRUE, RAW_MERRILL, RAW_QUATTRO, RAW_SDQ, RAW_SDQH, RAW_SDQH2];

const IMAGE_HEADER: usize = 28;

/// Magic-signature probe.
pub fn probe(data: &[u8]) -> bool {
    data.len() >= 16 && &data[..4] == b"FOVb"
}

/// Sigma TIFF/DNG files are DNGs (dispatched to the DNG decoder before reaching here);
/// any other Sigma TIFF is not a raw format this module knows.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    crate::misc::open_tiff(data, tiff, opts)
}

#[derive(Debug, Clone, Copy)]
struct Section {
    offset: usize,
    size: usize,
    ident: [u8; 4],
    /// `SECi`: (type << 16 | format, columns, rows, row_stride).
    image: Option<(u32, u32, u32, u32)>,
}

impl Section {
    fn bytes<'a>(&self, data: &'a [u8]) -> Result<&'a [u8]> {
        flashraw_core::bytes::slice_at(data, self.offset, self.size)
    }
}

fn le32(d: &[u8], o: usize) -> Result<u32> {
    let b = d.get(o..o.checked_add(4).ok_or_else(|| Error::corrupt("X3F offset overflow"))?);
    b.map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(|| Error::corrupt_at("truncated X3F", o as u64))
}

fn parse_directory(data: &[u8]) -> Result<Vec<Section>> {
    let dir = le32(data, data.len().saturating_sub(4))? as usize;
    if data.get(dir..dir + 4) != Some(b"SECd") {
        return Err(Error::corrupt("X3F directory signature missing"));
    }
    let n = le32(data, dir + 8)? as usize;
    if n > 50 {
        return Err(Error::corrupt(format!("X3F directory with {n} entries")));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let e = dir + 12 + i * 12;
        let offset = le32(data, e)? as usize;
        let size = le32(data, e + 4)? as usize;
        if offset.checked_add(size).is_none_or(|end| end > data.len()) {
            // Sections past the end of the file are ignored (truncated files keep
            // their remaining sections usable).
            continue;
        }
        let mut ident = [0u8; 4];
        if size >= 4 {
            ident.copy_from_slice(&data[offset..offset + 4]);
        }
        let image = if &ident == SEC_IMAGE && size >= IMAGE_HEADER {
            let t = le32(data, offset + 8)?;
            let f = le32(data, offset + 12)?;
            Some((
                (t << 16).wrapping_add(f),
                le32(data, offset + 16)?,
                le32(data, offset + 20)?,
                le32(data, offset + 24)?,
            ))
        } else {
            None
        };
        out.push(Section { offset, size, ident, image });
    }
    Ok(out)
}

/// Decode a UTF-16LE NUL-terminated string starting at code unit `idx`.
fn utf16_at(d: &[u8], idx: usize) -> String {
    let units: Vec<u16> = d
        .get(idx.saturating_mul(2)..)
        .unwrap_or(&[])
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .take(1024)
        .collect();
    String::from_utf16_lossy(&units)
}

fn parse_properties(sec: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(n) = le32(sec, 8) else { return out };
    let n = n as usize;
    let table = 24usize;
    let Some(data_start) = n.checked_mul(8).and_then(|v| v.checked_add(table)) else { return out };
    let Some(text) = sec.get(data_start..) else { return out };
    let max_units = (text.len() / 2).saturating_sub(2);
    for i in 0..n.min(4096) {
        let (Ok(no), Ok(vo)) = (le32(sec, table + i * 8), le32(sec, table + i * 8 + 4)) else { break };
        if no as usize > max_units || vo as usize > max_units {
            break;
        }
        out.push((utf16_at(text, no as usize), utf16_at(text, vo as usize)));
    }
    out
}

/// Days since 1970-01-01 → (year, month, day) (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn apply_properties(props: &[(String, String)], meta: &mut RawMetadata) {
    let num = |s: &str| s.trim().parse::<f64>().ok();
    for (k, v) in props {
        match k.as_str() {
            "CAMMANUF" => meta.make = v.trim().to_string(),
            "CAMMODEL" => meta.model = v.trim().to_string(),
            "CAMSERIAL" => meta.serial = Some(v.trim().to_string()).filter(|s| !s.is_empty()),
            "FIRMVERS" => meta.firmware = Some(v.trim().to_string()).filter(|s| !s.is_empty()),
            "ISO" => meta.exposure.iso = num(v).filter(|x| *x > 0.0).map(|x| x as u32),
            "SHUTTER" => meta.exposure.exposure_time = num(v).filter(|x| *x > 0.0),
            "APERTURE" => meta.exposure.f_number = num(v).filter(|x| *x > 0.0).map(|x| x as f32),
            "FLENGTH" => meta.exposure.focal_length = num(v).filter(|x| *x > 0.0).map(|x| x as f32),
            "FLEQ35MM" => meta.exposure.focal_length_35mm = num(v).filter(|x| *x > 0.0).map(|x| x as u32),
            "EXPCOMP" => meta.exposure.exposure_bias = num(v).map(|x| x as f32),
            "LENSMODEL" => meta.lens.model = Some(v.trim().to_string()).filter(|s| !s.is_empty()),
            "LENSFRANGE" => {
                let p: Vec<f32> = v.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                if let (Some(&a), Some(&b)) = (p.first(), p.last()) {
                    meta.lens.focal_range = Some((a.min(b), a.max(b)));
                }
            }
            "LENSARANGE" => {
                let p: Vec<f32> = v.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                if let (Some(&a), Some(&b)) = (p.first(), p.last()) {
                    meta.lens.aperture_range = Some((a.min(b), a.max(b)));
                }
            }
            "TIME" => {
                if let Ok(t) = v.trim().parse::<i64>() {
                    if t > 0 {
                        let (y, m, d) = civil_from_days(t.div_euclid(86_400));
                        let s = t.rem_euclid(86_400);
                        meta.capture_time =
                            Some(format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60));
                    }
                }
            }
            "WB_DESC" => meta.set_extra("white_balance", v.trim()),
            _ => {}
        }
    }
}

/// Quattro files carry no property list; the model is derived from the image
/// size and strings in the first 2 KiB of the file.
fn quattro_model(data: &[u8], w: u32, h: u32) -> Option<String> {
    if ![5888, 2944, 6656, 3328, 5504, 2752].contains(&w) {
        return None;
    }
    let head = &data[..data.len().min(2048)];
    if let Some(p) = flashraw_core::bytes::find(head, b"SIGMA dp") {
        let c = head.get(p + 8).copied().filter(|c| c.is_ascii_digit()).unwrap_or(b'2');
        return Some(format!("dp{} Quattro", c as char));
    }
    if let Some(p) = flashraw_core::bytes::find(head, b"sd Quatt") {
        let tail = &head[p..head.len().min(p + 20)];
        return Some(if flashraw_core::bytes::find(tail, b"sd Quattro H").is_some() {
            "sd Quattro H".into()
        } else {
            let end = head[p..].iter().position(|&b| b == 0).map(|e| p + e).unwrap_or(head.len());
            String::from_utf8_lossy(&head[p..end]).trim().to_string()
        });
    }
    Some(
        match (w, h) {
            (6656 | 3328 | 5504 | 2752, _) => "sd Quattro H",
            (_, 3776 | 1888) => "sd Quattro",
            _ => "dp2 Quattro",
        }
        .into(),
    )
}

/// Parsed raw image description.
#[derive(Clone)]
struct X3fRaw<'a> {
    section: &'a [u8],
    type_format: u32,
    width: u32,
    height: u32,
    row_stride: u32,
    swap_rb: bool,
    /// Black level used by the Quattro AF-pixel repair.
    black: u32,
    active: Rect,
}

/// Open an X3F file.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if !probe(data) {
        return Err(Error::UnsupportedFormat("not an X3F file".into()));
    }
    let version = le32(data, 4)?;
    let rotation = if version < 0x0004_0000 { le32(data, 36).unwrap_or(0) } else { le32(data, 48).unwrap_or(0) };
    let sections = parse_directory(data)?;
    let raw_sec = RAW_TYPES
        .iter()
        .find_map(|&t| sections.iter().find(|s| s.image.is_some_and(|i| i.0 == t)))
        .copied()
        .ok_or_else(|| Error::UnsupportedFormat("X3F without a supported raw image".into()))?;
    let (type_format, width, height, row_stride) = raw_sec.image.unwrap_or_default();
    if width == 0 || height == 0 || width > 65535 || height > 65535 {
        return Err(Error::corrupt(format!("X3F raw image of {width}x{height}")));
    }
    opts.limits.check_dims(width, height, 3)?;

    let mut meta = RawMetadata::default();
    if let Some(p) = sections.iter().find(|s| &s.ident == SEC_PROP) {
        apply_properties(&parse_properties(p.bytes(data)?), &mut meta);
    }
    if meta.make.is_empty() && meta.model.is_empty() {
        meta.make = "SIGMA".into();
        meta.model = quattro_model(data, width, height).unwrap_or_default();
    }
    meta.set_extra("x3f_version", format!("{}.{}", version >> 16, version & 0xFFFF));
    meta.set_extra("x3f_raw_type", format!("{:x}/{:x}", type_format >> 16, type_format & 0xFFFF));

    // Previews: JPEG and uncompressed RGB thumbnails.
    let orientation = match rotation {
        90 => Orientation::Rotate90,
        180 => Orientation::Rotate180,
        270 => Orientation::Rotate270,
        _ => Orientation::Normal,
    };
    for s in &sections {
        match s.image {
            Some((THUMB_JPEG, ..)) => {
                push_jpeg(data, s.offset + IMAGE_HEADER, s.size - IMAGE_HEADER, orientation, &mut meta.previews)
            }
            Some((THUMB_PLAIN, w, h, stride)) if w > 0 && h > 0 && stride == w * 3 => {
                let len = w as usize * h as usize * 3;
                if len <= s.size - IMAGE_HEADER {
                    meta.previews.push(PreviewInfo {
                        offset: (s.offset + IMAGE_HEADER) as u64,
                        length: len as u64,
                        width: w,
                        height: h,
                        encoding: PreviewEncoding::Rgb8,
                        orientation,
                        kind: PreviewKind::Thumbnail,
                    });
                }
            }
            _ => {}
        }
    }
    classify_previews(&mut meta.previews);

    // Geometry/levels: database entry for this body and raw size.
    let mode = format!("x3f-{width}x{height}");
    let cam = db_lookup(&mut meta, &mode).filter(|c| c.mode == mode);
    let mut info = FrameInfo::new(width, height, 12);
    info.cpp = 3;
    info.layout = SensorLayout::Foveon;
    info.compression = Compression::SigmaX3f;
    info.orientation = orientation;
    info.pixel_aspect = if height * 2 < width {
        0.5
    } else if height > width {
        2.0
    } else {
        1.0
    };
    // Defaults when no camera-database entry matches: maximum 0x3fff (or the colour
    // table's white), black from the colour table.
    let fallback = flashraw_camera_db::find_matrix(&meta.canonical_make, &meta.canonical_model);
    info.black_level = BlackLevel::uniform(fallback.map(|m| m.black).unwrap_or(0).max(0) as f32);
    info.white_level = WhiteLevel::uniform(fallback.map(|m| m.white).filter(|w| *w > 0).unwrap_or(0x3fff) as u32);
    apply_db(&mut info, &meta, cam, Known::default());
    info.crop = info.active_area;
    let black = info.black_level.values.first().copied().unwrap_or(0.0).max(0.0) as u32;

    let swap_rb = meta.make.eq_ignore_ascii_case("Polaroid") && meta.model.eq_ignore_ascii_case("x530");
    let raw = X3fRaw {
        section: raw_sec.bytes(data)?,
        type_format,
        width,
        height,
        row_stride,
        swap_rb,
        black,
        active: info.active_area,
    };
    let dec = SimpleDecoder::new(RawFormat::X3f, meta, info, Box::new(move |ctx| raw.decode(ctx)));
    Ok(Box::new(dec))
}

fn is_quattro(t: u32) -> bool {
    matches!(t, RAW_QUATTRO | RAW_SDQ | RAW_SDQH | RAW_SDQH2)
}

impl X3fRaw<'_> {
    fn decode(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let mut img = RawImage::new_u16(self.width, self.height, 3, &ctx.limits)?;
        let payload = self.section.get(IMAGE_HEADER..).unwrap_or(&[]);
        match self.type_format {
            RAW_X530 | RAW_HUFFMAN_10BIT => {
                ctx.profiler.time(Stage::EntropyDecode, || self.decode_huffman(ctx, payload, img.u16_mut()))?;
                if self.swap_rb {
                    for px in img.u16_mut().chunks_exact_mut(3) {
                        px.swap(0, 2);
                    }
                }
            }
            t => ctx.profiler.time(Stage::EntropyDecode, || self.decode_true(ctx, payload, is_quattro(t), &mut img))?,
        }
        Ok(img)
    }

    fn decode_huffman(&self, ctx: &DecodeContext, p: &[u8], out: &mut [u16]) -> Result<()> {
        let (w, h) = (self.width as usize, self.height as usize);
        let mapping: Vec<u16> = p
            .get(..2048)
            .ok_or_else(|| Error::corrupt("truncated X3F Huffman mapping"))?
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let rest = &p[2048..];
        if self.row_stride != 0 {
            return x3f::decode_simple(rest, self.row_stride as usize, &mapping, 10, w, h, out, ctx.parallel);
        }
        let table: Vec<u32> = rest
            .get(..4096)
            .ok_or_else(|| Error::corrupt("truncated X3F Huffman table"))?
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let body = &rest[4096..];
        let footer = h * 4;
        if body.len() <= footer {
            return Err(Error::corrupt("truncated X3F Huffman data"));
        }
        let (coded, offs) = body.split_at(body.len() - footer);
        let row_offsets: Vec<u32> =
            offs.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let code = x3f::huffman_code(&table, &mapping)?;
        x3f::decode_huffman(&code, coded, &row_offsets, w, h, out, ctx.parallel)
    }

    fn decode_true(&self, ctx: &DecodeContext, p: &[u8], quattro: bool, img: &mut RawImage) -> Result<()> {
        let hdr = x3f::TrueHeader::parse(p, quattro)?;
        let code = hdr.table()?;
        let data = &p[hdr.data_offset..];
        let ranges = hdr.plane_ranges(data.len())?;
        let (w, h) = (self.width as usize, self.height as usize);
        // Per-plane geometry: (decoded columns, rows, stored columns).
        let mut geom = [(w, h, w); 3];
        let mut layout_1114 = false;
        if let Some(pl) = hdr.planes {
            let (c0, r0) = (pl[0].0 as usize, pl[0].1 as usize);
            if r0 == h / 2 {
                layout_1114 = true;
                for (i, g) in geom.iter_mut().enumerate() {
                    let (c, r) = (pl[i].0 as usize, pl[i].1 as usize);
                    *g = if i < 2 { (c, r, c0) } else { (c, r, c) };
                }
            } else if r0 == h {
                for (i, g) in geom.iter_mut().enumerate() {
                    *g = (pl[i].0 as usize, pl[i].1 as usize, w);
                }
            } else {
                return Err(Error::corrupt("unexpected X3F Quattro plane geometry"));
            }
            let expect_rows = if layout_1114 { [r0, r0, pl[2].1 as usize] } else { [h; 3] };
            for (g, er) in geom.iter().zip(expect_rows) {
                if g.1 != er || g.0 < g.2 {
                    return Err(Error::corrupt("X3F Quattro plane geometry mismatch"));
                }
            }
        }
        for g in &geom {
            ctx.limits.check_dims(g.2 as u32, g.1 as u32, 1)?;
        }
        let planes: Vec<Result<Vec<u16>>> = flashraw_core::par::map((0..3).collect(), ctx.parallel, |i: usize| {
            let (cols, rows, out_cols) = geom[i];
            let mut buf = alloc_zeroed::<u16>(out_cols * rows)?;
            let (s, e) = ranges[i];
            x3f::decode_true_plane(&code, &data[s..e], hdr.seeds[i], cols, rows, out_cols, &mut buf)?;
            Ok(buf)
        });
        let mut pv = Vec::with_capacity(3);
        for p in planes {
            pv.push(p?);
        }
        let out = img.u16_mut();
        if !layout_1114 {
            let (a, b, c) = (&pv[0], &pv[1], &pv[2]);
            flashraw_core::par::for_each_row_chunk(out, w * 3, 32, ctx.parallel, |first, chunk| {
                for (i, row) in chunk.chunks_exact_mut(w * 3).enumerate() {
                    let o = (first + i) * w;
                    for (x, px) in row.chunks_exact_mut(3).enumerate() {
                        px[0] = a[o + x];
                        px[1] = b[o + x];
                        px[2] = c[o + x];
                    }
                }
            });
        } else {
            // Half-resolution bottom/middle layers on even sites, full-resolution top layer.
            let (hc, hr) = (geom[0].2, geom[0].1);
            let (tc, tr) = (geom[2].2, geom[2].1);
            let (a, b, t) = (&pv[0], &pv[1], &pv[2]);
            flashraw_core::par::for_each_row_chunk(out, w * 3, 32, ctx.parallel, |first, chunk| {
                for (i, row) in chunk.chunks_exact_mut(w * 3).enumerate() {
                    let y = first + i;
                    if y % 2 == 0 && y / 2 < hr && y / 2 < h / 2 {
                        let py = y / 2;
                        for pc in 0..hc.min(w / 2) {
                            row[pc * 6] = a[py * hc + pc];
                            row[pc * 6 + 1] = b[py * hc + pc];
                        }
                    }
                    if y < tr {
                        for x in 0..tc.min(w) {
                            row[x * 3 + 2] = t[y * tc + x];
                        }
                    }
                }
            });
        }
        if hdr.planes.is_some() {
            ctx.profiler.time(Stage::Calibration, || {
                quattro_af_repair(out, w, h, self.active, self.black);
                if layout_1114 {
                    replicate_lower_layers(out, w, h, ctx.parallel);
                }
            });
        }
        Ok(())
    }
}

/// Repair of the phase-detect AF pixels of Quattro sensors (always applied). Only the
/// bottom/middle channels of the AF sites are modified.
fn quattro_af_repair(img: &mut [u16], w: usize, h: usize, active: Rect, black: u32) {
    match (w, h) {
        (5888, 3672) => af_dp(img, w, h, active, black, 32, 8, 2),
        (2944, 1836) => af_dp(img, w, h, active, black, 16, 4, 1),
        (5888, 3776) => af_sd(img, w, h, active, (216, 464, w - 1, 3312), 16, 32, 2),
        (6656, 4480) => af_sd(img, w, h, active, (232, 592, w - 1, 3920), 16, 32, 2),
        (3328, 2240) => af_sd(img, w, h, active, (116, 296, w - 1, 2200), 8, 16, 1),
        (5504, 3680) => af_sd(img, w, h, active, (8, 192, w - 1, 3185), 16, 32, 2),
        (2752, 1840) => af_sd(img, w, h, active, (4, 96, w - 1, 1800), 8, 16, 1),
        (2944, 1888) => af_sd(img, w, h, active, (108, 232, w - 1, 1656), 8, 16, 1),
        _ => {}
    }
}

/// dp Quattro: AF sites on a regular grid; the bottom/middle values are rescaled by
/// the ratio of the top-layer response to the most similar 4-neighbour.
#[allow(clippy::too_many_arguments)]
fn af_dp(img: &mut [u16], w: usize, h: usize, a: Rect, black: u32, xstep: usize, ystep: usize, scale: usize) {
    let px = |x: usize, y: usize| (y * w + x) * 3;
    let (bk, bf, bd) = (black as i32, black as f32, black as f64);
    let ylim = (a.height + a.y) as usize;
    let xlim = (a.width + a.x) as usize;
    for y in (0..ylim).step_by(ystep) {
        if y < a.y as usize || y < scale {
            continue;
        }
        if y > h - scale || y + scale >= h {
            break;
        }
        for x in (0..xlim).step_by(xstep) {
            if x < a.x as usize || x < scale {
                continue;
            }
            if x > w - scale || x + scale >= w {
                break;
            }
            let p0 = px(x, y);
            let cands = [px(x, y - scale), px(x, y + scale), px(x - scale, y), px(x + scale, y)];
            let b0 = img[p0 + 2] as i32;
            let mut pf = cands[0];
            for &c in &cands[1..] {
                if (img[pf + 2] as i32 - b0).abs() > (img[c + 2] as i32 - b0).abs() {
                    pf = c;
                }
            }
            let (blocal, bnear) = (b0, img[pf + 2] as i32);
            if blocal < bk + 16 || bnear < bk + 16 {
                for c in 0..2 {
                    let v = (img[p0 + c] as u32).max(black);
                    img[p0 + c] = ((v - black) * 4 + black).min(16383) as u16;
                }
            } else {
                let multip = (bnear - bk) as f32 / (blocal - bk) as f32;
                for c in 0..2 {
                    let v = (img[p0 + c] as u32).max(black);
                    let mut f = img[pf + c] as f32;
                    if f < bf {
                        f = bf;
                    }
                    let near = (f - bf) * multip + bf;
                    let local = (v - black) as f64 * 3.75 + bd;
                    let r = (near as f64 + local) / 2.0;
                    img[p0 + c] = if r > 16383.0 { 16383 } else { r as u16 };
                }
            }
        }
    }
}

/// sd Quattro (H): AF sites in a window; bottom/middle replaced by the mean of the
/// eight surrounding same-phase samples, top-layer neighbours of the site averaged.
#[allow(clippy::too_many_arguments)]
fn af_sd(
    img: &mut [u16],
    w: usize,
    h: usize,
    a: Rect,
    (xstart, ystart, xend, yend): (usize, usize, usize, usize),
    xstep: usize,
    ystep: usize,
    scale: usize,
) {
    let px = |x: usize, y: usize| (y * w + x) * 3;
    let ylim = (a.height + a.y) as usize;
    let xlim = (a.width + a.x) as usize;
    let mut y = ystart;
    while y <= yend && y < ylim {
        if y < scale || y + scale >= h || y + 1 >= h {
            break;
        }
        let mut x = xstart;
        while x < xend && x < xlim {
            if x < scale || x + scale + 1 >= w {
                break;
            }
            let (mut sum_r, mut sum_g) = (0f32, 0f32);
            for xx in [-(scale as isize), 0, scale as isize] {
                let xi = (x as isize + xx) as usize;
                sum_r += img[px(xi, y - scale)] as f32;
                sum_r += img[px(xi, y + scale)] as f32;
                sum_g += img[px(xi, y - scale) + 1] as f32;
                sum_g += img[px(xi, y + scale) + 1] as f32;
                if xx != 0 {
                    sum_r += img[px(xi, y)] as f32;
                    sum_g += img[px(xi, y) + 1] as f32;
                }
            }
            let p0 = px(x, y);
            img[p0] = (sum_r / 8.0) as u16;
            img[p0 + 1] = (sum_g / 8.0) as u16;
            if scale == 2 {
                let (mut g0, mut g1, mut cnt) = (0f32, 0f32, 0f32);
                for xx in [-2isize, 0, 2] {
                    let xi = (x as isize + xx) as usize;
                    g0 += img[px(xi, y - 1) + 2] as f32;
                    g1 += img[px(xi, y + scale) + 2] as f32;
                    cnt += 1.0;
                    if xx != 0 {
                        g0 += img[px(xi, y) + 2] as f32;
                        g1 += img[px(xi, y + 1) + 2] as f32;
                        cnt += 1.0;
                    }
                }
                if cnt > 1.0 {
                    img[px(x + 1, y) + 2] = (g0 / cnt) as u16;
                    img[px(x + 1, y + 1) + 2] = (g1 / cnt) as u16;
                }
            }
            x += xstep;
        }
        y += ystep;
    }
}

/// Spread each half-resolution bottom/middle sample over its 2x2 block. Block rows
/// `2..h/2-2` and block columns `0..w/2-4` are filled.
fn replicate_lower_layers(img: &mut [u16], w: usize, h: usize, parallel: bool) {
    let (bw, bh) = (w / 2, h / 2);
    if bw < 5 || bh < 5 {
        return;
    }
    let rl = w * 3;
    // Block rows are independent: process pairs of image rows in parallel.
    flashraw_core::par::for_each_row_chunk(&mut img[..bh * 2 * rl], 2 * rl, 16, parallel, |first, chunk| {
        for (i, pair) in chunk.chunks_exact_mut(2 * rl).enumerate() {
            let by = first + i;
            if by < 2 || by >= bh - 2 {
                continue;
            }
            let (top, bottom) = pair.split_at_mut(rl);
            for bx in 0..bw - 4 {
                let s = bx * 6;
                for c in 0..2 {
                    let v = top[s + c];
                    top[s + 3 + c] = v;
                    bottom[s + c] = v;
                    bottom[s + 3 + c] = v;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn probe_and_garbage() {
        assert!(!probe(b"II*\0xxxxxxxxxxxxxxxx"));
        let mut d = b"FOVb\x02\x00\x02\x00".to_vec();
        d.resize(64, 0);
        assert!(probe(&d));
        assert!(open(&d, &OpenOptions::default()).is_err());
        // Directory pointing outside the file.
        let n = d.len();
        d[n - 4..].copy_from_slice(&1000u32.to_le_bytes());
        assert!(open(&d, &OpenOptions::default()).is_err());
    }

    fn build_x3f(sections: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut d = b"FOVb".to_vec();
        d.extend_from_slice(&0x0002_0002u32.to_le_bytes());
        d.resize(128, 0);
        let mut dir = Vec::new();
        for (kind, body) in sections {
            let off = d.len() as u32;
            d.extend_from_slice(body);
            dir.push((off, body.len() as u32, **kind));
        }
        let dir_off = d.len() as u32;
        d.extend_from_slice(b"SECd");
        d.extend_from_slice(&0x0002_0000u32.to_le_bytes());
        d.extend_from_slice(&(dir.len() as u32).to_le_bytes());
        for (o, s, k) in dir {
            d.extend_from_slice(&o.to_le_bytes());
            d.extend_from_slice(&s.to_le_bytes());
            d.extend_from_slice(&k);
        }
        d.extend_from_slice(&dir_off.to_le_bytes());
        d
    }

    fn image_header(t: u32, f: u32, w: u32, h: u32, stride: u32) -> Vec<u8> {
        let mut v = b"SECi".to_vec();
        for x in [0x0002_0000u32, t, f, w, h, stride] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v
    }

    fn prop_section(props: &[(&str, &str)]) -> Vec<u8> {
        let mut text: Vec<u16> = Vec::new();
        let mut table = Vec::new();
        for (k, v) in props {
            let ko = text.len() as u32;
            text.extend(k.encode_utf16());
            text.push(0);
            let vo = text.len() as u32;
            text.extend(v.encode_utf16());
            text.push(0);
            table.push((ko, vo));
        }
        text.extend([0, 0]);
        let mut s = b"SECp".to_vec();
        for x in [0x0002_0000u32, props.len() as u32, 0, 0, text.len() as u32] {
            s.extend_from_slice(&x.to_le_bytes());
        }
        for (a, b) in table {
            s.extend_from_slice(&a.to_le_bytes());
            s.extend_from_slice(&b.to_le_bytes());
        }
        for u in text {
            s.extend_from_slice(&u.to_le_bytes());
        }
        s
    }

    #[test]
    fn synthetic_uncompressed_file() {
        let (w, h) = (4u32, 3u32);
        let mut img = image_header(3, 5, w, h, w * 4);
        let mapping: Vec<u16> = (0..1024).map(|i| i as u16).collect();
        for m in &mapping {
            img.extend_from_slice(&m.to_le_bytes());
        }
        for i in 0..w * h {
            let v = (i % 4) | ((i % 3) << 10) | (1 << 20);
            img.extend_from_slice(&v.to_le_bytes());
        }
        let prop = prop_section(&[("CAMMANUF", "Polaroid"), ("CAMMODEL", "x530"), ("ISO", "100")]);
        let file = build_x3f(&[(b"IMAG", img), (b"PROP", prop)]);
        let dec = open(&file, &OpenOptions::default()).unwrap();
        assert_eq!(dec.metadata().make, "Polaroid");
        assert_eq!(dec.metadata().exposure.iso, Some(100));
        let fi = dec.frame_info();
        assert_eq!((fi.width, fi.height, fi.cpp, fi.layout), (w, h, 3, SensorLayout::Foveon));
        let p = Profiler::default();
        let out = dec.decode(&DecodeContext::new(&p)).unwrap();
        let d = out.u16();
        // Row 0, pixel 1: running sums R: 0+1, G: 0+1, B: 1+1; swapped R/B for the x530.
        assert_eq!(&d[3..6], &[2, 1, 1]);
        // Truncated payload must error, not panic.
        let short = build_x3f(&[(b"IMAG", image_header(3, 5, w, h, w * 4))]);
        if let Ok(dec) = open(&short, &OpenOptions::default()) {
            assert!(dec.decode(&DecodeContext::new(&p)).is_err());
        }
    }

    #[test]
    fn truncation_never_panics() {
        let (w, h) = (4u32, 3u32);
        let mut img = image_header(3, 0x1e, w, h, 0);
        for v in [512u16, 512, 512, 0] {
            img.extend_from_slice(&v.to_le_bytes());
        }
        img.extend_from_slice(&[1, 0x00, 1, 0x80, 0, 0]);
        for _ in 0..3 {
            img.extend_from_slice(&16u32.to_le_bytes());
        }
        img.extend_from_slice(&[0x55; 48]);
        let file = build_x3f(&[(b"IMAG", img)]);
        let p = Profiler::default();
        for cut in (0..file.len()).step_by(7) {
            let mut f = file[..cut].to_vec();
            f.resize(cut.max(20), 0);
            if let Ok(d) = open(&f, &OpenOptions::default()) {
                let _ = d.decode(&DecodeContext::new(&p));
            }
        }
        let d = open(&file, &OpenOptions::default()).unwrap();
        let _ = d.decode(&DecodeContext::new(&p));
    }
}
