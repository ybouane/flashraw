//! Optional CPU post-processing for convenience outputs (normalised samples, linear
//! RGB, display RGBA8). This is deliberately small: FlashRaw is a decoder, not a
//! raw developer. All RGB outputs come from one fused pass over the sensor data
//! (black subtraction, scaling, demosaic, white balance, colour matrix, transfer
//! curve, orientation), with no full-size intermediates.
//!
//! The same operations exist as WGSL compute shaders in `flashraw-gpu`; tests check
//! both paths agree.

use crate::options::Demosaic;
use flashraw_core::*;

/// sRGB (D65) → XYZ.
const XYZ_FROM_SRGB: [[f32; 3]; 3] =
    [[0.412_453, 0.357_580, 0.180_423], [0.212_671, 0.715_160, 0.072_169], [0.019_334, 0.119_193, 0.950_227]];

fn mat_mul(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut r = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn mat_inv(m: &[[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [(e * i - f * h) * inv, (c * h - b * i) * inv, (b * f - c * e) * inv],
        [(f * g - d * i) * inv, (a * i - c * g) * inv, (c * d - a * f) * inv],
        [(d * h - e * g) * inv, (b * g - a * h) * inv, (a * e - b * d) * inv],
    ])
}

/// rows of camera←sRGB are normalised to 1 so white balance maps white to white).
pub fn camera_to_srgb(cm: &ColorMatrices) -> [[f32; 3]; 3] {
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let Some(m) = cm.preferred() else { return identity };
    if m.values.len() < 9 {
        return identity;
    }
    let cam_xyz = [
        [m.values[0], m.values[1], m.values[2]],
        [m.values[3], m.values[4], m.values[5]],
        [m.values[6], m.values[7], m.values[8]],
    ];
    let mut cam_rgb = mat_mul(&cam_xyz, &XYZ_FROM_SRGB);
    for row in cam_rgb.iter_mut() {
        let s: f32 = row.iter().sum();
        if s.abs() > 1e-6 {
            row.iter_mut().for_each(|v| *v /= s);
        }
    }
    mat_inv(&cam_rgb).unwrap_or(identity)
}

/// Measure per-position black levels from masked (optical black) areas.
/// Returns a pattern with the CFA's dimensions (or 1x1 without CFA).
pub fn black_from_masked(img: &RawImage, info: &FrameInfo) -> Option<BlackLevel> {
    if info.masked_areas.is_empty() || img.cpp != 1 {
        return None;
    }
    let data = img.data.as_u16()?;
    let (pw, ph) = info.cfa.map(|p| (p.width(), p.height())).unwrap_or((1, 1));
    let mut sum = vec![0f64; pw * ph];
    let mut cnt = vec![0u64; pw * ph];
    for r in &info.masked_areas {
        let r = r.clamp_to(img.width, img.height);
        for y in r.y as usize..r.bottom() as usize {
            let row = &data[y * img.width as usize..];
            for x in r.x as usize..r.right() as usize {
                let i = (y % ph) * pw + x % pw;
                sum[i] += row[x] as f64;
                cnt[i] += 1;
            }
        }
    }
    if cnt.iter().any(|&c| c == 0) {
        let total: u64 = cnt.iter().sum();
        if total == 0 {
            return None;
        }
        let mean = (sum.iter().sum::<f64>() / total as f64) as f32;
        return Some(BlackLevel::uniform(mean));
    }
    let values: Vec<f32> = sum.iter().zip(&cnt).map(|(s, &c)| (s / c as f64) as f32).collect();
    Some(BlackLevel::pattern(ph, pw, values))
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}
fn lcm(a: usize, b: usize) -> usize {
    a / gcd(a, b) * b
}

/// Precomputed per-position calibration: `(v - black[pos]) * scale[pos]` gives
/// normalised [0,1] sensor values.
struct Calib {
    pw: usize,
    ph: usize,
    black: Vec<f32>,
    scale: Vec<f32>,
    row_deltas: Vec<f32>,
    col_deltas: Vec<f32>,
}

impl Calib {
    fn new(info: &FrameInfo, c: usize) -> Self {
        let (cw, chh) = info.cfa.map(|p| (p.width(), p.height())).unwrap_or((1, 1));
        let b = &info.black_level;
        let pw = lcm(cw, b.cols.max(1) as usize).min(64);
        let ph = lcm(chh, b.rows.max(1) as usize).min(64);
        let mut black = Vec::with_capacity(pw * ph);
        let mut scale = Vec::with_capacity(pw * ph);
        for y in 0..ph {
            for x in 0..pw {
                // Base pattern only; deltas are added separately.
                let bl = b
                    .values
                    .get(
                        ((y % b.rows.max(1) as usize) * b.cols.max(1) as usize + x % b.cols.max(1) as usize)
                            * b.samples.max(1) as usize
                            + c % b.samples.max(1) as usize,
                    )
                    .copied()
                    .unwrap_or(0.0);
                let color = info.cfa.map(|p| p.color_at(x, y) as usize).unwrap_or(c);
                let white = info.white_level.at(if info.white_level.values.len() > 1 {
                    color.min(info.white_level.values.len() - 1)
                } else {
                    0
                }) as f32;
                black.push(bl);
                scale.push(if white > bl { 1.0 / (white - bl) } else { 1.0 });
            }
        }
        Calib { pw, ph, black, scale, row_deltas: b.row_deltas.clone(), col_deltas: b.col_deltas.clone() }
    }
    #[inline(always)]
    fn norm(&self, v: f32, x: usize, y: usize) -> f32 {
        let i = (y % self.ph) * self.pw + x % self.pw;
        let mut b = self.black[i];
        if !self.row_deltas.is_empty() {
            b += self.row_deltas.get(y).copied().unwrap_or(0.0);
        }
        if !self.col_deltas.is_empty() {
            b += self.col_deltas.get(x).copied().unwrap_or(0.0);
        }
        (v - b) * self.scale[i]
    }
}

/// Normalise sensor samples to `[0, 1]` (black subtracted, white scaled), keeping layout.
pub fn normalize(img: &RawImage, info: &FrameInfo, origin: (u32, u32), parallel: bool) -> Vec<f32> {
    let cpp = img.cpp as usize;
    let calibs: Vec<Calib> = (0..cpp).map(|c| Calib::new(info, c)).collect();
    let w = img.width as usize;
    let mut out = vec![0f32; img.data.len()];
    let rl = w * cpp;
    par::for_each_row_chunk(&mut out, rl, 32, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_mut(rl).enumerate() {
            let y = first + i;
            for x in 0..w {
                for c in 0..cpp {
                    let v = img.sample(x as u32, y as u32, c as u32);
                    row[x * cpp + c] = calibs[c].norm(v, x + origin.0 as usize, y + origin.1 as usize).max(0.0);
                }
            }
        }
    });
    out
}

