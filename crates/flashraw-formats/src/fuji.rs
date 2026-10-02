//! Fujifilm RAF.
//!
//! ## Container
//! A RAF file starts with a 0x94-byte big-endian header (`FUJIFILMCCD-RAW `, camera
//! model, format version) pointing at three sections:
//! - an embedded JPEG preview whose EXIF block carries Make/Model/orientation/exposure
//!   and the Fujifilm maker note (crop mode);
//! - the *meta* directory: big-endian `(tag u16, len u16, data)` records — raw size
//!   (0x100), vendor crop (0x110/0x111), image size (0x121), SuperCCD layout (0x130),
//!   X-Trans layout (0x131), white balance (0x2ff0), black levels (0x4000) and the
//!   RAFData blob (0xc000) holding the recommended image size;
//! - the *CFA* section: on cameras since ~2010 a little TIFF whose 0xF000 sub-IFD holds
//!   the raw geometry (0xF001-F003), the payload (0xF007/0xF008), black levels (0xF00A)
//!   and the as-shot white balance (0xF00E); older cameras store bare 16-bit samples.
//!
//! ## Payloads
//! - 16-bit words (X-Trans/GFX uncompressed, older Bayer and SuperCCD bodies);
//! - 12-bit packed, LSB-first (EXR bodies) or MSB-first in 32-bit LE words (HS10, S1);
//! - 14-bit packed MSB-first in 32-bit LE words (X-A3/X-A5/X-T100/XF10 family);
//! - Fujifilm lossless/lossy compression (X-Trans and Bayer), decoded in parallel
//!   vertical stripes by [`flashraw_codecs::fuji_compressed`];
//! - the tiled layout of the DBP for GX680 back and the dual-frame F700/S20Pro layout.
//!
//! bit-for-bit. SuperCCD bodies (0x130 "fuji_width") deliver the unrotated sensor
//! `docs/formats/fuji.md`).

use crate::common::*;
use flashraw_codecs::fuji_compressed::{ColorTile, FujiCompressed};
use flashraw_codecs::packed;
use flashraw_core::par::{self};
use flashraw_core::tiff::{Tiff, TiffOptions, tags};
use flashraw_core::*;
use std::sync::Arc;

const MAGIC: &[u8] = b"FUJIFILMCCD-RAW";

/// Magic-signature probe.
pub fn probe(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

pub fn open_tiff<'a>(_data: &'a [u8], _tiff: Tiff<'a>, _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    // Fujifilm TIFFs that are not RAF/DNG are not raw files we know (e.g. edited TIFFs).
    Err(Error::UnsupportedFormat("Fujifilm TIFF without RAF container".into()))
}

/// Records from the RAF meta directory.
#[derive(Debug, Default, Clone)]
struct RafMeta {
    /// 0x100: full raw size (height, width).
    raw_size: Option<(u32, u32)>,
    /// 0x110: crop top-left (top, left).
    crop_tl: Option<(u32, u32)>,
    /// 0x111: cropped size (height, width).
    crop_size: Option<(u32, u32)>,
    /// 0x121: image size (height, width).
    image_size: Option<(u32, u32)>,
    /// 0x130: (fuji_layout, fuji_width).
    layout: Option<(bool, bool)>,
    /// 0x131: X-Trans pattern (colour indices, sensor origin).
    xtrans: Option<ColorTile>,
    /// 0x2ff0: as-shot WB (R, G, B).
    wb: Option<[f32; 3]>,
    /// 0x4000: per-channel black (R, G, B, G2).
    black: Option<[f32; 4]>,
    /// RAFData generation (0 = none, 1..4, 4096) and recommended (width, height).
    raf_gen: u32,
    size_table: Option<(u32, u32)>,
}

/// Fields of the CFA-section TIFF (0xF000 sub-IFD).
#[derive(Debug, Default, Clone)]
struct CfaTiff {
    width: u32,
    height: u32,
    bps: u32,
    packed_flag: Option<u32>,
    offset: usize,
    bytes: usize,
    black: Vec<u32>,
    wb: Option<[f32; 3]>,
}

/// How the payload is stored.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Layout {
    /// 16-bit words; `stride` samples per stored row (F700 stores two frames per row).
    Words {
        order: ByteOrder,
        stride: usize,
    },
    /// DBP for GX680: 16-bit big-endian words in `tiles` vertical tiles stored one after another.
    TiledWords {
        tiles: usize,
        order: ByteOrder,
    },
    Packed {
        bits: u32,
        order: PackOrder,
    },
    Compressed,
}

struct Raf<'a> {
    payload: &'a [u8],
    width: u32,
    height: u32,
    layout: Layout,
    colors: ColorTile,
}

fn be16(d: &[u8], i: usize) -> u32 {
    d.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as u32).unwrap_or(0)
}

