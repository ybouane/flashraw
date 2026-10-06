//! SRF: the encrypted raw format of the DSC-F828 and DSC-V3.
//!
//! IFD0 describes the sensor size but not where the data is. The data sits at a
//! fixed offset and is encrypted with the Sony key stream ([`super::crypt`]). The
//! raw key is itself encrypted: a pointer byte at 200896 locates a big-endian master
//! key, which decrypts a 40-byte header at 164600 whose bytes 22..26 hold the data
//! key. Samples are big-endian 16-bit (14 significant bits). The F828 has an RGBE
//! (emerald) four-colour filter array.

use super::crypt::{SonyCipher, keystream};
use crate::common::*;
use flashraw_core::tiff::{Tiff, tags};
use flashraw_core::*;

const KEY_POINTER: usize = 200896;
const HEADER_OFFSET: usize = 164600;

struct Model {
    name: &'static str,
    data_offset: usize,
    /// Visible area within the raw buffer (before even-phase alignment).
    left: u32,
    width: u32,
    /// Optical-black columns: `[mask_left, left)` and `[left + width, raw_width - mask_right_trim)`.
    mask_left: u32,
    mask_right_trim: u32,
}

const MODELS: [Model; 2] = [
    Model { name: "DSC-F828", data_offset: 862144, left: 5, width: 3288, mask_left: 0, mask_right_trim: 17 },
    Model { name: "DSC-V3", data_offset: 787392, left: 59, width: 3109, mask_left: 9, mask_right_trim: 0 },
];

pub fn is_srf(model: &str) -> bool {
    MODELS.iter().any(|m| m.name == model)
}

/// Recover the raw-data key from the encrypted header.
fn data_key(data: &[u8]) -> Result<u32> {
    let p = *data.get(KEY_POINTER).ok_or_else(|| Error::corrupt("SRF: key pointer beyond file"))? as usize;
    let k = flashraw_core::bytes::slice_at(data, KEY_POINTER + 4 * p, 4)?;
    let master = u32::from_be_bytes([k[0], k[1], k[2], k[3]]);
    let mut head = flashraw_core::bytes::slice_at(data, HEADER_OFFSET, 40)?.to_vec();
    SonyCipher::new(master).apply(&mut head);
    Ok(u32::from_le_bytes([head[22], head[23], head[24], head[25]]))
}

pub fn open<'a>(
    data: &'a [u8],
    tiff: &Tiff<'a>,
    mut meta: RawMetadata,
    opts: &OpenOptions,
) -> Result<Box<dyn RawDecoder + 'a>> {
    let m =
        MODELS.iter().find(|m| m.name == meta.model).ok_or_else(|| Error::invalid_metadata("SRF: unknown model"))?;
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("SRF: no IFD0"))?;
    let (width, height) = (ifd0.width(), ifd0.height());
    if width == 0 || height == 0 || width > 3360 || height > 2460 || width % 2 != 0 {
        return Err(Error::invalid_metadata(format!("SRF: unexpected dimensions {width}x{height}")));
    }
    opts.limits.check_dims(width, height, 1)?;
    let key = data_key(data)?;
    db_lookup(&mut meta, "srf");
    let mut info = FrameInfo::new(width, height, 14);
    info.compression = Compression::Uncompressed;
    // The image starts on an odd column; keep the visible
    // window on an even CFA phase by dropping its first column.
    let left = (m.left + (m.left & 1)).min(width);
    let vis_w = (m.width - (m.left & 1)).min(width - left);
    let vis = Rect::new(left, 0, vis_w, height);
    info.cfa = Some(if m.name == "DSC-F828" {
        // RGBE ("emerald") filter.
        info.layout = SensorLayout::OtherCfa;
        CfaPattern::new(2, 2, &[CfaColor::Emerald, CfaColor::Red, CfaColor::Blue, CfaColor::Green])
            .ok_or_else(|| Error::invalid_metadata("SRF: invalid CFA"))?
    } else {
        CfaPattern::GRBG
    });
    info.active_area = vis;
    info.crop = vis;
    // Black is measured from the optical-black columns left and right of the image.
    info.masked_areas = vec![
        Rect::new(m.mask_left, 0, left.saturating_sub(m.mask_left), height),
        Rect::new(left + vis_w, 0, width.saturating_sub(m.mask_right_trim + left + vis_w), height),
    ];
    info.black_from_masked = true;
    info.white_level = WhiteLevel::uniform(0x3ff0);
    info.orientation = Orientation::from_exif(ifd0.u32(tags::ORIENTATION).unwrap_or(1));
    info.color_matrices = super::table::color(super::table::lookup("Sony", m.name), &meta);
    meta.previews = tiff_previews(data, 0, tiff, info.orientation);
    let dec = SimpleDecoder::new(
        RawFormat::Arw,
        meta,
        info,
        Box::new(move |ctx| decode(data, m.data_offset, key, width, height, ctx)),
    );
    Ok(Box::new(dec))
}

/// Decrypt and unpack the SRF samples at `offset` of `data`. The key stream is generated
/// in bulk (it is sequential but cheap); XOR and byte swapping then run per row in
/// parallel. Truncated data fails (strict) or leaves the missing samples zero (recover).
pub fn decode(data: &[u8], offset: usize, key: u32, width: u32, height: u32, ctx: &DecodeContext) -> Result<RawImage> {
    let mut img = RawImage::new_u16(width, height, 1, &ctx.limits)?;
    let w = width as usize;
    if w % 2 != 0 {
        return Err(Error::corrupt("SRF: odd width"));
    }
    // Samples are decrypted in 32-bit pairs: a partial pair is not usable.
    let geometry = RowGeometry { rows: height as usize, stride: w * 2, row_bytes: w * 2, samples: w };
    let src = ctx.sensor_bytes(data, DataUnit::Payload, offset, geometry.needed())?;
    let (truncation, src) = (src.truncation, src.bytes);
    let ks = ctx.profiler.time(Stage::Unpack, || keystream(key, w / 2 * height as usize));
    let bad = std::sync::atomic::AtomicUsize::new(0);
    ctx.profiler.time(Stage::Unpack, || {
        par::for_each_row_chunk_bounded(img.u16_mut(), w, par::memory_bound_workers(), ctx.parallel, |first, chunk| {
            let mut nbad = 0;
            for (i, out) in chunk.chunks_exact_mut(w).enumerate() {
                let y = first + i;
                let s = src.get(y * w * 2..).unwrap_or(&[]);
                let s = &s[..(w * 2).min(s.len())];
                let k = &ks[y * w / 2..(y + 1) * w / 2];
                for ((o, b), &k) in out.chunks_exact_mut(2).zip(s.chunks_exact(4)).zip(k) {
                    let v = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) ^ k;
                    o[0] = (v >> 16) as u16;
                    o[1] = v as u16;
                    nbad += (o[0] >> 14 != 0) as usize + (o[1] >> 14 != 0) as usize;
                }
            }
            bad.fetch_add(nbad, std::sync::atomic::Ordering::Relaxed);
        });
    });
    let present = (src.len() / 4 * 2) as u64;
    if let Some(t) = truncation {
        ctx.recovered(CorruptReason::Truncated(t), geometry.total() - present);
    }
    if bad.into_inner() as u64 > present / 16 {
        return Err(Error::corrupt("SRF: decrypted samples out of range (wrong key or corrupt data)"));
    }
    Ok(img)
}
