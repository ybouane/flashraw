//! Raspberry Pi camera captures (`raspistill --raw`, picamera `bayer=True`): a JPEG
//! followed by a Broadcom `BRCM` block (32 KiB header, then the sensor raster).
//!
//! The block sits at a sensor/mode-specific distance from the end of the file; the
//! header at `BRCM + 0xB0` gives width, height, right padding, Bayer order and the
//! packing (8/10/12/14/16 bits). Rows are padded to 32 bytes. IMX219 files decode
//! as "Broadcom RPi IMX219" with black 66; the other sensors use black 16 (OV5647) or 256
//! (12-bit IMX477).

use crate::common::*;
use flashraw_codecs::misc_packed;
use flashraw_core::tiff::{Tiff, tags};
use flashraw_core::*;

/// Distances of the `BRCM` block from the end of the file for known sensor modes.
const TRAILERS: &[usize] = &[
    0x9cb600, 0x61b800, // IMX219 8 MP, OV5647 5 MP
    10_270_208, 2_678_784, 2_628_608, 1_963_008, 1_233_920, 445_440, // IMX219 modes
    3_375_104, 4_751_360, 18_711_040, 1_015_808, // IMX477 modes
    6_404_096, 2_717_696, 1_625_600, // OV5647 modes
];

#[derive(Debug, Clone, Copy)]
struct Brcm {
    /// Samples decoded per row (the visible width, except for the wider 5 MP OV5647 layout).
    raw_width: usize,
    width: usize,
    height: usize,
    stride: usize,
    bits: u32,
    bayer_order: u8,
    data_offset: usize,
}

fn le16(d: &[u8], o: usize) -> Option<u16> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn parse_brcm(data: &[u8], at: usize) -> Option<Brcm> {
    if data.get(at..at + 4) != Some(b"BRCM") {
        return None;
    }
    let h = at + 0xB0;
    let width = le16(data, h + 32)? as usize;
    let height = le16(data, h + 34)? as usize;
    let pad_right = le16(data, h + 36)? as usize;
    let format = le16(data, h + 66)?;
    let bayer_order = *data.get(h + 68)?;
    let bayer_format = *data.get(h + 69)?;
    // Header format 33 = Bayer; older firmware leaves it unset (always RAW10).
    let bits = if format == 33 {
        match bayer_format {
            2 => 8,
            3 => 10,
            4 => 12,
            5 => 14,
            6 => 16,
            _ => return None,
        }
    } else {
        10
    };
    let pw = width + pad_right;
    let row = match bits {
        8 => pw,
        10 => (pw * 5 + 3) >> 2,
        12 => (pw * 3 + 1) >> 1,
        14 => (pw * 7 + 3) >> 2,
        _ => pw << 1,
    };
    let stride = (row + 31) & !31;
    let data_offset = at + 0x8000;
    if width == 0 || height == 0 || data_offset.checked_add(stride.checked_mul(height)?)? > data.len() + stride {
        return None;
    }
    Some(Brcm { raw_width: width, width, height, stride, bits, bayer_order, data_offset })
}

/// Detect and open a JPEG with an appended Broadcom raw.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Option<Result<Box<dyn RawDecoder + 'a>>> {
    if data.len() < 0x9000 || &data[..4] != b"\xff\xd8\xff\xe1" || data.get(6..10) != Some(b"Exif") {
        return None;
    }
    let brcm = TRAILERS.iter().filter_map(|&t| data.len().checked_sub(t)).find_map(|at| parse_brcm(data, at))?;
    Some(open_brcm(data, brcm, opts))
}