fn be32(d: &[u8], i: usize) -> Result<usize> {
    let b = d.get(i..i + 4).ok_or_else(|| Error::corrupt("RAF header truncated"))?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

/// Fujifilm stores WB as G,R,G,B (and sometimes a second set to be averaged).
fn grgb(v: &[u8], average: bool) -> [i32; 4] {
    let mut c = [be16(v, 0), be16(v, 2), be16(v, 4), be16(v, 6)].map(|x| x as i32);
    if average && v.len() >= 16 {
        for (i, x) in c.iter_mut().enumerate() {
            *x = (*x + be16(v, 8 + 2 * i) as i32) / 2;
        }
    }
    // (G, R, G2, B) -> (R, G, B, G2)
    [c[1], c[0], c[3], c[2]]
}

fn parse_meta(data: &[u8], off: usize, model: &str) -> RafMeta {
    let mut m = RafMeta::default();
    let Some(d) = data.get(off..) else { return m };
    let n = be32(d, 0).unwrap_or(0);
    if n > 255 {
        return m;
    }
    let wb_corr = if ["S2Pro", "S20Pro", "F700", "S5000", "S7000"].iter().any(|k| model.contains(k)) {
        Some((10.0 / 17.0 / 0.652941, 2.0 / 3.0 / (3.0 / 4.0 + 1.0 / 300.0)))
    } else if model.contains("DBP") || model.contains("DX-2000") {
        Some((0.7632653061, 0.8591549296))
    } else {
        None
    };
    let mut p = 4usize;
    let mut inset = false;
    for _ in 0..n {
        let (tag, len) = (be16(d, p), be16(d, p + 2) as usize);
        let Some(v) = d.get(p + 4..p + 4 + len) else { break };
        match tag {
            0x100 if len >= 4 => {
                m.raw_size = Some((be16(v, 0), be16(v, 2)));
                inset = true;
            }
            0x110 if inset && len >= 4 => m.crop_tl = Some((be16(v, 0), be16(v, 2))),
            0x111 if inset && len >= 4 => m.crop_size = Some((be16(v, 0), be16(v, 2))),
            0x121 if len >= 4 => {
                let w = be16(v, 2);
                m.image_size = Some((be16(v, 0), if w == 4284 { w + 3 } else { w }));
            }
            0x130 if len >= 2 => m.layout = Some((v[0] >> 7 != 0, v[1] & 8 == 0)),
            0x131 if len >= 36 => {
                let mut t = [[0u8; 6]; 6];
                for (c, &b) in v[..36].iter().enumerate() {
                    let i = 35 - c;
                    t[i / 6][i % 6] = b.min(2);
                }
                m.xtrans = Some(t);
            }
            0x2ff0 if len >= 8 => {
                let mut c = grgb(v, len == 16);
                if let Some((rc, bc)) = wb_corr {
                    c[0] = (c[0] as f64 * rc) as i32;
                    c[2] = (c[2] as f64 * bc) as i32;
                }
                m.wb = Some([c[0] as f32, c[1] as f32, c[2] as f32]);
            }
            0x4000 if len == 8 || len == 16 => {
                let c = grgb(v, true);
                m.black = Some(c.map(|x| x as f32));
            }
            0xc000 => {
                if len > 20000 {
                    // RAFData (little endian): a generation-dependent prefix, then (width, height).
                    let le16 = |i: usize| v.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as u32).unwrap_or(0);
                    let le32 =
                        |i: usize| v.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
                    let (b01, b23) = (le16(0), le16(2));
                    let wrts = le32(4) == 0x5354_5257;
                    let (gen_, wh) = if b01 != 0 && b23 == 0 && b01 < 10000 {
                        (1, 0)
                    } else if b01 > 10000 && b23 == 0 {
                        (2, 4)
                    } else if b01 == 0 {
                        if wrts { (4, 8) } else { (3, 4) }
                    } else {
                        (0, 0)
                    };
                    if gen_ != 0 {
                        m.raf_gen = gen_;
                        m.size_table = Some((le32(wh), le32(wh + 4)));
                    }
                } else if len == 4096 {
                    m.raf_gen = 4096;
                }
            }
            _ => {}
        }
        p += 4 + len;
    }
    m
}

/// Parse the TIFF in the CFA section, if present.
fn parse_cfa_tiff(sec: &[u8]) -> Option<CfaTiff> {
    let order = match sec.get(0..2)? {
        b"II" => ByteOrder::Little,
        b"MM" => ByteOrder::Big,
        _ => return None,
    };
    let opts = TiffOptions { follow_sub_ifds: false, ..Default::default() };
    let first = sec.u32_at(order, 4).ok()?;
    let ifd0 = flashraw_core::tiff::parse_ifd_at(sec, order, first, opts).ok()?;
    let sub_off = ifd0.u32(0xF000)?;
    let ifd = flashraw_core::tiff::parse_ifd_at(sec, order, sub_off, opts).ok()?;
    let g = |t: u16| ifd.u32(t);
    let mut c = CfaTiff {
        width: g(0xF001)?,
        height: g(0xF002)?,
        bps: g(0xF003).unwrap_or(12),
        packed_flag: g(0xF006),
        offset: g(0xF007)? as usize,
        bytes: g(0xF008).unwrap_or(0) as usize,
        ..Default::default()
    };
    if let Some(e) = ifd.get(0xF00A) {
        c.black = e.u32s();
    }
    if let Some(e) = ifd.get(0xF00E) {
        let v = e.u32s();
        if v.len() >= 3 {
            // Stored as G, R, B.
            c.wb = Some([v[1] as f32, v[0] as f32, v[2] as f32]);
        }
    }
    Some(c)
}

/// EXIF (and Fujifilm maker note) from the embedded JPEG.
struct Exif {
    crop_mode: u32,
    /// Byte order of the EXIF TIFF: older bodies store bare samples in this order.
    order: Option<ByteOrder>,
}

