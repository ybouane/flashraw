//! Headerless sensor dumps identified by their exact file size (camera database entries
//! with a `filesize` hint).
//!
//! These files come from camera hacks (Canon CHDK `.CRW` dumps), action cameras, phones and
//! old compacts. They carry no header, so everything about them (geometry, layout flags,
//! optical-black margins, levels) is DATA in the camera database; there is no per-model table
//! in this module. What is code:
//!
//! * which entry applies (database order, `naked-*` entries before `dump-*` ones),
//! * the stored bit depth follows from the file size alone,
//! * the Samsung WB550 dump is little-endian although its entry flags big-endian
//!   (verified on a real dump),
//! * the Canon A610 and S2 IS dumps have one size and are told apart by a probe of the
//!   optical-black noise (S2 IS side verified; the A610 side is not).
//!
//! The packing of each layout is done by `flashraw_codecs::misc_packed`.

use crate::common::{Known, OpenOptions, SimpleDecoder, apply_db, db_color, db_lookup, layout_for, parse_db_cfa};
use flashraw_camera_db::Camera;
use flashraw_codecs::misc_packed::{self, PackedParams};
use flashraw_codecs::{PackedLayout, unpack};
use flashraw_core::*;

/// Find a database entry for this file size and open the file with it; `None` when no entry applies.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Option<Result<Box<dyn RawDecoder + 'a>>> {
    let len = data.len() as u64;
    let same_size = |c: &&'static Camera| number(c, "filesize") == Some(len);
    let candidates = || flashraw_camera_db::all().iter().filter(same_size);

    if let Some(entry) = candidates().find(|c| c.mode.starts_with("naked-") && !is_packed12_misfit(c, data)) {
        return Some(open_sized(data, entry, opts));
    }
    let entry = candidates().find(|c| c.mode.starts_with("dump-") && !c.no_size_match)?;
    Some(open_uniform(data, entry, opts))
}

/// Numeric database hint (decimal text).
fn number(c: &Camera, key: &str) -> Option<u64> {
    c.hint(key)?.trim().parse().ok()
}

/// Numeric hint, 0 when absent or unparsable.
fn number_or_0(c: &Camera, key: &str) -> u64 {
    number(c, key).unwrap_or(0)
}

/// A Kodak entry whose size is shared with 12-bit packed Bayer dumps and whose data reads as
/// such: it belongs to the Kodak reader's table, not to this one.
fn is_packed12_misfit(c: &Camera, data: &[u8]) -> bool {
    c.make == "Kodak"
        && c.cfa.is_none()
        && number(c, "full_width").is_some_and(|w| crate::kodak::looks_like_packed12(data, w as usize))
}

// ---------------------------------------------------------------------------------------------
// Size-table entries (`naked-<size>`)
// ---------------------------------------------------------------------------------------------

/// How the samples of a size-table dump are stored; chosen from the bits per sample the file
/// size implies.
#[derive(Debug, Clone, Copy)]
enum Storage {
    /// One byte per sample.
    Bytes,
    /// 10-bit samples, six per little-endian 64-bit word.
    Loose10,
    /// MIPI RAW10: four samples and a byte of low bits per five bytes.
    Mipi10,
    /// Generic bit-packed rows (the entry's layout flags apply).
    Packed(PackedParams),
    /// 16-bit words, optionally shifted down and/or stored bottom-up.
    Words { big_endian: bool, shift: u32, bottom_up: bool },
}

impl Storage {
    /// Row geometry used to check for truncation; `None` for layouts whose rows are irregular
    /// (padded or interlaced packed rows), which are sized by the file itself.
    fn geometry(&self, rw: usize, rh: usize) -> Option<RowGeometry> {
        match *self {
            Storage::Bytes => Some(RowGeometry::tight(rh, rw, 8)),
            Storage::Loose10 => {
                let stride = rw.div_ceil(6).saturating_mul(8);
                Some(RowGeometry { rows: rh, stride, row_bytes: stride, samples: rw })
            }
            Storage::Mipi10 => Some(RowGeometry::packed(rh, rw, 10, mipi_stride(rw))),
            Storage::Packed(p) if p.flags & 7 == 0 => Some(RowGeometry::packed(rh, rw, p.bits, p.row_bytes())),
            Storage::Packed(_) => None,
            Storage::Words { .. } => Some(RowGeometry::tight(rh, rw, 16)),
        }
    }
}

/// Row pitch of MIPI RAW10 rows: five bytes per four samples, rows padded to 8 bytes.
fn mipi_stride(width: usize) -> usize {
    width.saturating_mul(5).div_ceil(32).saturating_mul(8)
}

/// Everything the decode closure of a size-table dump needs.
struct SizedPlan {
    storage: Storage,
    offset: usize,
    width: u32,
    height: u32,
}