/// Parameters of the fused develop pass.
pub struct DevelopParams {
    pub demosaic: Demosaic,
    pub crop: Rect,
    /// White balance multipliers per CFA colour index (R, G, B, other).
    pub wb: [f32; 4],
    pub cam_to_rgb: [[f32; 3]; 3],
    pub orientation: Orientation,
}

impl DevelopParams {
    pub fn new(info: &FrameInfo, demosaic: Demosaic, crop: Rect) -> Self {
        let wb = info.white_balance.map(|w| w.coeffs).unwrap_or([1.0; 4]);
        DevelopParams {
            demosaic,
            crop,
            wb,
            cam_to_rgb: camera_to_srgb(&info.color_matrices),
            orientation: info.orientation,
        }
    }
}

/// Output of [`develop`].
pub struct Developed {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<f32>,
}

/// Demosaic + white balance + colour conversion to linear sRGB (f32 RGB), over `params.crop`
/// (frame coordinates). `origin` is the frame's position in sensor coordinates.
pub fn develop(
    img: &RawImage,
    info: &FrameInfo,
    origin: (u32, u32),
    params: &DevelopParams,
    parallel: bool,
) -> Result<Developed> {
    let crop = params.crop.clamp_to(img.width, img.height);
    if crop.is_empty() {
        return Err(Error::InvalidArgument("empty develop region".into()));
    }
    let half = params.demosaic == Demosaic::HalfSize;
    let (ow, oh) = if half { (crop.width / 2, crop.height / 2) } else { (crop.width, crop.height) };
    if ow == 0 || oh == 0 {
        return Err(Error::InvalidArgument("develop region too small".into()));
    }
    let mut out = vec![0f32; ow as usize * oh as usize * 3];
    let m = params.cam_to_rgb;
    let wb = params.wb;
    let w = img.width as usize;
    let h = img.height as usize;
    let cpp = img.cpp as usize;
    let calibs: Vec<Calib> = (0..cpp).map(|c| Calib::new(info, c)).collect();
    let (ox, oy) = (origin.0 as usize, origin.1 as usize);
    let sample = |x: usize, y: usize, c: usize| -> f32 {
        let v = match &img.data {
            PixelData::U16(d) => d[(y * w + x) * cpp + c] as f32,
            PixelData::F32(d) => d[(y * w + x) * cpp + c],
        };
        calibs[c].norm(v, x + ox, y + oy)
    };
    let color_of =
        |x: usize, y: usize| -> usize { info.cfa.map(|p| (p.color_at(x + ox, y + oy) as usize).min(3)).unwrap_or(1) };
    let finish = |mut rgb: [f32; 3], px: &mut [f32]| {
        // Clip at sensor saturation after WB (avoids magenta highlights), then convert.
        for v in rgb.iter_mut() {
            *v = v.clamp(0.0, 1.0);
        }
        for i in 0..3 {
            px[i] = m[i][0] * rgb[0] + m[i][1] * rgb[1] + m[i][2] * rgb[2];
        }
    };
    let rl = ow as usize * 3;
    let is_cfa = cpp == 1 && info.cfa.is_some();
    // Precompute 3x3 same-colour neighbour offsets per CFA position.
    let (pw, ph) = info.cfa.map(|p| (p.width(), p.height())).unwrap_or((1, 1));
    let mut neigh: Vec<[Vec<(isize, isize)>; 4]> = Vec::with_capacity(pw * ph);
    for py in 0..ph {
        for px in 0..pw {
            let mut lists: [Vec<(isize, isize)>; 4] = Default::default();
            for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    let c = info
                        .cfa
                        .map(|p| {
                            (p.color_at(
                                (px as isize + dx + pw as isize * 4) as usize,
                                (py as isize + dy + ph as isize * 4) as usize,
                            ) as usize)
                                .min(3)
                        })
                        .unwrap_or(1);
                    lists[c].push((dx, dy));
                }
            }
            neigh.push(lists);
        }
    }
    par::for_each_row_chunk(&mut out, rl, 16, parallel, |first, chunk| {
        for (i, orow) in chunk.chunks_mut(rl).enumerate() {
            let oyy = first + i;
            for oxx in 0..ow as usize {
                let px = &mut orow[oxx * 3..oxx * 3 + 3];
                if half {
                    let (x0, y0) = (crop.x as usize + oxx * 2, crop.y as usize + oyy * 2);
                    let mut acc = [0f32; 4];
                    let mut n = [0f32; 4];
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (x, y) = (x0 + dx, y0 + dy);
                        if x < w && y < h {
                            if is_cfa {
                                let c = color_of(x, y);
                                acc[c] += sample(x, y, 0);
                                n[c] += 1.0;
                            } else {
                                for c in 0..cpp.min(3) {
                                    acc[c] += sample(x, y, c);
                                    n[c] += 1.0;
                                }
                            }
                        }
                    }
                    let mut rgb = [0f32; 3];
                    for c in 0..3 {
                        rgb[c] = if n[c] > 0.0 { acc[c] / n[c] } else { 0.0 };
                    }
                    if cpp == 1 && info.cfa.is_none() {
                        rgb = [acc[1] / n[1].max(1.0); 3];
                    }
                    finish([rgb[0] * wb[0], rgb[1] * wb[1], rgb[2] * wb[2]], px);
                    continue;
                }
                let (x, y) = (crop.x as usize + oxx, crop.y as usize + oyy);
                let rgb = if is_cfa {
                    let lists = &neigh[(y + oy) % ph * pw + (x + ox) % pw];
                    let mut rgb = [0f32; 3];
                    for (c, list) in lists.iter().enumerate().take(3) {
                        let mut s = 0f32;
                        let mut n = 0f32;
                        for &(dx, dy) in list {
                            let (xx, yy) = (x as isize + dx, y as isize + dy);
                            if xx >= 0 && yy >= 0 && (xx as usize) < w && (yy as usize) < h {
                                s += sample(xx as usize, yy as usize, 0);
                                n += 1.0;
                            }
                        }
                        rgb[c] = if n > 0.0 { s / n } else { 0.0 };
                    }
                    rgb
                } else if cpp >= 3 {
                    [sample(x, y, 0), sample(x, y, 1), sample(x, y, 2)]
                } else {
                    let v = sample(x, y, 0);
                    [v, v, v]
                };
                finish([rgb[0] * wb[0], rgb[1] * wb[1], rgb[2] * wb[2]], px);
            }
        }
    });
    Ok(Developed { width: ow, height: oh, rgb: out })
}

