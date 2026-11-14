//! Epson ERF (R-D1 family).
//!
//! A TIFF whose SubIFD holds a 12-bit CFA image, either tightly packed (compression 1) or
//! in Epson's padded packing (compression 32769: groups of ten samples in 15 bytes followed
//! by one padding byte, so a row takes `ceil(width / 10) * 16` bytes). The maker note
//! (`EPSON\0` header, then a plain IFD) carries the black levels and the white balance.

use crate::common::*;
use flashraw_core::tiff::{Tiff, TiffOptions, parse_ifd_at, tags};
use flashraw_core::*;

/// Camera facts: the compression codes and the one supported sample depth.
const COMPRESSION_TIGHT: u32 = 1;
const COMPRESSION_PADDED: u32 = 32769;
const BITS: u32 = 12;
const CFA_PHOTOMETRIC: u32 = 32803;

/// Bytes of one padded group and the samples it holds (the 16th byte is padding).
const GROUP_BYTES: usize = 16;
const GROUP_SAMPLES: usize = 10;

/// Maker note layout.
const NOTE_MAGIC: &[u8; 6] = b"EPSON\0";
const NOTE_IFD_OFFSET: usize = 8;
const NOTE_BLACK_LEVELS: u16 = 0x0401;
const NOTE_WHITE_BALANCE: u16 = 0x0E80;
/// Byte positions of the red and blue gains in the white balance blob.
const WB_RED_AT: usize = 48;
const WB_BLUE_AT: usize = 50;
/// Full-scale multipliers the 16-bit gain fractions refer to (established from sample files).
const WB_RED_SCALE: f32 = 567.0;
const WB_BLUE_SCALE: f32 = 431.0;

/// Packing of the 12-bit samples in a row.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Packing {
    Padded,
    Tight,
}

impl Packing {
    /// Bytes from one row to the next, for `width` samples (saturating).
    fn stride(self, width: usize) -> usize {
        match self {
            Packing::Padded => width.div_ceil(GROUP_SAMPLES).saturating_mul(GROUP_BYTES),
            Packing::Tight => width.saturating_mul(BITS as usize).div_ceil(8),
        }
    }

    /// Bytes a row's samples need (the last group's padding and the stride padding are not
    /// needed).
    fn row_bytes(self, width: usize) -> usize {
        match self {
            Packing::Padded => {
                let (full, rest) = (width / GROUP_SAMPLES, width % GROUP_SAMPLES);
                let head = full.saturating_mul(GROUP_BYTES);
                if rest == 0 { head.saturating_sub(1) } else { head.saturating_add((rest * BITS as usize).div_ceil(8)) }
            }
            Packing::Tight => self.stride(width),
        }
    }

    /// Samples completely contained in the first `len` bytes of a row.
    fn samples_in(self, len: usize) -> usize {
        match self {
            Packing::Padded => len / GROUP_BYTES * GROUP_SAMPLES + (len % GROUP_BYTES).min(GROUP_BYTES - 1) * 8 / 12,
            Packing::Tight => len / 12 * 8 + len % 12 * 8 / 12,
        }
    }
}

/// Values from the maker note.
#[derive(Default)]
struct MakerNote {
    black: Option<[u32; 4]>,
    /// Red and blue white balance multipliers (green is 1).
    gains: Option<(f32, f32)>,
}

fn read_maker_note(tiff: &Tiff) -> MakerNote {
    let mut out = MakerNote::default();
    let Some(entry) = tiff.find_entry(tags::MAKER_NOTE) else { return out };
    let data = tiff.data;
    let pos = entry.value_pos;
    if data.get(pos..pos.saturating_add(NOTE_MAGIC.len())) != Some(&NOTE_MAGIC[..]) {
        return out;
    }
    let Ok(start) = u32::try_from(pos.saturating_add(NOTE_IFD_OFFSET)) else { return out };
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    let Ok(ifd) = parse_ifd_at(data, tiff.order, start, opts) else { return out };
    if let Some(e) = ifd.get(NOTE_BLACK_LEVELS) {
        if let [a, b, c, d, ..] = e.u32s()[..] {
            out.black = Some([a, b, c, d]);
        }
    }
    if let Some(e) = ifd.get(NOTE_WHITE_BALANCE) {
        let blob = e.bytes();
        if let (Ok(r), Ok(b)) = (blob.u16_at(tiff.order, WB_RED_AT), blob.u16_at(tiff.order, WB_BLUE_AT)) {
            out.gains = Some((r as f32 * WB_RED_SCALE / 65536.0, b as f32 * WB_BLUE_SCALE / 65536.0));
        }
    }
    out
}