fn parse_exif(data: &[u8], jpeg_off: usize, jpeg_len: usize, meta: &mut RawMetadata) -> Exif {
    let mut ex = Exif { crop_mode: 0, order: None };
    let Some(j) = data.get(jpeg_off..jpeg_off.saturating_add(jpeg_len).min(data.len())) else { return ex };
    if j.len() < 16 || j[0] != 0xFF || j[1] != 0xD8 || &j[6..10] != b"Exif" {
        return ex;
    }
    let Ok(t) = Tiff::parse(&j[12..]) else { return ex };
    ex.order = Some(t.order);
    if let Some(ifd0) = t.ifds.first() {
        flashraw_core::exif::read_ifd0(ifd0, meta);
        if let Some(mn) = ifd0.exif.as_ref().and_then(|e| e.get(tags::MAKER_NOTE)) {
            let b = mn.bytes();
            if b.len() > 12 && b.starts_with(b"FUJIFILM") {
                let off = u32::from_le_bytes([b[8], b[9], b[10], b[11]]);
                let opts = TiffOptions { follow_sub_ifds: false, ..Default::default() };
                if let Ok(mn) = flashraw_core::tiff::parse_ifd_at(b, ByteOrder::Little, off, opts) {
                    ex.crop_mode = mn.u32(0x104d).unwrap_or(0);
                    if let Some(s) = mn.string(0x0010) {
                        meta.serial.get_or_insert(s);
                    }
                }
            }
        }
    }
    ex
}

/// any CFA override for Bayer bodies (`Some(true)` = BGGR at the visible origin).
#[allow(clippy::too_many_arguments)]
fn active_area(
    model: &str,
    raw_w: u32,
    raw_h: u32,
    m: &RafMeta,
    crop_mode: u32,
    xtrans: bool,
    packed: bool,
    fuji_layout: bool,
) -> (i64, i64, i64, i64, bool, bool) {
    let (mut width, mut height) = m.image_size.map(|(h, w)| (w as i64, h as i64)).unwrap_or((0, 0));
    if m.raf_gen == 0 && fuji_layout {
        height <<= 1;
        width >>= 1;
    }
    if height == 0 {
        height = raw_h as i64;
    }
    if width == 0 {
        width = raw_w as i64;
    }
    let (rw, rh) = (raw_w as i64, raw_h as i64);
    let inset = match (m.crop_tl, m.crop_size) {
        (Some((t, l)), Some((h, w))) if w > 0 && (w as i64) <= rw && h > 0 && (h as i64) <= rh => {
            Some((l as i64, t as i64, w as i64, h as i64))
        }
        _ => None,
    };
    let mut bggr = false;
    let mut x3e00 = false;
    if m.raf_gen == 0 && rw == 2944 {
        // S2Pro
        height = 2144;
        width = 2880;
    } else if !packed && !model.starts_with("X-") && !xtrans {
        x3e00 = true;
    }
    let modern = m.raf_gen != 0 && m.raf_gen != 4096;
    if crop_mode == 0 && modern {
        if let Some((w, h)) = m.size_table {
            width = w as i64;
            height = h as i64;
        }
    } else if crop_mode == 1 {
        width = rw;
        height = rh;
    } else if crop_mode == 4 {
        height = rh;
    }
    let mut top = if rh >= height { (rh - height) >> 2 << 1 } else { 0 };
    let mut left = if rw >= width { (rw - width) >> 2 << 1 } else { 0 };
    let use_inset = |l: &mut i64, t: &mut i64, w: &mut i64, h: &mut i64| {
        if let Some((il, it, iw, ih)) = inset {
            *l = il;
            *t = it;
            *w = iw;
            *h = ih;
        }
    };
    if modern {
        match rw {
            2944 => bggr = true,
            4096 | 5120 | 6048 | 6160 => left = 0,
            4992 => left = 4,
            7872 => {
                if crop_mode == 0 {
                    top = 6;
                    left = 0;
                    width = 7752;
                    height = 5178;
                } else {
                    use_inset(&mut left, &mut top, &mut width, &mut height);
                }
            }
            6336 => match crop_mode {
                0 => {
                    top = 6;
                    left = 0;
                    width = if model.eq_ignore_ascii_case("X-S20") { 6252 } else { 6264 };
                    height = 4176;
                }
                2 | 4 => {
                    left = 630;
                    top = 0;
                    height = 3348;
                    width = 5004;
                }
                _ => use_inset(&mut left, &mut top, &mut width, &mut height),
            },
            6384 => {
                top = 0;
                match crop_mode {
                    0 => {
                        left = 0;
                        top = 6;
                        width = 6246;
                        height = 4170;
                    }
                    2 => {
                        left = 624;
                        width = 5004;
                        height = rh;
                    }
                    4 => {
                        left = 624;
                        width = 5004;
                    }
                    _ => {}
                }
            }
            6912 | 9216 => {
                left = 0;
                top = 0;
            }
            8472 => {
                left = 0;
                top = 0;
                width = rw - 192;
            }
            9696 | 11808 => {
                left = 0;
                width = rw - 146;
                height = rh - 8;
                top = 2;
            }
            _ => {}
        }
    } else if m.raf_gen == 0 {
        match rw {
            2304 => {
                top = 6;
                height -= 6;
            }
            3328 => {
                width = rw - 66;
                left = 34;
                if inset.is_some_and(|i| i.0 == 8) {
                    left = 0;
                    width += 2;
                    bggr = true;
                }
            }
            3664 => bggr = true,
            5504 => {
                left = 32;
                top = 8;
                width = rw - 64;
                height = rh - 16;
                bggr = true;
            }
            _ => {
                if !model.starts_with("S6000") && !model.starts_with("S6500") {
                    use_inset(&mut left, &mut top, &mut width, &mut height);
                }
            }
        }
    }
    (left, top, width, height, bggr, x3e00)
}