impl SizedPlan {
    fn decode(&self, data: &[u8], ctx: &DecodeContext) -> Result<RawImage> {
        let mut img = RawImage::new_u16(self.width, self.height, 1, &ctx.limits)?;
        let (w, h) = (self.width as usize, self.height as usize);
        let out = img.u16_mut();
        ctx.profiler.time(Stage::Unpack, || -> Result<()> {
            let geometry = self.storage.geometry(w, h);
            let mut src: &[u8] = &[];
            if let Some(g) = geometry {
                let got = ctx.sensor_bytes(data, DataUnit::Payload, self.offset, g.needed())?;
                src = got.bytes;
                if let Some(t) = got.truncation {
                    // Only whole rows are decoded: a partial row would decode padding.
                    let rows = g.complete_rows(src.len());
                    src = &src[..rows.saturating_mul(g.stride).min(src.len())];
                    ctx.recovered(CorruptReason::Truncated(t), ((h - rows) * w) as u64);
                }
            }
            match self.storage {
                Storage::Bytes => widen_bytes(src, w, out, ctx.parallel),
                Storage::Loose10 => misc_packed::loose10_rows(src, w, h, out, ctx.parallel),
                Storage::Mipi10 => misc_packed::mipi10_rows(src, w, h, mipi_stride(w), out, ctx.parallel),
                Storage::Packed(p) if geometry.is_some() => misc_packed::packed_rows(src, 0, p, out, ctx.parallel)?,
                Storage::Packed(p) => misc_packed::packed_rows(data, self.offset, p, out, ctx.parallel)?,
                Storage::Words { big_endian, shift, bottom_up } => {
                    misc_packed::u16_rows(src, w, h, big_endian, shift, bottom_up, out, ctx.parallel)
                }
            }
            Ok(())
        })?;
        Ok(img)
    }
}

/// One byte per sample, widened to 16 bits; a short source leaves the rest zero.
fn widen_bytes(src: &[u8], width: usize, out: &mut [u16], parallel: bool) {
    par::for_each_row_chunk(out, width, 32, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_mut(width).enumerate() {
            let stored = src.get((first + i) * width..).unwrap_or(&[]);
            for (o, &b) in row.iter_mut().zip(stored) {
                *o = u16::from(b);
            }
        }
    });
}

