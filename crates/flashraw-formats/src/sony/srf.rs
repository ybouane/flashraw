//! Sony SRF (DSC-F828, DSC-V3).
//!
//! A TIFF whose IFD0 gives the image size but not where the samples are: they sit
//! encrypted at a fixed position near the end of the file. The key stream cipher is
//! [`super::crypt`]; this module finds the key (a pointer byte selects a master key, which
//! decrypts a short header that holds the key of the sample data) and the sample window.

use super::crypt::{SonyCipher, keystream};
use crate::common::*;
use flashraw_core::tiff::{Tiff, tags};
use flashraw_core::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// Where the key material lies in the file (absolute offsets).
const KEY_POINTER_OFFSET: usize = 200_896;
const HEADER_OFFSET: usize = 164_600;
const HEADER_LEN: usize = 40;
/// Bytes of the decrypted header that form the little-endian data key.
const DATA_KEY_AT: usize = 22;

/// Largest raw frame (the DSC-F828 size); anything bigger is not an SRF.
const MAX_WIDTH: u32 = 3360;
const MAX_HEIGHT: u32 = 2460;
/// Measured: the DSC-F828 sample clips at 0x3ff0 (290204 image pixels).
const WHITE_LEVEL: u32 = 0x3ff0;
/// Samples are 14 bits; a decrypted sample with a higher bit set is implausible.
const SAMPLE_LIMIT: u16 = 0x4000;

/// Sensor layout per camera model.
struct Model {
    /// EXIF model string (also the colour table key).
    name: &'static str,
    /// Absolute file offset of the first sample.
    data_offset: usize,
    /// First visible column, width of the visible window.
    left: u32,
    visible_width: u32,
    /// Start of the left optical-black strip and columns trimmed off the right edge.
    mask_left: u32,
    mask_right_trim: u32,
    /// Colour filter at the buffer origin, row-major 2x2.
    cfa: [CfaColor; 4],
    layout: SensorLayout,
}

use CfaColor::{Blue, Emerald, Green, Red};

/// The two SRF cameras. Only the F828 has a sample file; the V3 row is carried over as is.
const MODELS: [Model; 2] = [
    Model {
        name: "DSC-F828",
        data_offset: 862_144,
        left: 5,
        visible_width: 3288,
        mask_left: 0,
        mask_right_trim: 17,
        cfa: [Emerald, Red, Blue, Green],
        layout: SensorLayout::OtherCfa,
    },
    Model {
        name: "DSC-V3",
        data_offset: 787_392,
        left: 59,
        visible_width: 3109,
        mask_left: 9,
        mask_right_trim: 0,
        cfa: [Green, Red, Blue, Green],
        layout: SensorLayout::Bayer,
    },
];

fn model_for(name: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.name == name)
}

/// True for the camera models stored as SRF.
pub fn is_srf(model: &str) -> bool {
    model_for(model).is_some()
}

/// Key of the sample data, recovered from the file.
fn data_key(data: &[u8]) -> Result<u32> {
    let pointer =
        *data.get(KEY_POINTER_OFFSET).ok_or_else(|| Error::corrupt("SRF: key pointer beyond the end of the file"))?;
    let master_at = KEY_POINTER_OFFSET + 4 * pointer as usize;
    let master = data.u32_at(ByteOrder::Big, master_at)?;
    let mut header = [0u8; HEADER_LEN];
    header.copy_from_slice(bytes::slice_at(data, HEADER_OFFSET, HEADER_LEN)?);
    SonyCipher::new(master).apply(&mut header);
    Ok(u32::from_le_bytes([
        header[DATA_KEY_AT],
        header[DATA_KEY_AT + 1],
        header[DATA_KEY_AT + 2],
        header[DATA_KEY_AT + 3],
    ]))
}

/// Visible window and optical-black strips of a `width x height` buffer.
struct Layout {
    visible: Rect,
    masked: [Rect; 2],
}

fn window(m: &Model, width: u32, height: u32) -> Layout {
    let odd = m.left % 2;
    let lead = (m.left + odd).min(width);
    let vis_w = (m.visible_width - odd).min(width - lead);
    let right = lead + vis_w;
    Layout {
        visible: Rect::new(lead, 0, vis_w, height),
        masked: [
            Rect::new(m.mask_left, 0, lead.saturating_sub(m.mask_left), height),
            Rect::new(right, 0, width.saturating_sub(m.mask_right_trim.saturating_add(right)), height),
        ],
    }
}

/// Decrypt `src` (big-endian word pairs) into `out`; returns how many decoded samples are
/// implausible (not 14-bit). Row-independent once the key stream is known.
fn decrypt_samples(src: &[u8], stream: &[u32], out: &mut [u16], width: usize, parallel: bool) -> u64 {
    let bad = AtomicU64::new(0);
    par::for_each_row_chunk_bounded(out, width, par::memory_bound_workers(), parallel, |first, chunk| {
        let skip = first * (width / 2);
        let mut count = 0u64;
        let words = src.chunks_exact(4).zip(stream).skip(skip);
        for ((w, k), o) in words.zip(chunk.chunks_exact_mut(2)) {
            let v = u32::from_be_bytes([w[0], w[1], w[2], w[3]]) ^ k;
            let (a, b) = ((v >> 16) as u16, v as u16);
            count += ((a >= SAMPLE_LIMIT) as u64) + ((b >= SAMPLE_LIMIT) as u64);
            o[0] = a;
            o[1] = b;
        }
        bad.fetch_add(count, Ordering::Relaxed);
    });
    bad.into_inner()
}

