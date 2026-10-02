//! Sinar backs (CaptureShop `.sti` TIFF files).
//!
//! The CFA image is one 16-bit uncompressed tile (big-endian). White balance comes
//! from the "Neutral r g b" line of the Sinar text tags 0xC516/0xC517. Four-shot
//! captures (four tile offsets) are multi-shot colour images and are not supported.

use crate::common::*;
use flashraw_core::tiff::{Tiff, tags};
use flashraw_core::*;

/// "Neutral r g b" from the Sinar text tags.
fn neutral(tiff: &Tiff) -> Option<[f32; 3]> {
    for tag in [0xC516u16, 0xC517] {
        let Some(e) = tiff.find_entry(tag) else { continue };
        let text = String::from_utf8_lossy(e.bytes());
        let mut found = None;
        for line in text.split('\n') {
            if let Some(rest) = line.trim_start_matches('\r').strip_prefix("Neutral ") {
                let v: Vec<f32> = rest.split_whitespace().filter_map(|t| t.parse().ok()).take(3).collect();
                if v.len() == 3 {
                    found = Some([v[0], v[1], v[2]]);
                }
            }
        }
        if found.is_some() {
            return found;
        }
    }
    None
}

/// CFA from TIFF-EP tags, tolerating the malformed repeat-dimension tag Sinar writes.
fn cfa(raw: &flashraw_core::tiff::Ifd) -> Option<CfaPattern> {
    if let Some(p) = tiff_ep_cfa(raw) {
        return Some(p);
    }
    let pat = raw.get(tags::CFA_PATTERN_EP)?.bytes();
    if pat.len() == 4 {
        let cols: Option<Vec<CfaColor>> = pat.iter().map(|&c| CfaColor::from_dng(c)).collect();
        return CfaPattern::new(2, 2, &cols?);
    }
    None
}

/// Open a Sinar TIFF raw.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let mut meta = RawMetadata::default();
    if let Some(i0) = tiff.ifds.first() {
        flashraw_core::exif::read_ifd0(i0, &mut meta);
    }
    // The IFD carrying make/model is not necessarily IFD0 in CaptureShop files.
    for i in tiff.all_ifds() {
        if meta.make.is_empty() {
            meta.make = i.string(tags::MAKE).unwrap_or_default();
        }
        if meta.model.is_empty() {
            meta.model = i.string(tags::MODEL).unwrap_or_default();
        }
    }
    let cam = db_lookup(&mut meta, "");
    let raw = tiff
        .all_ifds()
        .into_iter()
        .filter(|i| i.u32(tags::PHOTOMETRIC) == Some(32803) && i.width() > 0 && i.height() > 0)
        .max_by_key(|i| i.width() as u64 * i.height() as u64)
        .ok_or_else(|| Error::invalid_metadata("Sinar: no CFA image"))?;
    let (w, h) = (raw.width(), raw.height());
    opts.limits.check_dims(w, h, 1)?;
    let units = if raw.has(tags::TILE_OFFSETS) { tiles(raw) } else { strips(raw) };
    if units.len() == 4 && raw.has(tags::TILE_OFFSETS) {
        return Err(Error::unsupported_compression("Sinar", "four-shot capture"));
    }
    if raw.compression() != 1 || raw.bps() != 16 {
        return Err(Error::unsupported_compression(
            "Sinar",
            format!("compression {}, {} bits", raw.compression(), raw.bps()),
        ));
    }
    let tw = raw.u32(tags::TILE_WIDTH).unwrap_or(w);
    if units.len() != 1 || tw != w {
        return Err(Error::unsupported_compression("Sinar", "multi-tile layout"));
    }
    let (off, len) = units[0];
    let src = bytes::slice_at_lenient(data, off, len.max(w as usize * h as usize * 2))?;
    let mut info = FrameInfo::new(w, h, 14);
    info.white_level = WhiteLevel::uniform(0x3fff);
    let p = cfa(raw).unwrap_or(CfaPattern::RGGB);
    info.cfa = Some(p);
    info.layout = layout_for(&p);
    info.orientation = Orientation::from_exif(raw.u32(tags::ORIENTATION).unwrap_or(1));
    if let Some(n) = neutral(&tiff) {
        info.white_balance = WhiteBalance::from_multipliers(n[0], n[1], n[2], n[1]);
    }
    apply_db(
        &mut info,
        &meta,
        cam,
        Known { cfa: true, crop: true, active_area: true, black: true, white: true, masked: true, color: false },
    );
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);
    let order = tiff.order;
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
        ctx.profiler.time(Stage::Unpack, || {
            crate::medium::unpack_u16_rows(src, order, w as usize, img.u16_mut(), w as usize * 2, ctx.parallel)
        })?;
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(RawFormat::Other, meta, info, Box::new(decode));
    dec.random_access = RandomAccess::Rows;
    dec.packed = Some(PackedPayload {
        data: src,
        width: w,
        height: h,
        bits: 16,
        order: if order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes },
        row_stride: w as usize * 2,
    });
    Ok(Box::new(dec))
}