/// Open a RAF file.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if data.len() < 0x94 {
        return Err(Error::corrupt("RAF header truncated"));
    }
    let header_model = flashraw_core::bytes::ascii_field(&data[0x1c..0x3c]);
    let version = String::from_utf8_lossy(&data[0x3c..0x40]).to_string();
    let (jpeg_off, jpeg_len) = (be32(data, 84)?, be32(data, 88)?);
    let meta_off = be32(data, 92)?;
    let (cfa_off, cfa_len) = (be32(data, 100)?, be32(data, 104)?);
    // A second raw frame pointer (dual-exposure SuperCCD SR bodies).
    let second_frame = if jpeg_off > 120 { be32(data, 120)? != 0 } else { false };

    let mut meta = RawMetadata::default();
    let exif = parse_exif(data, jpeg_off, jpeg_len, &mut meta);
    if meta.make.is_empty() {
        meta.make = "FUJIFILM".into();
    }
    if meta.model.is_empty() {
        meta.model = header_model.clone();
    }
    let model = {
        let m = meta.model.trim().to_string();
        let m = m.strip_prefix("FUJIFILM").map(|s| s.trim().to_string()).unwrap_or(m);
        m.strip_prefix("FinePix").map(|s| s.trim().to_string()).unwrap_or(m)
    };
    let rmeta = parse_meta(data, meta_off, &model);
    let cfa_sec = data.get(cfa_off..).ok_or_else(|| Error::corrupt("RAF CFA section beyond end of file"))?;
    let ctiff = parse_cfa_tiff(cfa_sec);
    let (fuji_layout, fuji_width) = rmeta.layout.unwrap_or((false, false));

    // Payload description.
    let mut raw_w;
    let mut raw_h;
    let bps;
    let payload: &[u8];
    let mut layout;
    let mut packed_flag = false;
    let xtrans = rmeta.xtrans.is_some();
    if let Some(c) = &ctiff {
        raw_w = c.width;
        raw_h = c.height;
        bps = c.bps;
        let start = cfa_off.checked_add(c.offset).ok_or_else(|| Error::corrupt("RAF payload offset overflow"))?;
        payload = flashraw_core::bytes::slice_at_lenient(data, start, c.bytes)?;
        let size = c.bytes as u64;
        let (w, h) = (raw_w as u64, raw_h as u64);
        layout = Layout::Words { order: ByteOrder::Little, stride: raw_w as usize };
        if let (Some(f), true) = (c.packed_flag, bps <= 12) {
            packed_flag = true;
            layout = Layout::Packed { bits: bps, order: if f != 0 { PackOrder::Msb32 } else { PackOrder::LsbBytes } };
        } else if w * h * 2 != size {
            if w * h * 7 / 4 == size {
                layout = Layout::Packed { bits: 14, order: PackOrder::Msb32 };
            } else if flashraw_codecs::fuji_compressed::FujiHeader::parse(payload).is_ok() {
                layout = Layout::Compressed;
            }
        } else if model.eq_ignore_ascii_case("X-H2S")
            && (data.len() as u64) < size + start as u64
            && flashraw_codecs::fuji_compressed::FujiHeader::parse(payload).is_ok()
        {
            // X-H2S may record the uncompressed size for compressed data.
            layout = Layout::Compressed;
        }
    } else {
        // Bare 16-bit samples in the EXIF byte order; geometry from the meta directory.
        let (h, w) = rmeta.raw_size.ok_or_else(|| Error::invalid_metadata("RAF without raw size record"))?;
        raw_w = w;
        raw_h = h;
        bps = 12;
        // Dual-frame bodies store the second frame after the first: the section length
        // covers only one frame, so the samples run to the end of the file.
        let _ = cfa_len;
        payload = data.get(cfa_off..).filter(|p| !p.is_empty()).ok_or_else(|| Error::corrupt("RAF without payload"))?;
        let order = exif.order.unwrap_or(ByteOrder::Little);
        if fuji_layout && second_frame {
            raw_w *= 2;
        }
        layout = if raw_w == 5504 {
            Layout::TiledWords { tiles: 8, order }
        } else if model.starts_with("S20Pro") || model.starts_with("F700") {
            // Both frames are interleaved row by row; the first one is the main exposure.
            let stride = raw_w as usize;
            raw_w /= 2;
            Layout::Words { order, stride }
        } else {
            Layout::Words { order, stride: raw_w as usize }
        };
    }
    let compressed = layout == Layout::Compressed;
    let fc = if compressed { Some(FujiCompressed::new(payload)?) } else { None };
    // replaces the buffer size.
    let (geo_w, geo_h) = (raw_w, raw_h);
    if let Some(f) = &fc {
        raw_w = f.header.width;
        raw_h = f.header.height;
    }
    opts.limits.check_dims(raw_w, raw_h, 1)?;
    if raw_w < 22 || raw_h < 22 {
        return Err(Error::invalid_metadata(format!("RAF raw size {raw_w}x{raw_h} too small")));
    }

    let mode = if compressed { "compressed" } else { "" };
    let cam = db_lookup(&mut meta, mode);
    meta.set_extra("raf_version", version.trim());
    if exif.crop_mode != 0 {
        meta.set_extra("crop_mode", exif.crop_mode);
    }
    if fuji_width {
        meta.set_extra("fuji_rotate", if fuji_layout { "layout1" } else { "layout0" });
    }

    let (mut left, mut top, mut width, mut height, bggr, x3e00) =
        active_area(&model, geo_w, geo_h, &rmeta, exif.crop_mode, xtrans, packed_flag, fuji_layout);
    let align = if xtrans {
        6
    } else if fuji_width {
        1
    } else {
        2
    };
    if align > 1 {
        let up = |v: i64| (v.max(0) + align - 1) / align * align;
        let (nt, nl) = (up(top), up(left));
        height -= nt - top;
        width -= nl - left;
        top = nt;
        left = nl;
    }
    // used area spans `fuji_width` (doubled for layout 0) columns from the left margin.
    let superccd = fuji_width && raw_w != 5504;
    let fw = if superccd { width >> if fuji_layout { 0 } else { 1 } } else { 0 };
    if superccd {
        width = fw << if fuji_layout { 0 } else { 1 };
        height = raw_h as i64 - 2 * top;
    }
    let mut info = FrameInfo::new(raw_w, raw_h, bps.min(16) as u8);
    let clampu = |v: i64| v.clamp(0, u32::MAX as i64) as u32;
    info.active_area = Rect::new(clampu(left), clampu(top), clampu(width), clampu(height)).clamp_to(raw_w, raw_h);
    info.crop = match (rmeta.crop_tl, rmeta.crop_size) {
        (Some((t, l)), Some((h, w))) if !fuji_width => {
            Rect::new(l, t, w, h).intersect(&info.active_area).unwrap_or(info.active_area)
        }
        _ => info.active_area,
    };
    info.compression = if compressed { Compression::FujiCompressed } else { Compression::Uncompressed };
    info.orientation = meta.orientation;
    if rmeta.raf_gen == 0 && geo_w == 2944 || raw_w == 5504 {
        // S2Pro and the DBP back are mounted rotated.
        info.orientation = Orientation::Rotate90;
    }

    // CFA.
    let colors: ColorTile;
    if let Some(x) = rmeta.xtrans {
        colors = x;
        let cols: Vec<CfaColor> = x.iter().flatten().map(|&c| CfaColor::from_index(c)).collect();
        let p = CfaPattern::new(6, 6, &cols).ok_or_else(|| Error::invalid_metadata("bad X-Trans layout"))?;
        info.cfa = Some(p);
        info.layout = SensorLayout::XTrans;
    } else if superccd {
        let p = superccd_cfa(fuji_layout, fw, left, top)
            .ok_or_else(|| Error::invalid_metadata("invalid SuperCCD geometry"))?;
        info.cfa = Some(p);
        info.layout = SensorLayout::OtherCfa;
        colors = std::array::from_fn(|r| std::array::from_fn(|c| p.color_at(c, r) as u8));
    } else {
        let visible = if bggr { CfaPattern::BGGR } else { CfaPattern::RGGB };
        let a = info.active_area;
        let p = visible.shifted(2 - a.x as usize % 2, 2 - a.y as usize % 2);
        info.cfa = Some(p);
        info.layout = SensorLayout::Bayer;
        colors = std::array::from_fn(|r| std::array::from_fn(|c| p.color_at(c, r) as u8));
    }

    // Black level.
    let mut known = Known { cfa: true, crop: true, active_area: true, ..Default::default() };
    if let Some(c) = ctiff.as_ref().filter(|c| !c.black.is_empty()) {
        let n = c.black.len();
        let dim = ((n as f64).sqrt() as usize).clamp(1, 8);
        if dim * dim <= n {
            let vals: Vec<f32> = c.black[..dim * dim].iter().map(|&v| v as f32).collect();
            info.black_level = BlackLevel::pattern(dim, dim, vals);
            known.black = true;
        }
    } else if let Some(b) = rmeta.black {
        // (R, G, B, G2) per CFA colour, placed on the 2x2 pattern.
        let p = info.cfa.unwrap_or(CfaPattern::RGGB);
        let mut v = [0f32; 4];
        let mut seen_g = false;
        for (i, slot) in v.iter_mut().enumerate() {
            let col = p.color_at(i % 2, i / 2);
            *slot = match col {
                CfaColor::Red => b[0],
                CfaColor::Blue => b[2],
                _ => {
                    let g = if seen_g { b[3] } else { b[1] };
                    seen_g = true;
                    g
                }
            };
        }
        info.black_level = BlackLevel::pattern2x2(v);
        known.black = true;
    }
    // White level.
    info.white_level = WhiteLevel::uniform(if x3e00 {
        0x3e00
    } else if bps >= 16 {
        65535
    } else {
        (1u32 << bps) - 1
    });
    if (geo_w == 9696 || geo_w == 11808) && bps == 16 {
        info.white_level = WhiteLevel::uniform(65535);
    }
    known.white = true;
    let table = fallback_table("Fujifilm", &model);
    if let Some(t) = table {
        if t.black > 0 {
            info.black_level = BlackLevel::uniform(t.black as f32);
        } else if t.black < 0 && info.black_level.values.iter().all(|&v| v == 0.0) {
            info.black_level = BlackLevel::uniform(-t.black as f32);
        }
        if t.white > 0 {
            info.white_level = WhiteLevel::uniform(t.white as u32);
        }
    }

    // samples span 14 bits whenever the white level says so.
    if ctiff.is_none() && info.white_level.max() > 4095 {
        info.bits_per_sample = 14;
    }

    // White balance.
    let wb = ctiff.as_ref().and_then(|c| c.wb).or(rmeta.wb);
    if let Some([r, g, b]) = wb {
        info.white_balance = WhiteBalance::from_multipliers(r, g, b, g);
    }
    if let Some(cm) = table.and_then(|t| {
        ColorMatrix::new(Illuminant::D65, 3, t.color_matrix.iter().take(9).map(|&v| v as f32 / 10000.0).collect())
    }) {
        info.color_matrices = ColorMatrices { color: vec![cm], source: ColorSource::CameraDb, ..Default::default() };
        known.color = true;
    }
    apply_db(&mut info, &meta, cam, known);

    meta.previews = Vec::new();
    push_jpeg(data, jpeg_off, jpeg_len, info.orientation, &mut meta.previews);
    classify_previews(&mut meta.previews);

    let raf = Arc::new(Raf { payload, width: raw_w, height: raw_h, layout, colors });
    let units = match &fc {
        Some(f) => {
            let ranges = f.block_ranges(payload);
            let base = payload.as_ptr() as usize - data.as_ptr() as usize;
            ranges
                .iter()
                .enumerate()
                .map(|(i, &(o, l))| DecodeUnit {
                    rect: Rect::new(i as u32 * f.header.block_size, 0, f.header.block_width(i as u32), raw_h),
                    offset: (base + o) as u64,
                    length: l as u64,
                })
                .collect()
        }
        None => Vec::new(),
    };
    let fc = fc.map(Arc::new);
    let fc2 = fc.clone();
    let r1 = raf.clone();
    let mut dec = SimpleDecoder::new(
        RawFormat::Raf,
        meta,
        info,
        Box::new(move |ctx| {
            let mut img = RawImage::new_u16(r1.width, r1.height, 1, &ctx.limits)?;
            match (&r1.layout, &fc) {
                (Layout::Compressed, Some(f)) => ctx.profiler.time(Stage::EntropyDecode, || {
                    f.decode(img.u16_mut(), r1.width, r1.height, &r1.colors, ctx.parallel)
                })?,
                _ => ctx.profiler.time(Stage::Unpack, || r1.unpack(img.u16_mut(), ctx))?,
            }
            Ok(img)
        }),
    );
    // Region decoding touches only the stripes (compressed) or rows (uncompressed) needed.
    let (r2, fc2) = (raf.clone(), fc2);
    dec.region = Some(Box::new(move |ctx, rect| {
        let full = Rect::full(r2.width, r2.height);
        let rect =
            rect.intersect(&full).ok_or_else(|| Error::InvalidArgument(format!("region {rect} outside image")))?;
        match (&r2.layout, &fc2) {
            (Layout::Compressed, Some(f)) => {
                let mut img = RawImage::new_u16(rect.width, rect.height, 1, &ctx.limits)?;
                ctx.profiler
                    .time(Stage::EntropyDecode, || f.decode_region(img.u16_mut(), rect, &r2.colors, ctx.parallel))?;
                Ok(img)
            }
            (Layout::Words { .. } | Layout::Packed { .. }, _) => {
                let mut img = RawImage::new_u16(rect.width, rect.height, 1, &ctx.limits)?;
                ctx.profiler.time(Stage::Unpack, || r2.unpack_region(img.u16_mut(), rect, ctx))?;
                Ok(img)
            }
            _ => {
                let mut img = RawImage::new_u16(r2.width, r2.height, 1, &ctx.limits)?;
                r2.unpack(img.u16_mut(), ctx)?;
                img.crop(rect)
            }
        }
    }));
    dec.random_access = match raf.layout {
        Layout::Compressed => RandomAccess::Blocks,
        Layout::TiledWords { .. } => RandomAccess::Sequential,
        _ => RandomAccess::Rows,
    };
    dec.units = units;
    dec.packed = match raf.layout {
        Layout::Words { order, stride } if stride == raw_w as usize => Some(PackedPayload {
            data: payload,
            width: raw_w,
            height: raw_h,
            bits: 16,
            order: if order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes },
            row_stride: stride * 2,
        }),
        Layout::Packed { bits, order } => Some(PackedPayload {
            data: payload,
            width: raw_w,
            height: raw_h,
            bits,
            order,
            row_stride: raw_w as usize * bits as usize / 8,
        }),
        _ => None,
    };
    Ok(Box::new(dec))
}