/// Open an SRF file whose TIFF structure and IFD0 metadata have already been read.
pub fn open<'a>(
    data: &'a [u8],
    tiff: &Tiff<'a>,
    mut meta: RawMetadata,
    opts: &OpenOptions,
) -> Result<Box<dyn RawDecoder + 'a>> {
    let model = model_for(&meta.model).ok_or_else(|| Error::invalid_metadata("SRF: unknown model"))?;
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("SRF without IFD0"))?;
    let (w, h) = (ifd0.width(), ifd0.height());
    if w == 0 || h == 0 || w > MAX_WIDTH || h > MAX_HEIGHT || w % 2 != 0 {
        return Err(Error::invalid_metadata(format!("SRF: unsupported image size {w}x{h}")));
    }
    opts.limits.check_dims(w, h, 1)?;
    let key = data_key(data)?;
    db_lookup(&mut meta, "srf");

    let mut info = FrameInfo::new(w, h, 14);
    info.compression = Compression::Uncompressed;
    info.layout = model.layout;
    info.cfa = CfaPattern::new(2, 2, &model.cfa);
    let area = window(model, w, h);
    info.active_area = area.visible;
    info.crop = area.visible;
    info.masked_areas = area.masked.to_vec();
    info.black_from_masked = true;
    info.white_level = WhiteLevel::uniform(WHITE_LEVEL);
    info.orientation = Orientation::from_exif(ifd0.u32(tags::ORIENTATION).unwrap_or(1));
    info.color_matrices = super::table::color(super::table::lookup("Sony", model.name), &meta);
    meta.previews = tiff_previews(data, 0, tiff, info.orientation);

    let (width, height) = (w as usize, h as usize);
    let offset = model.data_offset;
    let geometry = RowGeometry { rows: height, stride: width * 2, row_bytes: width * 2, samples: width };
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
        let src = ctx.sensor_bytes(data, DataUnit::Payload, offset, geometry.needed())?;
        let words = src.bytes.len() / 4;
        let present = words * 2;
        let total = width * height;
        let stream = keystream(key, words);
        let bad = ctx.profiler.time(Stage::Unpack, || {
            decrypt_samples(src.bytes, &stream, &mut img.u16_mut()[..total], width, ctx.parallel)
        });
        if let Some(t) = src.truncation {
            ctx.recovered(CorruptReason::Truncated(t), (total - present) as u64);
        }
        if bad > (present / 16) as u64 {
            return Err(Error::corrupt("SRF: decrypted samples out of range (wrong key or corrupt data)"));
        }
        Ok(img)
    };
    Ok(Box::new(SimpleDecoder::new(RawFormat::Arw, meta, info, Box::new(decode))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models() {
        assert!(is_srf("DSC-F828") && is_srf("DSC-V3"));
        assert!(!is_srf("dsc-f828") && !is_srf(" DSC-V3") && !is_srf("DSC-F828 "));
    }

    #[test]
    fn windows() {
        let f = window(&MODELS[0], 3360, 2460);
        assert_eq!(f.visible, Rect::new(6, 0, 3287, 2460));
        assert_eq!(f.masked, [Rect::new(0, 0, 6, 2460), Rect::new(3293, 0, 50, 2460)]);
        let v = window(&MODELS[1], 3360, 2460);
        assert_eq!(v.visible, Rect::new(60, 0, 3108, 2460));
        assert_eq!(v.masked, [Rect::new(9, 0, 51, 2460), Rect::new(3168, 0, 192, 2460)]);
        // Narrow frames cap at the width; masks may be empty.
        let n = window(&MODELS[0], 2, 4);
        assert_eq!(n.visible, Rect::new(2, 0, 0, 4));
        assert_eq!(n.masked[1].width, 0);
    }

    /// Plaintext words -> big-endian bytes XOR key stream.
    fn encrypt(samples: &[u16], key: u32) -> Vec<u8> {
        let ks = keystream(key, samples.len() / 2);
        samples
            .chunks_exact(2)
            .zip(ks)
            .flat_map(|(p, k)| (((p[0] as u32) << 16 | p[1] as u32) ^ k).to_be_bytes())
            .collect()
    }

    #[test]
    fn plausibility_counts() {
        let key = 0x1234_5678;
        let width = 8;
        let mut plain: Vec<u16> = (0..64).map(|i| i as u16 * 3).collect();
        plain[5] = 0x4000;
        plain[40] = 0xffff;
        let src = encrypt(&plain, key);
        let stream = keystream(key, 32);
        for parallel in [false, true] {
            let mut out = vec![0u16; 64];
            assert_eq!(decrypt_samples(&src, &stream, &mut out, width, parallel), 2);
            assert_eq!(out, plain);
        }
    }
}
