//! Kodak raws: DCR (DCS Pro / DCS 7x0, 65000 compression), DCS TIFF (8-bit with
//! linearisation curve, lossless JPEG), KDC (EasyShare / P-series 12-bit packed,
//! DC120 rotated rows, DC40/DC50 RADC) and the header-less C330/C603 dumps.
//!
//! Output is the linearised raw buffer: stored linearisation curves (TIFF tag 0x0123 /
//! DNG LinearizationTable / Kodak IFD tag 0x090D) are applied by the loaders, white
//! is the curve's value at 12 bits (or per-camera table values), and white balance
//! comes from the Kodak IFD (tag 0x8290) as-shot multipliers. The YCbCr compact
//! cameras (C330/C603 "YRGB") decode to 3-sample RGB (there is no fourth channel to carry).

use crate::common::*;
use crate::medium::{cfa_at_origin, even_margins, fallback_table, matrix_from};
use flashraw_codecs::kodak as kc;
use flashraw_core::tiff::{Entry, Ifd, Tiff, tags};
use flashraw_core::*;
use std::sync::Arc;

/// Magic-signature probe: the C603 "KodakCommonInfo" dump header.
pub fn probe(data: &[u8]) -> bool {
    data.starts_with(b"KodakCommonInfo")
}

/// Header-less dumps are identified by size (see [`open_headerless`]).
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    open_headerless(data, opts).unwrap_or_else(|| Err(Error::UnsupportedFormat("unknown Kodak dump size".into())))
}

/// Short Kodak model name ("KODAK EasyShare Z981 Digital Camera" -> "EasyShare Z981").
fn kodak_model(raw: &str) -> String {
    let mut m = raw.trim().to_string();
    let lower = m.to_ascii_lowercase();
    if let Some(i) = lower.find(" digital camera") {
        m.truncate(i);
    }
    if let Some(i) = m.find("FILE VERSION") {
        m.truncate(i);
    }
    let mut m = m.trim().to_string();
    for p in ["eastman kodak company ", "kodak ", "digital camera ", "digital science "] {
        if m.len() > p.len() && m[..p.len()].eq_ignore_ascii_case(p) {
            m = m[p.len()..].trim().to_string();
        }
    }
    // The early DC series spell the model inside a longer name.
    for k in ["DC120", "DC50", "DC40", "DC25"] {
        if m.split_whitespace().any(|w| w == k) {
            return k.to_string();
        }
    }
    m
}

