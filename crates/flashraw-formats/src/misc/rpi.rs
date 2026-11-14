//! Raspberry Pi raw captures: a camera JPEG (with EXIF) followed, at the very end of the
//! file, by a Broadcom "BRCM" block: a 32 KiB header area and the unprocessed sensor raster.
//!
//! The block start is not stored anywhere. It lies at a distance from the end of the file
//! that depends on the sensor mode (raster size plus a constant trailer), so the known
//! distances are tried in turn and the one whose header is consistent wins.

use crate::common::*;
use flashraw_core::tiff::{Tiff, tags};
use flashraw_core::*;

/// Bytes from the end of the file to the "BRCM" tag, per Pi camera mode, in the order tried:
/// IMX219 3280x2464, OV5647 2592x1944, IMX219 1920x1080 / 1640x1232 / 1640x922 / 1280x720 /
/// 640x480, IMX477 2028x1080 / 2028x1520 / 4056x3040 / 1012x760, OV5647 1920x1080 / 1296x972.
/// Verified by files: 10270208, 6404096, 18711040.
const TRAILER_DISTANCES: [usize; 13] = [
    10_270_208, 6_404_096, 2_678_784, 2_628_608, 1_963_008, 1_233_920, 445_440, 3_375_104, 4_751_360, 18_711_040,
    1_015_808, 2_717_696, 1_625_600,
];

/// Header layout, relative to the "BRCM" tag.
const MAGIC: &[u8; 4] = b"BRCM";
const FIELDS_AT: usize = 0xB0;
const WIDTH_AT: usize = 32;
const HEIGHT_AT: usize = 34;
const PAD_RIGHT_AT: usize = 36;
const FORMAT_AT: usize = 66;
const BAYER_ORDER_AT: usize = 68;
const BAYER_FORMAT_AT: usize = 69;
/// The raster starts this far after the tag.
const RASTER_AT: usize = 0x8000;
/// `format` value of Bayer rasters; older firmware leaves it unset (10-bit data then).
const FORMAT_BAYER: u16 = 33;
/// Rows start at multiples of this many bytes.
const STRIDE_ALIGN: usize = 32;
/// Smallest file considered.
const MIN_FILE: usize = 0x9000;

/// Width of the 5 MP OV5647 mode's buffer: 2592 image columns plus padding up to this many.
const OV5647_WIDE_IMAGE: u32 = 2592;
const OV5647_WIDE_BUFFER: usize = 2611;

/// Longest maker note prefix looked at for the gains.
const NOTE_PREFIX: usize = 511;

/// Parsed BRCM header.
#[derive(Debug, Clone, Copy)]
struct Brcm {
    /// File offset of the "BRCM" tag.
    at: usize,
    width: u32,
    height: u32,
    bits: u32,
    bayer_order: u8,
    stride: usize,
}

impl Brcm {
    fn raster(&self) -> usize {
        self.at + RASTER_AT
    }

    /// Bytes of one row's samples for `samples` columns.
    fn row_bytes(&self, samples: usize) -> usize {
        row_bytes(self.bits, samples).min(self.stride)
    }
}

/// Bytes holding `samples` samples (whole groups) at `bits` per sample.
fn row_bytes(bits: u32, samples: usize) -> usize {
    match bits {
        8 => samples,
        10 => samples.div_ceil(4) * 5,
        12 => samples.div_ceil(2) * 3,
        14 => samples.div_ceil(4) * 7,
        _ => samples * 2,
    }
}

/// Bytes of a stored row holding `padded` samples, before alignment.
fn packed_row(bits: u32, padded: usize) -> usize {
    match bits {
        8 => padded,
        10 => (padded * 5).div_ceil(4),
        12 => (padded * 3).div_ceil(2),
        14 => (padded * 7).div_ceil(4),
        _ => padded * 2,
    }
}

