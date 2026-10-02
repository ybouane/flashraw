//! SRF: the encrypted raw format of the DSC-F828 and DSC-V3.
//!
//! IFD0 describes the sensor size but not where the data is. The data sits at a
//! fixed offset and is encrypted with the Sony key stream ([`super::crypt`]). The
//! raw key is itself encrypted: a pointer byte at 200896 locates a big-endian master
//! key, which decrypts a 40-byte header at 164600 whose bytes 22..26 hold the data
//! key. Samples are big-endian 16-bit (14 significant bits). The F828 has an RGBE
//! (emerald) four-colour filter array.

use super::crypt::SonyCipher;
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
    let len = width as usize * height as usize * 2;
    let src = flashraw_core::bytes::slice_at(data, m.data_offset, len)?;
    db_lookup(&mut meta, "srf");
    let mut info = FrameInfo::new(width, height, 14);
    info.compression = Compression::Uncompressed;
    // window on an even CFA phase by dropping its first column.
    let left = (m.left + (m.left & 1)).min(width);
    let vis_w = (m.width - (m.left & 1)).min(width - left);
    let vis = Rect::new(left, 0, vis_w, height);
    info.cfa = Some(if m.name == "DSC-F828" {
        // RGBE ("emerald") filter.
        info.layout = SensorLayout::OtherCfa;
        CfaPattern::new(2, 2, &[CfaColor::Emerald, CfaColor::Red, CfaColor::Blue, CfaColor::Green])
            .ok_or_else(|| Error::invalid_metadata("SRF: bad CFA"))?
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
    let dec = SimpleDecoder::new(RawFormat::Arw, meta, info, Box::new(move |ctx| decode(src, key, width, height, ctx)));
    Ok(Box::new(dec))
}

/// Decrypt and unpack the SRF sample stream (sequential: one key stream for the frame).
pub fn decode(src: &[u8], key: u32, width: u32, height: u32, ctx: &DecodeContext) -> Result<RawImage> {
    let mut img = RawImage::new_u16(width, height, 1, &ctx.limits)?;
    let w = width as usize;
    let mut cipher = SonyCipher::new(key);
    let mut row = vec![0u8; w * 2];
    let mut bad = 0usize;
    ctx.profiler.time(Stage::Unpack, || {
        for (y, out) in img.u16_mut().chunks_exact_mut(w).enumerate() {
            let s = src.get(y * w * 2..(y + 1) * w * 2).ok_or_else(|| Error::corrupt("SRF: data truncated"))?;
            row.copy_from_slice(s);
            cipher.apply(&mut row);
            for (o, b) in out.iter_mut().zip(row.chunks_exact(2)) {
                *o = u16::from_be_bytes([b[0], b[1]]);
                bad += (*o >> 14 != 0) as usize;
            }
        }
        Ok::<(), Error>(())
    })?;
    if bad > w * height as usize / 16 {
        return Err(Error::corrupt("SRF: decrypted samples out of range (wrong key or corrupt data)"));
    }
    Ok(img)
}