/// Normalised Kodak model (drives table lookups): drop "EasyShare"/"ZOOM".
fn normalized_model(model: &str) -> String {
    model
        .split_whitespace()
        .filter(|w| !w.eq_ignore_ascii_case("EasyShare") && !w.eq_ignore_ascii_case("ZOOM"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Values from the Kodak private IFD (tag 0x8290 / 0xFE00).
#[derive(Debug, Default)]
struct KodakIfd {
    wb: Option<[f32; 4]>,
    linear: Option<Vec<u16>>,
    width: Option<u32>,
    height: Option<u32>,
}

fn parse_kodak_ifd(data: &[u8], tiff: &Tiff) -> KodakIfd {
    let mut k = KodakIfd::default();
    let Some(e) = tiff.find_entry(0x8290).or_else(|| tiff.find_entry(0xFE00)) else { return k };
    let Some(off) = e.u32(0) else { return k };
    let opts = flashraw_core::tiff::TiffOptions { follow_sub_ifds: false, ..Default::default() };
    let Ok(ifd) = flashraw_core::tiff::parse_ifd_at(data, tiff.order, off, opts) else { return k };
    // White-balance presets; `wbi` selects the as-shot one (tags are seen in order).
    const KDC_PRESETS: [u16; 7] = [0xfa25, 0xfa28, 0xfa27, 0xfa29, 0, 0, 0xfa2a];
    const DCR_PRESETS: [u16; 6] = [0x0848, 0x0849, 0x084a, 0x084b, 0x084c, 0x084d];
    let mut wbi: Option<u16> = None; // tag of the selected preset
    let mut entries: Vec<&Entry> = ifd.entries.iter().collect();
    entries.sort_by_key(|e| e.tag);
    let mut software_wb = false;
    for e in entries {
        match e.tag {
            0x03fc => {
                if let Some(i) = e.i32(0) {
                    wbi = usize::try_from(i).ok().and_then(|i| DCR_PRESETS.get(i).copied());
                }
            }
            0x03fd if e.count == 72 => {
                let b = e.bytes();
                let g = |i: usize| b.u16_at(tiff.order, 40 + 2 * i).map(|v| 2048.0 / (v.max(1) as f32)).ok();
                if let (Some(r), Some(gg), Some(bl)) = (g(0), g(1), g(2)) {
                    k.wb = Some([r, gg, bl, gg]);
                    software_wb = true;
                    wbi = None;
                }
            }
            0x0848..=0x084d if !software_wb => {
                let is_auto = e.tag == 0x084d;
                if is_auto && wbi.is_none() && !software_wb {
                    wbi = Some(0x084d);
                }
                if wbi == Some(e.tag) {
                    let v: Vec<f32> = (0..3).map(|i| e.f32(i).filter(|&x| x > 0.001).unwrap_or(1.0)).collect();
                    let m2 = v[1] * v[1];
                    k.wb = Some([(m2 / v[0]).trunc(), v[1].trunc(), (m2 / v[2]).trunc(), v[1].trunc()]);
                }
            }
            0x090d => {
                let v: Vec<u16> = e.u32s().into_iter().map(|x| x as u16).take(0x10000).collect();
                if !v.is_empty() {
                    k.linear = Some(v);
                }
            }
            0xfa0d => {
                if let Some(i) = e.bytes().first() {
                    wbi = KDC_PRESETS.get(*i as usize).copied().filter(|&t| t != 0);
                }
            }
            0xfa13 => k.width = e.u32(0),
            0xfa14 => k.height = e.u32(0).map(|h| (h + 1) & !1),
            0xfa25 | 0xfa27 | 0xfa28 | 0xfa29 | 0xfa2a if !software_wb && wbi == Some(e.tag) => {
                let v = e.u32s();
                if v.len() >= 3 {
                    k.wb = Some([v[0] as f32, v[1] as f32, v[2] as f32, v[1] as f32]);
                }
            }
            _ => {}
        }
    }
    k
}

/// Linearisation table: the stored curve extended to 16 bits, and its white value.
fn linear_curve(v: &[u16]) -> (Vec<u16>, u32) {
    let mut c = v.to_vec();
    let last = *c.last().unwrap_or(&0);
    c.resize(0x10000, last);
    let white = c[if v.len() < 0x1000 { 0xfff } else { v.len() - 1 }] as u32;
    (c, white)
}

#[derive(Clone)]
enum Kind {
    /// 65000-compressed CFA.
    K65000 {
        offset: usize,
    },
    /// 65000-compressed RGB / YCbCr colour images (photometric 2 / 6).
    K65000Rgb {
        offset: usize,
    },
    K65000Ycbcr {
        offset: usize,
        bits: u32,
    },
    /// Lossless JPEG CFA (DCS 5x0/6x0).
    Ljpeg {
        offset: usize,
        len: usize,
    },
    /// 8-bit samples through the curve.
    Eight {
        offset: usize,
        stride: usize,
    },
    /// 12-bit MSB packed (KDC).
    Packed12 {
        offset: usize,
    },
    /// Little-endian 16-bit samples with `bits` significant bits.
    Raw16 {
        offset: usize,
    },
    Dc120 {
        offset: usize,
    },
    Radc {
        offset: usize,
        cbpp: u32,
    },
    C330 {
        offset: usize,
        skip32: bool,
    },
    C603 {
        offset: usize,
    },
}

struct Plan<'a> {
    data: &'a [u8],
    kind: Kind,
    /// Buffer dimensions.
    width: u32,
    height: u32,
    /// Curve applied to decoded values (identity when empty).
    curve: Arc<Vec<u16>>,
}

impl Plan<'_> {
    fn cpp(&self) -> u32 {
        match self.kind {
            Kind::K65000Rgb { .. } | Kind::K65000Ycbcr { .. } | Kind::C330 { .. } | Kind::C603 { .. } => 3,
            _ => 1,
        }
    }

    fn decode(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let (w, h) = (self.width as usize, self.height as usize);
        let cpp = self.cpp();
        let mut img = RawImage::new_u16(self.width, self.height, cpp, &ctx.limits)?;
        let out = img.u16_mut();
        let data = self.data;
        let id: Vec<u16>;
        let curve: &[u16] = if self.curve.is_empty() {
            id = (0..=0xffffu32).map(|v| v as u16).collect();
            &id
        } else {
            &self.curve
        };
        let unit = DataUnit::Payload;
        match self.kind {
            Kind::K65000 { offset } => {
                let rows = ctx.profiler.time(Stage::ContainerParse, || kc::scan_65000_rows(data, offset, w, h))?;
                let errs = crate::medium::ErrorSlot::default();
                ctx.profiler.time(Stage::EntropyDecode, || {
                    par::for_each_row_chunk(out, w, 16, ctx.parallel, |first, chunk| {
                        for (i, row) in chunk.chunks_mut(w).enumerate() {
                            if let Err(e) = kc::decode_65000_row(data, rows[first + i], row, curve) {
                                errs.set(e);
                                return;
                            }
                        }
                    })
                });
                errs.into_result()?;
            }
            Kind::K65000Rgb { offset } => ctx
                .profiler
                .time(Stage::EntropyDecode, || decode_65000_color(data, offset, w, h, curve, out, false, 12))?,
            Kind::K65000Ycbcr { offset, bits } => ctx
                .profiler
                .time(Stage::EntropyDecode, || decode_65000_color(data, offset, w, h, curve, out, true, bits))?,
            Kind::Ljpeg { offset, len } => ctx.profiler.time(Stage::EntropyDecode, || {
                let out = std::cell::RefCell::new(&mut *out);
                let decode = |src: &[u8]| -> Result<()> {
                    let mut out = out.borrow_mut();
                    flashraw_codecs::ljpeg::decode_into_par(src, Default::default(), &mut out, w, w, h, ctx.parallel)
                        .map(|_| ())
                };
                let samples = (w * h) as u64;
                compressed_unit(data, ctx, unit, (offset, len), samples, decode, || out.borrow_mut().fill(0))?;
                if !self.curve.is_empty() {
                    apply_curve(&mut out.borrow_mut(), curve, ctx.parallel);
                }
                Ok::<(), Error>(())
            })?,
            Kind::Eight { offset, stride } => ctx.profiler.time(Stage::Unpack, || {
                let src = ctx.sensor_rows(data, unit, offset, RowGeometry::packed(h, w, 8, stride))?;
                // Samples missing from truncated data (recover) stay zero.
                par::for_each_row_chunk(out, w, 64, ctx.parallel, |first, chunk| {
                    for (i, row) in chunk.chunks_mut(w).enumerate() {
                        let Some(s) = src.get((first + i) * stride..) else { break };
                        for (o, &b) in row.iter_mut().zip(s) {
                            *o = curve[b as usize];
                        }
                    }
                });
                Ok::<(), Error>(())
            })?,
            Kind::Packed12 { offset } => ctx.profiler.time(Stage::Unpack, || {
                let src = ctx.sensor_rows(data, unit, offset, RowGeometry::tight(h, w, 12))?;
                flashraw_codecs::packed::unpack(
                    src,
                    flashraw_codecs::PackedLayout::tight(w, h, 12, PackOrder::MsbBytes),
                    out,
                    w,
                    ctx.parallel,
                    ctx.simd,
                    ctx.recover,
                )
            })?,
            Kind::Raw16 { offset } => ctx.profiler.time(Stage::Unpack, || {
                crate::medium::decode_u16_rows(ctx, data, unit, offset, ByteOrder::Little, w, w * 2, out)
            })?,
            Kind::Dc120 { offset } => ctx.profiler.time(Stage::Unpack, || {
                // Rows of 848 bytes, usable only whole: missing rows stay zero (recover).
                let src = ctx.sensor_bytes(data, unit, offset, 848 * h)?;
                for (row, o) in out.chunks_mut(w).enumerate() {
                    let Some(s) = src.bytes.get(row * 848..row * 848 + 848) else { break };
                    kc::dc120_row(s, row, o);
                }
                if let Some(t) = src.truncation {
                    ctx.recovered(CorruptReason::Truncated(t), ((h - src.bytes.len() / 848) * w) as u64);
                }
                Ok::<(), Error>(())
            })?,
            Kind::Radc { offset, cbpp } => ctx.profiler.time(Stage::EntropyDecode, || {
                let src = data.get(offset..).ok_or_else(|| Error::corrupt("RADC: offset beyond file"))?;
                kc::decode_radc(src, w, h, cbpp, out)
            })?,
            Kind::C330 { offset, skip32 } => ctx.profiler.time(Stage::Unpack, || {
                // Rows of 2 * raw_width bytes (raw_width = width + 16); the 24-bit
                // variant inserts 32 extra rows after every 32 rows.
                let rw = w + 16;
                let row_pos = |row: usize| row * rw * 2 + if skip32 { row / 32 * rw * 32 } else { 0 };
                // Rows are usable only whole: missing rows stay zero (recover).
                let src = ctx.sensor_bytes(data, unit, offset, row_pos(h - 1) + rw * 2)?;
                let mut lost = 0;
                for (row, o) in out.chunks_mut(w * 3).enumerate() {
                    let Some(px) = src.bytes.get(row_pos(row)..row_pos(row) + rw * 2) else {
                        lost += w * 3;
                        continue;
                    };
                    kc::c330_row(px, w, curve, o);
                }
                if let Some(t) = src.truncation {
                    ctx.recovered(CorruptReason::Truncated(t), lost as u64);
                }
                Ok::<(), Error>(())
            })?,
            Kind::C603 { offset } => ctx.profiler.time(Stage::Unpack, || {
                // One record of `w * 3` bytes per row pair, usable only whole: missing
                // pairs stay zero (recover).
                let src = ctx.sensor_bytes(data, unit, offset, h.div_ceil(2) * w * 3)?;
                par::for_each_row_chunk(out, w * 3, 2, ctx.parallel, |first, chunk| {
                    let rec = (first / 2) * w * 3;
                    if let Some(px) = src.bytes.get(rec..rec + w * 3) {
                        for (i, o) in chunk.chunks_mut(w * 3).enumerate() {
                            kc::c603_row(px, w, (first + i) & 1 == 1, curve, o);
                        }
                    }
                });
                if let Some(t) = src.truncation {
                    let rows = (src.bytes.len() / (w * 3) * 2).min(h);
                    ctx.recovered(CorruptReason::Truncated(t), ((h - rows) * w * 3) as u64);
                }
                Ok::<(), Error>(())
            })?,
        }
        Ok(img)
    }
}

/// 65000-compressed colour images (DCS Pro RGB / YCbCr variants), as 3-sample RGB.
#[allow(clippy::too_many_arguments)]
fn decode_65000_color(
    data: &[u8],
    mut pos: usize,
    w: usize,
    h: usize,
    curve: &[u16],
    out: &mut [u16],
    ycbcr: bool,
    bits: u32,
) -> Result<()> {
    let mut buf = [0i16; 776];
    if !ycbcr {
        for row in 0..h {
            let mut col = 0;
            while col < w {
                let len = 256.min(w - col);
                let raw = kc::decode_block_65000(data, pos, len * 3, &mut buf)?;
                pos += kc::block_len_65000(data, pos, len * 3)
                    .ok_or_else(|| Error::corrupt("Kodak 65000: truncated data"))?
                    .0;
                let mut rgb = [0i32; 3];
                for i in 0..len {
                    for c in 0..3 {
                        let d = buf[i * 3 + c] as i32;
                        let v = if raw { d } else { rgb[c] + d };
                        rgb[c] = v;
                        out[((row * w) + col + i) * 3 + c] = v as u16;
                    }
                }
                col += 256;
            }
        }
        return Ok(());
    }
    for row in (0..h).step_by(2) {
        let mut col = 0;
        while col < w {
            let len = 128.min(w - col);
            kc::decode_block_65000(data, pos, len * 3, &mut buf)?;
            pos +=
                kc::block_len_65000(data, pos, len * 3).ok_or_else(|| Error::corrupt("Kodak 65000: truncated data"))?.0;
            let mut y = [[0i32; 2]; 2];
            let (mut cb, mut cr) = (0i32, 0i32);
            let mut bp = 0usize;
            for i in (0..len).step_by(2) {
                cb += buf[bp + 4] as i32;
                cr += buf[bp + 5] as i32;
                let g = -((cb + cr + 2) >> 2);
                let rgb = [g + cr, g, g + cb];
                for j in 0..2 {
                    for k in 0..2 {
                        y[j][k] = y[j][k ^ 1] + buf[bp] as i32;
                        bp += 1;
                        let _ = bits;
                        let (r, c2) = (row + j, col + i + k);
                        if r < h && c2 < w {
                            for c in 0..3 {
                                out[(r * w + c2) * 3 + c] = curve[(y[j][k] + rgb[c]).clamp(0, 0xfff) as usize];
                            }
                        }
                    }
                }
                bp += 2;
            }
            col += 128;
        }
    }
    Ok(())
}

fn find_raw<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    tiff.all_ifds()
        .into_iter()
        .filter(|i| i.width() > 0 && i.height() > 0 && i.u32(tags::NEW_SUBFILE_TYPE).unwrap_or(0) & 1 == 0)
        .filter(|i| {
            i.u32(tags::PHOTOMETRIC) == Some(32803)
                || i.compression() == 65000
                || i.bps() > 8
                || (i.bps() == 8
                    && i.u32(tags::PHOTOMETRIC) == Some(1)
                    && i.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1) == 1)
        })
        .max_by_key(|i| i.width() as u64 * i.height() as u64)
}