/// CFA of the stored (unrotated) SuperCCD buffer, anchored at the buffer origin.
///
/// image position `r = fw - 1 - col + row/2, c = col + (row + 1)/2` (layout 1) or
/// `r = fw - 1 + row - col/2, c = row + (col + 1)/2` (layout 0), where the rotated
/// image carries a 2x2 Bayer pattern (RGGB for odd `fw`, GBRG for even). That makes
/// the stored buffer a regular 2x4 (layout 1) or 4x2 (layout 0) pattern of green rows
/// (columns) and alternating red/blue rows (columns).
fn superccd_cfa(layout1: bool, fw: i64, left: i64, top: i64) -> Option<CfaPattern> {
    let rotated = if fw & 1 != 0 { CfaPattern::RGGB } else { CfaPattern::GBRG };
    let (pw, ph) = if layout1 { (2usize, 4usize) } else { (4, 2) };
    let mut colors = Vec::with_capacity(pw * ph);
    for y in 0..ph as i64 {
        for x in 0..pw as i64 {
            // Patterns repeat every 4 rows/columns, so shift into non-negative range.
            let row = (y - top).rem_euclid(8);
            let col = (x - left).rem_euclid(8);
            let (r, c) = if layout1 {
                (fw - 1 - col + (row >> 1), col + ((row + 1) >> 1))
            } else {
                (fw - 1 + row - (col >> 1), row + ((col + 1) >> 1))
            };
            colors.push(rotated.color_at(c.rem_euclid(2) as usize, r.rem_euclid(2) as usize));
        }
    }
    CfaPattern::new(pw, ph, &colors)
}

