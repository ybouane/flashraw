//! Mamiya MEF (Mamiya ZD).
//!
//! Plain TIFF: the CFA image lives in a SubIFD as 12-bit samples packed MSB-first
//! without row padding (TIFF compression 1). Mamiya-branded Leaf backs (DM series)
//! write Leaf MOS files and are handled by the MOS decoder.

use crate::common::*;
use flashraw_core::tiff::{Tiff, tags};
use flashraw_core::*;

/// Open a Mamiya MEF.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("MEF without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let cam = db_lookup(&mut meta, "");
    let raw = tiff
        .all_ifds()
        .into_iter()
        .filter(|i| i.u32(tags::PHOTOMETRIC) == Some(32803) && i.width() > 0 && i.height() > 0)
        .max_by_key(|i| i.width() as u64 * i.height() as u64)
        .ok_or_else(|| Error::invalid_metadata("MEF: no CFA image"))?;
    let (w, h, bps) = (raw.width(), raw.height(), raw.bps());
    opts.limits.check_dims(w, h, 1)?;
    if raw.compression() != 1 || !(bps == 12 || bps == 16) {
        return Err(Error::unsupported_compression("MEF", format!("compression {}, {bps} bits", raw.compression())));
    }
    let (off, len) = *strips(raw).first().ok_or_else(|| Error::invalid_metadata("MEF: no strips"))?;
    let rb = (w as usize * bps as usize).div_ceil(8);
    let src = bytes::slice_at_lenient(data, off, len.max(rb * h as usize))?;

    let mut info = FrameInfo::new(w, h, bps as u8);
    let cfa = tiff_ep_cfa(raw).unwrap_or(CfaPattern::RGGB);
    info.cfa = Some(cfa);
    info.layout = layout_for(&cfa);
    info.orientation = Orientation::from_exif(raw.u32(tags::ORIENTATION).or(ifd0.u32(tags::ORIENTATION)).unwrap_or(1));
    apply_db(
        &mut info,
        &meta,
        cam,
        Known { cfa: true, crop: true, active_area: true, black: true, white: true, masked: true, color: false },
    );
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);
    let wu = w as usize;
    let order = if bps == 16 && tiff.order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes };
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        if src.len() < rb {
            return Err(Error::corrupt("MEF: raw data truncated"));
        }
        let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
        ctx.profiler.time(Stage::Unpack, || {
            flashraw_codecs::packed::unpack(
                src,
                flashraw_codecs::PackedLayout::tight(wu, h as usize, bps, order),
                img.u16_mut(),
                wu,
                ctx.parallel,
                ctx.simd,
                true,
            )
        })?;
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(RawFormat::Mef, meta, info, Box::new(decode));
    dec.random_access = RandomAccess::Rows;
    dec.packed = Some(PackedPayload { data: src, width: w, height: h, bits: bps, order, row_stride: rb });
    Ok(Box::new(dec))
}