/// Open a Kodak TIFF-based raw (DCR, KDC, DCS TIFF).
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("Kodak TIFF without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let model = kodak_model(&meta.model);
    let norm = normalized_model(&model);
    // The database is keyed on the EXIF strings; report the short model names otherwise.
    let cam = db_lookup(&mut meta, "");
    meta.make = "Kodak".into();
    meta.model = model.clone();
    if cam.is_none() {
        meta.canonical_model = norm.clone();
    }
    let kifd = parse_kodak_ifd(data, &tiff);
    let raw = find_raw(&tiff);
    let p1 = tiff.find_entry(0xfd04);

    // Curve: raw-IFD LinearizationTable (tag 0x0123 / 0xC618), Kodak IFD 0x090D wins.
    let mut lin: Option<Vec<u16>> = None;
    for i in tiff.all_ifds() {
        if let Some(e) = i.get(0x0123).or_else(|| i.get(tags::LINEARIZATION_TABLE)) {
            let v: Vec<u16> = e.u32s().into_iter().map(|x| x as u16).take(0x10000).collect();
            if !v.is_empty() {
                lin = Some(v);
            }
        }
    }
    if let Some(v) = &kifd.linear {
        lin = Some(v.clone());
    }
    let (curve, lin_white) = match &lin {
        Some(v) => {
            let (c, w) = linear_curve(v);
            (c, Some(w))
        }
        None => (Vec::new(), None),
    };

    let mut white: Option<u32> = None; // set by loaders that define it
    let mut bits = 12u8;
    let (kind, mut w, mut h) = if model == "DC120" {
        let r = raw.ok_or_else(|| Error::invalid_metadata("DC120: no raw image"))?;
        if r.compression() == 7 {
            return Err(Error::unsupported_compression("KDC", "DC120 JPEG-compressed raw"));
        }
        white = Some(0xff);
        bits = 8;
        (Kind::Dc120 { offset: first_strip(r)? }, 848, 976)
    } else if model == "DC50" || model == "DC40" {
        let r = raw.ok_or_else(|| Error::invalid_metadata("RADC: no raw image"))?;
        let cbpp = r.get(0x9102).or_else(|| tiff.find_entry(0x9102)).and_then(|e| e.bytes().u32_at(tiff.order, 0).ok());
        white = Some(0x3fff);
        bits = 14;
        let offset = if model == "DC40" { 1152 } else { 19712 };
        (Kind::Radc { offset, cbpp: cbpp.unwrap_or(0) }, 768, 512)
    } else if let Some(e) = p1.filter(|e| e.count >= 13) {
        // KDC P-series / EasyShare: two partial offsets in tag 0xFD04.
        let b = e.bytes();
        let a = b.u32_at(tiff.order, 16).unwrap_or(0) as usize;
        let c = b.u32_at(tiff.order, 48).unwrap_or(0) as usize;
        let mut offset = a.saturating_add(c);
        if model.len() >= 9 && model[..9].eq_ignore_ascii_case("EasyShare") {
            offset = if offset < 0x15000 { 0x15000 } else { 0x17000 };
        }
        let (w, h) = (kifd.width.unwrap_or(0), kifd.height.unwrap_or(0));
        (Kind::Packed12 { offset }, w, h)
    } else {
        let r = raw.ok_or_else(|| no_raw_ifd(&tiff, "Kodak", Error::invalid_metadata("Kodak: no raw image")))?;
        let (w, h) = (r.width(), r.height());
        let offset = first_strip(r)?;
        let kind = match (r.compression(), r.u32(tags::PHOTOMETRIC).unwrap_or(32803), r.bps()) {
            (65000, 2, _) => Kind::K65000Rgb { offset },
            (65000, 6, _) => Kind::K65000Ycbcr { offset, bits: 10 },
            (65000, _, _) => Kind::K65000 { offset },
            (7 | 99, _, _) => {
                let len = strips(r).first().map(|s| s.1).unwrap_or(0).max(data.len().saturating_sub(offset));
                Kind::Ljpeg { offset, len }
            }
            (1, _, 8) => {
                bits = 8;
                white = Some(curve.get(0xff).copied().unwrap_or(0xff) as u32);
                Kind::Eight { offset, stride: w as usize }
            }
            (1, _, 12) => Kind::Packed12 { offset },
            (1, _, 16) => {
                bits = 16;
                Kind::Raw16 { offset }
            }
            (262, _, _) => return Err(Error::unsupported_compression("Kodak", "262 (DCS Pro 14 legacy)")),
            (c, p, b) => {
                return Err(Error::unsupported_compression(
                    "Kodak",
                    format!("compression {c}, photometric {p}, {b} bits"),
                ));
            }
        };
        (kind, w, h)
    };
    if w == 0 || h == 0 {
        return Err(Error::invalid_metadata("Kodak: missing raw dimensions"));
    }
    opts.limits.check_dims(w, h, 3)?;
    let colour = matches!(kind, Kind::K65000Rgb { .. } | Kind::K65000Ycbcr { .. });
    if colour {
        // Colour images are decoded without margins.
        w = w.max(1);
        h = h.max(1);
    }

    let table = fallback_table("Kodak", &norm);
    let mut info = FrameInfo::new(w, h, bits);
    info.compression = match kind {
        Kind::K65000 { .. } => Compression::Kodak65000,
        Kind::K65000Rgb { .. } => Compression::KodakRgb,
        Kind::K65000Ycbcr { .. } => Compression::KodakYcbcr,
        Kind::Ljpeg { .. } => Compression::LosslessJpeg,
        Kind::Dc120 { .. } => Compression::KodakDc120,
        Kind::Radc { .. } => Compression::Other,
        _ => Compression::Uncompressed,
    };
    // White: loader-defined, else per-camera table, else curve, else bit depth.
    let white = white
        .or(table.map(|t| t.white).filter(|&v| v > 0).map(|v| v as u32))
        .or(lin_white)
        .unwrap_or((1u32 << bits.min(16)) - 1);
    info.white_level = WhiteLevel::uniform(white.min(0xffff));
    let mut black = 0f32;
    if model == "DCS660M" {
        black = 214.0;
    }
    if let Some(t) = table {
        if t.black > 0 || (t.black < 0 && black == 0.0) {
            black = t.black.unsigned_abs() as f32;
        }
    }
    info.black_level = BlackLevel::uniform(black);
    // Geometry: a 2-pixel border on each side for the NC2000 / EOS DCS / DCS4xx.
    let vis = if model.starts_with("NC2000") || model.starts_with("EOSDCS") || model.starts_with("DCS4") {
        Rect::new(2, 0, w.saturating_sub(4), h)
    } else {
        Rect::full(w, h)
    };
    info.active_area = even_margins(vis);
    info.crop = info.active_area;
    if colour {
        info.cpp = 3;
        info.cfa = None;
        info.layout = SensorLayout::Linear;
    } else {
        // CFA: TIFF-EP pattern of the raw IFD, else the camera database, else GRBG.
        let db_cfa = cam.and_then(|c| c.cfa).and_then(parse_db_cfa);
        let p = raw.and_then(tiff_ep_cfa).filter(|_| !matches!(kind, Kind::Packed12 { .. })).or(db_cfa);
        let p = p.unwrap_or_else(|| cfa_at_origin(CfaPattern::GRBG, vis.x, vis.y));
        info.cfa = Some(p);
        info.layout = layout_for(&p);
    }
    let exif = raw
        .and_then(|r| r.u32(tags::ORIENTATION))
        .or(ifd0.u32(tags::ORIENTATION))
        .and_then(Orientation::from_exif_checked);
    if let Some(m) = kifd.wb {
        info.white_balance = WhiteBalance::from_multipliers(m[0], m[1], m[2], m[3]);
    }
    if matches!(kind, Kind::Radc { .. }) {
        // The RADC curve already balances the channels.
        info.white_balance = WhiteBalance::from_multipliers(1.0, 1.0, 1.0, 1.0);
    }
    info.color_matrices = table.map(matrix_from).unwrap_or_default();
    if model == "DC120" {
        info.pixel_aspect = (976.0 / 0.75 / 848.0) as f32;
    }
    apply_db(
        &mut info,
        &meta,
        cam,
        Known { cfa: true, crop: true, active_area: true, black: true, white: true, color: true, masked: true },
    );
    // EasyShare bodies write their orientation sensor into the EXIF block of the
    // embedded full-size JPEG (the largest preview) rather than the KDC's own IFDs.
    let mut previews = tiff_previews(data, 0, &tiff, Orientation::Normal);
    let camera_jpeg = previews
        .iter()
        .filter(|p| p.encoding == PreviewEncoding::Jpeg)
        .max_by_key(|p| p.width as u64 * p.height as u64)
        .and_then(|p| data.get(p.offset as usize..(p.offset + p.length) as usize))
        .and_then(crate::orientation::jpeg_exif_orientation);
    let policy = crate::orientation::policy_for(&meta, cam);
    info.orientation = crate::orientation::apply(&mut meta, exif, camera_jpeg, None, policy);
    crate::orientation::resolve_previews(data, &mut previews, info.orientation, info.crop.width, info.crop.height);
    meta.previews = previews;
    meta.set_extra("kodak_model", norm);
    let format = match plan_kind_format(&kind) {
        Some(f) => f,
        None if tiff.find_entry(0x8290).is_some() => RawFormat::Dcs,
        None => RawFormat::Dcs,
    };
    let plan = Plan { data, kind, width: w, height: h, curve: Arc::new(curve) };
    let dec = SimpleDecoder::new(format, meta, info, Box::new(move |ctx| plan.decode(ctx)));
    Ok(Box::new(dec))
}

