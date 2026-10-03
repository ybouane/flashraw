//! Olympus / OM System ORF and ORI.
//!
//! ORF is TIFF with a vendor magic (`IIRO`, `IIRS`, `MMOR`). The sensor image is
//! the single (or multi-strip) image of IFD0, stored as one of:
//!
//! | Layout | Detection | Decoder |
//! |---|---|---|
//! | Olympus lossless compression | 16 bps, single strip, `bytes * 7 > w * h` | [`flashraw_codecs::olympus`] |
//! | 12-bit packed in 32-bit LE words | 12 bps, `bytes = w*h*1.5` | packed (MSB in 32-bit words) |
//! | 12-bit packed, MSB-first | 12 bps otherwise | packed (MSB bytes) |
//! | 12-bit LSB-first with a pad byte per 10 pixels | `bytes = w*h*1.6` (E-300/330/500, high-res shots) | packed |
//! | 16-bit words, data in the top bits | multi-strip 16 bps (E-1, E-10/20, E-400) | unpacked + shift |
//!
//! Calibration lives in the makernote (`OLYMPUS\0II`, `OM SYSTEM\0..`, or the old
//! `OLYMP\0` form): the ImageProcessing directory (0x2040) holds the white-balance
//! R/B levels (0x100), per-channel black (0x600), valid bits (0x611), the default
//! crop (0x612..0x615) and the compression parameters (0x640..0x653). ORI files
//! (the base frame saved next to a high-resolution composite) are identical ORFs.

use crate::common::*;
use crate::sony::table;
use flashraw_codecs::olympus::{self as codec, OlympusParams};
use flashraw_core::tiff::{Entry, Ifd, Tiff, TiffOptions, tags};
use flashraw_core::*;

/// ORF magics: `IIRO`, `IIRS` (little-endian), `MMOR` (big-endian).
pub fn probe(data: &[u8]) -> bool {
    matches!(data.get(..4), Some(b"IIRO") | Some(b"IIRS") | Some(b"MMOR"))
}

pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let tiff = Tiff::parse(data)?;
    open_tiff(data, tiff, opts)
}

/// Values gathered from the makernote.
#[derive(Debug, Default, Clone)]
struct Makernote {
    /// ImageProcessing 0x100 (or old 0x1017/0x1018): R and B levels, 1/256 units.
    wb_rb: [Option<u32>; 2],
    /// Black per channel in R, G, G2, B order (0x600 / old 0x1012).
    black_rggb: Option<[u32; 4]>,
    valid_bits: Option<u32>,
    /// 0x612..0x615: crop left, top, width, height.
    crop: [Option<u32>; 4],
    /// 0x640..=0x653.
    coder: [Option<u32>; 14],
    /// Embedded JPEGs `(absolute offset, length)`: the makernote thumbnail (0x100)
    /// and the CameraSettings preview (0x2020: 0x101/0x102).
    previews: Vec<(usize, usize)>,
}

/// Payload layout.
#[derive(Debug, Clone, Copy)]
enum Payload {
    Compressed(OlympusParams),
    /// 12-bit, MSB-first inside 32-bit little-endian words.
    Packed12Words,
    /// 12-bit, MSB-first bytes.
    Packed12Msb,
    /// 12-bit MSB-first, even rows first; the odd-row field starts at the next
    /// 2048-byte boundary after the even field (C-/SP-series "RGB" photometric files).
    Packed12Interlaced,
    /// 12-bit, LSB-first, one padding byte after every 10 samples.
    Packed12Padded,
    /// 16-bit words in file order, value in the top bits (`>> shift`).
    Unpacked {
        big_endian: bool,
        shift: u32,
    },
}

pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let mut meta = RawMetadata::default();
    for ifd in &tiff.ifds {
        flashraw_core::exif::read_ifd0(ifd, &mut meta);
    }
    let raw = tiff
        .all_ifds()
        .into_iter()
        .filter(|i| i.width() > 0 && i.height() > 0 && i.has(tags::STRIP_OFFSETS))
        .filter(|i| i.compression() == 1)
        .max_by_key(|i| i.width() as u64 * i.height() as u64)
        .ok_or_else(|| Error::invalid_metadata("ORF: no raw image"))?;
    let (raw_w, raw_h) = (raw.width(), raw.height());
    let bps = raw.bps();
    let strips = strips(raw);
    let &(offset, first_len) = strips.first().ok_or_else(|| Error::invalid_metadata("ORF: no strip offsets"))?;
    let area = raw_w as u64 * raw_h as u64;
    let bytes = first_len as u64;
    let mn = read_makernote(data, &tiff);
    let model = meta.model.trim().to_string();
    let valid_bits = mn.valid_bits.filter(|b| (8..=16).contains(b));

    // of the first strip).
    let payload = if bytes * 5 == area * 8 {
        Payload::Packed12Padded
    } else {
        match bps {
            12 if raw.u32(tags::PHOTOMETRIC) == Some(2) => Payload::Packed12Interlaced,
            12 if bytes * 2 == area * 3 => Payload::Packed12Words,
            12 => Payload::Packed12Msb,
            14 | 16 if bytes * 7 > area && bps == 16 => Payload::Compressed(OlympusParams::from_tags(&mn.coder)),
            14 | 16 => {
                let shift = match valid_bits {
                    Some(10) => 6,
                    _ => 4,
                };
                Payload::Unpacked { big_endian: tiff.order == ByteOrder::Big, shift }
            }
            b => return Err(Error::unsupported_compression("ORF", format!("{b}-bit uncompressed layout"))),
        }
    };
    let bits = match payload {
        Payload::Packed12Words | Payload::Packed12Msb | Payload::Packed12Padded | Payload::Packed12Interlaced => 12,
        _ => valid_bits.unwrap_or(12),
    };

    // Geometry: the buffer height is rounded up to even, the visible width trimmed.
    let buf_h = raw_h + (raw_h & 1);
    opts.limits.check_dims(raw_w, buf_h, 1)?;
    let (vis_w, vis_h) = visible_size(&model, raw_w, buf_h, &payload);

    // Levels.
    let mut cblack = [0u32; 4]; // R, G, B, G2
    if let Some(b) = mn.black_rggb {
        cblack = [b[0], b[1], b[3], b[2]];
    }
    let mut white: Option<u32> = None;
    if bits == 14 {
        // 14-bit files keep the black level in 12-bit units.
        cblack = cblack.map(|c| c * 4);
    }
    let unpacked = matches!(payload, Payload::Unpacked { .. });
    if unpacked && matches!(model.as_str(), "E-300" | "E-500") {
        white = Some(0xfc3);
        cblack = [0; 4];
    } else if model.starts_with("STYLUS1") {
        white = Some(0xfff);
    } else if unpacked && model == "E-330" {
        white = Some(0xf79);
    }
    let norm = normalized_model(&model);
    let entry = table::lookup("Olympus", norm).or_else(|| table::lookup("OM System", norm));
    let mut black = 0u32;
    table::apply_levels(entry, &mut black, &mut cblack, &mut white);

    let mode = match payload {
        Payload::Compressed(_) => "compressed",
        Payload::Packed12Words | Payload::Packed12Msb => "12bit-packed",
        Payload::Packed12Interlaced => "12bit-packed-interlaced",
        Payload::Packed12Padded => "12bit-packed-padded",
        Payload::Unpacked { .. } => "uncompressed",
    };
    db_lookup(&mut meta, mode);
    let mut info = FrameInfo::new(raw_w, buf_h, bits as u8);
    info.compression = match payload {
        Payload::Compressed(_) => Compression::OlympusCompressed,
        _ => Compression::Uncompressed,
    };
    info.active_area = Rect::new(0, 0, vis_w, vis_h);
    info.crop = match mn.crop {
        [Some(x), Some(y), Some(w), Some(h)] if w > 0 && h > 0 => {
            Rect::new(x, y, w, h).intersect(&info.active_area).unwrap_or(info.active_area)
        }
        _ => info.active_area,
    };
    let cfa =
        tiff.all_ifds().into_iter().find_map(|i| tiff_ep_cfa(i)).filter(|c| c.is_bayer()).unwrap_or(CfaPattern::RGGB);
    info.cfa = Some(cfa);
    info.layout = layout_for(&cfa);
    info.black_level = crate::sony::cfa_black(&cfa, black, &cblack);
    info.white_level = WhiteLevel::uniform(white.unwrap_or((1u32 << bits.min(16)) - 1));
    info.white_balance = match mn.wb_rb {
        [Some(r), Some(b)] if r > 0 && b > 0 => {
            let (r, b) = (r as f32 / 256.0, b as f32 / 256.0);
            let max = r.max(b).max(1.0);
            let min = r.min(b).min(1.0);
            if min > 0.001 && max / min <= 100.0 { WhiteBalance::from_multipliers(r, 1.0, b, 1.0) } else { None }
        }
        _ => None,
    };
    info.orientation = file_orientation(&tiff);
    info.color_matrices = table::color(entry, &meta);
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);
    for &(o, l) in &mn.previews {
        push_jpeg(data, o, l, info.orientation, &mut meta.previews);
    }
    classify_previews(&mut meta.previews);

    // row of odd-height frames comes from the bytes after the strip).
    let src = data.get(offset..).ok_or_else(|| Error::corrupt("ORF: strip offset beyond file"))?;
    let (w, h) = (raw_w as usize, buf_h as usize);
    let mut dec = SimpleDecoder::new(
        RawFormat::Orf,
        meta,
        info,
        Box::new(move |ctx| {
            let mut img = RawImage::new_u16(w as u32, h as u32, 1, &ctx.limits)?;
            decode(src, payload, img.u16_mut(), w, h, raw_h as usize, ctx)?;
            Ok(img)
        }),
    );
    if !matches!(payload, Payload::Compressed(_)) {
        dec.random_access = RandomAccess::Rows;
    }
    if let Some((bits, order, stride)) = match payload {
        Payload::Packed12Words => Some((12, PackOrder::Msb32, w * 3 / 2)),
        Payload::Packed12Msb => Some((12, PackOrder::MsbBytes, w * 3 / 2)),
        _ => None,
    } {
        if let Some(d) = src.get(..stride * raw_h as usize) {
            dec.packed =
                Some(PackedPayload { data: d, width: w as u32, height: raw_h, bits, order, row_stride: stride });
        }
    }
    Ok(Box::new(dec))
}