/// Place the four maker-note black levels `[R, G(red rows), G(blue rows), B]` on the 2x2 CFA.
fn black_on_cfa(levels: [u32; 4], cfa: &CfaPattern) -> BlackLevel {
    let row_has_red = |y: usize| (0..cfa.width()).any(|x| cfa.color_at(x, y) == CfaColor::Red);
    let at = |x: usize, y: usize| -> f32 {
        let i = match cfa.color_at(x, y) {
            CfaColor::Red => 0,
            CfaColor::Blue => 3,
            _ if row_has_red(y) => 1,
            _ => 2,
        };
        levels[i] as f32
    };
    BlackLevel::pattern2x2([at(0, 0), at(1, 0), at(0, 1), at(1, 1)])
}

/// Two samples from three bytes (MSB first).
#[inline(always)]
fn split_pair(b: &[u8]) -> (u16, u16) {
    ((b[0] as u16) << 4 | (b[1] as u16) >> 4, ((b[1] as u16) & 15) << 8 | b[2] as u16)
}

/// Unpack one row of Epson 12-bit data from its padded groups into `out`.
///
/// Chunks of ten output samples pair with groups of 16 source bytes (the last byte of a
/// group never contributes). Source bytes missing at the end read as zero; output chunks
/// without a source group are left untouched; surplus source groups are ignored.
pub fn unpack_row_12_padded(src: &[u8], out: &mut [u16]) {
    for (group, chunk) in src.chunks(GROUP_BYTES).zip(out.chunks_mut(GROUP_SAMPLES)) {
        if group.len() == GROUP_BYTES && chunk.len() == GROUP_SAMPLES {
            for (b, o) in group.chunks_exact(3).zip(chunk.chunks_exact_mut(2)) {
                (o[0], o[1]) = split_pair(b);
            }
            continue;
        }
        let mut padded = [0u8; GROUP_BYTES];
        padded[..group.len()].copy_from_slice(group);
        for (b, o) in padded.chunks_exact(3).zip(chunk.chunks_mut(2)) {
            let (a, c) = split_pair(b);
            o[0] = a;
            if let Some(second) = o.get_mut(1) {
                *second = c;
            }
        }
    }
}

