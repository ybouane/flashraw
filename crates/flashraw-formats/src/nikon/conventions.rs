//! Conventions shared by the Nikon, Pentax and Samsung
//! decoders: raw IFD selection, white-balance plausibility, colour-table lookup,
//! orientation and visible-area normalisation.

use crate::common::*;
use flashraw_camera_db::FallbackMatrix;
use flashraw_codecs::packed::{self, PackedLayout};
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// A row-addressable payload: a standard packed layout plus a right shift applied to
/// every sample.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RowLayout {
    pub layout: PackedLayout,
    pub shift: u32,
}

/// Geometry of a row-addressable payload's rows `0..rows` (for truncation checks).
pub(crate) fn row_geometry(l: &PackedLayout, rows: usize) -> RowGeometry {
    RowGeometry::packed(rows.min(l.height), l.width, l.bits, l.row_stride)
}

/// The strip `[offset, offset + declared)` of sensor data `unit`, sequentially decoded into
/// the first `rows` rows of `out` (`width` samples each), ran out of data in row
/// `first_missing` (`None`: it did not). When the strip runs past the end of the
/// `data_len`-byte file that is truncation: the recover policy zero-fills the rows from
/// `first_missing` on and reports them, the strict policy fails with the structured
/// truncation. A stream that runs dry inside the file is left to the caller.
/// Returns whether the stream was truncated.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_truncated_stream(
    ctx: &DecodeContext,
    data_len: usize,
    unit: DataUnit,
    offset: usize,
    declared: usize,
    out: &mut [u16],
    width: usize,
    rows: usize,
    first_missing: Option<usize>,
) -> Result<bool> {
    let Some(first) = first_missing else { return Ok(false) };
    let Err(t) = flashraw_core::integrity::check_range(data_len, unit, offset, declared) else { return Ok(false) };
    if !ctx.recover {
        return Err(Error::truncated(t));
    }
    let first = first.min(rows);
    if let Some(rest) = out.get_mut(first * width..rows * width) {
        rest.fill(0);
    }
    ctx.recovered(CorruptReason::Truncated(t), ((rows - first) * width) as u64);
    Ok(true)
}

/// Decode only `region` of a row-addressable payload starting at `offset` of `data` (rows
/// are unpacked in parallel straight from the source; unrelated rows are never touched).
/// Rows the file does not hold fail (strict) or stay zero with a warning (recover).
pub(crate) fn decode_rows_region(
    data: &[u8],
    offset: usize,
    unit: DataUnit,
    rl: RowLayout,
    region: Rect,
    ctx: &DecodeContext,
) -> Result<RawImage> {
    let l = rl.layout;
    let full = Rect::full(l.width as u32, l.height as u32);
    let r = region.intersect(&full).ok_or_else(|| Error::InvalidArgument(format!("region {region} outside image")))?;
    let src = ctx.sensor_bytes(data, unit, offset, row_geometry(&l, r.bottom() as usize).needed())?;
    let (truncation, src) = (src.truncation, src.bytes);
    let mut img = RawImage::new_u16(r.width, r.height, 1, &ctx.limits)?;
    let (row_bytes, out_w, x0) = (l.row_bytes(), r.width as usize, r.x as usize);
    let truncated = AtomicBool::new(false);
    let filled = std::sync::atomic::AtomicU64::new(0);
    let (simd, shift) = (ctx.simd, rl.shift);
    ctx.profiler.time(Stage::Unpack, || {
        par::for_each_row_chunk_bounded(
            img.u16_mut(),
            out_w,
            par::memory_bound_workers(),
            ctx.parallel,
            |first, chunk| {
                let mut tmp = vec![0u16; l.width];
                for (i, row) in chunk.chunks_mut(out_w).enumerate() {
                    let y = r.y as usize + first + i;
                    let s = match src.get(y * l.row_stride..y * l.row_stride + row_bytes) {
                        Some(s) => s,
                        None if truncation.is_some() => {
                            // Recovering: complete samples of a partial row, then zeros.
                            let s = src.get(y * l.row_stride..).unwrap_or(&[]);
                            let n = packed::unpack_row_partial(s, &mut tmp, l.bits, l.order, simd);
                            let keep = n.saturating_sub(x0).min(out_w);
                            for (o, &v) in row[..keep].iter_mut().zip(&tmp[x0..]) {
                                *o = v >> shift;
                            }
                            let rows_left = (chunk.len() / out_w - i - 1) as u64;
                            filled.fetch_add((out_w - keep) as u64 + rows_left * out_w as u64, Ordering::Relaxed);
                            break;
                        }
                        None => {
                            truncated.store(true, Ordering::Relaxed);
                            break;
                        }
                    };
                    packed::unpack_row(s, &mut tmp, l.bits, l.order, simd);
                    for (o, &v) in row.iter_mut().zip(&tmp[x0..x0 + out_w]) {
                        *o = v >> shift;
                    }
                }
            },
        )
    });
    if truncated.into_inner() {
        return Err(Error::corrupt("truncated packed data inside the requested region"));
    }
    if let Some(t) = truncation {
        ctx.recovered(CorruptReason::Truncated(t), filled.into_inner());
    }
    Ok(img)
}