fn open_brcm<'a>(data: &'a [u8], mut b: Brcm, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    opts.limits.check_dims(b.width as u32, b.height as u32, 1)?;
    let mut meta = RawMetadata::default();
    let mut gains = None;
    if let Ok(t) = Tiff::parse(&data[12..]) {
        if let Some(ifd0) = t.ifds.first() {
            flashraw_core::exif::read_ifd0(ifd0, &mut meta);
            // The maker note is a text block: "ev=.. mlux=.. gain_r=1.70 gain_b=1.63 ...".
            let note = ifd0.exif.as_ref().and_then(|e| e.get(tags::MAKER_NOTE)).map(|e| e.bytes());
            gains = note.and_then(parse_gains);
        }
    }
    let exif_model = meta.model.trim().to_string();
    let lower = exif_model.to_ascii_lowercase();
    // Model names and black levels per sensor.
    let (model, black) = if lower.starts_with("rp_imx219") || lower.starts_with("rp_imx") && b.bits == 10 {
        ("RPi IMX219", 66.0)
    } else if lower.starts_with("ov5647") {
        ("RPi OV5647 v.1", 16.0)
    } else if lower.starts_with("rp_ov5647") {
        ("RPi OV5647 v.2", 16.0)
    } else if lower.contains("imx477") || lower.starts_with("rp_testc") {
        ("RP_imx477", if b.bits == 10 { 64.0 } else { 256.0 })
    } else {
        ("Pi", 16.0)
    };
    meta.make = "RaspberryPi".into();
    meta.model = model.into();
    meta.canonical_make = "Raspberry Pi".into();
    meta.canonical_model = model.into();
    meta.set_extra("sensor", exif_model);
    // The JPEG in front of the raw block is the preview.
    push_jpeg(data, 0, b.data_offset - 0x8000, Orientation::Normal, &mut meta.previews);
    classify_previews(&mut meta.previews);

    if model.starts_with("RPi OV5647") && b.bits == 10 && b.width == 2592 {
        // The 5 MP OV5647 frame is decoded 2611 samples wide (into the row padding).
        b.raw_width = 2611.min(b.stride * 4 / 5);
    }
    let mut info = FrameInfo::new(b.raw_width as u32, b.height as u32, b.bits as u8);
    info.active_area = Rect::new(0, 0, b.width as u32, b.height as u32);
    let cfa = match b.bayer_order {
        0 => CfaPattern::RGGB,
        1 => CfaPattern::GBRG,
        3 => CfaPattern::GRBG,
        _ => CfaPattern::BGGR,
    };
    info.cfa = Some(cfa);
    info.layout = SensorLayout::Bayer;
    info.black_level = BlackLevel::uniform(black);
    info.white_level = WhiteLevel::uniform((1u32 << b.bits) - 1);
    info.compression = Compression::Uncompressed;
    info.color_matrices = db_color(&meta, None);
    info.white_balance = gains.and_then(|(r, b)| WhiteBalance::from_multipliers(r, 1.0, b, 1.0));
    let mut dec = SimpleDecoder::new(
        RawFormat::Other,
        meta,
        info,
        Box::new(move |ctx| {
            let (w, h) = (b.raw_width, b.height);
            let mut img = RawImage::new_u16(w as u32, h as u32, 1, &ctx.limits)?;
            // Rows hold `w` samples within their stride (wider rows wrap around in it).
            let row_bytes = match b.bits {
                8 => w,
                10 => w.div_ceil(4) * 5,
                12 => w.div_ceil(2) * 3,
                14 => w.div_ceil(4) * 7,
                _ => w * 2,
            }
            .min(b.stride);
            let geometry = RowGeometry { rows: h, stride: b.stride, row_bytes, samples: w };
            let src = ctx.sensor_bytes(data, DataUnit::Payload, b.data_offset, geometry.needed())?;
            // Recovering: only whole rows decode (partial ones would read padding).
            let rows = geometry.complete_rows(src.bytes.len());
            let bytes =
                if src.truncation.is_some() { &src.bytes[..(rows * b.stride).min(src.bytes.len())] } else { src.bytes };
            let out = img.u16_mut();
            ctx.profiler.time(Stage::Unpack, || decode_rows(bytes, b, out, ctx.parallel));
            if let Some(t) = src.truncation {
                out[rows * w..].fill(0);
                ctx.recovered(CorruptReason::Truncated(t), ((h - rows) * w) as u64);
            }
            Ok(img)
        }),
    );
    dec.random_access = RandomAccess::Rows;
    Ok(Box::new(dec))
}

/// `gain_r=`/`gain_b=` values of the Raspberry Pi maker-note text.
fn parse_gains(note: &[u8]) -> Option<(f32, f32)> {
    let text = String::from_utf8_lossy(&note[..note.len().min(511)]);
    let get = |key: &str| -> Option<f32> {
        let rest = &text[text.find(key)? + key.len()..];
        let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len());
        rest[..end].parse().ok()
    };
    let (r, b) = (get("gain_r=")?, get("gain_b=")?);
    (r > 0.001 && b > 0.001).then_some((r, b))
}

