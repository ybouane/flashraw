//! decoders: raw IFD selection, white-balance plausibility, colour-table lookup,
//! orientation and visible-area normalisation.

use crate::common::*;
use flashraw_camera_db::FallbackMatrix;
use flashraw_core::tiff::{Ifd, Tiff, tags};
use flashraw_core::*;

/// The raw IFD: the largest image (by samples x bits) that is not a JPEG preview.
/// Ties keep the first IFD in file order (the first frame of multi-shot files).
pub(crate) fn select_raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
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
pub(crate) fn wb_from_cam_mul(m: [f32; 4]) -> Option<WhiteBalance> {
    if m[..3].iter().any(|&v| !(v > 0.001)) {
        return None;
    }
    let min = m[..3].iter().copied().fold(f32::MAX, f32::min);
    let max = m[..3].iter().copied().fold(0.0, f32::max);
    if max / min > 100.0 {
        return None;
    }
    WhiteBalance::from_multipliers(m[0], m[1], m[2], if m[3] > 0.0 { m[3] } else { m[1] })
}

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