/// Decode a payload into `out` (`w x h`, `coded_rows` rows present in the strip).
fn decode(
    src: &[u8],
    payload: Payload,
    out: &mut [u16],
    w: usize,
    h: usize,
    coded_rows: usize,
    ctx: &DecodeContext,
) -> Result<()> {
    match payload {
        Payload::Compressed(p) => {
            ctx.profiler.time(Stage::EntropyDecode, || codec::decode_with(src, &p, out, w, h, ctx.parallel))
        }
        Payload::Packed12Words | Payload::Packed12Msb => {
            let words = matches!(payload, Payload::Packed12Words);
            let order = if words { PackOrder::Msb32 } else { PackOrder::MsbBytes };
            if w % 2 != 0 {
                return Err(Error::unsupported_compression("ORF", "12-bit packed rows with an odd width"));
            }
            let stride = w * 3 / 2;
            require(src, stride * coded_rows)?;
            if words && stride % 4 != 0 {
                // Rows do not start on a word boundary: one continuous stream.
                return ctx.profiler.time(Stage::Unpack, || {
                    let mut p = flashraw_core::bits::BitPumpMsb32::new(src);
                    for v in out.iter_mut() {
                        *v = flashraw_core::bits::BitRead::get(&mut p, 12) as u16;
                    }
                    Ok(())
                });
            }
            let layout = flashraw_codecs::PackedLayout { width: w, height: h, bits: 12, order, row_stride: stride };
            ctx.profiler.time(Stage::Unpack, || {
                // Rows past the strip (odd-height frames) read the following file bytes.
                flashraw_codecs::unpack(src, layout, out, w, ctx.parallel, ctx.simd, true)
            })
        }
        Payload::Packed12Interlaced => {
            let stride = w * 12 / 8;
            let half = h.div_ceil(2);
            let second = (half * stride).div_ceil(2048) * 2048;
            require(src, second + stride * (coded_rows / 2))?;
            ctx.profiler.time(Stage::Unpack, || {
                par::for_each_row_chunk_bounded(out, w, par::memory_bound_workers(), ctx.parallel, |first, chunk| {
                    for (i, row) in chunk.chunks_mut(w).enumerate() {
                        let y = first + i;
                        let off = if y % 2 == 0 { y / 2 * stride } else { second + y / 2 * stride };
                        let Some(s) = src.get(off..(off + stride).min(src.len())) else { continue };
                        flashraw_codecs::packed::unpack_row(s, row, 12, PackOrder::MsbBytes, ctx.simd);
                    }
                });
            });
            Ok(())
        }
        Payload::Packed12Padded => {
            if w % 10 != 0 {
                return Err(Error::unsupported_compression("ORF", "padded 12-bit rows not a multiple of 10 pixels"));
            }
            let stride = w / 10 * 16;
            require(src, stride * coded_rows)?;
            ctx.profiler.time(Stage::Unpack, || {
                par::for_each_row_chunk_bounded(out, w, par::memory_bound_workers(), ctx.parallel, |first, chunk| {
                    for (i, row) in chunk.chunks_mut(w).enumerate() {
                        let y = first + i;
                        let Some(s) = src.get(y * stride..(y + 1) * stride) else { continue };
                        for (group, px) in s.chunks_exact(16).zip(row.chunks_exact_mut(10)) {
                            flashraw_codecs::packed::unpack_row(&group[..15], px, 12, PackOrder::LsbBytes, false);
                        }
                    }
                });
            });
            Ok(())
        }
        Payload::Unpacked { big_endian, shift } => {
            require(src, w * 2 * coded_rows)?;
            let order = if big_endian { PackOrder::MsbBytes } else { PackOrder::LsbBytes };
            ctx.profiler.time(Stage::Unpack, || {
                par::for_each_row_chunk_bounded(out, w, par::memory_bound_workers(), ctx.parallel, |first, chunk| {
                    for (i, row) in chunk.chunks_mut(w).enumerate() {
                        let y = first + i;
                        let Some(s) = src.get(y * w * 2..(y + 1) * w * 2) else { continue };
                        flashraw_codecs::packed::unpack_row(s, row, 16, order, true);
                        if shift > 0 {
                            row.iter_mut().for_each(|v| *v >>= shift);
                        }
                    }
                });
            });
            Ok(())
        }
    }
}