fn decode_rows(src: &[u8], b: Brcm, out: &mut [u16], parallel: bool) {
    let (w, h, stride) = (b.raw_width, b.height, b.stride);
    if b.bits == 10 && w.div_ceil(4) * 5 <= stride {
        return misc_packed::mipi10_rows(src, w, h, stride, out, parallel);
    }
    par::for_each_row_chunk_bounded(&mut out[..w * h], w, par::memory_bound_workers(), parallel, |first, chunk| {
        // The row buffer holds the row twice; groups reading past the stride
        // (OV5647's 2611-sample rows) pick up the start of the row again.
        let mut tmp = vec![0u8; 2 * stride + 8];
        for (i, row) in chunk.chunks_exact_mut(w).enumerate() {
            let Some(s) = src.get((first + i) * stride..) else { break };
            let n = stride.min(s.len());
            tmp[..n].copy_from_slice(&s[..n]);
            tmp[n..stride].fill(0);
            tmp.copy_within(..stride, stride);
            match b.bits {
                10 => {
                    for (px, d) in row.chunks_mut(4).zip(tmp.chunks_exact(5)) {
                        for (c, o) in px.iter_mut().enumerate() {
                            *o = ((d[c] as u16) << 2) | ((d[4] >> (c * 2)) & 3) as u16;
                        }
                    }
                }
                8 => row.iter_mut().zip(&tmp).for_each(|(o, &v)| *o = v as u16),
                12 => {
                    for (px, d) in row.chunks_mut(2).zip(tmp.chunks_exact(3)) {
                        for (c, o) in px.iter_mut().enumerate() {
                            *o = ((d[c] as u16) << 4) | ((d[2] >> (c * 4)) & 0xF) as u16;
                        }
                    }
                }
                14 => {
                    // Raspberry Pi RAW14 bit layout: 4 samples in 7 bytes.
                    for (px, d) in row.chunks_mut(4).zip(tmp.chunks_exact(7)) {
                        let v = [
                            ((d[0] as u16) << 6) | (d[4] >> 2) as u16,
                            ((d[1] as u16) << 6) | (((d[4] & 0x3) as u16) << 4) | ((d[5] & 0xf0) >> 4) as u16,
                            ((d[2] as u16) << 6) | (((d[5] & 0xf) as u16) << 2) | ((d[6] & 0xc0) >> 6) as u16,
                            ((d[3] as u16) << 6) | (((d[6] & 0x3f) as u16) << 2),
                        ];
                        for (o, x) in px.iter_mut().zip(v) {
                            *o = x;
                        }
                    }
                }
                _ => {
                    for (o, d) in row.iter_mut().zip(tmp.chunks_exact(2)) {
                        *o = u16::from_le_bytes([d[0], d[1]]);
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brcm_header_parsing() {
        let mut d = vec![0u8; 0x8000 + 64 * 4];
        d[..4].copy_from_slice(b"BRCM");
        let h = 0xB0;
        d[h + 32..h + 34].copy_from_slice(&8u16.to_le_bytes());
        d[h + 34..h + 36].copy_from_slice(&4u16.to_le_bytes());
        d[h + 66..h + 68].copy_from_slice(&33u16.to_le_bytes());
        d[h + 68] = 1;
        d[h + 69] = 4;
        let b = parse_brcm(&d, 0).unwrap();
        assert_eq!((b.width, b.height, b.bits, b.stride), (8, 4, 12, 32));
        assert!(parse_brcm(&d, 1).is_none());
        let mut out = vec![0u16; 32];
        decode_rows(&d[b.data_offset..], b, &mut out, false);
    }

    #[test]
    fn maker_note_gains() {
        assert_eq!(parse_gains(b"ev=-1 mlux=-1 gain_r=1.707 gain_b=1.628 greenness=3"), Some((1.707, 1.628)));
        assert_eq!(parse_gains(b"gain_r=0 gain_b=1"), None);
        assert_eq!(parse_gains(b"nothing"), None);
    }

    #[test]
    fn non_rpi_jpeg_is_ignored() {
        let mut d = vec![0u8; 0x10000];
        d[..4].copy_from_slice(b"\xff\xd8\xff\xe1");
        d[6..10].copy_from_slice(b"Exif");
        assert!(open(&d, &OpenOptions::default()).is_none());
    }
}