/// Container flavour implied by the payload kind (KDC: compact/P-series, DCR: 65000).
fn plan_kind_format(k: &Kind) -> Option<RawFormat> {
    match k {
        Kind::Packed12 { .. } | Kind::Dc120 { .. } | Kind::Radc { .. } => Some(RawFormat::Kdc),
        Kind::K65000 { .. } | Kind::K65000Rgb { .. } | Kind::K65000Ycbcr { .. } => Some(RawFormat::Dcr),
        _ => None,
    }
}

fn first_strip(r: &Ifd) -> Result<usize> {
    strips(r).first().map(|s| s.0).ok_or_else(|| Error::invalid_metadata("Kodak: raw image without strips"))
}

/// Header-less Kodak dumps identified by file size (C330, C603, "12MP", KAI-0340).
pub fn open_headerless<'a>(data: &'a [u8], opts: &OpenOptions) -> Option<Result<Box<dyn RawDecoder + 'a>>> {
    // (size, raw w, raw h, left, top, right, bottom, load flags, CFA code, model, offset)
    type Dump = (usize, u32, u32, u32, u32, u32, u32, u32, u8, &'static str, usize);
    #[rustfmt::skip]
    const TABLE: &[Dump] = &[
        (4159302, 2338, 1779, 1, 33, 1, 2, 0, 0x94, "C330", 0),
        (4162462, 2338, 1779, 1, 33, 1, 2, 0, 0x94, "C330", 3160),
        (2247168, 1232, 912, 0, 0, 16, 0, 0, 0x00, "C330", 0),
        (3370752, 1232, 912, 0, 0, 16, 0, 0, 0x00, "C330", 0),
        (6163328, 2864, 2152, 0, 0, 0, 0, 0, 0x94, "C603", 0),
        (6166488, 2864, 2152, 0, 0, 0, 0, 0, 0x94, "C603", 3160),
        (460800, 640, 480, 0, 0, 0, 0, 0, 0x00, "C603", 0),
        (9116448, 2848, 2134, 0, 0, 0, 0, 0, 0x00, "C603", 0),
        (12241200, 4040, 3030, 2, 0, 0, 13, 0, 0x49, "12MP", 0),
        (12272756, 4040, 3030, 2, 0, 0, 13, 0, 0x49, "12MP", 31556),
        (18000000, 4000, 3000, 0, 0, 0, 0, 0, 0x00, "12MP", 0),
        (614400, 640, 480, 0, 3, 0, 0, 64, 0x94, "KAI-0340", 0),
    ];
    let &(_, rw, rh, lm, tm, rm, bm, lf, cf, model, offset) = TABLE.iter().find(|t| t.0 == data.len())?;
    Some(open_dump(data, opts, rw, rh, Rect::new(lm, tm, rw - lm - rm, rh - tm - bm), lf, cf, model, offset))
}