fn require(src: &[u8], n: usize) -> Result<()> {
    if src.len() < n {
        return Err(Error::corrupt(format!("ORF: payload truncated ({} of {n} bytes)", src.len())));
    }
    Ok(())
}

fn visible_size(model: &str, raw_w: u32, h: u32, payload: &Payload) -> (u32, u32) {
    let (mut w, mut h) = (raw_w, h);
    match raw_w {
        4100 => w -= 4,
        4080 => w -= 24,
        10400 => w -= 12,
        8200 => w -= 30,
        8180 => w -= 10,
        9280 => {
            w -= 6;
            h -= 6;
        }
        _ => {}
    }
    match model {
        "E-300" | "E-500" => w -= 20,
        "E-330" => w -= 30,
        "TG-4" => w -= 16,
        "TG-5" | "TG-6" | "TG-7" => w -= 26,
        m if m.starts_with("STYLUS1") => w -= 16,
        _ => {}
    }
    let _ = payload;
    (w.min(raw_w), h)
}

/// Reference model for Olympus EXIF names that differ from marketing names (camera database aliases).
fn normalized_model(model: &str) -> &str {
    flashraw_camera_db::base_model("Olympus", model)
}

/// First non-default orientation in IFD order.
fn file_orientation(tiff: &Tiff) -> Orientation {
    tiff.all_ifds()
        .into_iter()
        .filter_map(|i| i.u32(tags::ORIENTATION))
        .find(|&o| (2..=8).contains(&o))
        .map(Orientation::from_exif)
        .unwrap_or(Orientation::Normal)
}