/// The raw IFD: the largest image (by samples x bits) that is not a preview. Ties keep
/// the first IFD in file order (the first frame of multi-shot files).
///
/// Previews are never candidates, so a file truncated before its raw IFD has none
/// (rather than a thumbnail standing in for the sensor image): JPEG data, reduced-
/// resolution images and, unless `rgb_images` (film scanner files, whose image is RGB),
/// 8-bit RGB / YCbCr images.
pub(crate) fn select_raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>, rgb_images: bool) -> Option<&'t Ifd<'a>> {
    let mut best: Option<(&Ifd, u64)> = None;
    for ifd in tiff.all_ifds() {
        let (w, h) = (ifd.width() as u64, ifd.height() as u64);
        let bps = ifd.bps() as u64;
        if w == 0 || h == 0 || bps == 0 || bps > 32 || !ifd.has(tags::STRIP_OFFSETS) {
            continue;
        }
        if matches!(ifd.compression(), 6 | 7) {
            continue;
        }
        if ifd.is_reduced_resolution() || (!rgb_images && is_preview_ifd(ifd)) {
            continue;
        }
        let score = w * h * bps;
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((ifd, score));
        }
    }
    best.map(|(i, _)| i)
}

/// Orientation from the first IFD with a non-default orientation tag.
pub(crate) fn file_orientation(tiff: &Tiff) -> Orientation {
    tiff.all_ifds()
        .into_iter()
        .filter_map(|i| i.u32(tags::ORIENTATION))
        .find(|&o| (2..=8).contains(&o))
        .map(Orientation::from_exif)
        .unwrap_or(Orientation::Normal)
}

/// As-shot multipliers (R, G, B, G2) → normalised white balance, rejecting the values
/// that are implausible (non-positive, or a > 100:1 spread).
pub(crate) fn wb_from_cam_mul(m: [f32; 4]) -> Option<WhiteBalance> {
    if m[..3].iter().any(|&v| v.is_nan() || v <= 0.001) {
        return None;
    }
    let min = m[..3].iter().copied().fold(f32::MAX, f32::min);
    let max = m[..3].iter().copied().fold(0.0, f32::max);
    if max / min > 100.0 {
        return None;
    }
    WhiteBalance::from_multipliers(m[0], m[1], m[2], if m[3] > 0.0 { m[3] } else { m[1] })
}

/// Colour matrices: the fallback colour table first, then the
/// camera database entry.
pub(crate) fn color_from_table(
    table: Option<&'static FallbackMatrix>,
    meta: &RawMetadata,
    cam: Option<&'static flashraw_camera_db::Camera>,
) -> ColorMatrices {
    if let Some(f) = table.filter(|f| f.color_matrix.len() >= 9) {
        let mut cm = ColorMatrices::default();
        if let Some(m) =
            ColorMatrix::new(Illuminant::D65, 3, f.color_matrix.iter().take(9).map(|&v| v as f32 / 10000.0).collect())
        {
            cm.color.push(m);
            cm.source = ColorSource::CameraDb;
        }
        return cm;
    }
    db_color(meta, cam)
}