fn le16(data: &[u8], at: usize) -> Option<u16> {
    let b = data.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

/// Check for a header whose "BRCM" tag is at `at`.
fn parse_header(data: &[u8], at: usize) -> Option<Brcm> {
    if data.get(at..at.checked_add(4)?)? != MAGIC {
        return None;
    }
    let h = at.checked_add(FIELDS_AT)?;
    let width = le16(data, h + WIDTH_AT)?;
    let height = le16(data, h + HEIGHT_AT)?;
    let pad_right = le16(data, h + PAD_RIGHT_AT)?;
    let format = le16(data, h + FORMAT_AT)?;
    let bayer_order = *data.get(h + BAYER_ORDER_AT)?;
    let bayer_format = *data.get(h + BAYER_FORMAT_AT)?;
    let bits = if format == FORMAT_BAYER {
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
    let padded = width as usize + pad_right as usize;
    let stride = packed_row(bits, padded).div_ceil(STRIDE_ALIGN) * STRIDE_ALIGN;
    let end = at.checked_add(RASTER_AT)?.checked_add(stride.checked_mul(height as usize)?)?;
    if width == 0 || height == 0 || end > data.len().checked_add(stride)? {
        return None;
    }
    Some(Brcm { at, width: width as u32, height: height as u32, bits, bayer_order, stride })
}

fn find_header(data: &[u8]) -> Option<Brcm> {
    TRAILER_DISTANCES.iter().filter_map(|&t| data.len().checked_sub(t)).find_map(|at| parse_header(data, at))
}

/// The camera as named in the EXIF, with the black level the sensor needs. The BRCM header and
/// the maker note state none and the rasters have no optical-black area, so the values are the
/// sensor makers' pedestals: IMX219 64 at 10 bits (Sony datasheet; Raspberry Pi's own tuning
/// states 4096 at 16 bits), OV5647 16 at 10 bits (Raspberry Pi's tuning states 1024 at 16 bits).
/// The samples agree: no image pixel of the IMX219 sample is below 81, of the OV5647 sample below 34.
struct Sensor {
    model: &'static str,
    black: f32,
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn contains_ci(s: &str, needle: &str) -> bool {
    s.as_bytes().windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Reported model and black level by the EXIF model string (first matching rule).
fn classify(exif_model: &str, bits: u32) -> Sensor {
    if starts_with_ci(exif_model, "rp_imx219") || (starts_with_ci(exif_model, "rp_imx") && bits == 10) {
        Sensor { model: "RPi IMX219", black: 64.0 }
    } else if starts_with_ci(exif_model, "ov5647") {
        Sensor { model: "RPi OV5647 v.1", black: 16.0 }
    } else if starts_with_ci(exif_model, "rp_ov5647") {
        Sensor { model: "RPi OV5647 v.2", black: 16.0 }
    } else if contains_ci(exif_model, "imx477") || starts_with_ci(exif_model, "rp_testc") {
        // 12-bit IMX477 data: a 12-bit IMX477 DNG written on the Pi states BlackLevel 256.
        Sensor { model: "RP_imx477", black: if bits == 12 { 256.0 } else { 0.0 } }
    } else {
        Sensor { model: "Pi", black: 0.0 }
    }
}

/// The number following `key` in `text`: the longest run of digits, '.' and '-'.
fn number_after(text: &str, key: &str) -> Option<f32> {
    let rest = &text[text.find(key)? + key.len()..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// White balance from the maker note text ("gain_r=1.707 gain_b=1.628 ...").
fn gains_from_note(note: &[u8]) -> Option<WhiteBalance> {
    let text = String::from_utf8_lossy(&note[..note.len().min(NOTE_PREFIX)]);
    let (r, b) = (number_after(&text, "gain_r=")?, number_after(&text, "gain_b=")?);
    if r > 0.001 && b > 0.001 { WhiteBalance::from_multipliers(r, 1.0, b, 1.0) } else { None }
}

fn cfa_for(order: u8) -> CfaPattern {
    match order {
        0 => CfaPattern::RGGB,
        1 => CfaPattern::GBRG,
        3 => CfaPattern::GRBG,
        _ => CfaPattern::BGGR,
    }
}

/// Unpack groups of `B` bytes into `S` samples each. Bytes missing from `src` read as
/// zero; the samples of a group beyond `out` are not written.
#[inline(always)]
fn unpack_groups<const B: usize, const S: usize>(src: &[u8], out: &mut [u16], f: impl Fn(&[u8; B]) -> [u16; S]) {
    let full = (out.len() / S).min(src.len() / B);
    let (head, tail) = out.split_at_mut(full * S);
    for (o, g) in head.chunks_exact_mut(S).zip(src.chunks_exact(B)) {
        let mut bytes = [0u8; B];
        bytes.copy_from_slice(g);
        o.copy_from_slice(&f(&bytes));
    }
    for (i, o) in tail.chunks_mut(S).enumerate() {
        let mut bytes = [0u8; B];
        let from = ((full + i) * B).min(src.len());
        let n = (src.len() - from).min(B);
        bytes[..n].copy_from_slice(&src[from..from + n]);
        let s = f(&bytes);
        o.copy_from_slice(&s[..o.len()]);
    }
}

/// Unpack one row of `bits`-bit samples (see the packings of the BRCM raster).
fn unpack_row(bits: u32, src: &[u8], out: &mut [u16]) {
    let wide = |b: u8, shift: u32| (b as u16) << shift;
    match bits {
        8 => unpack_groups::<1, 1>(src, out, |d| [d[0] as u16]),
        10 => {
            unpack_groups::<5, 4>(src, out, |d| std::array::from_fn(|c| wide(d[c], 2) | ((d[4] >> (2 * c)) & 3) as u16))
        }
        12 => unpack_groups::<3, 2>(src, out, |d| {
            std::array::from_fn(|c| wide(d[c], 4) | ((d[2] >> (4 * c)) & 15) as u16)
        }),
        14 => unpack_groups::<7, 4>(src, out, |d| {
            [
                wide(d[0], 6) | (d[4] >> 2) as u16,
                wide(d[1], 6) | ((d[4] & 3) as u16) << 4 | (d[5] >> 4) as u16,
                wide(d[2], 6) | ((d[5] & 15) as u16) << 2 | (d[6] >> 6) as u16,
                wide(d[3], 6) | ((d[6] & 63) as u16) << 2,
            ]
        }),
        _ => unpack_groups::<2, 1>(src, out, |d| [u16::from_le_bytes([d[0], d[1]])]),
    }
}

/// Maker note text of IFD0's EXIF sub-IFD.
fn maker_note<'a>(tiff: &Tiff<'a>) -> Option<&'a [u8]> {
    let exif = tiff.ifds.first()?.exif.as_ref()?;
    Some(exif.get(tags::MAKER_NOTE)?.bytes())
}

/// Detect a JPEG with an appended Broadcom raw block and open it. `None` if the data is not
/// such a file.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Option<Result<Box<dyn RawDecoder + 'a>>> {
    if data.len() < MIN_FILE || data[..4] != [0xFF, 0xD8, 0xFF, 0xE1] || &data[6..10] != b"Exif" {
        return None;
    }
    let brcm = find_header(data)?;
    Some(build(data, brcm, opts))
}

fn build<'a>(data: &'a [u8], brcm: Brcm, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let Brcm { at, width, height, bits, bayer_order, stride } = brcm;
    opts.limits.check_dims(width, height, 1)?;

    let mut meta = RawMetadata::default();
    let mut gains = None;
    if let Ok(tiff) = Tiff::parse(&data[12..]) {
        if let Some(ifd0) = tiff.ifds.first() {
            flashraw_core::exif::read_ifd0(ifd0, &mut meta);
            gains = maker_note(&tiff).and_then(gains_from_note);
        }
    }
    let exif_model = meta.model.trim().to_string();
    let sensor = classify(&exif_model, bits);
    meta.make = "RaspberryPi".into();
    meta.canonical_make = "Raspberry Pi".into();
    meta.model = sensor.model.into();
    meta.canonical_model = sensor.model.into();
    meta.set_extra("sensor", &exif_model);
    push_jpeg(data, 0, at, Orientation::Normal, &mut meta.previews);
    classify_previews(&mut meta.previews);

    let raw_width = if sensor.model.starts_with("RPi OV5647") && bits == 10 && width == OV5647_WIDE_IMAGE {
        OV5647_WIDE_BUFFER.min(stride * 4 / 5)
    } else {
        width as usize
    };
    let mut info = FrameInfo::new(raw_width as u32, height, bits as u8);
    info.active_area = Rect::new(0, 0, width, height);
    info.layout = SensorLayout::Bayer;
    info.cfa = Some(cfa_for(bayer_order));
    info.black_level = BlackLevel::uniform(sensor.black);
    info.white_level = WhiteLevel::uniform((1u32 << bits) - 1);
    info.compression = Compression::Uncompressed;
    info.color_matrices = db_color(&meta, None);
    info.white_balance = gains;

    let rows = height as usize;
    let geometry = RowGeometry { rows, stride, row_bytes: brcm.row_bytes(raw_width), samples: raw_width };
    let raster = brcm.raster();
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(raw_width as u32, height, 1, &ctx.limits)?;
        let avail = ctx.sensor_bytes(data, DataUnit::Payload, raster, geometry.needed())?;
        let mut src = avail.bytes;
        if let Some(t) = avail.truncation {
            let whole = geometry.complete_rows(src.len());
            src = &src[..src.len().min(whole * stride)];
            ctx.recovered(CorruptReason::Truncated(t), ((rows - whole) * raw_width) as u64);
        }
        ctx.profiler.time(Stage::Unpack, || unpack_rows(&mut img, src, bits, stride, raw_width, ctx.parallel));
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(RawFormat::Other, meta, info, Box::new(decode));
    dec.random_access = RandomAccess::Rows;
    Ok(Box::new(dec))
}

/// Bytes per group and samples per group of each packing.
fn group_shape(bits: u32) -> (usize, usize) {
    match bits {
        8 => (1, 1),
        10 => (5, 4),
        12 => (3, 2),
        14 => (7, 4),
        _ => (2, 1),
    }
}

/// Decode a row whose last group(s) run past the end of its `stride` bytes: the groups
/// inside are decoded in place, the rest from a small buffer in which the row's bytes
/// repeat once more and zeros follow.
fn unpack_wrapped_row(bits: u32, line: &[u8], stride: usize, out: &mut [u16], tail: &mut Vec<u8>) {
    let (bytes, samples) = group_shape(bits);
    let inside = (stride / bytes).min(out.len().div_ceil(samples));
    let head = (inside * samples).min(out.len());
    unpack_row(bits, line, &mut out[..head]);
    let start = inside * bytes;
    tail.clear();
    tail.extend(
        (start..start + row_bytes(bits, out.len() - head))
            .map(|p| if p < 2 * stride { line.get(p % stride).copied().unwrap_or(0) } else { 0 }),
    );
    unpack_row(bits, tail, &mut out[head..]);
}

/// Decode every row that has bytes in `src` (rows are `stride` bytes apart).
///
/// A group that starts inside a row and runs past its end continues with the first bytes of
/// the same row (the 5 MP OV5647 mode relies on it); the bytes after that read as zero.
fn unpack_rows(img: &mut RawImage, src: &[u8], bits: u32, stride: usize, width: usize, parallel: bool) {
    let out = img.u16_mut();
    if bits == 10 && row_bytes(10, width) <= stride {
        flashraw_codecs::misc_packed::mipi10_rows(src, width, out.len() / width.max(1), stride, out, parallel);
        return;
    }
    let wraps = row_bytes(bits, width) > stride;
    par::for_each_row_chunk_bounded(out, width, par::memory_bound_workers(), parallel, |first, chunk| {
        let mut tail = Vec::new();
        for (i, row) in chunk.chunks_mut(width).enumerate() {
            let Some(line) = src.get((first + i) * stride..) else { break };
            let line = &line[..line.len().min(stride)];
            if wraps {
                unpack_wrapped_row(bits, line, stride, row, &mut tail);
            } else {
                unpack_row(bits, line, row);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packings() {
        let mut out = [0u16; 4];
        unpack_row(10, &[1, 2, 3, 4, 0b1110_0100], &mut out);
        assert_eq!(out, [1 << 2, 2 << 2 | 1, 3 << 2 | 2, 4 << 2 | 3]);
        let mut out = [0u16; 2];
        unpack_row(12, &[0xAB, 0xCD, 0x5E], &mut out);
        assert_eq!(out, [0xAB0 | 0xE, 0xCD0 | 5]);
        let mut out = [0u16; 4];
        unpack_row(14, &[1, 2, 3, 4, 0b1101_1011, 0b1010_0110, 0b1100_0111], &mut out);
        assert_eq!(
            out,
            [1 << 6 | 0b110110, 2 << 6 | 0b11 << 4 | 0b1010, 3 << 6 | 0b0110 << 2 | 0b11, 4 << 6 | 0b000111 << 2]
        );
        let mut out = [0u16; 2];
        unpack_row(16, &[1, 2, 3, 4], &mut out);
        assert_eq!(out, [0x0201, 0x0403]);
        unpack_row(8, &[9, 8], &mut out);
        assert_eq!(out, [9, 8]);
        // Missing bytes read as zero; a partial group writes only what fits.
        let mut out = [7u16; 3];
        unpack_row(10, &[255, 255], &mut out);
        assert_eq!(out, [1020, 1020, 0]);
    }

    #[test]
    fn row_wrap() {
        // 5 samples of 10 bit need 10 bytes, the row has 9: the last byte of the second group
        // is the first byte of the row.
        let mut img = RawImage::new_u16(5, 1, 1, &Limits::default()).unwrap();
        let row: [u8; 9] = [10, 20, 30, 40, 0, 50, 60, 70, 80];
        unpack_rows(&mut img, &row, 10, 9, 5, false);
        let s = img.u16_mut();
        assert_eq!(&s[..4], &[10 << 2, 20 << 2, 30 << 2, 40 << 2]);
        assert_eq!(s[4], 50 << 2 | (10 & 3));
    }

    #[test]
    fn gains() {
        let wb = gains_from_note(b"ev=-1 gain_r=1.707 gain_b=1.628 greenness=3").unwrap();
        assert!((wb.coeffs[0] - 1.707).abs() < 1e-5 && (wb.coeffs[2] - 1.628).abs() < 1e-5);
        assert!(gains_from_note(b"gain_r=0 gain_b=1.5").is_none());
        assert!(gains_from_note(b"gain_r=1.5").is_none());
        assert!(gains_from_note(b"gain_r=1.2.3 gain_b=1.5").is_none());
        let mut late = vec![b' '; 520];
        late.extend(b"gain_r=1.5 gain_b=1.5");
        assert!(gains_from_note(&late).is_none());
    }

    #[test]
    fn header_and_models() {
        assert_eq!(packed_row(10, 3282), 4103);
        assert_eq!(packed_row(12, 4058), 6087);
        assert_eq!(classify("RP_imx219", 10).model, "RPi IMX219");
        assert_eq!(classify("rp_imx477", 12).black, 256.0);
        assert_eq!(classify("RP_imx477", 10).model, "RPi IMX219");
        assert_eq!(classify("RP_imx219", 10).black, 64.0);
        assert_eq!(classify("RP_ov5647", 10).black, 16.0);
        assert_eq!(classify("RP_ov5647", 10).model, "RPi OV5647 v.2");
        assert_eq!(classify("OV5647", 10).model, "RPi OV5647 v.1");
        assert_eq!(classify("RP_testc", 10).black, 0.0);
        assert_eq!(classify("RP_testc", 12).black, 256.0);
        assert_eq!(classify("something", 10).model, "Pi");
        assert!(open(&[0; 100], &OpenOptions::default()).is_none());
    }
}