/// Locate and parse the Olympus makernote and its sub-directories.
fn read_makernote(data: &[u8], tiff: &Tiff) -> Makernote {
    let mut mn = Makernote::default();
    let Some(e) = tiff.find_entry(tags::MAKER_NOTE) else { return mn };
    let start = e.value_pos;
    let len = e.data.len();
    let Some(blob) = data.get(start..start + len) else { return mn };
    // (base the offsets are relative to, byte order, IFD offset relative to base)
    let (base, order, ifd_off) = if blob.starts_with(b"OLYMPUS\0") && blob.len() > 12 {
        (start, order_of(&blob[8..10], tiff.order), 12)
    } else if blob.starts_with(b"OM SYSTEM\0") && blob.len() > 16 {
        (start, order_of(&blob[12..14], tiff.order), 16)
    } else if blob.starts_with(b"OLYMP\0") && blob.len() > 8 {
        (0, tiff.order, start + 8)
    } else {
        return mn;
    };
    let Some(scope) = data.get(base..) else { return mn };
    let opts = TiffOptions { follow_sub_ifds: false, ..Default::default() };
    let Ok(root) = flashraw_core::tiff::parse_ifd_at(scope, order, ifd_off as u32, opts) else { return mn };
    apply_root(&root, &mut mn);
    if let Some(t) = root.get(0x0100).filter(|e| e.typ == 7) {
        mn.previews.push((base + t.value_pos, t.data.len()));
    }
    for sub in [0x2010u16, 0x2020, 0x2040] {
        let Some(e) = root.get(sub) else { continue };
        let off = match e.typ {
            // Embedded directory: offsets inside are relative to the makernote base too.
            7 => e.value_pos as u32,
            _ => match e.u32(0) {
                Some(o) => o,
                None => continue,
            },
        };
        if let Ok(ifd) = flashraw_core::tiff::parse_ifd_at(scope, order, off, opts) {
            if sub == 0x2040 {
                apply_image_processing(&ifd, &mut mn);
            } else if sub == 0x2020 {
                if let (Some(o), Some(l)) = (ifd.u32(0x0101), ifd.u32(0x0102)) {
                    mn.previews.push((base + o as usize, l as usize));
                }
            }
        }
    }
    mn
}

fn order_of(b: &[u8], default: ByteOrder) -> ByteOrder {
    match b {
        b"II" => ByteOrder::Little,
        b"MM" => ByteOrder::Big,
        _ => default,
    }
}

fn u16s(e: &Entry, n: usize) -> Option<Vec<u32>> {
    let v: Vec<u32> = (0..n).map(|i| e.u32(i)).collect::<Option<_>>()?;
    Some(v)
}

/// Tags of the root makernote directory (older bodies keep calibration here).
fn apply_root(ifd: &Ifd, mn: &mut Makernote) {
    if let Some(v) = ifd.get(0x1012).and_then(|e| u16s(e, 4)) {
        mn.black_rggb = Some([v[0], v[1], v[2], v[3]]);
    }
    if let Some(v) = ifd.get(0x1017).and_then(|e| e.u32(0)) {
        mn.wb_rb[0] = Some(v);
    }
    if let Some(v) = ifd.get(0x1018).and_then(|e| e.u32(0)) {
        mn.wb_rb[1] = Some(v);
    }
    if let Some(v) = ifd.get(0x102c).and_then(|e| e.u32(0)) {
        mn.valid_bits = Some(v);
    }
}

/// ImageProcessing (0x2040) directory.
fn apply_image_processing(ifd: &Ifd, mn: &mut Makernote) {
    if let Some(v) = ifd.get(0x0100).and_then(|e| u16s(e, 2)) {
        mn.wb_rb = [Some(v[0]), Some(v[1])];
    }
    if let Some(v) = ifd.get(0x0600).and_then(|e| u16s(e, 4)) {
        mn.black_rggb = Some([v[0], v[1], v[2], v[3]]);
    }
    if let Some(v) = ifd.get(0x0611).and_then(|e| e.u32(0)) {
        mn.valid_bits = Some(v);
    }
    for (i, t) in (0x0612u16..=0x0615).enumerate() {
        mn.crop[i] = ifd.get(t).and_then(|e| e.u32(0));
    }
    for (i, t) in [
        0x0640u16, 0x0641, 0x0642, 0x0643, 0x0644, 0x0645, 0x0646, 0x0647, 0x0648, 0x0649, 0x0650, 0x0651, 0x0652,
        0x0653,
    ]
    .into_iter()
    .enumerate()
    {
        mn.coder[i] = ifd.get(t).and_then(|e| e.u32(0));
    }
}