/// Apply a colour-table black/white entry: a positive
/// black overrides the file, a negative one is used only when the file has none; a
/// non-zero white overrides the default.
pub(crate) fn apply_table_levels(
    table: Option<&FallbackMatrix>,
    base_black: &mut u32,
    cblack: &mut [u32; 4],
    maximum: &mut u32,
) {
    let Some(f) = table else { return };
    let rblack = *base_black + cblack.iter().sum::<u32>() / 4;
    if f.black > 0 {
        *base_black = f.black as u32;
        *cblack = [0; 4];
    } else if f.black < 0 && rblack == 0 {
        *base_black = f.black.unsigned_abs();
        *cblack = [0; 4];
    }
    if f.white > 0 {
        *maximum = f.white as u32;
    }
}

/// Move odd visible-area origins to the next even coordinate (Bayer data keeps whole
/// CFA cells in the visible area).
pub(crate) fn even_margins(r: Rect) -> Rect {
    let mut r = r;
    if r.x % 2 == 1 {
        r.x += 1;
        r.width = r.width.saturating_sub(1);
    }
    if r.y % 2 == 1 {
        r.y += 1;
        r.height = r.height.saturating_sub(1);
    }
    r
}

/// A 2x2 pattern given at visible origin `(left, top)` re-anchored at the buffer origin.
pub(crate) fn cfa_at_buffer_origin(p: CfaPattern, left: u32, top: u32) -> CfaPattern {
    p.shifted((2 - left as usize % 2) % 2, (2 - top as usize % 2) % 2)
}