#[allow(clippy::too_many_arguments)]
fn open_dump<'a>(
    data: &'a [u8],
    opts: &OpenOptions,
    rw: u32,
    rh: u32,
    vis: Rect,
    lf: u32,
    cf: u8,
    model: &str,
    offset: usize,
) -> Result<Box<dyn RawDecoder + 'a>> {
    opts.limits.check_dims(rw, rh, 3)?;
    let mut meta = RawMetadata::new("Kodak", model);
    let cam = db_lookup(&mut meta, "");
    let bps = ((data.len() - offset) as u64 * 8 / (rw as u64 * rh as u64)) as u32;
    let cfa = match cf {
        0x94 => Some(CfaPattern::RGGB),
        0x49 => Some(CfaPattern::GBRG),
        0x16 => Some(CfaPattern::BGGR),
        0x61 => Some(CfaPattern::GRBG),
        _ => None,
    };
    let kodak_compact = matches!(model, "C603" | "C330" | "12MP");
    // Curve: stored after the dump header for CFA dumps with a header, else a gamma curve.
    let curve: Vec<u16> = if kodak_compact {
        if cfa.is_some() && offset > 0 {
            let at = if offset < 4096 { 168 } else { 5252 };
            let mut c: Vec<u16> = (0..256).map(|i| data.u16_at(ByteOrder::Little, at + 2 * i).unwrap_or(0)).collect();
            let last = c[255];
            c.resize(0x10000, last);
            c
        } else {
            kc::gamma_curve(0.0, 3.875, 255)
        }
    } else {
        Vec::new()
    };
    let (kind, w, h, bits, white) = match (cfa, kodak_compact) {
        (None, true) if model == "C330" => {
            let (w, h) = (vis.width, vis.height);
            (Kind::C330 { offset, skip32: bps > 16 }, w, h, 16u8, curve[0xff] as u32)
        }
        (None, true) => (Kind::C603 { offset }, vis.width, vis.height, 16u8, curve[0xff] as u32),
        (Some(_), true) => (Kind::Eight { offset, stride: rw as usize }, rw, rh, 8u8, curve[0xff] as u32),
        (_, false) => {
            // KAI-0340: 12 significant bits in little-endian 16-bit words.
            let _ = lf;
            (Kind::Raw16 { offset }, rw, rh, 12u8, 4095)
        }
    };
    let mut info = FrameInfo::new(w, h, bits);
    info.white_level = WhiteLevel::uniform(white);
    if let Some(p) = cfa {
        info.cfa = Some(cfa_at_origin(p, vis.x, vis.y));
        info.layout = SensorLayout::Bayer;
        info.active_area = even_margins(vis.clamp_to(w, h));
        info.crop = info.active_area;
    } else {
        info.cpp = 3;
        info.layout = SensorLayout::Linear;
    }
    info.compression = match kind {
        Kind::C330 { .. } | Kind::C603 { .. } => Compression::KodakYcbcr,
        _ => Compression::Uncompressed,
    };
    apply_db(
        &mut info,
        &meta,
        cam,
        Known { cfa: true, crop: true, active_area: true, black: true, white: true, color: false, masked: true },
    );
    let plan = Plan { data, kind, width: w, height: h, curve: Arc::new(curve) };
    Ok(Box::new(SimpleDecoder::new(RawFormat::Other, meta, info, Box::new(move |ctx| plan.decode(ctx)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names() {
        assert_eq!(kodak_model("KODAK EasyShare Z981 Digital Camera    "), "EasyShare Z981");
        assert_eq!(kodak_model("Kodak DC120 ZOOM Digital Camera"), "DC120");
        assert_eq!(kodak_model("Kodak Digital Science DC50 Zoom Camera"), "DC50");
        assert_eq!(kodak_model("DCS460D         FILE VERSION 3  "), "DCS460D");
        assert_eq!(kodak_model("KODAK P880 ZOOM DIGITAL CAMERA"), "P880 ZOOM");
        assert_eq!(normalized_model("EASYSHARE Z1015 IS"), "Z1015 IS");
        assert_eq!(normalized_model("P712 ZOOM"), "P712");
    }

    #[test]
    fn curve_white() {
        let v: Vec<u16> = (0..256).map(|i| i * 10).collect();
        let (c, w) = linear_curve(&v);
        assert_eq!(c.len(), 0x10000);
        assert_eq!(c[300], 2550);
        assert_eq!(w, 2550);
    }

    #[test]
    fn headerless_sizes_only() {
        assert!(open_headerless(&[0u8; 1000], &OpenOptions::default()).is_none());
    }
}