fn open_sized<'a>(data: &'a [u8], entry: &'static Camera, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let len = data.len() as u64;
    let rw = number_or_0(entry, "full_width");
    let rh = number_or_0(entry, "full_height");
    let flags = number_or_0(entry, "layout_flags") as u32;
    let shift_limit = number_or_0(entry, "shift_limit") as u32;
    // `reversed`: the data starts at the file start and its rows run bottom-up.
    let reversed = entry.hint("offset") == Some("reversed");
    let offset = if reversed { 0 } else { number_or_0(entry, "offset") };
    if rw == 0 || rh == 0 || offset >= len {
        return Err(Error::invalid_metadata("headerless dump: bad geometry"));
    }
    opts.limits.check_dims(u32::try_from(rw).unwrap_or(u32::MAX), u32::try_from(rh).unwrap_or(u32::MAX), 1)?;
    let (rw, rh) = (rw as usize, rh as usize);

    // Bits per sample follow from the byte count alone.
    let stored = len - offset;
    let bits = (stored.saturating_mul(8) / (rw as u64 * rh as u64)) as u32;
    let loose10_fits = (stored / rh as u64).saturating_mul(3) >= rw as u64 * 4;
    let offset = offset as usize;

    let wb550 = entry.make == "Samsung" && entry.model == "WB550";
    let (storage, depth) = match bits {
        8 => (Storage::Bytes, 8),
        10 if loose10_fits => (Storage::Loose10, 10),
        10 if flags & 1 != 0 => (Storage::Mipi10, 10),
        10 | 12 => (Storage::Packed(PackedParams { width: rw, height: rh, bits, flags: flags | 128 }), bits),
        16 => {
            let shift = (flags >> 1) & 7;
            let big_endian = flags & 1 != 0 && !wb550;
            let depth = 16u32.saturating_sub(flags >> 4).saturating_sub(shift);
            (Storage::Words { big_endian, shift, bottom_up: reversed }, depth)
        }
        _ => return Err(Error::unsupported_compression("headerless raw", format!("{bits} bits per sample"))),
    };
    if depth == 0 || depth > 16 {
        return Err(Error::corrupt(format!("headerless dump: {depth} significant bits")));
    }

    // Identification.
    let mut model = entry.model;
    if entry.make == "Canon" && entry.model == "PowerShot A610" && is_s2is(data) {
        model = "PowerShot S2 IS";
    }
    let mut meta = RawMetadata::new(entry.make, model);
    let _ = db_lookup(&mut meta, entry.mode);
    meta.canonical_make = if entry.canonical_make.is_empty() { entry.make } else { entry.canonical_make }.to_string();
    meta.canonical_model = model.to_string();
    meta.set_extra("identified_by", "file size");

    // Frame description.
    let mut info = FrameInfo::new(rw as u32, rh as u32, depth as u8);
    info.orientation = super::orientation_from_flip(number_or_0(entry, "flip") as u32);
    info.compression = Compression::Uncompressed;

    let [cx, cy, cw, ch] = entry.crop.unwrap_or([0, 0, rw as i32, rh as i32]).map(|v| v.max(0) as u32);
    let pattern = if entry.hint("monochrome").is_some() { None } else { entry.cfa.and_then(parse_db_cfa) };
    let mut area = Rect::new(cx, cy, cw, ch);
    if pattern.is_some() {
        // Keep the CFA phase at the origin of the active area even.
        if cx & 1 != 0 {
            area.x = cx.saturating_add(1);
            area.width = cw.saturating_sub(1);
        }
        if cy & 1 != 0 {
            area.y = cy.saturating_add(1);
            area.height = ch.saturating_sub(1);
        }
    }
    info.active_area = area;
    info.crop = area;
    match pattern {
        Some(p) => {
            // The database gives the pattern as seen from the crop origin; the frame wants it at (0, 0).
            let dx = (p.width() - cx as usize % p.width()) % p.width();
            let dy = (p.height() - cy as usize % p.height()) % p.height();
            let anchored = p.shifted(dx, dy);
            info.layout = layout_for(&anchored);
            info.cfa = Some(anchored);
        }
        None => info.layout = SensorLayout::Monochrome,
    }

    // Levels: the fallback colour table first, the entry's own sensor data over it.
    let fallback = flashraw_camera_db::find_matrix(&meta.canonical_make, &meta.canonical_model);
    let mut black = fallback.map_or(0, |f| f.black.max(0)) as f32;
    let mut white = match storage {
        Storage::Bytes => 255,
        _ => (1u32 << depth) - (1u32 << shift_limit.min(depth)),
    };
    if let Some(f) = fallback.filter(|f| f.white > 0) {
        white = f.white as u32;
    }
    if let Some(s) = entry.sensor(0) {
        if s.black >= 0 {
            black = s.black as f32;
        }
        if s.white > 0 {
            white = s.white as u32;
        }
    }
    info.black_level = BlackLevel::uniform(black);
    info.white_level = WhiteLevel::uniform(white);
    info.color_matrices = db_color(&meta, None);

    // Optical-black strips beside the active area.
    let strips = (matches!(storage, Storage::Bytes) && !model.starts_with("DC2"))
        || matches!(storage, Storage::Packed(p) if p.flags & 32 != 0);
    if strips {
        let trim = number_or_0(entry, "right_mask_columns").min(u64::from(u32::MAX)) as u32;
        let a = info.active_area;
        let right_x = a.x.saturating_add(a.width);
        let left = Rect::new(0, a.y, a.x, a.height);
        let right = Rect::new(right_x, a.y, (rw as u32).saturating_sub(right_x).saturating_sub(trim), a.height);
        info.masked_areas = [left, right].into_iter().filter(|r| r.width > 0 && r.height > 0).collect();
        info.black_from_masked = !info.masked_areas.is_empty();
    }

    let plan = SizedPlan { storage, offset, width: rw as u32, height: rh as u32 };
    let decode = move |ctx: &DecodeContext| plan.decode(data, ctx);
    let mut dec = SimpleDecoder::new(RawFormat::Nakedraw, meta, info, Box::new(decode));
    dec.random_access = RandomAccess::Rows;
    Ok(Box::new(dec))
}

/// Canon A610 and S2 IS dumps are the same size. The S2 IS stores optical-black noise in the
/// last 56 bytes of each 3340-byte row, the A610 leaves them zero; sampled on the first rows.
fn is_s2is(data: &[u8]) -> bool {
    (0..100usize).any(|row| data.get(row * 3340 + 3284).is_some_and(|&b| b > 15))
}

// ---------------------------------------------------------------------------------------------
// Uniform layout entries (`dump-<layout>-<size>`)
// ---------------------------------------------------------------------------------------------