/// Black level per CFA site from RGGB-ordered values (R, G on red rows, G on blue
/// rows, B) plus a common base.
pub(crate) fn black_for_cfa(cfa: &CfaPattern, base: u32, rggb: [u32; 4]) -> BlackLevel {
    let mut pat = [0f32; 4];
    for y in 0..2 {
        for x in 0..2 {
            let v = match cfa.color_at(x, y) {
                CfaColor::Red => rggb[0],
                CfaColor::Blue => rggb[3],
                _ => {
                    if cfa.color_at(x ^ 1, y) == CfaColor::Red {
                        rggb[1]
                    } else {
                        rggb[2]
                    }
                }
            };
            pat[y * 2 + x] = (base + v) as f32;
        }
    }
    if pat.iter().all(|&v| v == pat[0]) { BlackLevel::uniform(pat[0]) } else { BlackLevel::pattern2x2(pat) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wb_plausibility() {
        assert!(wb_from_cam_mul([0.0, 1.0, 1.0, 1.0]).is_none());
        assert!(wb_from_cam_mul([1000.0, 1.0, 1.0, 1.0]).is_none());
        let wb = wb_from_cam_mul([2.0, 1.0, 1.5, 1.0]).unwrap();
        assert_eq!(wb.coeffs[0], 2.0);
    }

    #[test]
    fn margins_and_cfa() {
        assert_eq!(even_margins(Rect::new(1, 29, 100, 50)), Rect::new(2, 30, 99, 49));
        assert_eq!(cfa_at_buffer_origin(CfaPattern::RGGB, 1, 0), CfaPattern::GRBG);
        assert_eq!(cfa_at_buffer_origin(CfaPattern::GRBG, 48, 29), CfaPattern::BGGR);
        let b = black_for_cfa(&CfaPattern::GRBG, 10, [1, 2, 3, 4]);
        assert_eq!(b.values, vec![12.0, 11.0, 14.0, 13.0]);
    }
}

/// Test support: corpus access and malformed-input exercising for the vendor modules.
#[cfg(test)]
pub(crate) mod robustness {
    use crate::common::OpenOptions;
    use flashraw_core::{DecodeContext, Profiler, RawImage, Result};
    use std::path::PathBuf;

    /// A corpus sample (relative to the corpus root), if the corpus is available.
    /// The root is `$FLASHRAW_CORPUS` or `<workspace>/corpus`.
    pub fn corpus_file(rel: &str) -> Option<Vec<u8>> {
        let root = std::env::var_os("FLASHRAW_CORPUS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
        std::fs::read(root.join(rel)).ok()
    }

    /// Open and fully decode `data`.
    pub fn open_decode(data: &[u8]) -> Result<RawImage> {
        let dec = crate::open(data, &OpenOptions::default())?;
        let p = Profiler::new();
        let ctx = DecodeContext::new(&p);
        dec.decode(&ctx)
    }

    /// For row-addressable payloads: a region decode equals the crop of a full decode.
    /// Returns whether the file advertises row random access.
    pub fn region_matches_full(data: &[u8]) -> bool {
        use flashraw_core::{RandomAccess, Rect};
        let dec = crate::open(data, &OpenOptions::default()).expect("open");
        if dec.random_access() != RandomAccess::Rows {
            return false;
        }
        let p = Profiler::new();
        let ctx = DecodeContext::new(&p);
        let full = dec.decode(&ctx).expect("decode");
        let (w, h) = (full.width, full.height);
        for r in
            [Rect::new((w / 3) | 1, (h / 4) | 1, w / 3, h / 5), Rect::new(0, 0, w, 3), Rect::new(w - 7, h - 5, 7, 5)]
        {
            let got = dec.decode_region(&ctx, r).expect("region");
            assert_eq!(got, full.crop(r).unwrap(), "region {r}");
        }
        true
    }

    /// A sequentially coded payload cut by the end of the file (half of it, and the last
    /// byte): the rows decoded from real data are kept and the rest zero-filled with one
    /// warning under the recover policy; a structured truncation under strict. Cuts that
    /// lose nothing (trailing bytes no sample needs) must decode exactly.
    pub fn truncated_stream_recovers(name: &str, data: &[u8]) {
        let full = open_decode(data).unwrap_or_else(|e| panic!("{name}: {e}"));
        let w = full.width as usize;
        for cut in [data.len() / 2, data.len() - 1] {
            let dec = crate::open(&data[..cut], &OpenOptions::default()).unwrap();
            let p = Profiler::new();
            let mut ctx = DecodeContext::new(&p);
            ctx.recover = false;
            let strict = dec.decode(&ctx);
            let mut ctx = DecodeContext::new(&p);
            ctx.recover = true;
            let img = dec.decode(&ctx).unwrap_or_else(|e| panic!("{name} cut at {cut}: {e}"));
            let warnings = ctx.take_warnings();
            if warnings.is_empty() {
                assert!(strict.is_ok(), "{name} cut at {cut}");
                assert_eq!(img, full, "{name} cut at {cut}: silent change");
                continue;
            }
            let e = strict.expect_err("strict must fail when recover warns");
            assert!(e.truncation().is_some(), "{name} cut at {cut}: {e}");
            assert_eq!(warnings.len(), 1, "{name} cut at {cut}");
            let filled = warnings[0].samples_filled as usize;
            assert!(filled > 0 && filled % w == 0, "{name}: {filled}");
            // The rows before the zero-filled ones equal the full decode.
            let kept = img.u16().iter().rposition(|&v| v != 0).map_or(0, |i| i / w + 1) * w;
            assert!(kept < full.u16().len());
            let first_diff = img.u16()[..kept].iter().zip(full.u16()).position(|(a, b)| a != b);
            assert!(
                first_diff.is_none(),
                "{name} cut at {cut}: row {:?} of the kept rows differs",
                first_diff.map(|i| i / w)
            );
        }
    }

    /// Truncate and corrupt `data` in many ways; every variant must return (any
    /// result) without panicking. Returns how many variants were rejected with an error.
    pub fn mutate(data: &[u8], seed: u64) -> usize {
        let mut errors = 0;
        let n = data.len();
        // Truncations: inside the header, the metadata, and the payload.
        for cut in [0, 8, 64, 512, 4096, n / 8, n / 3, n / 2, n - n / 10, n.saturating_sub(64)] {
            if cut < n && open_decode(&data[..cut]).is_err() {
                errors += 1;
            }
        }
        // Byte corruption: sparse flips in the first 64 KiB (structures) and in the payload.
        let mut x = seed | 1;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for round in 0..8 {
            let mut d = data.to_vec();
            let region = if round % 2 == 0 { n.min(65536) } else { n };
            for _ in 0..(4 + round * 4) {
                let i = (rnd() as usize) % region.max(1);
                d[i] = rnd() as u8;
            }
            if open_decode(&d).is_err() {
                errors += 1;
            }
        }
        errors
    }
}