/// Linear → sRGB-encoded 8-bit lookup (4096 entries over [0, 1]).
fn srgb_lut() -> &'static [u8; 4097] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[u8; 4097]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut l = [0u8; 4097];
        for (i, v) in l.iter_mut().enumerate() {
            let x = i as f32 / 4096.0;
            let s = if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
            *v = (s * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
        l
    })
}

/// Encode linear RGB to sRGB RGBA8 and apply EXIF orientation.
pub fn to_rgba8(d: &Developed, orientation: Orientation, parallel: bool) -> (u32, u32, Vec<u8>) {
    let lut = srgb_lut();
    let (w, h) = (d.width as usize, d.height as usize);
    let (ow, oh) = if orientation.swaps_dimensions() { (h, w) } else { (w, h) };
    let mut out = vec![0u8; ow * oh * 4];
    par::for_each_row_chunk(&mut out, ow * 4, 16, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_mut(ow * 4).enumerate() {
            let oy = first + i;
            for ox in 0..ow {
                // Map output pixel back to source pixel.
                let (sx, sy) = match orientation {
                    Orientation::Normal => (ox, oy),
                    Orientation::FlipH => (w - 1 - ox, oy),
                    Orientation::Rotate180 => (w - 1 - ox, h - 1 - oy),
                    Orientation::FlipV => (ox, h - 1 - oy),
                    Orientation::Transpose => (oy, ox),
                    Orientation::Rotate90 => (oy, h - 1 - ox),
                    Orientation::Transverse => (w - 1 - oy, h - 1 - ox),
                    Orientation::Rotate270 => (w - 1 - oy, ox),
                };
                let s = &d.rgb[(sy * w + sx) * 3..(sy * w + sx) * 3 + 3];
                let p = &mut row[ox * 4..ox * 4 + 4];
                for c in 0..3 {
                    p[c] = lut[(s[c].clamp(0.0, 1.0) * 4096.0) as usize];
                }
                p[3] = 255;
            }
        }
    });
    (ow as u32, oh as u32, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_matrix_without_color() {
        let m = camera_to_srgb(&ColorMatrices::default());
        assert_eq!(m[0], [1.0, 0.0, 0.0]);
    }

    #[test]
    fn matrix_maps_white_to_white() {
        // Any XYZ->cam matrix: after normalisation, camera white (1,1,1) maps to sRGB white.
        let cm = ColorMatrices {
            color: vec![
                ColorMatrix::new(Illuminant::D65, 3, vec![0.6, -0.1, -0.05, -0.4, 1.2, 0.2, -0.1, 0.25, 0.6]).unwrap(),
            ],
            ..Default::default()
        };
        let m = camera_to_srgb(&cm);
        for row in m {
            let s: f32 = row.iter().sum();
            assert!((s - 1.0).abs() < 1e-4, "{s}");
        }
    }

    #[test]
    fn bilinear_flat_field() {
        // A flat grey Bayer image develops to flat grey.
        let mut img = RawImage::new_u16(8, 8, 1, &Limits::default()).unwrap();
        img.u16_mut().iter_mut().for_each(|v| *v = 1000);
        let mut info = FrameInfo::new(8, 8, 12);
        info.cfa = Some(CfaPattern::RGGB);
        info.black_level = BlackLevel::uniform(0.0);
        info.white_level = WhiteLevel::uniform(4000);
        let p = DevelopParams::new(&info, Demosaic::Bilinear, Rect::full(8, 8));
        let d = develop(&img, &info, (0, 0), &p, false).unwrap();
        assert!(d.rgb.iter().all(|&v| (v - 0.25).abs() < 1e-5));
        let p = DevelopParams::new(&info, Demosaic::HalfSize, Rect::full(8, 8));
        let d = develop(&img, &info, (0, 0), &p, false).unwrap();
        assert_eq!((d.width, d.height), (4, 4));
    }

    #[test]
    fn orientation_rotation() {
        let d = Developed { width: 2, height: 1, rgb: vec![1.0, 0.0, 0.0, 0.0, 0.0, 1.0] };
        let (w, h, px) = to_rgba8(&d, Orientation::Rotate90, false);
        assert_eq!((w, h), (1, 2));
        // Rotate 90 CW: left pixel (red) ends on top.
        assert_eq!(&px[0..3], &[255, 0, 0]);
        assert_eq!(&px[4..7], &[0, 0, 255]);
    }
}