fn open_uniform<'a>(data: &'a [u8], entry: &'static Camera, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let len = data.len() as u64;
    let w = number_or_0(entry, "full_width");
    let h = number_or_0(entry, "full_height");
    let offset = number_or_0(entry, "offset");
    if w == 0 || h == 0 || offset >= len {
        return Err(Error::invalid_metadata("headerless dump: bad geometry"));
    }
    opts.limits.check_dims(u32::try_from(w).unwrap_or(u32::MAX), u32::try_from(h).unwrap_or(u32::MAX), 1)?;
    let bits = number(entry, "bits").unwrap_or_else(|| (len - offset).saturating_mul(8) / w / h);
    if !(1..=16).contains(&bits) {
        return Err(Error::unsupported_compression("headerless raw", format!("{bits} bits per sample")));
    }
    let bits = bits as u32;
    let mut order = match entry.hint("order") {
        Some("plain") => PackOrder::LsbBytes,
        Some("jpeg") => PackOrder::MsbBytes,
        Some("jpeg32") => PackOrder::Msb32,
        Some("jpeg16") | None => PackOrder::Msb16,
        Some(other) => return Err(Error::invalid_metadata(format!("unknown bit order {other:?}"))),
    };
    if bits == 16 && !matches!(order, PackOrder::MsbBytes) {
        order = PackOrder::LsbBytes;
    }
    if bits == 8 {
        order = PackOrder::MsbBytes;
    }
    let (w, h, offset) = (w as usize, h as usize, offset as usize);

    let mut meta = RawMetadata::new(entry.make, entry.model);
    let db = db_lookup(&mut meta, entry.mode);
    if meta.canonical_model.is_empty() {
        meta.canonical_model = entry.canonical_model.to_string();
    }
    meta.set_extra("identified_by", "file size");
    let mut info = FrameInfo::new(w as u32, h as u32, bits as u8);
    info.compression = Compression::Uncompressed;
    apply_db(&mut info, &meta, Some(db.unwrap_or(entry)), Known::default());

    let layout = PackedLayout::tight(w, h, bits, order);
    let stride = layout.row_stride;
    let word_order = matches!(order, PackOrder::Msb16 | PackOrder::Msb32);
    let geometry =
        if word_order { RowGeometry::packed(h, stride, 8, stride) } else { RowGeometry::packed(h, w, bits, stride) };
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(w as u32, h as u32, 1, &ctx.limits)?;
        ctx.profiler.time(Stage::Unpack, || -> Result<()> {
            let got = ctx.sensor_bytes(data, DataUnit::Payload, offset, geometry.needed())?;
            let mut src = got.bytes;
            let truncated = got.truncation.is_some();
            if let Some(t) = got.truncation {
                let rows = geometry.complete_rows(src.len());
                src = &src[..rows.saturating_mul(geometry.stride).min(src.len())];
                ctx.recovered(CorruptReason::Truncated(t), ((h - rows) * w) as u64);
            }
            unpack(src, layout, img.u16_mut(), w, ctx.parallel, ctx.simd, truncated)
        })?;
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(RawFormat::Nakedraw, meta, info, Box::new(decode));
    dec.random_access = RandomAccess::Rows;
    dec.packed = Some(PackedPayload {
        data: &data[offset..],
        width: w as u32,
        height: h as u32,
        bits,
        order,
        row_stride: stride,
    });
    Ok(Box::new(dec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{Lcg, check_golden, corpus};

    fn entry(mode: &str) -> &'static Camera {
        flashraw_camera_db::all().iter().find(|c| c.mode == mode).unwrap_or_else(|| panic!("no entry {mode}"))
    }

    fn size(c: &Camera) -> usize {
        number(c, "filesize").unwrap() as usize
    }

    fn open_data(data: &[u8]) -> Box<dyn RawDecoder + '_> {
        open(data, &OpenOptions::default()).expect("a database size").expect("opens")
    }

    fn decode(d: &dyn RawDecoder, recover: bool) -> (Result<RawImage>, Vec<DecodeWarning>) {
        let p = Profiler::default();
        let mut ctx = DecodeContext::new(&p);
        ctx.recover = recover;
        ctx.parallel = false;
        let r = d.decode(&ctx);
        (r, ctx.take_warnings())
    }

    fn samples(img: &RawImage) -> Vec<u16> {
        img.u16().to_vec()
    }

    /// What an ideal reader of 16-bit words does with `bytes`.
    fn words(bytes: &[u8], big_endian: bool, shift: u32) -> Vec<u16> {
        bytes
            .chunks_exact(2)
            .map(|b| if big_endian { u16::from_be_bytes([b[0], b[1]]) } else { u16::from_le_bytes([b[0], b[1]]) } >> shift)
            .collect()
    }

    #[test]
    fn sizes_select_entries() {
        assert!(open(&[0u8; 1000], &OpenOptions::default()).is_none());
        assert!(open(&[], &OpenOptions::default()).is_none());
        // naked-* entries come before dump-* ones of the same size.
        let c = entry("naked-1409024");
        assert!(open(&vec![0u8; size(c)], &OpenOptions::default()).is_some());
        // A dump entry without a naked twin: Kodak DC20.
        let dc20 = entry("dump-bytes-62464");
        let data = vec![0u8; size(dc20)];
        let d = open_data(&data);
        assert_eq!(d.metadata().extra("identified_by"), Some("file size"));
        assert_eq!(d.metadata().extra("mode"), Some("dump-bytes-62464"));
        // One byte off: no entry.
        assert!(open(&vec![0u8; size(c) + 1], &OpenOptions::default()).is_none());
        // Every size of the database opens (or answers with an error) without panicking and, when it
        // opens, decodes without panicking.
        let mut rng = Lcg(11);
        for c in flashraw_camera_db::all() {
            let Some(n) = number(c, "filesize").map(|n| n as usize).filter(|&n| n <= 3_000_000) else { continue };
            let data = rng.bytes(n);
            if let Some(Ok(d)) = open(&data, &OpenOptions::default()) {
                let _ = decode(d.as_ref(), true);
                let _ = decode(d.as_ref(), false);
            }
        }
    }

    #[test]
    fn eight_bit_dumps_are_bytes() {
        let c = entry("naked-307200");
        let data: Vec<u8> = (0..size(c)).map(|i| (i * 7 % 251) as u8).collect();
        let d = open_data(&data);
        let f = d.frame_info();
        assert_eq!((f.width, f.height, f.bits_per_sample, f.cpp), (640, 480, 8, 1));
        assert_eq!(f.white_level.max(), 255);
        assert_eq!(d.format(), RawFormat::Nakedraw);
        assert_eq!(d.random_access(), RandomAccess::Rows);
        assert_eq!(d.metadata().extra("identified_by"), Some("file size"));
        let (img, w) = decode(d.as_ref(), false);
        assert!(w.is_empty());
        assert_eq!(samples(&img.unwrap()), data.iter().map(|&b| u16::from(b)).collect::<Vec<_>>());
    }

    /// Sample value used by the packing tests.
    fn pattern(x: usize, y: usize) -> u16 {
        ((x * 7 + y * 13) & 1023) as u16
    }

    #[test]
    fn loose_ten_bit_rows_hold_six_samples_per_word() {
        let c = entry("naked-3170304");
        let (w, h) = (1976usize, 1200usize);
        let stride = w.div_ceil(6) * 8;
        let mut data = vec![0u8; size(c)];
        for y in 0..h {
            for x in 0..w {
                let word = y * stride + x / 6 * 8;
                let mut v = u64::from_le_bytes(data[word..word + 8].try_into().unwrap());
                v |= u64::from(pattern(x, y)) << (10 * (x % 6));
                data[word..word + 8].copy_from_slice(&v.to_le_bytes());
            }
        }
        let d = open_data(&data);
        assert_eq!(d.frame_info().bits_per_sample, 10);
        let img = decode(d.as_ref(), false).0.unwrap();
        let got = samples(&img);
        assert!((0..h).all(|y| (0..w).all(|x| got[y * w + x] == pattern(x, y))));
    }

    #[test]
    fn mipi_ten_bit_rows_hold_four_samples_per_five_bytes() {
        let c = entry("naked-2969600");
        let (w, h) = (1976usize, 1200usize);
        let stride = (5 * w).div_ceil(32) * 8;
        let mut data = vec![0u8; size(c)];
        for y in 0..h {
            for x in 0..w {
                let at = y * stride + x / 4 * 5;
                data[at + x % 4] = (pattern(x, y) >> 2) as u8;
                data[at + 4] |= ((pattern(x, y) & 3) as u8) << (2 * (x % 4));
            }
        }
        let img = decode(open_data(&data).as_ref(), false).0.unwrap();
        let got = samples(&img);
        assert!((0..h).all(|y| (0..w).all(|x| got[y * w + x] == pattern(x, y))));
    }

    #[test]
    fn sixteen_bit_words_follow_the_entry_flags() {
        // The flags of a big-endian entry: bit 0.
        let c = entry("naked-2658304");
        let flags = number(c, "layout_flags").unwrap() as u32;
        assert_eq!(flags & 1, 1);
        let mut rng = Lcg(5);
        let data = rng.bytes(size(c));
        let d = open_data(&data);
        let shift = (flags >> 1) & 7;
        assert_eq!(d.frame_info().bits_per_sample as u32, 16 - (flags >> 4) - shift);
        let img = decode(d.as_ref(), false).0.unwrap();
        // The file is a few rows longer than the raster; the rest is ignored.
        let got = samples(&img);
        assert_eq!(got, words(&data, true, shift)[..got.len()]);
    }

    #[test]
    fn the_samsung_wb550_is_little_endian() {
        let c = entry("naked-24000000");
        assert_eq!((c.make, c.model), ("Samsung", "WB550"));
        let flags = number(c, "layout_flags").unwrap() as u32;
        assert_eq!(flags & 1, 1, "the entry flags big-endian");
        let shift = (flags >> 1) & 7;
        let data = Lcg(9).bytes(size(c));
        let d = open_data(&data);
        let f = d.frame_info();
        assert_eq!(f.bits_per_sample as u32, 16 - (flags >> 4) - shift);
        assert_eq!(
            f.white_level.max(),
            (1 << f.bits_per_sample) - (1 << number(c, "shift_limit").unwrap().min(f.bits_per_sample as u64))
        );
        let img = decode(d.as_ref(), false).0.unwrap();
        assert_eq!(samples(&img), words(&data, false, shift));
    }

    #[test]
    fn canon_packed_dumps_have_masked_strips_and_an_anchored_cfa() {
        // A620-class dump: 10-bit packed rows (flags 40), margins of the sensor around the active area.
        let c = entry("naked-5298000");
        let [cx, cy, cw, ch] = c.crop.unwrap().map(|v| v as u32);
        let data = vec![0xffu8; size(c)];
        let d = open_data(&data);
        let f = d.frame_info();
        assert_eq!((f.width, f.height, f.bits_per_sample), (2400, 1766, 10));
        assert_eq!(f.active_area, Rect::new(cx + cx % 2, cy + cy % 2, cw - cx % 2, ch - cy % 2));
        assert_eq!(f.crop, f.active_area);
        let trim = number(c, "right_mask_columns").unwrap_or(0) as u32;
        let a = f.active_area;
        let mut strips =
            vec![Rect::new(0, a.y, a.x, a.height), Rect::new(a.right(), a.y, 2400 - a.right() - trim, a.height)];
        strips.retain(|r| r.width > 0);
        assert_eq!(f.masked_areas, strips);
        assert!(f.black_from_masked);
        // The pattern database gives at the crop origin appears unchanged there.
        let db = parse_db_cfa(c.cfa.unwrap()).unwrap();
        let cfa = f.cfa.unwrap();
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(cfa.color_at(cx as usize + x, cy as usize + y), db.color_at(x, y));
            }
        }
        // All-ones data unpacks to full scale.
        let img = decode(d.as_ref(), false).0.unwrap();
        assert!(samples(&img).iter().all(|&v| v == 1023));
        assert_eq!(f.white_level.max(), 1023);
    }

    #[test]
    fn cfa_phase_and_strips_hold_for_every_small_dump() {
        for c in flashraw_camera_db::all().iter().filter(|c| c.mode.starts_with("naked-")) {
            let Some(n) = number(c, "filesize").map(|n| n as usize).filter(|&n| n <= 12_000_000) else { continue };
            let data = vec![0u8; n];
            let Some(Ok(d)) = open(&data, &OpenOptions::default()) else { continue };
            let f = d.frame_info();
            if d.metadata().extra("mode") != Some(c.mode) {
                continue; // another entry of the same size answered
            }
            if let (Some(db), Some(cfa)) = (c.cfa.and_then(parse_db_cfa), f.cfa) {
                let [cx, cy, ..] = c.crop.unwrap_or([0; 4]).map(|v| v.max(0) as usize);
                let (pw, ph) = (db.width(), db.height());
                for y in 0..2 * ph {
                    for x in 0..2 * pw {
                        let (dx, dy) = ((x + pw * 64 - cx % pw) % pw, (y + ph * 64 - cy % ph) % ph);
                        assert_eq!(cfa.color_at(x, y), db.color_at(dx, dy), "{} {x},{y}", c.mode);
                    }
                }
                // The active area starts on an even pixel so that the pattern phase is even there.
                assert!(f.active_area.x % 2 == 0 || f.active_area.x == 0 && cx == 0, "{}", c.mode);
                assert!(f.active_area.y % 2 == 0, "{}", c.mode);
            }
            for r in &f.masked_areas {
                assert!(r.intersect(&f.active_area).is_none(), "{} {r:?}", c.mode);
            }
        }
    }

    #[test]
    fn the_s2_is_probe_looks_at_the_first_hundred_rows() {
        let c = entry("naked-6573120");
        let probe_at = |row: usize, v: u8| {
            let mut data = vec![0u8; size(c)];
            data[row * 3340 + 3284] = v;
            open_data(&data).metadata().model.clone()
        };
        assert_eq!(probe_at(0, 0), "PowerShot A610");
        assert_eq!(probe_at(0, 15), "PowerShot A610");
        for row in [0, 1, 50, 99] {
            assert_eq!(probe_at(row, 16), "PowerShot S2 IS", "row {row}");
            assert_eq!(probe_at(row, 255), "PowerShot S2 IS", "row {row}");
        }
        for row in [100, 101, 300, 1500, 1967] {
            assert_eq!(probe_at(row, 255), "PowerShot A610", "row {row}");
        }
        let d = {
            let mut data = vec![0u8; size(c)];
            data[3284] = 20;
            let d = open_data(&data);
            (d.metadata().canonical_model.clone(), d.metadata().extra("mode").map(String::from))
        };
        assert_eq!(d.0, "PowerShot S2 IS");
    }

    #[test]
    fn truncated_rows_are_decoded_whole_or_not_at_all() {
        let (w, h) = (4u32, 3u32);
        let plan = SizedPlan {
            storage: Storage::Words { big_endian: false, shift: 0, bottom_up: false },
            offset: 2,
            width: w,
            height: h,
        };
        let mut data = vec![0xAAu8, 0xBB];
        for i in 0..(w * h) {
            data.extend_from_slice(&(100 + i as u16).to_le_bytes());
        }
        let run = |data: &[u8], recover: bool| {
            let p = Profiler::default();
            let mut ctx = DecodeContext::new(&p);
            ctx.recover = recover;
            let r = plan.decode(data, &ctx);
            (r, ctx.take_warnings())
        };
        let (full, warn) = run(&data, false);
        assert_eq!(full.unwrap().u16(), (0..12).map(|i| 100 + i as u16).collect::<Vec<_>>());
        assert!(warn.is_empty());
        // Cut inside row 1: row 0 only.
        let cut = &data[..2 + 2 * 4 + 3];
        let (r, warn) = run(cut, true);
        let img = r.unwrap();
        assert_eq!(&img.u16()[..4], &[100, 101, 102, 103]);
        assert!(img.u16()[4..].iter().all(|&v| v == 0));
        assert_eq!(warn.len(), 1);
        assert_eq!(warn[0].samples_filled, 8);
        let e = run(cut, false).0.err().unwrap();
        assert_eq!(e.kind(), "CorruptFile");
        assert!(e.truncation().is_some());
        // Not even one row: everything is missing.
        let (r, warn) = run(&data[..4], true);
        assert!(r.unwrap().u16().iter().all(|&v| v == 0));
        assert_eq!(warn[0].samples_filled, 12);
        // The other layouts follow the same rule.
        for storage in [
            Storage::Bytes,
            Storage::Loose10,
            Storage::Mipi10,
            Storage::Packed(PackedParams { width: 4, height: 3, bits: 12, flags: 128 }),
        ] {
            let plan = SizedPlan { storage, offset: 0, width: 4, height: 3 };
            let p = Profiler::default();
            let mut ctx = DecodeContext::new(&p);
            ctx.recover = true;
            let g = storage.geometry(4, 3).unwrap();
            let bytes = vec![0xffu8; g.needed()];
            let cut = &bytes[..g.stride + g.row_bytes / 2];
            let img = plan.decode(cut, &ctx).unwrap();
            assert!(img.u16()[8..].iter().all(|&v| v == 0), "{storage:?}");
            assert_eq!(ctx.take_warnings().len(), 1, "{storage:?}");
        }
    }

    #[test]
    fn uniform_packed_entries_choose_the_bit_order() {
        // Kodak DC20: 8 bits, forced to most-significant-bit-first bytes.
        let dc20 = entry("dump-bytes-62464");
        let data: Vec<u8> = (0..size(dc20)).map(|i| (i % 253) as u8).collect();
        let d = open_data(&data);
        let p = d.packed_payload().unwrap();
        assert_eq!(
            (p.bits, p.order, p.row_stride, p.data.len()),
            (8, PackOrder::MsbBytes, p.width as usize, data.len())
        );
        let img = decode(d.as_ref(), false).0.unwrap();
        assert_eq!(samples(&img), data.iter().map(|&b| u16::from(b)).collect::<Vec<_>>());
        // 16 bits without an explicit byte order read as little-endian bytes.
        let pl = flashraw_camera_db::all().iter().find(|c| c.make == "Paralenz").unwrap();
        let data = Lcg(2).bytes(size(pl));
        let d = open_data(&data);
        let p = d.packed_payload().unwrap();
        assert_eq!((p.bits, p.order), (16, PackOrder::LsbBytes));
        let img = decode(d.as_ref(), false).0.unwrap();
        assert_eq!(samples(&img), words(&data, false, 0));
    }

    // -----------------------------------------------------------------------------------------
    // Real dumps (need the corpus; skipped without it)
    // -----------------------------------------------------------------------------------------

    /// Paths of the dumps this reader answers for, taken from the regression rows.
    fn real_dumps() -> Vec<&'static str> {
        include_str!("../../../../tests/regression/misc.tsv")
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split('\t').next())
            .collect()
    }

    #[test]
    #[ignore = "needs the corpus"]
    fn golden_fingerprints() {
        let print = std::env::var_os("FLASHRAW_PRINT_GOLDEN").is_some();
        let mine: Vec<(&str, &str)> = real_dumps()
            .into_iter()
            .filter(|rel| {
                corpus(rel).is_some_and(|d| {
                    crate::open(&d, &OpenOptions::default())
                        .is_ok_and(|d| d.metadata().extra("identified_by") == Some("file size"))
                })
            })
            .map(|rel| (rel, ""))
            .collect();
        if print {
            check_golden(&mine);
        } else {
            check_golden(&GOLDEN_FINGERPRINTS);
        }
    }

    /// Fingerprints (format, metadata, frame description, packed payload) of the real dumps.
    const GOLDEN_FINGERPRINTS: [(&str, &str); 44] = [
        ("Canon/A570 IS/CRW_2554.CRW", "584b2409d09714d6"),
        ("Canon/Canon IXUS900Ti/RAW_CANON_IXUS900TI_CHDK.CRW", "219e70a28941891b"),
        ("Canon/Canon PowerShot A540/CANON_A540_CHDK_CRW_0015.CRW", "0db17bddb8eefdc3"),
        ("Canon/Canon PowerShot A620/Canon_PowerShot_A620_CHDK_1.0.0.1892.CRW", "584b2409d09714d6"),
        ("Canon/Canon PowerShot A630/RAW_CANON_POWERSHOT_A630_CHDK.CRW", "8226a13b84c223b8"),
        ("Canon/Canon PowerShot A710 IS/RAW_CANON_A710.CRW", "584b2409d09714d6"),
        ("Canon/Canon PowerShot ELPH 130 IS/CANON_IXUS140_ELPH130_CHDK_CRW_5646.CRW", "9f1ff09c367d3f91"),
        ("Canon/Canon PowerShot ELPH 180/CANON_IXUS175_ELPH180_CHDK_CRW_0195.CRW", "2fab9b349655df9a"),
        ("Canon/Canon PowerShot S2 IS/RAW_CANON_S2IS.CRW", "0e77271de7ae8f5e"),
        ("Canon/Canon PowerShot S3IS/CRW_8760.CRW", "0db17bddb8eefdc3"),
        ("Canon/Canon PowerShot S5 IS/RAW_CANON_S5IS.CRW", "a3296d91cfa92ff6"),
        ("Canon/Canon PowerShot SD750/RAW_CANON_SD750.CRW", "584b2409d09714d6"),
        ("Canon/Canon Powershot A650/RAW_CANON_POWERSHOT_A650_CHDK.CRW", "6dbda3b728931270"),
        ("Canon/PowerShot A550/CRW_0007.CRW", "584b2409d09714d6"),
        ("Canon/PowerShot D10/CANON_D10_CHDK_CRW_0627.CRW", "7e87c37e210c4d02"),
        ("Canon/PowerShot S100/CRW_1376.CRW", "642105531de1a01c"),
        ("Canon/PowerShot SX110 IS/CRW_5178.CRW", "b7487a0edf5fc589"),
        ("Canon/PowerShot SX160 IS/CANON_SX160IS_CHDK_CRW_5151.CRW", "9f1ff09c367d3f91"),
        ("Canon/PowerShot SX20 IS/CRW_6488.CRW", "730bb71757c37105"),
        ("Canon/PowerShot SX220 HS/CRW_7732.CRW", "6b6d66555f8f20aa"),
        ("Canon/PowerShot SX260 HS/CRW_3026.CRW", "7da969d8ce47e904"),
        ("Canon/PowerShot SX710 HS/CANON_SX710HS_CHDK_CRW_0717.CRW", "4f904fa64aa6e990"),
        ("Canon/Powershot A640/CRW_0441.CRW", "219e70a28941891b"),
        ("Canon/Powershot A720IS/IMG_2115.CRW", "a3296d91cfa92ff6"),
        ("Canon/Poweshot S3IS/RAW_CANON_S3IS.CRW", "0db17bddb8eefdc3"),
        ("Canon/ixus 75/2008-06-10_Moscow_0012.CRW", "584b2409d09714d6"),
        ("GITUP/GIT2/GITUP-GIT2-16:9.RAW", "bd1b00daf13ad7fc"),
        ("GITUP/GIT2/GITUP-GIT2-4:3.RAW", "25f28ba807e136ff"),
        ("GITUP/GIT2P/git2p_169.raw", "bacc5116875740eb"),
        ("GITUP/GIT2P/git2p_43.raw", "2cc0ebd4348e15fd"),
        ("Gitup/G3 Duo 170/Gitup_G3_duo_170°.RAW", "139e389e30a5573a"),
        ("Pentax/Pentax Optio S4/RAW_PENTAX_OPTIOS4.RAW", "4e5468599d017bec"),
        ("SJCAM/M20/2016_0101_013059_001.RAW", "bd1b00daf13ad7fc"),
        ("SJCAM/SJ6 LEGEND/2018_0107_140544_001.RAW", "bacc5116875740eb"),
        ("SJCam/5000X Elite/2015_0101_035947_003.RAW", "cf34cd4fbe7938fd"),
        ("SJCam/5000X Elite/2016_0104_005847_665To_send.RAW", "cf34cd4fbe7938fd"),
        ("Xiaomi/Yi/YDXJ0309.RAW", "25f28ba807e136ff"),
        ("Canon/IXY 220F/canon_ixy_220f.CRW", "f84174c2941a94e4"),
        ("ImBack/ImB35mm/ttx.raw", "92370e6a3424926b"),
        ("Paralenz/+/IMG_0005.RAW", "201c458b7622097c"),
        ("Paralenz/Dive Camera/IMG_0005.RAW", "201c458b7622097c"),
        ("SJCAM/SJ6 LEGEND/2018_0107_140610_003.RAW", "e273e59d6133c5b1"),
        ("SJCAM/SJ6 LEGEND/2018_0107_140646_005.RAW", "d4b16f3da8d04270"),
        ("Xiro/Xplorer V/2017_0908_092308_001.RAW", "62b36174666e6972"),
    ];
}