/// Open an Epson ERF.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("ERF without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let cam = db_lookup(&mut meta, "");

    let raw = tiff
        .all_ifds()
        .into_iter()
        .filter(|i| i.u32(tags::PHOTOMETRIC) == Some(CFA_PHOTOMETRIC) && i.width() > 0 && i.height() > 0)
        .max_by_key(|i| i.width() as u64 * i.height() as u64)
        .ok_or_else(|| no_raw_ifd(&tiff, "ERF", Error::invalid_metadata("ERF: no CFA image")))?;
    let (w, h) = (raw.width(), raw.height());
    opts.limits.check_dims(w, h, 1)?;
    let packing = match (raw.compression(), raw.bps()) {
        (COMPRESSION_PADDED, BITS) => Packing::Padded,
        (COMPRESSION_TIGHT, BITS) => Packing::Tight,
        (c, b) => return Err(Error::unsupported_compression("ERF", format!("compression {c}, {b} bits"))),
    };
    let (offset, _declared) = *strips(raw).first().ok_or_else(|| Error::invalid_metadata("ERF: no strips"))?;

    let note = read_maker_note(&tiff);
    let mut info = FrameInfo::new(w, h, BITS as u8);
    info.compression = Compression::Uncompressed;
    let cfa = tiff_ep_cfa(raw).unwrap_or(CfaPattern::RGGB);
    info.cfa = Some(cfa);
    info.layout = layout_for(&cfa);
    info.orientation = Orientation::from_exif(ifd0.u32(tags::ORIENTATION).unwrap_or(1));
    if let Some(levels) = note.black {
        info.black_level = black_on_cfa(levels, &cfa);
    }
    if let Some((r, b)) = note.gains {
        info.white_balance = WhiteBalance::from_multipliers(r, 1.0, b, 1.0);
    }
    apply_db(
        &mut info,
        &meta,
        cam,
        Known {
            cfa: true,
            crop: true,
            active_area: true,
            white: true,
            masked: true,
            black: note.black.is_some(),
            color: false,
        },
    );
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);

    let (width, height) = (w as usize, h as usize);
    let stride = packing.stride(width);
    let geometry = RowGeometry { rows: height, stride, row_bytes: packing.row_bytes(width), samples: width };
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
        let src = ctx.sensor_bytes(data, DataUnit::Strip(0), offset, geometry.needed())?;
        let bytes = src.bytes;
        let row_src = |y: usize| -> &[u8] {
            let row = bytes.get(y.saturating_mul(stride)..).unwrap_or(&[]);
            &row[..row.len().min(stride)]
        };
        let samples_of = |y: usize| packing.samples_in(row_src(y).len()).min(width);
        ctx.profiler.time(Stage::Unpack, || {
            par::for_each_row_chunk_bounded(
                img.u16_mut(),
                width,
                par::memory_bound_workers(),
                ctx.parallel,
                |first, chunk| {
                    for (i, row) in chunk.chunks_mut(width).enumerate() {
                        let y = first + i;
                        let n = samples_of(y);
                        let line = row_src(y);
                        match packing {
                            Packing::Padded => unpack_row_12_padded(line, &mut row[..n]),
                            Packing::Tight => flashraw_codecs::packed::unpack_row(
                                line,
                                &mut row[..n],
                                BITS,
                                PackOrder::MsbBytes,
                                ctx.simd,
                            ),
                        }
                    }
                },
            )
        });
        if let Some(t) = src.truncation {
            let missing: u64 = (0..height).map(|y| (width - samples_of(y)) as u64).sum();
            ctx.recovered(CorruptReason::Truncated(t), missing);
        }
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(RawFormat::Erf, meta, info, Box::new(decode));
    dec.random_access = RandomAccess::Rows;
    Ok(Box::new(dec))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Twenty samples 0..20 in two padded groups.
    fn two_groups() -> Vec<u8> {
        let mut v = Vec::new();
        for g in 0..2u16 {
            for k in 0..5u16 {
                let (a, b) = (g * 10 + 2 * k, g * 10 + 2 * k + 1);
                v.extend([(a >> 4) as u8, ((a & 15) << 4 | b >> 8) as u8, b as u8]);
            }
            v.push(0xEE);
        }
        v
    }

    #[test]
    fn padded_groups_unpack() {
        let src = two_groups();
        let mut out = [0u16; 20];
        unpack_row_12_padded(&src, &mut out);
        assert_eq!(out, std::array::from_fn::<u16, 20, _>(|i| i as u16));
        // Missing source bytes read as zero; chunks without a source group stay untouched.
        let mut out = [7u16; 25];
        unpack_row_12_padded(&src[..17], &mut out);
        assert_eq!(&out[..10], &(0..10).collect::<Vec<u16>>()[..]);
        assert_eq!(&out[10..20], &[0; 10]);
        assert_eq!(&out[20..], &[7; 5]);
        // A short output stops inside a group.
        let mut out = [9u16; 13];
        unpack_row_12_padded(&src, &mut out);
        assert_eq!(&out[..], &(0..13).collect::<Vec<u16>>()[..]);
        unpack_row_12_padded(&[], &mut out);
        unpack_row_12_padded(&src, &mut []);
    }

    #[test]
    fn complete_samples_boundaries() {
        assert_eq!(Packing::Padded.samples_in(31), 20);
        assert_eq!(Packing::Padded.samples_in(30), 19);
        assert_eq!(Packing::Padded.samples_in(16), 10);
        assert_eq!(Packing::Padded.samples_in(15), 10);
        assert_eq!(Packing::Tight.samples_in(3), 2);
        assert_eq!(Packing::Padded.row_bytes(3040), 304 * 16 - 1);
        assert_eq!(Packing::Padded.row_bytes(13), 16 + 5);
        assert_eq!(Packing::Padded.stride(3040), 4864);
        assert_eq!(Packing::Tight.stride(3), 5);
        assert_eq!(Packing::Padded.row_bytes(0), 0);
    }

    #[test]
    fn black_placement() {
        let b = [10, 11, 12, 13];
        let at = |p: CfaPattern| black_on_cfa(b, &p).values.clone();
        assert_eq!(at(CfaPattern::RGGB), vec![10.0, 11.0, 12.0, 13.0]);
        assert_eq!(at(CfaPattern::GRBG), vec![11.0, 10.0, 13.0, 12.0]);
        assert_eq!(at(CfaPattern::BGGR), vec![13.0, 12.0, 11.0, 10.0]);
        assert_eq!(at(CfaPattern::GBRG), vec![12.0, 13.0, 10.0, 11.0]);
    }
}