/// gives the X-T20 the X-T2 entry and the S200EXR the "S20" entry.
fn fallback_table(make: &str, model: &str) -> Option<&'static flashraw_camera_db::FallbackMatrix> {
    let m = model.to_ascii_uppercase();
    flashraw_camera_db::MATRICES
        .iter()
        .find(|t| t.canonical_make.eq_ignore_ascii_case(make) && m.starts_with(&t.model_prefix.to_ascii_uppercase()))
}

impl Raf<'_> {
    /// Bytes per stored row and the per-row unpacker of row-addressable layouts.
    fn row_layout(&self) -> Option<(usize, u32, PackOrder)> {
        match self.layout {
            Layout::Words { order, stride } => Some((
                stride * 2,
                16,
                if order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes },
            )),
            Layout::Packed { bits, order } => Some(((self.width as usize * bits as usize).div_ceil(8), bits, order)),
            _ => None,
        }
    }

    /// Unpack only the rows/columns of `rect` (row-addressable layouts).
    fn unpack_region(&self, out: &mut [u16], rect: Rect, ctx: &DecodeContext) -> Result<()> {
        let (row_bytes, bits, order) =
            self.row_layout().ok_or_else(|| Error::InvalidArgument("layout is not row addressable".into()))?;
        let (w, rw) = (self.width as usize, rect.width as usize);
        let needed = (rect.bottom() as usize).saturating_sub(1) * row_bytes + (w * bits as usize).div_ceil(8);
        if self.payload.len() < needed {
            return Err(Error::corrupt("RAF payload truncated"));
        }
        let simd = ctx.simd;
        par::for_each_row_chunk(&mut out[..rw * rect.height as usize], rw, 64, ctx.parallel, |first, chunk| {
            let mut tmp = vec![0u16; w];
            for (i, row) in chunk.chunks_mut(rw).enumerate() {
                let y = rect.y as usize + first + i;
                let src = &self.payload[y * row_bytes..];
                let src = &src[..(w * bits as usize).div_ceil(8).min(src.len())];
                packed::unpack_row(src, &mut tmp, bits, order, simd);
                row.copy_from_slice(&tmp[rect.x as usize..rect.x as usize + rw]);
            }
        });
        Ok(())
    }

    fn unpack(&self, out: &mut [u16], ctx: &DecodeContext) -> Result<()> {
        let (w, h) = (self.width as usize, self.height as usize);
        match self.layout {
            Layout::Words { order, stride } => {
                let row_bytes = stride * 2;
                let rows = (self.payload.len() / row_bytes.max(1)).min(h);
                if rows == 0 {
                    return Err(Error::corrupt("RAF payload truncated"));
                }
                let po = if order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
                let simd = ctx.simd;
                let src = self.payload;
                par::for_each_row_chunk(&mut out[..w * h], w, 64, ctx.parallel, |first, chunk| {
                    for (i, row) in chunk.chunks_mut(w).enumerate() {
                        let y = first + i;
                        if y >= rows {
                            break;
                        }
                        let s = &src[y * row_bytes..y * row_bytes + w * 2];
                        packed::unpack_row(s, row, 16, po, simd);
                    }
                });
                if rows < h {
                    return Err(Error::corrupt(format!("RAF payload truncated: {rows}/{h} rows")));
                }
                Ok(())
            }
            Layout::TiledWords { tiles, order } => {
                let po = if order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
                let tw = w / tiles;
                let need = tw * tiles * h * 2;
                if self.payload.len() < need || tw == 0 {
                    return Err(Error::corrupt("RAF tiled payload truncated"));
                }
                for t in 0..tiles {
                    let base = t * tw * h * 2;
                    for y in 0..h {
                        let s = &self.payload[base + y * tw * 2..base + (y + 1) * tw * 2];
                        packed::unpack_row(s, &mut out[y * w + t * tw..y * w + (t + 1) * tw], 16, po, false);
                    }
                }
                Ok(())
            }
            Layout::Packed { bits, order } => {
                let lay = packed::PackedLayout::tight(w, h, bits, order);
                packed::unpack(self.payload, lay, out, w, ctx.parallel, ctx.simd, false)
            }
            Layout::Compressed => Err(Error::corrupt("RAF compressed payload without header")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b16(v: u16) -> [u8; 2] {
        v.to_be_bytes()
    }

    /// Build a minimal modern RAF: meta directory + CFA TIFF + 16-bit LE payload.
    fn synthetic_raf(w: u16, h: u16, samples: &[u16], xtrans: bool) -> Vec<u8> {
        let mut meta = Vec::new();
        let mut recs: Vec<(u16, Vec<u8>)> = vec![
            (0x100, [b16(h), b16(w)].concat()),
            (0x130, vec![0x0a, 0x0b, 0x09, 0x08]),
            (0x2ff0, [b16(302), b16(500), b16(302), b16(400)].concat()),
        ];
        if xtrans {
            // Stored reversed: byte c is pattern element 35 - c.
            let pat = [
                1u8, 1, 0, 1, 1, 2, 1, 1, 2, 1, 1, 0, 2, 0, 1, 0, 2, 1, 1, 1, 2, 1, 1, 0, 1, 1, 0, 1, 1, 2, 0, 2, 1, 2,
                0, 1,
            ];
            recs.push((0x131, pat.iter().rev().copied().collect()));
        }
        meta.extend((recs.len() as u32).to_be_bytes());
        for (t, v) in &recs {
            meta.extend(b16(*t));
            meta.extend(b16(v.len() as u16));
            meta.extend(v);
        }
        // CFA TIFF: IFD0 with one 0xF000 entry -> sub IFD at 26.
        let mut tiff = b"II*\0".to_vec();
        tiff.extend(8u32.to_le_bytes());
        tiff.extend(1u16.to_le_bytes());
        tiff.extend(0xF000u16.to_le_bytes());
        tiff.extend(13u16.to_le_bytes());
        tiff.extend(1u32.to_le_bytes());
        tiff.extend(26u32.to_le_bytes());
        tiff.extend(0u32.to_le_bytes());
        let data_off = 256u32;
        let entries: Vec<(u16, u16, u32)> = vec![
            (0xF001, 4, w as u32),
            (0xF002, 4, h as u32),
            (0xF003, 4, 14),
            (0xF007, 4, data_off),
            (0xF008, 4, samples.len() as u32 * 2),
            (0xF00A, 4, 1024),
        ];
        tiff.extend((entries.len() as u16).to_le_bytes());
        for (t, ty, v) in entries {
            tiff.extend(t.to_le_bytes());
            tiff.extend(ty.to_le_bytes());
            tiff.extend(1u32.to_le_bytes());
            tiff.extend(v.to_le_bytes());
        }
        tiff.extend(0u32.to_le_bytes());
        tiff.resize(data_off as usize, 0);
        for s in samples {
            tiff.extend(s.to_le_bytes());
        }
        let mut f = vec![0u8; 0x94];
        f[..16].copy_from_slice(b"FUJIFILMCCD-RAW ");
        f[0x1c..0x1c + 6].copy_from_slice(b"X-TEST");
        f[0x3c..0x40].copy_from_slice(b"0100");
        let meta_off = f.len();
        let cfa_off = meta_off + meta.len();
        f[92..96].copy_from_slice(&(meta_off as u32).to_be_bytes());
        f[96..100].copy_from_slice(&(meta.len() as u32).to_be_bytes());
        f[100..104].copy_from_slice(&(cfa_off as u32).to_be_bytes());
        f[104..108].copy_from_slice(&(tiff.len() as u32).to_be_bytes());
        f.extend(meta);
        f.extend(tiff);
        f
    }

    fn decode(data: &[u8]) -> Result<(FrameInfo, Vec<u16>)> {
        let d = open(data, &OpenOptions::default())?;
        let p = Profiler::new();
        let img = d.decode(&DecodeContext::new(&p))?;
        Ok((d.frame_info().clone(), img.u16().to_vec()))
    }

    #[test]
    fn synthetic_uncompressed_raf() {
        let (w, h) = (48u16, 36u16);
        let samples: Vec<u16> = (0..w as u32 * h as u32).map(|i| (i * 37 % 16384) as u16).collect();
        let f = synthetic_raf(w, h, &samples, true);
        assert!(probe(&f));
        let (info, px) = decode(&f).unwrap();
        assert_eq!((info.width, info.height), (48, 36));
        assert_eq!(px, samples);
        assert_eq!(info.layout, SensorLayout::XTrans);
        assert_eq!(info.black_level.values, vec![1024.0]);
        let wb = info.white_balance.unwrap();
        assert!((wb.coeffs[0] - 500.0 / 302.0).abs() < 1e-4 && (wb.coeffs[2] - 400.0 / 302.0).abs() < 1e-4);
        // X-Trans pattern parsed from the reversed 0x131 record.
        let p = info.cfa.unwrap();
        assert_eq!(p.color_at(2, 0), CfaColor::Red);
        assert_eq!(p.color_at(5, 0), CfaColor::Blue);
    }

    #[test]
    fn synthetic_raf_truncation_and_corruption_never_panic() {
        let (w, h) = (48u16, 36u16);
        let samples: Vec<u16> = (0..w as u32 * h as u32).map(|i| i as u16).collect();
        let f = synthetic_raf(w, h, &samples, false);
        for cut in [0, 10, 0x60, 0x94, 0xa0, 0x100, f.len() / 2, f.len() - 100, f.len() - 1] {
            let r = decode(&f[..cut]);
            assert!(r.is_err(), "truncated at {cut} must fail");
        }
        let mut x = 0x9e37_79b9u32;
        for i in 0..400 {
            let mut g = f.clone();
            for _ in 0..4 {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let pos = (x as usize) % g.len().min(0x200 + i);
                g[pos] = (x >> 8) as u8;
            }
            let _ = decode(&g);
        }
    }

    #[test]
    fn superccd_cfa_layouts() {
        // Layout 1: green rows alternate with red/blue rows (2 wide, 4 tall).
        let p = superccd_cfa(true, 1600, 0, 0).unwrap();
        assert_eq!((p.width(), p.height()), (2, 4));
        let greens =
            (0..4).filter(|&y| p.color_at(0, y) == CfaColor::Green && p.color_at(1, y) == CfaColor::Green).count();
        assert_eq!(greens, 2);
        // Layout 0: the same arrangement transposed.
        let q = superccd_cfa(false, 1600, 0, 0).unwrap();
        assert_eq!((q.width(), q.height()), (4, 2));
    }

    /// Corpus files (when present): truncated or corrupted copies must fail cleanly.
    #[test]
    fn corpus_malformed_inputs() {
        let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
        let files = [
            "Fujifilm/X-E4/fuji_xe4_32_lossy.RAF",
            "Fujifilm/XQ1/DSCF2004.RAF",
            "Fujifilm/FinePix HS10 HS11/RAW_FUJI_FINEPIX_HS10.RAF",
            "Fujifilm/X-A5/DSCF0617.RAF",
            "Fujifilm/FinePix S5600/RAW_FUJI_S5600S5200.RAF",
            "Fujifilm/X100F/DSCF5760_x100f_lossless_compressed_raw_Temple.RAF",
        ];
        for f in files {
            let Ok(data) = std::fs::read(corpus.join(f)) else { continue };
            assert!(decode(&data).is_ok(), "{f} should decode");
            for frac in [0.0005, 0.02, 0.5, 0.9] {
                let cut = (data.len() as f64 * frac) as usize;
                assert!(decode(&data[..cut]).is_err(), "{f} truncated to {cut} bytes must fail");
            }
            // Corrupt the payload region: decoding may fail or produce garbage, never panic.
            let mut g = data.clone();
            let n = g.len();
            for k in 0..64 {
                let pos = n / 2 + k * 977 % (n / 3);
                g[pos] ^= 0x5a;
            }
            let _ = decode(&g);
        }
    }
}
