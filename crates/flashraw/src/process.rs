//! Optional CPU post-processing for convenience outputs (normalised samples, linear
//! RGB, display RGBA8). This is deliberately small: FlashRaw is a decoder, not a
//! raw developer (the opt-in edge-aware demosaic is the one larger piece). All RGB outputs come from one fused pass over the sensor data
//! (black subtraction, scaling, demosaic, white balance, colour matrix, transfer
//! curve, orientation), with no full-size intermediates.
//!
//! The same operations exist as WGSL compute shaders in `flashraw-gpu`; tests check
//! both paths agree.

use crate::RawPixelData;
use crate::ahd;
use crate::bayer_alt;
use crate::color::{ColorSpace, OutputCurve};
use crate::edgeaware;
use crate::options::{Demosaic, Highlights};
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

/// Camera RGB → linear sRGB matrix from an XYZ→camera matrix (rows of camera←sRGB are normalised to 1 so white balance maps white to white).
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

/// Four camera channels → linear sRGB (3x4) for a four-colour CFA, from its 4-row
/// XYZ→camera matrix (rows in [`CfaPattern::four_colour_order`]): the pseudo-inverse
/// of camera←sRGB with rows normalised to 1, so unit channels map to white. `None`
/// without a usable 4-row matrix.
pub fn camera4_to_srgb(cm: &ColorMatrices) -> Option<[[f32; 4]; 3]> {
    let mut a = camera4_from_srgb(cm)?;
    for row in a.iter_mut() {
        let s: f64 = row.iter().sum();
        if s.abs() > 1e-6 {
            row.iter_mut().for_each(|v| *v /= s);
        }
    }
    // (AᵀA)⁻¹ Aᵀ
    let mut ata = [[0f32; 3]; 3];
    for (i, r) in ata.iter_mut().enumerate() {
        for (j, v) in r.iter_mut().enumerate() {
            *v = (0..4).map(|k| a[k][i] * a[k][j]).sum::<f64>() as f32;
        }
    }
    let inv = mat_inv(&ata)?;
    let mut out = [[0f32; 4]; 3];
    for (i, r) in out.iter_mut().enumerate() {
        for (k, v) in r.iter_mut().enumerate() {
            *v = (0..3).map(|j| inv[i][j] as f64 * a[k][j]).sum::<f64>() as f32;
        }
    }
    out.iter().flatten().all(|v| v.is_finite()).then_some(out)
}

/// Camera←sRGB (4x3) from a 4-row XYZ→camera matrix.
fn camera4_from_srgb(cm: &ColorMatrices) -> Option<[[f64; 3]; 4]> {
    let m = cm.preferred().filter(|m| m.rows == 4 && m.values.len() == 12)?;
    let mut a = [[0f64; 3]; 4];
    for (i, row) in a.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| m.values[i * 3 + k] as f64 * XYZ_FROM_SRGB[k][j] as f64).sum();
        }
    }
    Some(a)
}

/// Daylight (D65) white balance of a four-colour CFA from its 4-row colour matrix: the
/// reciprocal camera response to white, for files that record no white balance.
pub fn camera4_daylight_wb(cm: &ColorMatrices) -> Option<[f32; 4]> {
    let m = cm.preferred().filter(|m| m.rows == 4)?;
    m.multipliers_for([1.0; 3])?.try_into().ok()
}

/// Whether the white balance of a frame can be replaced: a three-colour mosaic the camera did not
/// balance, not Foveon.
fn wb_replaceable(info: &FrameInfo) -> bool {
    !info.white_balance_applied
        && info.layout != SensorLayout::Foveon
        && info.cfa.is_some_and(|p| p.four_colour_order().is_none())
}

/// Daylight (D65) multipliers of a frame from its colour matrix (`WhiteBalanceMode::Daylight`),
/// `None` where the balance cannot be replaced or the file has no 3-row matrix.
pub(crate) fn daylight_white_balance(info: &FrameInfo) -> Option<[f32; 4]> {
    if !wb_replaceable(info) {
        return None;
    }
    let m = info.color_matrices.preferred().filter(|m| m.rows == 3)?;
    let v = m.multipliers_for([1.0; 3])?;
    explicit_white_balance(info, [v[0], v[1], v[2]])
}

/// Explicit red, green and blue multipliers (green normalised to 1) as the four multipliers of a
/// frame; `None` where the balance cannot be replaced.
pub(crate) fn explicit_white_balance(info: &FrameInfo, m: [f32; 3]) -> Option<[f32; 4]> {
    wb_replaceable(info).then(|| [m[0] / m[1], 1.0, m[2] / m[1], 1.0])
}

/// Measure per-position black levels from masked (optical black) areas.
/// Returns a pattern with the CFA's dimensions (or 1x1 without CFA), anchored at buffer (0, 0);
/// images with several samples per pixel get one value per sample.
pub fn black_from_masked(img: &RawImage, info: &FrameInfo) -> Option<BlackLevel> {
    black_from_masked_at(img, info, (0, 0))
}

/// [`black_from_masked`] for an image whose top-left is at `origin` in the sensor buffer
/// (region decodes): `info.masked_areas` are buffer coordinates, the pattern is still
/// anchored at buffer (0, 0).
pub fn black_from_masked_at(img: &RawImage, info: &FrameInfo, origin: (u32, u32)) -> Option<BlackLevel> {
    if info.masked_areas.is_empty() {
        return None;
    }
    let data = img.data.as_u16()?;
    if img.cpp != 1 {
        return black_per_sample(data, img, info, origin);
    }
    let (pw, ph) = info.cfa.map(|p| (p.width(), p.height())).unwrap_or((1, 1));
    let w = img.width as usize;
    let (ox, oy) = (origin.0 as usize, origin.1 as usize);
    let frame = Rect::new(origin.0, origin.1, img.width, img.height);
    // (row, x0, x1) spans of every masked area (buffer coordinates); rows are summed in
    // parallel with integer accumulators and no per-pixel modulo.
    let mut spans: Vec<(usize, usize, usize)> = Vec::new();
    for r in &info.masked_areas {
        let Some(r) = r.intersect(&frame) else { continue };
        for y in r.y as usize..r.bottom() as usize {
            spans.push((y, r.x as usize, r.right() as usize));
        }
    }
    let partial = par::map(spans, true, |(y, x0, x1)| {
        let mut sum = vec![0u64; pw * ph];
        let mut cnt = vec![0u64; pw * ph];
        let row = &data[(y - oy) * w..(y - oy) * w + w];
        let base = (y % ph) * pw;
        for k in 0..pw {
            // First x >= x0 with x % pw == k.
            let mut x = x0 + (k + pw - x0 % pw) % pw;
            while x < x1 {
                sum[base + k] += row[x - ox] as u64;
                cnt[base + k] += 1;
                x += pw;
            }
        }
        (sum, cnt)
    });
    let mut sum = vec![0f64; pw * ph];
    let mut cnt = vec![0u64; pw * ph];
    for (s, c) in partial {
        for i in 0..pw * ph {
            sum[i] += s[i] as f64;
            cnt[i] += c[i];
        }
    }
    if cnt.contains(&0) {
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

/// Masked-area black of a multi-sample image (Foveon, linear RGB): one mean per sample.
fn black_per_sample(data: &[u16], img: &RawImage, info: &FrameInfo, origin: (u32, u32)) -> Option<BlackLevel> {
    let (w, cpp) = (img.width as usize, img.cpp as usize);
    let frame = Rect::new(origin.0, origin.1, img.width, img.height);
    let mut sum = vec![0u64; cpp];
    let mut cnt = 0u64;
    for r in &info.masked_areas {
        let Some(r) = r.intersect(&frame) else { continue };
        for y in r.y as usize..r.bottom() as usize {
            let row = (y - origin.1 as usize) * w;
            let px = &data
                [(row + r.x as usize - origin.0 as usize) * cpp..(row + r.right() as usize - origin.0 as usize) * cpp];
            for p in px.chunks_exact(cpp) {
                for (s, &v) in sum.iter_mut().zip(p) {
                    *s += v as u64;
                }
            }
            cnt += r.width as u64;
        }
    }
    if cnt == 0 {
        return None;
    }
    let values = sum.iter().map(|&s| (s as f64 / cnt as f64) as f32).collect();
    Some(BlackLevel { rows: 1, cols: 1, samples: cpp as u8, values, row_deltas: vec![], col_deltas: vec![] })
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

/// Exposure and black/white points of RGB outputs, applied in linear space after the
/// colour conversion: `v' = (v * 2^exposure - black) / (white - black)`, i.e.
/// `v * scale - offset`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    pub scale: f32,
    pub offset: f32,
}

impl Levels {
    /// Validated levels; `None` for the identity (0 EV, black 0, white 1), which leaves
    /// outputs bit-identical to a develop without levels.
    pub fn new(exposure: f32, black: f32, white: f32) -> Result<Option<Levels>> {
        if !exposure.is_finite() || exposure.abs() > 32.0 {
            return Err(Error::InvalidArgument(format!("exposure {exposure} EV outside [-32, 32]")));
        }
        if !black.is_finite() || !white.is_finite() || white <= black {
            return Err(Error::InvalidArgument(format!(
                "white point {white} must be finite and above the black point {black}"
            )));
        }
        if exposure == 0.0 && black == 0.0 && white == 1.0 {
            return Ok(None);
        }
        let range = white as f64 - black as f64;
        Ok(Some(Levels { scale: (2f64.powf(exposure as f64) / range) as f32, offset: (black as f64 / range) as f32 }))
    }
}

/// Parameters of the fused develop pass.
#[derive(Debug, Clone)]
pub struct DevelopParams {
    pub demosaic: Demosaic,
    pub crop: Rect,
    /// White balance multipliers per CFA colour index (R, G, B, other), or per channel of
    /// a four-colour CFA (in [`CfaPattern::four_colour_order`]).
    pub wb: [f32; 4],
    /// The balance the edge-aware demosaic applies to the mosaic before interpolating (the
    /// same as `wb` except in `DevelopParams::camera_rgb`, which develops unbalanced).
    pub dm_wb: [f32; 4],
    pub cam_to_rgb: [[f32; 3]; 3],
    /// Four-colour CFAs: camera channels → linear sRGB (see [`camera4_to_srgb`]); when set
    /// it replaces `cam_to_rgb` and all four channels are demosaiced.
    pub cam4_to_rgb: Option<[[f32; 4]; 3]>,
    pub orientation: Orientation,
    /// Exposure / black / white points (`None`: none).
    pub levels: Option<Levels>,
    /// Clipped highlights (see [`Highlights`]).
    pub highlights: Highlights,
    /// The model of [`Highlights::Reconstruct`] (predicts nothing by default).
    pub recon: ReconModel,
    /// Smoothing level of the edge-aware demosaic.
    pub noise: crate::NoiseReduction,
    /// Transfer curve of the display encoders (sRGB unless an output colour space or
    /// profile is set; the matching primaries are already folded into the matrices).
    pub curve: OutputCurve,
}

impl DevelopParams {
    pub fn new(info: &FrameInfo, demosaic: Demosaic, crop: Rect) -> Self {
        let cam4_to_rgb =
            info.cfa.and_then(|p| p.four_colour_order()).and_then(|_| camera4_to_srgb(&info.color_matrices));
        let wb = match info.white_balance {
            // The camera already balanced the samples (YCbCr M/S and sNEF payloads).
            _ if info.white_balance_applied => [1.0; 4],
            Some(w) => w.coeffs,
            // Four-colour sensors without a recorded balance: daylight from the matrix.
            None if cam4_to_rgb.is_some() => camera4_daylight_wb(&info.color_matrices).unwrap_or([1.0; 4]),
            None => [1.0; 4],
        };
        let wb = if info.layout == SensorLayout::Foveon { normalized_min(wb) } else { wb };
        DevelopParams {
            demosaic,
            crop,
            // Foveon balances scale a layer down (blue ~0.6): with the smallest multiplier
            // at 1, saturated samples stay saturated in every layer and render white.
            wb,
            dm_wb: wb,
            cam_to_rgb: camera_to_srgb(&info.color_matrices),
            cam4_to_rgb,
            orientation: info.orientation,
            levels: None,
            highlights: Highlights::Clip,
            recon: ReconModel::default(),
            noise: crate::NoiseReduction::Auto,
            curve: OutputCurve::Srgb,
        }
    }

    /// Develop to `cs` instead of sRGB: its primaries are folded into `cam_to_rgb` (and
    /// `cam4_to_rgb`) once, so the per-pixel cost is unchanged, and the display encoders
    /// use its transfer curve. sRGB leaves the parameters untouched.
    pub fn with_color_space(self, cs: ColorSpace) -> Self {
        if matches!(cs, ColorSpace::Srgb | ColorSpace::Custom) {
            return self;
        }
        self.with_output(cs.matrix_from_srgb(), cs.output_curve())
    }

    /// Fold `m` (linear sRGB → output RGB) into the camera matrices and select the
    /// encoder curve.
    pub fn with_output(mut self, m: [[f64; 3]; 3], curve: OutputCurve) -> Self {
        // m (3x3) times a (3 x N), in f64.
        let fold = |a: &[f32], n: usize| -> Vec<f32> {
            (0..3 * n).map(|k| (0..3).map(|j| m[k / n][j] * a[j * n + k % n] as f64).sum::<f64>() as f32).collect()
        };
        let flat: Vec<f32> = self.cam_to_rgb.iter().flatten().copied().collect();
        let f = fold(&flat, 3);
        self.cam_to_rgb = std::array::from_fn(|i| std::array::from_fn(|j| f[i * 3 + j]));
        self.cam4_to_rgb = self.cam4_to_rgb.map(|a| {
            let flat: Vec<f32> = a.iter().flatten().copied().collect();
            let f = fold(&flat, 4);
            std::array::from_fn(|i| std::array::from_fn(|j| f[i * 4 + j]))
        });
        self.curve = curve;
        self
    }

    /// The same develop up to camera RGB: no white balance, identity matrix, no levels.
    pub(crate) fn camera_rgb(&self) -> Self {
        DevelopParams {
            wb: [1.0; 4],
            cam_to_rgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            levels: None,
            ..self.clone()
        }
    }
}

/// Normalised level (per channel, before white balance) from which a sample counts as clipped
/// for [`Highlights::Reconstruct`].
const CLIP_LEVEL: f32 = 0.98;

/// The largest factor over its clipped value that a prediction may raise a channel by. Fitted on
/// the frame's unclipped blocks, the prediction can overshoot badly where the colour of the
/// clipped area is unlike the rest of the frame. In a simulation (26 crops of real frames, the
/// brightest 4 % of the greens clipped, the clipped pixels rebuilt and compared with the unclipped
/// development in the log domain), capping the prediction at 1.5 times the clipped value cut the
/// worst case against plain clipping from +0.067 to +0.010 and the mean error from 0.0517 to 0.0484
/// (plain clipping 0.0561). The simulation clips only a little above the unclipped values, so it
/// favours a low cap; real saturation reaches higher, which is why the cap is not lower.
const RECON_HEADROOM: f32 = 1.5;

/// What [`Highlights::Reconstruct`] knows about a frame: predictions of a clipped channel from
/// the others in white-balanced linear camera RGB, fitted on the frame's unclipped blocks
/// (`fit_reconstruction`). The default predicts nothing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ReconModel {
    /// `alone[k]`: channel `k` clipped alone is `a * first other + b * second other`.
    alone: [Option<[f32; 2]>; 3],
    /// `ratio[k][j]`: channel `k` clipped with one more is `ratio * channel j` (j unclipped).
    ratio: [[Option<f32>; 3]; 3],
}

impl ReconModel {
    /// The balanced pixel `u` with its clipped channels (`clipped[k]`) replaced by their
    /// predictions when those are larger.
    #[inline]
    fn apply(&self, u: &mut [f32; 3], clipped: [bool; 3]) {
        match clipped.iter().filter(|&&c| c).count() {
            1 => {
                let k = clipped.iter().position(|&c| c).unwrap();
                if let Some([a, b]) = self.alone[k] {
                    let (i, j) = ((k + 1) % 3, (k + 2) % 3);
                    u[k] = u[k].max((a * u[i] + b * u[j]).min(RECON_HEADROOM * u[k]));
                }
            }
            2 => {
                let j = clipped.iter().position(|&c| !c).unwrap();
                for k in 0..3 {
                    if clipped[k] {
                        if let Some(r) = self.ratio[k][j] {
                            u[k] = u[k].max((r * u[j]).min(RECON_HEADROOM * u[k]));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Means per colour of the 6x6 blocks of a colour filter array frame (a multiple of the Bayer
/// and X-Trans periods), without the blocks that hold a sample at or above `clip`.
fn block_means(img: &RawImage, info: &FrameInfo, origin: (u32, u32), clip: f32) -> Option<Vec<[f32; 3]>> {
    let cfa = info.cfa?;
    if img.cpp != 1
        || info.white_balance_applied
        || cfa.four_colour_order().is_some()
        || info.layout == SensorLayout::Foveon
        || 6 % cfa.width() != 0
        || 6 % cfa.height() != 0
    {
        return None;
    }
    let k = normalizer(img, info, origin);
    let (w, h) = (img.width as usize, img.height as usize);
    let (ox, oy) = k.origin;
    let mut rows = vec![vec![0f32; w]; 6];
    let mut blocks: Vec<[f32; 3]> = Vec::new();
    for by in 0..h / 6 {
        for (i, r) in rows.iter_mut().enumerate() {
            k.normalize_row(by * 6 + i, r);
        }
        for bx in 0..w / 6 {
            let (mut sum, mut n, mut clipped) = ([0f32; 3], [0f32; 3], false);
            for (i, r) in rows.iter().enumerate() {
                for (j, &v) in r[bx * 6..bx * 6 + 6].iter().enumerate() {
                    let c = k.chan[cfa.color_at(bx * 6 + j + ox, by * 6 + i + oy) as usize] as usize;
                    if c < 3 {
                        sum[c] += v;
                        n[c] += 1.0;
                        clipped |= v >= clip;
                    }
                }
            }
            if !clipped && n.iter().all(|&c| c > 0.0) {
                blocks.push([sum[0] / n[0], sum[1] / n[1], sum[2] / n[2]]);
            }
        }
    }
    Some(blocks)
}

/// Fit the [`ReconModel`] of a frame balanced with `wb`: least squares (no intercept) over the
/// unclipped 6x6 blocks whose predicted channel is in the upper part of its range (above 40 %
/// of its clipping level, where the prediction is needed), at least 300 blocks per model.
pub(crate) fn fit_reconstruction(
    img: &RawImage,
    info: &FrameInfo,
    origin: (u32, u32),
    wb: [f32; 4],
) -> Option<ReconModel> {
    let blocks = block_means(img, info, origin, CLIP_LEVEL)?;
    let bal: Vec<[f32; 3]> = blocks.iter().map(|b| [b[0] * wb[0], b[1] * wb[1], b[2] * wb[2]]).collect();
    let mut model = ReconModel::default();
    for k in 0..3 {
        let (i, j) = ((k + 1) % 3, (k + 2) % 3);
        let lo = 0.4 * wb[k] * CLIP_LEVEL;
        // Two unknowns: normal equations of b_k = a b_i + b b_j.
        let (mut sii, mut sij, mut sjj, mut sik, mut sjk, mut n) = (0f64, 0f64, 0f64, 0f64, 0f64, 0u32);
        let (mut rj_num, mut rj_den) = ([0f64; 3], [0f64; 3]);
        for b in &bal {
            if b[k] < lo {
                continue;
            }
            let (x, y, z) = (b[i] as f64, b[j] as f64, b[k] as f64);
            sii += x * x;
            sij += x * y;
            sjj += y * y;
            sik += x * z;
            sjk += y * z;
            n += 1;
            for (c, o) in [(i, x), (j, y)] {
                rj_num[c] += z * o;
                rj_den[c] += o * o;
            }
        }
        if n >= 300 {
            // A small ridge keeps nearly collinear channels (neutral scenes) well conditioned.
            let ridge = 1e-4 * (sii + sjj);
            let (sii, sjj) = (sii + ridge, sjj + ridge);
            let det = sii * sjj - sij * sij;
            if det > 0.0 {
                let a = (sik * sjj - sjk * sij) / det;
                let b = (sjk * sii - sik * sij) / det;
                if a.is_finite() && b.is_finite() && a >= 0.0 && b >= 0.0 {
                    model.alone[k] = Some([a as f32, b as f32]);
                }
            }
            for c in [i, j] {
                if rj_den[c] > 0.0 {
                    let r = (rj_num[c] / rj_den[c]) as f32;
                    if r.is_finite() && r > 0.0 {
                        model.ratio[k][c] = Some(r);
                    }
                }
            }
        }
    }
    Some(model)
}

/// Normalised level (per channel, before white balance) where [`Highlights::Neutral`] and [`Highlights::Blend`] start
/// fading a pixel to white; it is fully white at 1, the sensor's saturation.
const NEUTRAL_START: f32 = 0.95;

/// Automatic white balance of a colour filter array frame: starting from `start` (the
/// camera's multipliers), refine them over the near-neutral areas. The frame is cut into 6x6
/// blocks (a multiple of the Bayer and X-Trans periods) and each block's mean per colour is
/// balanced with the current multipliers; the blocks that are mid-tone (green between 5 % and
/// 80 % of the white level), unclipped and near neutral (red and blue within 30 % of green)
/// then pull the multipliers so that their means are equal, three times over. Coloured areas
/// that dominate the scene do not take part, and the result stays within a factor of two of
/// `start`. `None` when the frame is not a three-colour mosaic that still needs balancing, or
/// fewer than 200 blocks are near neutral.
pub(crate) fn auto_white_balance(
    img: &RawImage,
    info: &FrameInfo,
    origin: (u32, u32),
    start: [f32; 4],
) -> Option<[f32; 4]> {
    let blocks = block_means(img, info, origin, 0.95)?;
    let mut wb = [start[0].max(1e-3), 1.0, start[2].max(1e-3)];
    for _ in 0..3 {
        let (mut acc, mut n) = ([0f64; 3], 0u32);
        for b in &blocks {
            let (r, g, bl) = (b[0] * wb[0], b[1], b[2] * wb[2]);
            if g > 0.05 && g < 0.8 && (r / g - 1.0).abs() < 0.3 && (bl / g - 1.0).abs() < 0.3 {
                acc[0] += r as f64;
                acc[1] += g as f64;
                acc[2] += bl as f64;
                n += 1;
            }
        }
        if n < 200 {
            return None;
        }
        wb[0] *= (acc[1] / acc[0]) as f32;
        wb[2] *= (acc[1] / acc[2]) as f32;
        wb[0] = wb[0].clamp(start[0] * 0.5, start[0] * 2.0);
        wb[2] = wb[2].clamp(start[2] * 0.5, start[2] * 2.0);
    }
    Some([wb[0], 1.0, wb[2], start[3]])
}

/// White balance, clip at sensor saturation, camera → linear sRGB, levels.
#[inline(always)]
pub(crate) fn finish_rgb(
    rgb: [f32; 3],
    wb: &[f32; 4],
    m: &[[f32; 3]; 3],
    levels: Option<Levels>,
    highlights: Highlights,
    recon: &ReconModel,
) -> [f32; 3] {
    let (mut r, mut g, mut b);
    if highlights == Highlights::Blend || highlights == Highlights::Reconstruct {
        // Scale the balanced pixel down together when a channel exceeds the white level, so the
        // ratios between the channels (the colour) survive; below it nothing changes. Clipped
        // channels are first replaced by the model's prediction (`Reconstruct`).
        let mut u = [(rgb[0] * wb[0]).max(0.0), (rgb[1] * wb[1]).max(0.0), (rgb[2] * wb[2]).max(0.0)];
        if highlights == Highlights::Reconstruct {
            recon.apply(&mut u, rgb.map(|v| v >= CLIP_LEVEL));
        }
        let k = 1.0 / u[0].max(u[1]).max(u[2]).max(1.0);
        (r, g, b) = (u[0] * k, u[1] * k, u[2] * k);
    } else {
        r = (rgb[0] * wb[0]).clamp(0.0, 1.0);
        g = (rgb[1] * wb[1]).clamp(0.0, 1.0);
        b = (rgb[2] * wb[2]).clamp(0.0, 1.0);
    }
    if highlights != Highlights::Clip {
        // The brightest channel (as measured, before white balance) tells how close the pixel
        // is to clipping: fade the others up to the brightest balanced one.
        // `Reconstruct` fades only pixels that are near saturation in every channel.
        let s = if highlights == Highlights::Reconstruct {
            rgb[0].min(rgb[1]).min(rgb[2])
        } else {
            rgb[0].max(rgb[1]).max(rgb[2])
        };
        if s > NEUTRAL_START {
            let x = ((s - NEUTRAL_START) / (1.0 - NEUTRAL_START)).min(1.0);
            let t = x * x * (3.0 - 2.0 * x);
            let mx = r.max(g).max(b);
            r += (mx - r) * t;
            g += (mx - g) * t;
            b += (mx - b) * t;
        }
    }
    let mut o = [
        m[0][0] * r + m[0][1] * g + m[0][2] * b,
        m[1][0] * r + m[1][1] * g + m[1][2] * b,
        m[2][0] * r + m[2][1] * g + m[2][2] * b,
    ];
    if let Some(l) = levels {
        for v in &mut o {
            *v = *v * l.scale - l.offset;
        }
    }
    o
}

/// [`finish_rgb`] of `src` (each pixel scaled by `k` first, unit white balance) into `dst`.
/// Compiled a second time for AVX2 on x86-64 (no FMA: the same operations in the same order, so
/// the same result).
fn finish_run(
    src: &[[f32; 3]],
    dst: &mut [[f32; 3]],
    k: [f32; 3],
    m: &[[f32; 3]; 3],
    levels: Option<Levels>,
    highlights: Highlights,
    recon: &ReconModel,
) {
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports AVX2 (checked above).
        return unsafe { finish_run_avx2(src, dst, k, m, levels, highlights, recon) };
    }
    finish_run_impl(src, dst, k, m, levels, highlights, recon)
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn finish_run_avx2(
    src: &[[f32; 3]],
    dst: &mut [[f32; 3]],
    k: [f32; 3],
    m: &[[f32; 3]; 3],
    levels: Option<Levels>,
    highlights: Highlights,
    recon: &ReconModel,
) {
    finish_run_impl(src, dst, k, m, levels, highlights, recon)
}

#[inline(always)]
fn finish_run_impl(
    src: &[[f32; 3]],
    dst: &mut [[f32; 3]],
    k: [f32; 3],
    m: &[[f32; 3]; 3],
    levels: Option<Levels>,
    highlights: Highlights,
    recon: &ReconModel,
) {
    // One loop per highlight mode and levels setting, so the common one (clip, no levels) is a
    // straight-line loop the compiler vectorises.
    macro_rules! run {
        ($h:expr, $l:expr) => {
            for (d, s) in dst.iter_mut().zip(src) {
                *d = finish_rgb([s[0] * k[0], s[1] * k[1], s[2] * k[2]], &[1.0; 4], m, $l, $h, recon);
            }
        };
    }
    match (highlights, levels) {
        (Highlights::Clip, None) => run!(Highlights::Clip, None),
        _ => run!(highlights, levels),
    }
}

/// Sample encoding of an RGB output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgbEncoding {
    /// sRGB 8-bit RGBA (alpha 255), orientation applied.
    Rgba8,
    /// sRGB 16-bit RGB, orientation applied.
    Srgb16,
    /// Linear 16-bit RGB (`[0, 1]` → `[0, 65535]`).
    Linear16,
    /// Linear IEEE half-float RGB (bit patterns).
    LinearF16,
    /// Linear f32 RGB.
    LinearF32,
}

impl RgbEncoding {
    /// Display encodings apply the EXIF orientation; linear ones keep the sensor's.
    pub fn oriented(self) -> bool {
        matches!(self, RgbEncoding::Rgba8 | RgbEncoding::Srgb16)
    }
    /// Samples per pixel.
    pub fn channels(self) -> u32 {
        if self == RgbEncoding::Rgba8 { 4 } else { 3 }
    }
}

/// Fills frame row `y` (all `width * cpp` samples) with the values the demosaic reads,
/// replacing the black/white normalisation (staged DNG opcode develop).
pub(crate) type RowSource<'a> = dyn Fn(usize, &mut [f32]) + Sync + 'a;
/// Processes a band of developed camera-RGB rows (`first` = index of its first developed
/// row) before white balance and colour conversion (DNG OpcodeList3).
pub(crate) type BandHook<'a> = dyn Fn(usize, &mut [[f32; 3]]) + Sync + 'a;

/// Output of [`develop`].
pub struct Developed {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<f32>,
}

/// Rows per destination strip processed by one task.
const STRIP: u32 = 16;

/// Receiver of developed pixels: `(a, b, pixels)`, whose meaning the caller documents
/// (a column chunk of a band, or a run of a strip row).
pub(crate) type PixelSink<'a> = dyn FnMut(usize, usize, &[[f32; 3]]) + 'a;

/// Same-colour neighbour offsets `(dx, dy)` and the reciprocal of their count.
type NeighbourList = (Vec<(isize, isize)>, f32);

/// Per-image state of the fused develop kernel.
pub(crate) struct Kernel<'a> {
    img: &'a RawImage,
    /// Replaces [`Kernel::normalize_row`] when set.
    source: Option<&'a RowSource<'a>>,
    calibs: Vec<Calib>,
    origin: (usize, usize),
    cfa: Option<CfaPattern>,
    /// Same-colour 3x3 neighbour offsets per CFA position and channel, with 1/count.
    neigh: Vec<[NeighbourList; 4]>,
    /// Channel of each [`CfaColor`] (by value); `NO_CHANNEL` for colours not developed.
    chan: [u8; 8],
    /// Four-colour CFAs: camera channels → linear sRGB.
    m4: Option<[[f32; 4]; 3]>,
    pw: usize,
    ph: usize,
    wb: [f32; 4],
    m: [[f32; 3]; 3],
    levels: Option<Levels>,
    highlights: Highlights,
    recon: ReconModel,
    crop: Rect,
    half: bool,
    /// Edge-aware demosaic of Bayer / X-Trans frames (`Demosaic::EdgeAware`).
    edge: Option<edgeaware::Plan>,
    /// Homogeneity-directed demosaic of Bayer frames (`Demosaic::Ahd`).
    ahd: Option<ahd::Plan>,
    /// Alternative Bayer demosaics (`Demosaic::Ppg`, `Rcd`).
    alt: Option<bayer_alt::Plan>,
    /// Balance applied to the mosaic by the edge-aware demosaic.
    dm_wb: [f32; 4],
}

/// Small cache of normalised sensor rows (three are live at a time).
struct RowCache {
    rows: [(usize, Vec<f32>); 4],
    tick: [u64; 4],
    clock: u64,
}

impl RowCache {
    fn new(len: usize) -> Self {
        RowCache { rows: std::array::from_fn(|_| (usize::MAX, vec![0f32; len])), tick: [0; 4], clock: 0 }
    }
    fn get(&mut self, k: &Kernel, y: usize) -> usize {
        self.clock += 1;
        if let Some(i) = self.rows.iter().position(|r| r.0 == y) {
            self.tick[i] = self.clock;
            return i;
        }
        let i = (0..4).min_by_key(|&i| self.tick[i]).unwrap();
        self.tick[i] = self.clock;
        self.rows[i].0 = y;
        match k.source {
            Some(f) => f(y, &mut self.rows[i].1),
            None => k.normalize_row(y, &mut self.rows[i].1),
        }
        i
    }
}

impl Kernel<'_> {
    /// Black-subtracted, white-normalised samples of frame row `y`.
    pub(crate) fn normalize_row(&self, y: usize, out: &mut [f32]) {
        let img = self.img;
        let (w, cpp) = (img.width as usize, img.cpp as usize);
        let (ox, oy) = self.origin;
        let has_deltas = self.calibs.iter().any(|c| !c.row_deltas.is_empty() || !c.col_deltas.is_empty());
        if !has_deltas && cpp > 1 && self.calibs.iter().all(|c| c.pw == 1 && c.ph == 1) {
            // Interleaved channels with constant per-channel calibration.
            let blk: Vec<f32> = self.calibs.iter().map(|c| c.black[0]).collect();
            let scl: Vec<f32> = self.calibs.iter().map(|c| c.scale[0]).collect();
            match &img.data {
                PixelData::U16(d) => {
                    for (o, px) in out.chunks_exact_mut(cpp).zip(d[y * w * cpp..(y + 1) * w * cpp].chunks_exact(cpp)) {
                        for c in 0..cpp {
                            o[c] = (px[c] as f32 - blk[c]) * scl[c];
                        }
                    }
                }
                PixelData::F32(d) => {
                    for (o, px) in out.chunks_exact_mut(cpp).zip(d[y * w * cpp..(y + 1) * w * cpp].chunks_exact(cpp)) {
                        for c in 0..cpp {
                            o[c] = (px[c] - blk[c]) * scl[c];
                        }
                    }
                }
            }
            return;
        }
        if has_deltas || cpp != 1 {
            for (i, o) in out.iter_mut().enumerate().take(w * cpp) {
                let v = match &img.data {
                    PixelData::U16(d) => d[y * w * cpp + i] as f32,
                    PixelData::F32(d) => d[y * w * cpp + i],
                };
                *o = self.calibs[i % cpp].norm(v, i / cpp + ox, y + oy);
            }
            return;
        }
        // Per-row black/scale table over one pattern period, then a modulo-free loop.
        let c = &self.calibs[0];
        let base = ((y + oy) % c.ph) * c.pw;
        let period = c.pw;
        // The period repeated to at least 32 samples, so the inner loop is long enough to vectorise
        // (periods are 1 or 2 for most sensors).
        let len = period * 32usize.div_ceil(period);
        let blk: Vec<f32> = (0..len).map(|i| c.black[base + (i + ox) % period]).collect();
        let scl: Vec<f32> = (0..len).map(|i| c.scale[base + (i + ox) % period]).collect();
        match &img.data {
            PixelData::U16(d) => {
                let row = &d[y * w..(y + 1) * w];
                for (oc, rc) in out[..w].chunks_mut(len).zip(row.chunks(len)) {
                    for (((o, &v), &b), &s) in oc.iter_mut().zip(rc).zip(&blk).zip(&scl) {
                        *o = (v as f32 - b) * s;
                    }
                }
            }
            PixelData::F32(d) => {
                let row = &d[y * w..(y + 1) * w];
                for (oc, rc) in out[..w].chunks_mut(len).zip(row.chunks(len)) {
                    for (((o, &v), &b), &s) in oc.iter_mut().zip(rc).zip(&blk).zip(&scl) {
                        *o = (v - b) * s;
                    }
                }
            }
        }
    }

    /// White balance, clipping and colour conversion of the developed channels (the
    /// fourth is used by four-colour CFAs only).
    #[inline(always)]
    fn finish_n(&self, v: [f32; 4]) -> [f32; 3] {
        let Some(m) = &self.m4 else { return self.finish([v[0], v[1], v[2]]) };
        let c: [f32; 4] = std::array::from_fn(|i| (v[i] * self.wb[i]).clamp(0.0, 1.0));
        std::array::from_fn(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2] + m[i][3] * c[3])
    }

    #[inline(always)]
    fn finish(&self, rgb: [f32; 3]) -> [f32; 3] {
        finish_rgb(rgb, &self.wb, &self.m, self.levels, self.highlights, &self.recon)
    }

    /// Develop output row `oy` (crop/half coordinates) into `out` (one RGB per output pixel).
    fn row(&self, cache: &mut RowCache, oy: usize, out: &mut [[f32; 3]]) {
        self.row_cols(cache, oy, out, 0..0)
    }

    /// [`Kernel::row`] leaving the pixels `skip` (output columns) untouched.
    fn row_cols(&self, cache: &mut RowCache, oy: usize, out: &mut [[f32; 3]], skip: std::ops::Range<usize>) {
        let img = self.img;
        let (w, h, cpp) = (img.width as usize, img.height as usize, img.cpp as usize);
        let (ox0, oy0) = (self.origin.0, self.origin.1);
        let color = |x: usize, y: usize| {
            self.cfa.map(|p| self.chan[p.color_at(x + ox0, y + oy0) as usize] as usize).unwrap_or(1)
        };
        let nch = if self.m4.is_some() { 4 } else { 3 };
        if self.half {
            let y0 = self.crop.y as usize + oy * 2;
            let r0 = cache.get(self, y0);
            let r1 = if y0 + 1 < h { Some(cache.get(self, y0 + 1)) } else { None };
            // Single-component CFA rows: the colours of the 2x2 block are the same for every
            // output pixel of a row when the pattern is 2x2 (`x0` steps by 2). Interior pixels
            // take this path, with the same sums in the same order as the general loop below.
            let fast = cpp == 1 && self.cfa.is_some_and(|p| p.width() == 2 && p.height() == 2) && r1.is_some();
            let mut done = 0;
            if fast {
                let x0 = self.crop.x as usize;
                let c4 = [color(x0, y0), color(x0 + 1, y0), color(x0, y0 + 1), color(x0 + 1, y0 + 1)];
                let (a, b) = (&cache.rows[r0].1, &cache.rows[r1.unwrap()].1);
                let n = out.len().min((w - x0) / 2);
                for (i, o) in out[..n].iter_mut().enumerate() {
                    let x = x0 + i * 2;
                    let mut acc = [0f32; 4];
                    let mut cnt = [0f32; 4];
                    for (k, v) in [a[x], a[x + 1], b[x], b[x + 1]].into_iter().enumerate() {
                        acc[c4[k]] += v;
                        cnt[c4[k]] += 1.0;
                    }
                    let mut rgb = [0f32; 4];
                    for c in 0..nch {
                        rgb[c] = if cnt[c] > 0.0 { acc[c] / cnt[c] } else { 0.0 };
                    }
                    *o = self.finish_n(rgb);
                }
                done = n;
            }
            for (i, o) in out.iter_mut().enumerate().skip(done) {
                let x0 = self.crop.x as usize + i * 2;
                let mut acc = [0f32; 4];
                let mut n = [0f32; 4];
                for (dy, ri) in [(0usize, Some(r0)), (1, r1)] {
                    let Some(ri) = ri else { continue };
                    let row = &cache.rows[ri].1;
                    for dx in 0..2 {
                        let x = x0 + dx;
                        if x >= w {
                            continue;
                        }
                        if cpp == 1 && self.cfa.is_some() {
                            let c = color(x, y0 + dy);
                            acc[c] += row[x];
                            n[c] += 1.0;
                        } else if cpp == 1 {
                            // Monochrome: average into the green slot, replicated below.
                            acc[1] += row[x];
                            n[1] += 1.0;
                        } else {
                            for c in 0..cpp.min(3) {
                                acc[c] += row[x * cpp + c];
                                n[c] += 1.0;
                            }
                        }
                    }
                }
                let mut rgb = [0f32; 4];
                for c in 0..nch {
                    rgb[c] = if n[c] > 0.0 { acc[c] / n[c] } else { 0.0 };
                }
                if cpp == 1 && self.cfa.is_none() {
                    rgb = [acc[1] / n[1].max(1.0); 4];
                }
                *o = self.finish_n(rgb);
            }
            return;
        }
        let y = self.crop.y as usize + oy;
        let cur = cache.get(self, y);
        if !(cpp == 1 && self.cfa.is_some()) {
            let row = &cache.rows[cur].1;
            for (i, o) in out.iter_mut().enumerate() {
                let x = self.crop.x as usize + i;
                let rgb = if cpp >= 3 { [row[x * cpp], row[x * cpp + 1], row[x * cpp + 2]] } else { [row[x * cpp]; 3] };
                *o = self.finish(rgb);
            }
            return;
        }
        let up = if y > 0 { Some(cache.get(self, y - 1)) } else { None };
        let down = if y + 1 < h { Some(cache.get(self, y + 1)) } else { None };
        let rows: [Option<&[f32]>; 3] =
            [up.map(|i| &cache.rows[i].1[..]), Some(&cache.rows[cur].1[..]), down.map(|i| &cache.rows[i].1[..])];
        let interior_y = up.is_some() && down.is_some();
        let (r_up, r_mid, r_dn) = (rows[0].unwrap_or(&[]), rows[1].unwrap(), rows[2].unwrap_or(&[]));
        let py = (y + oy0) % self.ph;
        let bayer = self.cfa.is_some_and(|p| p.is_bayer());
        // Bayer fast path: colour of this row's sites indexed by absolute column parity.
        let row_colors = [color((2 - ox0 % 2) % 2, y), color((3 - ox0 % 2) % 2, y)];
        let rows3 = [r_up, r_mid, r_dn];
        // Neighbour lists of this row's pattern phases; the column phase steps with x.
        let nb = &self.neigh[py * self.pw..(py + 1) * self.pw];
        let mut phase = (self.crop.x as usize + ox0) % self.pw;
        for (i, o) in out.iter_mut().enumerate() {
            let px = phase;
            phase = if phase + 1 == self.pw { 0 } else { phase + 1 };
            if skip.contains(&i) {
                continue;
            }
            let x = self.crop.x as usize + i;
            if bayer && interior_y && x > 0 && x + 1 < w {
                let c0 = row_colors[(x + ox0) & 1];
                let own = r_mid[x];
                let (l, r) = (r_mid[x - 1], r_mid[x + 1]);
                let (u, d) = (r_up[x], r_dn[x]);
                let rgb = if c0 == 1 {
                    // Green site: horizontal neighbours carry this row's other colour.
                    let ch = row_colors[(x + ox0 + 1) & 1];
                    let mut v = [0f32; 3];
                    v[1] = own;
                    v[ch] = (l + r) * 0.5;
                    v[2 - ch] = (u + d) * 0.5;
                    v
                } else {
                    let orth = (l + r + u + d) * 0.25;
                    let diag = (r_up[x - 1] + r_up[x + 1] + r_dn[x - 1] + r_dn[x + 1]) * 0.25;
                    if c0 == 0 { [own, orth, diag] } else { [diag, orth, own] }
                };
                *o = self.finish(rgb);
                continue;
            }
            let lists = &nb[px];
            let mut rgb = [0f32; 4];
            if interior_y && x > 0 && x + 1 < w {
                // Fast path: all 3x3 neighbours exist.
                for c in 0..nch {
                    let (list, inv) = &lists[c];
                    let mut s = 0f32;
                    for &(dx, dy) in list {
                        s += rows3[(dy + 1) as usize][(x as isize + dx) as usize];
                    }
                    rgb[c] = s * inv;
                }
            } else {
                // Border: average only neighbours inside the image.
                for c in 0..nch {
                    let mut s = 0f32;
                    let mut n = 0f32;
                    for &(dx, dy) in &lists[c].0 {
                        let xx = x as isize + dx;
                        if xx < 0 || xx as usize >= w {
                            continue;
                        }
                        if let Some(r) = rows[(dy + 1) as usize] {
                            s += r[xx as usize];
                            n += 1.0;
                        }
                    }
                    rgb[c] = if n > 0.0 { s / n } else { 0.0 };
                }
            }
            *o = self.finish_n(rgb);
        }
    }
}

impl Kernel<'_> {
    /// Output rows developed together by one task: the edge-aware demosaic works in bands.
    pub(crate) fn strip_rows(&self) -> usize {
        if self.edge.is_some() {
            edgeaware::BAND_ROWS
        } else if self.ahd.is_some() {
            ahd::BAND_ROWS
        } else if self.alt.is_some() {
            bayer_alt::BAND_ROWS
        } else {
            STRIP as usize
        }
    }

    /// Develop output rows `first..first + count` into `out` (`count` rows of one RGB per
    /// output pixel). The edge-aware demosaic handles the pixels at least
    /// [`edgeaware::HALO`] away from the frame border; the others are bilinear.
    fn strip(&self, cache: &mut RowCache, first: usize, count: usize, out: &mut [[f32; 3]]) {
        let ow = out.len() / count.max(1);
        self.strip_to(cache, first, count, ow, &mut |i, x0, px| {
            out[i * ow + x0..i * ow + x0 + px.len()].copy_from_slice(px);
        });
    }

    /// [`Kernel::strip`] handing the finished pixels to `sink(i, x0, pixels)`: output row
    /// `first + i`, output columns `x0..x0 + pixels.len()`; every pixel of the `ow`-wide strip
    /// exactly once. The demosaics that work in column chunks hand them over chunk by chunk,
    /// so a strip is never held whole (a band across a large frame does not fit in cache).
    fn strip_to(
        &self,
        cache: &mut RowCache,
        first: usize,
        count: usize,
        ow: usize,
        sink: &mut crate::process::PixelSink<'_>,
    ) {
        let w = self.img.width as usize;
        let halo = match (&self.edge, &self.ahd, &self.alt) {
            (Some(_), _, _) => edgeaware::HALO,
            (None, Some(_), _) => ahd::HALO,
            (None, None, Some(_)) => bayer_alt::HALO,
            (None, None, None) => 0,
        };
        let mut row = vec![[0f32; 3]; ow];
        if halo == 0 {
            for i in 0..count {
                self.row(cache, first + i, &mut row);
                sink(i, 0, &row);
            }
            return;
        }
        let h = self.img.height as usize;
        let (cx, cy) = (self.crop.x as usize, self.crop.y as usize);
        // Output columns whose pixel is far enough from the left / right frame border.
        let ia = halo.saturating_sub(cx).min(ow);
        let ib = (w.saturating_sub(halo).saturating_sub(cx)).min(ow).max(ia);
        // Output rows (of this strip) far enough from the top / bottom.
        let lo = halo.saturating_sub(cy).clamp(first, first + count);
        let hi = h.saturating_sub(halo).saturating_sub(cy).clamp(lo, first + count);
        for i in 0..count {
            let oy = first + i;
            if oy >= lo && oy < hi && ib > ia {
                self.row_cols(cache, oy, &mut row, ia..ib);
                sink(i, 0, &row[..ia]);
                sink(i, ib, &row[ib..]);
            } else {
                self.row(cache, oy, &mut row);
                sink(i, 0, &row);
            }
        }
        if hi > lo && ib > ia {
            let (y0, y1, xa, xb) = (cy + lo, cy + hi, cx + ia, cx + ib);
            let mut fetch = |y: usize, row: &mut [f32]| match self.source {
                Some(f) => f(y, row),
                None => self.normalize_row(y, row),
            };
            // Back to this kernel's balance (1 unless it develops unbalanced camera RGB).
            let k = [0, 1, 2].map(|c| self.wb[c] / self.dm_wb[c]);
            let mut fin: Vec<[f32; 3]> = Vec::new();
            let mut emit = |c0: usize, cw: usize, px: &[[f32; 3]]| {
                fin.resize(px.len(), [0.0; 3]);
                finish_run(px, &mut fin, k, &self.m, self.levels, self.highlights, &self.recon);
                for (n, r) in fin.chunks_exact(cw).enumerate() {
                    sink(lo - first + n, ia + c0, r);
                }
            };
            match (&self.edge, &self.ahd, &self.alt) {
                (Some(p), _, _) => p.develop_chunks(w, (y0, y1), (xa, xb), &self.dm_wb, &mut fetch, &mut emit),
                (None, Some(p), _) => p.develop_chunks(w, (y0, y1), (xa, xb), &self.dm_wb, &mut fetch, &mut emit),
                (None, None, Some(p)) => p.develop_chunks(w, (y0, y1), (xa, xb), &self.dm_wb, &mut fetch, &mut emit),
                (None, None, None) => unreachable!("halo > 0 needs a plan"),
            }
        }
    }
}

fn kernel<'a>(
    img: &'a RawImage,
    info: &FrameInfo,
    origin: (u32, u32),
    params: &DevelopParams,
    crop: Rect,
) -> Kernel<'a> {
    kernel_with(img, info, origin, params, crop, None)
}

/// A kernel that only normalises rows ([`Kernel::normalize_row`]) of `img` at `origin`.
pub(crate) fn normalizer<'a>(img: &'a RawImage, info: &FrameInfo, origin: (u32, u32)) -> Kernel<'a> {
    let full = Rect::full(img.width, img.height);
    kernel(img, info, origin, &DevelopParams::new(info, Demosaic::Bilinear, full), full)
}

fn kernel_with<'a>(
    img: &'a RawImage,
    info: &FrameInfo,
    origin: (u32, u32),
    params: &DevelopParams,
    crop: Rect,
    source: Option<&'a RowSource<'a>>,
) -> Kernel<'a> {
    let cpp = img.cpp as usize;
    let (pw, ph) = info.cfa.map(|p| (p.width(), p.height())).unwrap_or((1, 1));
    let m4 = params.cam4_to_rgb.filter(|_| cpp == 1);
    let chan = channel_map(info.cfa.and_then(|p| p.four_colour_order()).filter(|_| m4.is_some()));
    let mut neigh = Vec::with_capacity(pw * ph);
    for py in 0..ph {
        for px in 0..pw {
            let mut lists: [NeighbourList; 4] = Default::default();
            for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    let c = info
                        .cfa
                        .map(|p| {
                            chan[p.color_at(
                                (px as isize + dx + pw as isize * 4) as usize,
                                (py as isize + dy + ph as isize * 4) as usize,
                            ) as usize] as usize
                        })
                        .unwrap_or(1);
                    if c < 4 {
                        lists[c].0.push((dx, dy));
                    }
                }
            }
            // The site's own colour is measured, not interpolated.
            let own = info.cfa.map(|p| chan[p.color_at(px, py) as usize] as usize).unwrap_or(1);
            if own < 4 {
                lists[own].0 = vec![(0, 0)];
            }
            for l in lists.iter_mut() {
                l.1 = if l.0.is_empty() { 0.0 } else { 1.0 / l.0.len() as f32 };
            }
            neigh.push(lists);
        }
    }
    let mut k = Kernel {
        img,
        source,
        calibs: (0..cpp).map(|c| Calib::new(info, c)).collect(),
        origin: (origin.0 as usize, origin.1 as usize),
        cfa: info.cfa,
        neigh,
        chan,
        m4,
        pw,
        ph,
        wb: if m4.is_some() { normalized_min(params.wb) } else { params.wb },
        m: params.cam_to_rgb,
        levels: params.levels,
        highlights: params.highlights,
        recon: params.recon,
        crop,
        half: params.demosaic == Demosaic::HalfSize,
        edge: match (params.demosaic, info.cfa) {
            (Demosaic::EdgeAware, Some(cfa)) if cpp == 1 && m4.is_none() => {
                edgeaware::Plan::new(&cfa, &chan, origin_usize(origin))
            }
            // Patterns without a homogeneity-directed demosaic use the edge-aware one.
            (Demosaic::Ahd, Some(cfa)) if cpp == 1 && m4.is_none() && ahd::Plan::new(&cfa, &chan, (0, 0)).is_none() => {
                edgeaware::Plan::new(&cfa, &chan, origin_usize(origin))
            }
            _ => None,
        },
        ahd: match (params.demosaic, info.cfa) {
            (Demosaic::Ahd, Some(cfa)) if cpp == 1 && m4.is_none() => ahd::Plan::new(&cfa, &chan, origin_usize(origin)),
            _ => None,
        },
        alt: match (params.demosaic, info.cfa) {
            (d @ (Demosaic::Ppg | Demosaic::Rcd), Some(cfa)) if cpp == 1 && m4.is_none() => {
                let method = match d {
                    Demosaic::Ppg => bayer_alt::Method::Ppg,
                    _ => bayer_alt::Method::Rcd,
                };
                bayer_alt::Plan::new(method, &cfa, &chan, origin_usize(origin))
            }
            _ => None,
        },
        dm_wb: params.dm_wb,
    };
    if let Some(mut plan) = k.edge.take() {
        let (w, h, g) = (img.width as usize, img.height as usize, k.dm_wb[1]);
        let kr = &k;
        let mut fetch = |y: usize, row: &mut [f32]| match kr.source {
            Some(f) => f(y, row),
            None => kr.normalize_row(y, row),
        };
        plan.set_noise_from(w, h, g, &mut fetch);
        plan.apply_level(params.noise);
        k.edge = Some(plan);
    }
    k
}

/// The edge-aware plan of a single-component mosaic whose sample `(0, 0)` is at sensor position
/// `origin` (what `Demosaic::EdgeAware` builds for the CPU), or `None` when the pattern is
/// neither Bayer nor X-Trans.
#[cfg(feature = "gpu")]
pub(crate) fn edge_plan(cfa: &CfaPattern, origin: (u32, u32)) -> Option<edgeaware::Plan> {
    edgeaware::Plan::new(cfa, &channel_map(None), origin_usize(origin))
}

fn origin_usize(o: (u32, u32)) -> (usize, usize) {
    (o.0 as usize, o.1 as usize)
}

/// Colour channels developed: R, G, B as 0, 1, 2 (other colours unused), or the
/// channels of a four-colour CFA in `order`.
/// True when `info` describes a plain Bayer mosaic (the sensors the Bayer-only demosaics accept).
pub(crate) fn is_bayer(info: &FrameInfo) -> bool {
    info.cfa.is_some_and(|c| ahd::Plan::new(&c, &channel_map(None), (0, 0)).is_some())
}

fn channel_map(order: Option<[CfaColor; 4]>) -> [u8; 8] {
    let mut map = [NO_CHANNEL; 8];
    match order {
        Some(o) => o.iter().enumerate().for_each(|(i, c)| map[*c as usize] = i as u8),
        None => map[..3].copy_from_slice(&[0, 1, 2]),
    }
    map
}

/// Channel index of colours that are not developed (a fourth slot that only
/// four-colour CFAs use).
const NO_CHANNEL: u8 = 3;

/// Multipliers scaled so the smallest is 1 (saturated samples stay saturated in every
/// channel after white balance).
fn normalized_min(wb: [f32; 4]) -> [f32; 4] {
    let min = wb.iter().copied().filter(|v| v.is_finite() && *v > 0.0).fold(f32::MAX, f32::min);
    if min == f32::MAX { [1.0; 4] } else { wb.map(|v| if v.is_finite() && v > 0.0 { v / min } else { 1.0 }) }
}

fn output_dims(img: &RawImage, params: &DevelopParams) -> Result<(Rect, u32, u32)> {
    let crop = params.crop.clamp_to(img.width, img.height);
    if crop.is_empty() {
        return Err(Error::InvalidArgument("empty develop region".into()));
    }
    let (ow, oh) = if params.demosaic == Demosaic::HalfSize {
        (crop.width / 2, crop.height / 2)
    } else {
        (crop.width, crop.height)
    };
    if ow == 0 || oh == 0 {
        return Err(Error::InvalidArgument("develop region too small".into()));
    }
    Ok((crop, ow, oh))
}

/// How the display encoders read a curve: one table for all channels (the sRGB curve, the
/// built-in spaces and profiles with equal curves) or one per channel.
enum Tables<'a, T> {
    One(&'a [T]),
    Each([&'a [T]; 3]),
}

fn tables8(curve: &OutputCurve) -> Tables<'_, u8> {
    match curve {
        OutputCurve::Srgb => Tables::One(&srgb_lut()[..]),
        OutputCurve::Shared(t) => Tables::One(&t.lut8),
        OutputCurve::Owned(t) => Tables::One(&t.lut8),
        OutputCurve::PerChannel(a) => Tables::Each([&a[0].lut8, &a[1].lut8, &a[2].lut8]),
    }
}

fn tables16(curve: &OutputCurve) -> Tables<'_, f32> {
    match curve {
        OutputCurve::Srgb => Tables::One(srgb16_lut()),
        OutputCurve::Shared(t) => Tables::One(&t.lut16),
        OutputCurve::Owned(t) => Tables::One(&t.lut16),
        OutputCurve::PerChannel(a) => Tables::Each([&a[0].lut16, &a[1].lut16, &a[2].lut16]),
    }
}

/// Run `$body` with `$enc` bound to the 8-bit encoder through the tables of `$curve`:
/// `table[trunc(clamp(v, 0, 1) * segments)]`. One arm per table layout, so the common
/// single-table case keeps its own tight encoder.
macro_rules! with_encoder8 {
    ($curve:expr, |$enc:ident| $body:expr) => {
        match tables8($curve) {
            // The sRGB curve keeps its original fixed-size table and constant scale.
            // The sRGB curve (4096 segments, the `rgba8_encoder` table) and single-table curves.
            Tables::One(t) => {
                let $enc = Lut8(t);
                $body
            }
            Tables::Each(t) => {
                let n = (t[0].len() - 1) as f32;
                let $enc = Pixel(move |p: [f32; 3]| {
                    [
                        t[0][(p[0].clamp(0.0, 1.0) * n) as usize],
                        t[1][(p[1].clamp(0.0, 1.0) * n) as usize],
                        t[2][(p[2].clamp(0.0, 1.0) * n) as usize],
                        255,
                    ]
                });
                $body
            }
        }
    };
}

/// As [`with_encoder8!`] for the 16-bit encoder.
macro_rules! with_encoder16 {
    ($curve:expr, |$enc:ident| $body:expr) => {
        match tables16($curve) {
            _ if matches!($curve, OutputCurve::Srgb) => {
                let $enc = Srgb16;
                $body
            }
            Tables::One(t) => {
                let $enc = Sample(move |v: f32| srgb16(v, t));
                $body
            }
            Tables::Each(t) => {
                let $enc = Pixel(move |p: [f32; 3]| [srgb16(p[0], t[0]), srgb16(p[1], t[1]), srgb16(p[2], t[2])]);
                $body
            }
        }
    };
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
    let (crop, ow, oh) = output_dims(img, params)?;
    let k = kernel(img, info, origin, params, crop);
    let mut out = vec![0f32; ow as usize * oh as usize * 3];
    let rl = ow as usize * 3;
    par::for_each_row_chunk(&mut out, rl, k.strip_rows(), parallel, |first, chunk| {
        let mut cache = RowCache::new(img.width as usize * img.cpp as usize);
        let n = chunk.len() / rl;
        k.strip_to(&mut cache, first, n, ow as usize, &mut |i, x0, px| {
            chunk[i * rl + x0 * 3..i * rl + (x0 + px.len()) * 3].copy_from_slice(px.as_flattened());
        });
    });
    Ok(Developed { width: ow, height: oh, rgb: out })
}

/// Fused develop straight to oriented RGBA8 in the colour space of `params` (sRGB by default;
/// no intermediate RGB image).
/// Returns `(width, height, pixels)` after orientation.
pub fn develop_rgba8(
    img: &RawImage,
    info: &FrameInfo,
    origin: (u32, u32),
    params: &DevelopParams,
    parallel: bool,
) -> Result<(u32, u32, Vec<u8>)> {
    let (crop, w, h) = output_dims(img, params)?;
    let k = kernel(img, info, origin, params, crop);
    with_encoder8!(&params.curve, |enc| run_oriented(
        &k,
        w as usize,
        h as usize,
        params.orientation,
        parallel,
        None,
        enc
    ))
}

/// Fused develop to any [`RgbEncoding`] (no intermediate RGB image; display encodings
/// apply `params.orientation`, linear ones keep the sensor orientation). Returns
/// `(width, height, channels, pixels)`.
pub fn develop_encoded(
    img: &RawImage,
    info: &FrameInfo,
    origin: (u32, u32),
    params: &DevelopParams,
    enc: RgbEncoding,
    parallel: bool,
) -> Result<(u32, u32, u32, RawPixelData)> {
    if enc == RgbEncoding::LinearF32 {
        let d = develop(img, info, origin, params, parallel)?;
        return Ok((d.width, d.height, 3, RawPixelData::F32(d.rgb)));
    }
    develop_staged(img, info, origin, params, enc, parallel, None, None)
}

/// [`develop_encoded`] with the stage hooks of the DNG opcode develop: `rows` replaces
/// the normalisation of sensor rows, and `camera` (when set) receives bands of demosaiced
/// camera RGB before white balance, colour conversion and levels.
#[allow(clippy::too_many_arguments)]
pub(crate) fn develop_staged(
    img: &RawImage,
    info: &FrameInfo,
    origin: (u32, u32),
    params: &DevelopParams,
    enc: RgbEncoding,
    parallel: bool,
    rows: Option<&RowSource<'_>>,
    camera: Option<&BandHook<'_>>,
) -> Result<(u32, u32, u32, RawPixelData)> {
    let (crop, w, h) = output_dims(img, params)?;
    let (w, h) = (w as usize, h as usize);
    let o = if enc.oriented() { params.orientation } else { Orientation::Normal };
    let finish = |first: usize, band: &mut [[f32; 3]]| {
        if let Some(f) = camera {
            f(first, band);
        }
        for p in band.iter_mut() {
            *p = finish_rgb(*p, &params.wb, &params.cam_to_rgb, params.levels, params.highlights, &params.recon);
        }
    };
    let (k, band): (Kernel, Option<&BandHook<'_>>) = match camera {
        Some(_) => (kernel_with(img, info, origin, &params.camera_rgb(), crop, rows), Some(&finish)),
        None => (kernel_with(img, info, origin, params, crop, rows), None),
    };
    let ch = enc.channels();
    Ok(match enc {
        RgbEncoding::Rgba8 => {
            let (dw, dh, px) = with_encoder8!(&params.curve, |enc| run_oriented(&k, w, h, o, parallel, band, enc))?;
            (dw, dh, ch, RawPixelData::U8(px))
        }
        RgbEncoding::Srgb16 => {
            let (dw, dh, px) = with_encoder16!(&params.curve, |enc| run_oriented(&k, w, h, o, parallel, band, enc))?;
            (dw, dh, ch, RawPixelData::U16(px))
        }
        RgbEncoding::Linear16 => {
            let (dw, dh, px) = run_oriented(&k, w, h, o, parallel, band, Sample(linear16))?;
            (dw, dh, ch, RawPixelData::U16(px))
        }
        RgbEncoding::LinearF16 => {
            let (dw, dh, px) = run_oriented(&k, w, h, o, parallel, band, Sample(f32_to_f16))?;
            (dw, dh, ch, RawPixelData::F16(px))
        }
        RgbEncoding::LinearF32 => {
            let (dw, dh, px) = run_oriented(&k, w, h, o, parallel, band, Sample(|v: f32| v))?;
            (dw, dh, ch, RawPixelData::F32(px))
        }
    })
}

fn rgba8_encoder() -> impl Fn([f32; 3]) -> [u8; 4] + Sync {
    let lut = srgb_lut();
    move |p: [f32; 3]| {
        [
            lut[(p[0].clamp(0.0, 1.0) * 4096.0) as usize],
            lut[(p[1].clamp(0.0, 1.0) * 4096.0) as usize],
            lut[(p[2].clamp(0.0, 1.0) * 4096.0) as usize],
            255,
        ]
    }
}

/// An output encoder: linear RGB pixels to `N` samples of `T` each.
pub(crate) trait Encode<T: Copy, const N: usize>: Sync {
    /// One pixel.
    fn px(&self, p: [f32; 3]) -> [T; N];
    /// `src` in order into `dst` (`src.len() * N` samples).
    #[inline(always)]
    fn run(&self, src: &[[f32; 3]], dst: &mut [T]) {
        for (o, p) in dst.chunks_exact_mut(N).zip(src) {
            o.copy_from_slice(&self.px(*p));
        }
    }
}

/// An encoder given per pixel.
pub(crate) struct Pixel<F>(pub F);

impl<T: Copy, const N: usize, F: Fn([f32; 3]) -> [T; N] + Sync> Encode<T, N> for Pixel<F> {
    #[inline(always)]
    fn px(&self, p: [f32; 3]) -> [T; N] {
        (self.0)(p)
    }
}

/// An encoder that treats every sample alike (three samples per pixel): runs are encoded as one
/// flat loop, which vectorises (compiled a second time for AVX2 on x86-64; the same operations
/// in the same order, so the result is the same).
pub(crate) struct Sample<F>(pub F);

impl<T: Copy, F: Fn(f32) -> T + Sync> Encode<T, 3> for Sample<F> {
    #[inline(always)]
    fn px(&self, p: [f32; 3]) -> [T; 3] {
        p.map(&self.0)
    }
    #[inline(always)]
    fn run(&self, src: &[[f32; 3]], dst: &mut [T]) {
        let src = src.as_flattened();
        #[cfg(all(feature = "simd", target_arch = "x86_64"))]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: the CPU supports AVX2 (checked above).
            return unsafe { encode_samples_avx2(&self.0, src, dst) };
        }
        encode_samples(&self.0, src, dst)
    }
}

#[inline(always)]
fn encode_samples<T>(f: &impl Fn(f32) -> T, src: &[f32], dst: &mut [T]) {
    for (o, &v) in dst.iter_mut().zip(src) {
        *o = f(v);
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn encode_samples_avx2<T>(f: &impl Fn(f32) -> T, src: &[f32], dst: &mut [T]) {
    encode_samples(f, src, dst)
}

/// The 8-bit encoder of one table shared by the three channels:
/// `table[trunc(clamp(v, 0, 1) * (len - 1))]`, alpha 255. Runs are encoded in blocks: the table
/// indices first (a loop that vectorises), then the table reads; the same operations per sample
/// as the per-pixel form, so the same result.
pub(crate) struct Lut8<'a>(pub &'a [u8]);

impl Encode<u8, 4> for Lut8<'_> {
    #[inline(always)]
    fn px(&self, p: [f32; 3]) -> [u8; 4] {
        let n = (self.0.len() - 1) as f32;
        let t = self.0;
        [
            t[(p[0].clamp(0.0, 1.0) * n) as usize],
            t[(p[1].clamp(0.0, 1.0) * n) as usize],
            t[(p[2].clamp(0.0, 1.0) * n) as usize],
            255,
        ]
    }
    #[inline(always)]
    fn run(&self, src: &[[f32; 3]], dst: &mut [u8]) {
        let src = src.as_flattened();
        #[cfg(all(feature = "simd", target_arch = "x86_64"))]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: the CPU supports AVX2 (checked above).
            return unsafe { lut8_run_avx2(self.0, src, dst) };
        }
        lut8_run(self.0, src, dst)
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn lut8_run_avx2(lut: &[u8], src: &[f32], dst: &mut [u8]) {
    lut8_run(lut, src, dst)
}

#[inline(always)]
fn lut8_run(lut: &[u8], src: &[f32], dst: &mut [u8]) {
    const B: usize = 96; // samples per block, a multiple of 3
    let n = (lut.len() - 1) as f32;
    let last = lut.len() - 1;
    let mut idx = [0u32; B];
    for (s, o) in src.chunks(B).zip(dst.chunks_mut(B / 3 * 4)) {
        for (i, &v) in idx.iter_mut().zip(s) {
            *i = (v.clamp(0.0, 1.0) * n) as u32;
        }
        for (px, ix) in o.chunks_exact_mut(4).zip(idx[..s.len()].chunks_exact(3)) {
            px[0] = lut[(ix[0] as usize).min(last)];
            px[1] = lut[(ix[1] as usize).min(last)];
            px[2] = lut[(ix[2] as usize).min(last)];
            px[3] = 255;
        }
    }
}

/// The 16-bit sRGB encoder ([`srgb16_paired`]). Runs are encoded in blocks: the table indices
/// and fractions first (a loop that vectorises), then the table reads, then the interpolation
/// (vectorises again); one fused loop does not vectorise, and with AVX2 gathers it is slower
/// than scalar code. The same operations as `srgb16_paired`, so the same result.
pub(crate) struct Srgb16;

impl Encode<u16, 3> for Srgb16 {
    #[inline(always)]
    fn px(&self, p: [f32; 3]) -> [u16; 3] {
        let lut = srgb16_pairs();
        p.map(|v| srgb16_paired(v, lut))
    }
    #[inline(always)]
    fn run(&self, src: &[[f32; 3]], dst: &mut [u16]) {
        let src = src.as_flattened();
        #[cfg(all(feature = "simd", target_arch = "x86_64"))]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: the CPU supports AVX2 (checked above).
            return unsafe { srgb16_run_avx2(src, dst) };
        }
        srgb16_run(src, dst)
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn srgb16_run_avx2(src: &[f32], dst: &mut [u16]) {
    srgb16_run(src, dst)
}

#[inline(always)]
fn srgb16_run(src: &[f32], dst: &mut [u16]) {
    const B: usize = 96;
    let lut = srgb16_pairs();
    let (mut idx, mut t, mut a, mut d) = ([0u32; B], [0f32; B], [0f32; B], [0f32; B]);
    for (s, o) in src.chunks(B).zip(dst.chunks_mut(B)) {
        let n = s.len();
        for k in 0..n {
            let x = s[k].clamp(0.0, 1.0) * SRGB16_SEGMENTS as f32;
            let i = (x as u32).min(SRGB16_SEGMENTS as u32 - 1);
            idx[k] = i;
            t[k] = x - i as f32;
        }
        for k in 0..n {
            [a[k], d[k]] = lut[idx[k] as usize];
        }
        for k in 0..n {
            o[k] = (a[k] + d[k] * t[k] + 0.5) as u16;
        }
    }
}

/// Develop `w x h` output pixels with `k` and store them encoded by `enc` (`N` samples per
/// pixel) under orientation `o`. Without `band`, rows are developed and stored one at a
/// time; with it, each task develops its strip of rows, hands it to `band` (in developed
/// row order), then stores it.
fn run_oriented<T, const N: usize>(
    k: &Kernel,
    w: usize,
    h: usize,
    o: Orientation,
    parallel: bool,
    band: Option<&BandHook<'_>>,
    enc: impl Encode<T, N>,
) -> Result<(u32, u32, Vec<T>)>
where
    T: Copy + Default + Send + Sync,
{
    let swap = o.swaps_dimensions();
    let (dw, dh) = if swap { (h, w) } else { (w, h) };
    let mut out = vec![T::default(); dw * dh * N];
    // Destination strips: horizontal (rows) normally, vertical (columns) when transposing,
    // so each strip corresponds to a contiguous range of developed rows.
    let strip = k.strip_rows();
    let (tw, th) = if swap { (strip as u32, dh as u32) } else { (dw as u32, strip as u32) };
    let grid = par::GridWriter::new(&mut out, dw as u32, dh as u32, N as u32, (0, 0), tw, th, dw as u32, dh as u32)
        .ok_or_else(|| Error::InvalidArgument("invalid output size".into()))?;
    let n = if swap { grid.tiles_x() } else { grid.tiles_y() } as usize;
    // Destination row (or column) `d` shows developed row `d`, or `h - 1 - d` when reversed;
    // inside it, output pixel `i` is developed column `i` or `w - 1 - i`.
    let (rev_y, rev_x) = if swap {
        (
            matches!(o, Orientation::Rotate90 | Orientation::Transverse),
            matches!(o, Orientation::Rotate270 | Orientation::Transverse),
        )
    } else {
        (
            matches!(o, Orientation::Rotate180 | Orientation::FlipV),
            matches!(o, Orientation::FlipH | Orientation::Rotate180),
        )
    };
    par::for_each_index(n, parallel, |t| {
        let Some(mut tile) = (if swap { grid.claim(t as u32, 0) } else { grid.claim(0, t as u32) }) else { return };
        let mut cache = RowCache::new(k.img.width as usize * k.img.cpp as usize);
        let (tw_, th_) = tile.size();
        let count = if swap { tw_ } else { th_ };
        let src_row = |i: usize| {
            let d = t * strip + i;
            if rev_y { h - 1 - d } else { d }
        };
        let mut put = |i: usize, rgb: &[[f32; 3]]| {
            if !swap {
                let row = tile.row(i);
                if rev_x {
                    for (px, p) in row.chunks_exact_mut(N).zip(rgb[..w].iter().rev()) {
                        px.copy_from_slice(&enc.px(*p));
                    }
                } else {
                    enc.run(&rgb[..w], &mut row[..w * N]);
                }
            } else {
                for oy in 0..th_ {
                    let sx = if rev_x { w - 1 - oy } else { oy };
                    tile.row(oy)[i * N..i * N + N].copy_from_slice(&enc.px(rgb[sx]));
                }
            }
        };
        match band {
            None if k.strip_rows() == STRIP as usize => {
                let mut rgb = vec![[0f32; 3]; w];
                for i in 0..count {
                    k.row(&mut cache, src_row(i), &mut rgb);
                    put(i, &rgb);
                }
            }
            None => {
                // Finished pixels go straight to the destination, chunk by chunk.
                let first = if rev_y { src_row(count - 1) } else { src_row(0) };
                k.strip_to(&mut cache, first, count, w, &mut |n, x0, px| {
                    // Destination index of developed row `first + n` within this tile.
                    let i = if rev_y { h - 1 - t * strip - (first + n) } else { n };
                    if !swap {
                        let row = tile.row(i);
                        if rev_x {
                            for (j, p) in px.iter().enumerate() {
                                let d = w - 1 - (x0 + j);
                                row[d * N..d * N + N].copy_from_slice(&enc.px(*p));
                            }
                        } else {
                            enc.run(px, &mut row[x0 * N..(x0 + px.len()) * N]);
                        }
                    } else {
                        for (j, p) in px.iter().enumerate() {
                            let x = x0 + j;
                            let oy = if rev_x { w - 1 - x } else { x };
                            tile.row(oy)[i * N..i * N + N].copy_from_slice(&enc.px(*p));
                        }
                    }
                });
            }
            _ => {
                let first = if rev_y { src_row(count - 1) } else { src_row(0) };
                let mut buf = vec![[0f32; 3]; w * count];
                k.strip(&mut cache, first, count, &mut buf);
                if let Some(hook) = band {
                    hook(first, &mut buf);
                }
                for i in 0..count {
                    let s = src_row(i) - first;
                    put(i, &buf[s * w..(s + 1) * w]);
                }
            }
        }
    });
    drop(grid);
    Ok((dw as u32, dh as u32, out))
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

/// Segments of the 16-bit sRGB curve table.
pub const SRGB16_SEGMENTS: usize = 16384;

/// The sRGB transfer curve scaled to 16 bits at `SRGB16_SEGMENTS + 1` evenly spaced
/// points of `[0, 1]` (exact in f64, stored as f32), interpolated linearly by [`srgb16`].
pub fn srgb16_lut() -> &'static [f32] {
    use std::sync::OnceLock;
    static LUT: OnceLock<Vec<f32>> = OnceLock::new();
    LUT.get_or_init(|| {
        (0..=SRGB16_SEGMENTS)
            .map(|i| {
                let x = i as f64 / SRGB16_SEGMENTS as f64;
                let s = if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
                (s * 65535.0) as f32
            })
            .collect()
    })
}

/// Linear `[0, 1]` → sRGB-encoded 16-bit sample (within one code of the exact curve).
#[inline(always)]
pub fn srgb16(v: f32, lut: &[f32]) -> u16 {
    let x = v.clamp(0.0, 1.0) * SRGB16_SEGMENTS as f32;
    let i = (x as usize).min(SRGB16_SEGMENTS - 1);
    let t = x - i as f32;
    let ab = &lut[i..i + 2];
    let (a, b) = (ab[0], ab[1]);
    (a + (b - a) * t + 0.5) as u16
}

/// [`srgb16_lut`] as `(value, step to the next value)` pairs, so [`srgb16_paired`] reads one
/// pair per sample. The steps are the same f32 differences [`srgb16`] computes.
fn srgb16_pairs() -> &'static [[f32; 2]; SRGB16_SEGMENTS] {
    use std::sync::OnceLock;
    static PAIRS: OnceLock<Box<[[f32; 2]; SRGB16_SEGMENTS]>> = OnceLock::new();
    PAIRS.get_or_init(|| {
        let lut = srgb16_lut();
        let v: Vec<[f32; 2]> = (0..SRGB16_SEGMENTS).map(|i| [lut[i], lut[i + 1] - lut[i]]).collect();
        v.into_boxed_slice().try_into().unwrap()
    })
}

/// [`srgb16`] from [`srgb16_pairs`]: the same result, one table read.
#[inline(always)]
fn srgb16_paired(v: f32, pairs: &[[f32; 2]; SRGB16_SEGMENTS]) -> u16 {
    let x = v.clamp(0.0, 1.0) * SRGB16_SEGMENTS as f32;
    let i = (x as usize).min(SRGB16_SEGMENTS - 1);
    let t = x - i as f32;
    let [a, d] = pairs[i];
    (a + d * t + 0.5) as u16
}

/// Linear `[0, 1]` → 16-bit sample (clipped, rounded).
#[inline(always)]
pub fn linear16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
}

/// f32 → IEEE binary16 bits, rounded to nearest even; finite values beyond the half range
/// saturate to ±65504, NaN stays NaN. Branch-free (selects), so loops over it vectorise.
#[inline(always)]
pub fn f32_to_f16(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = (b >> 16) & 0x8000;
    let a = b & 0x7fff_ffff;
    let odd = (a >> 13) & 1;
    let normal = a.wrapping_add(0xc800_0fff).wrapping_add(odd) >> 13;
    // Below the smallest normal half: let the FPU round into the subnormal range.
    let sub = (f32::from_bits(a) + 0.5).to_bits().wrapping_sub(0x3f00_0000);
    let mut h = if a < 0x3880_0000 { sub } else { normal };
    // >= 65520 would round to infinity: saturate (NaN stays NaN).
    let big = if a > 0x7f80_0000 { 0x7e00 } else { 0x7bff };
    h = if a >= 0x477f_f000 { big } else { h };
    (sign | h) as u16
}

/// IEEE binary16 bits → f32 (exact).
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h as u32) & 0x8000) << 16;
    let e = ((h >> 10) & 0x1f) as u32;
    let m = (h & 0x3ff) as u32;
    let bits = match e {
        0 if m == 0 => sign,
        0 => {
            // Subnormal: m * 2^-24.
            let v = m as f32 * f32::from_bits(0x3380_0000);
            return if sign != 0 { -v } else { v };
        }
        31 => sign | 0x7f80_0000 | (m << 13),
        _ => sign | ((e + 112) << 23) | (m << 13),
    };
    f32::from_bits(bits)
}

/// Encode linear RGB to sRGB RGBA8 and apply EXIF orientation.
pub fn to_rgba8(d: &Developed, orientation: Orientation, parallel: bool) -> (u32, u32, Vec<u8>) {
    let (w, h, px) = encode_developed(d, Pixel(rgba8_encoder()), orientation, parallel);
    (w, h, px)
}

/// Encode a developed image with `enc` (`N` samples per pixel) under `orientation`.
fn encode_developed<T, const N: usize>(
    d: &Developed,
    enc: impl Encode<T, N>,
    orientation: Orientation,
    parallel: bool,
) -> (u32, u32, Vec<T>)
where
    T: Copy + Default + Send,
{
    let (w, h) = (d.width as usize, d.height as usize);
    let (ow, oh) = if orientation.swaps_dimensions() { (h, w) } else { (w, h) };
    let mut out = vec![T::default(); ow * oh * N];
    par::for_each_row_chunk(&mut out, ow * N, 16, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_mut(ow * N).enumerate() {
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
                row[ox * N..ox * N + N].copy_from_slice(&enc.px([s[0], s[1], s[2]]));
            }
        }
    });
    (ow as u32, oh as u32, out)
}

/// Encode a developed image (`d`: linear RGB after colour conversion) to `enc`.
pub(crate) fn encode_image(
    d: Developed,
    enc: RgbEncoding,
    curve: &OutputCurve,
    orientation: Orientation,
    parallel: bool,
) -> (u32, u32, u32, RawPixelData) {
    let o = if enc.oriented() { orientation } else { Orientation::Normal };
    let ch = enc.channels();
    match enc {
        RgbEncoding::Rgba8 => {
            let (w, h, px) = with_encoder8!(curve, |enc| encode_developed(&d, enc, o, parallel));
            (w, h, ch, RawPixelData::U8(px))
        }
        RgbEncoding::Srgb16 => {
            let (w, h, px) = with_encoder16!(curve, |enc| encode_developed(&d, enc, o, parallel));
            (w, h, ch, RawPixelData::U16(px))
        }
        RgbEncoding::Linear16 => {
            let (w, h, px) = encode_developed(&d, Sample(linear16), o, parallel);
            (w, h, ch, RawPixelData::U16(px))
        }
        RgbEncoding::LinearF16 => {
            let (w, h, px) = encode_developed(&d, Sample(f32_to_f16), o, parallel);
            (w, h, ch, RawPixelData::F16(px))
        }
        RgbEncoding::LinearF32 => (d.width, d.height, ch, RawPixelData::F32(d.rgb)),
    }
}

/// Encode a developed image whose values are already display-encoded device values in
/// `[0, 1]` (the output of a profile transform) to `enc` (`Rgba8` or `Srgb16` only).
#[cfg(feature = "icc")]
pub(crate) fn encode_device(
    d: Developed,
    enc: RgbEncoding,
    orientation: Orientation,
    parallel: bool,
) -> (u32, u32, u32, RawPixelData) {
    let ch = enc.channels();
    match enc {
        RgbEncoding::Rgba8 => {
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            let (w, h, px) =
                encode_developed(&d, Pixel(|p: [f32; 3]| [q(p[0]), q(p[1]), q(p[2]), 255]), orientation, parallel);
            (w, h, ch, RawPixelData::U8(px))
        }
        _ => {
            let (w, h, px) = encode_developed(&d, Sample(linear16), orientation, parallel);
            (w, h, ch, RawPixelData::U16(px))
        }
    }
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
    fn blend_highlights_keep_the_colour_above_white_and_change_nothing_below() {
        let wb = [2.0, 1.0, 1.5, 1.0];
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let f = |px: [f32; 3], h: Highlights| finish_rgb(px, &wb, &id, None, h, &ReconModel::default());
        // Red above white after balance (0.8 * 2 = 1.6), green and blue low: Clip loses the ratio,
        // Blend keeps it and reaches exactly white level in the brightest channel.
        let red = [0.8, 0.2, 0.1];
        let (c, b) = (f(red, Highlights::Clip), f(red, Highlights::Blend));
        assert_eq!(b[0], 1.0);
        assert!((b[1] / b[0] - 0.2 / 1.6).abs() < 1e-6 && (b[2] / b[0] - 0.15 / 1.6).abs() < 1e-6, "{b:?}");
        assert!(c[1] / c[0] > b[1] / b[0], "{c:?} {b:?}");
        // At sensor saturation both fading modes are white.
        assert_eq!(f([0.45, 1.0, 0.6], Highlights::Blend), [1.0, 1.0, 1.0]);
        // Pixels whose balanced channels are all within [0, 1] are identical in every mode.
        for px in [[0.3, 0.4, 0.2], [0.0, 0.0, 0.0], [0.4, 0.9, 0.5]] {
            assert_eq!(f(px, Highlights::Clip), f(px, Highlights::Blend), "{px:?}");
        }
    }

    #[test]
    fn reconstruct_predicts_clipped_channels_from_a_fitted_model() {
        // A frame whose unclipped blocks follow R = t, B = 0.8 t, G = 1.3 t (wb 1): the model
        // recovers a green clipped at the sensor level from red and blue.
        let (w, h) = (192u32, 192u32);
        let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
        for y in 0..h as usize {
            for x in 0..w as usize {
                // Per 6x6 block a level t in (0.2, 0.75): green stays below the clipping level.
                let t = 0.2 + 0.55 * (((x / 6) * 7 + (y / 6) * 3) % 32) as f32 / 31.0;
                let v = match (y % 2, x % 2) {
                    (0, 0) => t,
                    (1, 1) => 0.8 * t,
                    _ => 1.3 * t,
                };
                img.u16_mut()[y * w as usize + x] = (v * 4000.0) as u16;
            }
        }
        let mut info = FrameInfo::new(w, h, 12);
        info.cfa = Some(CfaPattern::RGGB);
        info.black_level = BlackLevel::uniform(0.0);
        info.white_level = WhiteLevel::uniform(4000);
        let model = fit_reconstruction(&img, &info, (0, 0), [1.0; 4]).unwrap();
        let [a, b] = model.alone[1].unwrap();
        assert!((a + b * 0.8 - 1.3).abs() < 0.05, "{a} {b}");
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let f = |px: [f32; 3], h: Highlights| finish_rgb(px, &[1.0; 4], &id, None, h, &model);
        // Green clipped where t = 0.85 (red 0.85, blue 0.68; green really 1.105): the colour comes back.
        let px = [0.85, 1.0, 0.68];
        let rec = f(px, Highlights::Reconstruct);
        assert!((rec[1] / rec[0] - 1.3).abs() < 0.05 && rec[1] == 1.0, "{rec:?}");
        assert!(f(px, Highlights::Blend)[1] / f(px, Highlights::Blend)[0] < 1.25);
        // Without a model the same pixel keeps its measured, clipped ratio.
        assert_eq!(finish_rgb(px, &[1.0; 4], &id, None, Highlights::Reconstruct, &ReconModel::default())[1], 1.0);
        // Pixels without a clipped channel are untouched, and all-clipped pixels are white.
        let ok = [0.4, 0.5, 0.3];
        assert_eq!(f(ok, Highlights::Reconstruct), f(ok, Highlights::Clip));
        assert_eq!(f([1.0, 1.0, 1.0], Highlights::Reconstruct), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn neutral_highlights_remove_the_cast_of_partly_clipped_pixels() {
        let wb = [2.0, 1.0, 1.5, 1.0];
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let f = |px: [f32; 3], h: Highlights| finish_rgb(px, &wb, &id, None, h, &ReconModel::default());
        // Green at saturation, red and blue below it: clip keeps a green cast, neutral is white.
        let partly = [0.45, 1.0, 0.6];
        assert!(f(partly, Highlights::Clip)[0] < 0.95);
        assert_eq!(f(partly, Highlights::Neutral), [1.0, 1.0, 1.0]);
        // Halfway to saturation the fade is partial and monotonic.
        let near = f([0.4, 0.975, 0.5], Highlights::Neutral);
        let clip = f([0.4, 0.975, 0.5], Highlights::Clip);
        assert!(near[0] > clip[0] && near[0] < 0.975 + 1e-3, "{near:?} {clip:?}");
        // Pixels away from saturation, and fully clipped ones, are identical in both modes.
        for px in [[0.3, 0.4, 0.2], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]] {
            assert_eq!(f(px, Highlights::Clip), f(px, Highlights::Neutral), "{px:?}");
        }
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
    fn camera_applied_white_balance_develops_with_unit_multipliers() {
        let mut info = FrameInfo::new(8, 8, 14);
        info.cpp = 3;
        info.layout = SensorLayout::Linear;
        info.white_balance = WhiteBalance::from_multipliers(2.0, 1.0, 1.5, 1.0);
        assert_eq!(DevelopParams::new(&info, Demosaic::Bilinear, Rect::full(8, 8)).wb, [2.0, 1.0, 1.5, 1.0]);
        info.white_balance_applied = true;
        assert_eq!(DevelopParams::new(&info, Demosaic::Bilinear, Rect::full(8, 8)).wb, [1.0; 4]);
        info.white_balance = None;
        assert_eq!(DevelopParams::new(&info, Demosaic::Bilinear, Rect::full(8, 8)).wb, [1.0; 4]);
    }

    #[test]
    fn reconstruct_prediction_is_capped_over_the_clipped_value() {
        let mut m = ReconModel::default();
        m.alone[0] = Some([10.0, 0.0]);
        m.ratio[1][0] = Some(10.0);
        let mut u = [1.0, 0.5, 0.5];
        m.apply(&mut u, [true, false, false]);
        assert_eq!(u[0], RECON_HEADROOM);
        // Two channels clipped: both are raised from the unclipped one, each at most to the cap.
        let mut u = [1.0, 1.0, 0.2];
        m.ratio[0][2] = Some(1.2);
        m.ratio[1][2] = Some(10.0);
        m.apply(&mut u, [true, true, false]);
        assert!((u[0] - 1.0f32.max(1.2 * 0.2)).abs() < 1e-6 && u[1] == RECON_HEADROOM);
    }

    #[test]
    fn daylight_and_explicit_white_balance() {
        let mut info = FrameInfo::new(8, 8, 12);
        info.cfa = Some(CfaPattern::RGGB);
        // No matrix: nothing to compute; explicit multipliers are normalised by green.
        assert_eq!(daylight_white_balance(&info), None);
        assert_eq!(explicit_white_balance(&info, [3.0, 2.0, 4.0]), Some([1.5, 1.0, 2.0, 1.0]));
        let cm = ColorMatrix::new(Illuminant::D65, 3, vec![0.5, 0.2, 0.1, 0.1, 1.0, 0.1, 0.0, 0.2, 0.8]).unwrap();
        info.color_matrices.color.push(cm.clone());
        let wb = daylight_white_balance(&info).unwrap();
        // The camera response to D65 times the multipliers is equal in every channel.
        let r = cm.multipliers_for([1.0; 3]).unwrap();
        assert!((wb[0] - r[0] / r[1]).abs() < 1e-5 && (wb[2] - r[2] / r[1]).abs() < 1e-5 && wb[1] == 1.0);
        // Camera-balanced frames keep their own balance.
        info.white_balance_applied = true;
        assert_eq!(daylight_white_balance(&info), None);
        assert_eq!(explicit_white_balance(&info, [1.0, 1.0, 1.0]), None);
    }

    #[test]
    fn auto_white_balance_refines_the_start_over_neutral_areas_and_ignores_coloured_ones() {
        let (w, h) = (192u32, 192u32);
        let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
        // RGGB, black 0, white 4000. A neutral surface under a light that the start balance
        // (2.0, 1.5) gets wrong: the true multipliers are (2.2, 1.6), so neutral reads
        // R = 1000, G = 2200, B = 1375. The top third is a saturated red object.
        for y in 0..h as usize {
            for x in 0..w as usize {
                let ripple = ((x * 7 + y * 13) % 11) as u16 * 3;
                let v = if y < 64 {
                    match (y % 2, x % 2) {
                        (0, 0) => 3200,
                        (1, 1) => 200,
                        _ => 600,
                    }
                } else {
                    match (y % 2, x % 2) {
                        (0, 0) => 1000,
                        (1, 1) => 1375,
                        _ => 2200,
                    }
                };
                img.u16_mut()[y * w as usize + x] = v + ripple;
            }
        }
        let mut info = FrameInfo::new(w, h, 12);
        info.cfa = Some(CfaPattern::RGGB);
        info.black_level = BlackLevel::uniform(0.0);
        info.white_level = WhiteLevel::uniform(4000);
        let wb = auto_white_balance(&img, &info, (0, 0), [2.0, 1.0, 1.5, 1.0]).unwrap();
        assert!((wb[0] - 2.2).abs() < 0.05 && wb[1] == 1.0 && (wb[2] - 1.6).abs() < 0.05, "{wb:?}");
        // The multipliers never leave a factor of two around the start.
        let far = auto_white_balance(&img, &info, (0, 0), [5.0, 1.0, 1.0, 1.0]);
        assert!(far.is_none_or(|f| f[0] >= 2.5 && f[2] >= 0.5));
        // Frames the camera balanced, and sensors without a three-colour mosaic, are left alone.
        info.white_balance_applied = true;
        assert_eq!(auto_white_balance(&img, &info, (0, 0), [2.0, 1.0, 1.5, 1.0]), None);
        info.white_balance_applied = false;
        info.cfa = None;
        assert_eq!(auto_white_balance(&img, &info, (0, 0), [2.0, 1.0, 1.5, 1.0]), None);
    }

    #[test]
    fn fused_rgba8_matches_two_pass_for_all_orientations() {
        let (w, h) = (37u32, 23u32);
        let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
        for (i, v) in img.u16_mut().iter_mut().enumerate() {
            *v = ((i * 7919) % 3900 + 100) as u16;
        }
        for cfa in [CfaPattern::RGGB, CfaPattern::parse("GGRGGB/GGBGGR/BRGRBG/GGBGGR/GGRGGB/RBGBRG").unwrap()] {
            let mut info = FrameInfo::new(w, h, 12);
            info.cfa = Some(cfa);
            info.black_level = BlackLevel::uniform(64.0);
            info.white_level = WhiteLevel::uniform(4095);
            info.white_balance = WhiteBalance::from_multipliers(2.0, 1.0, 1.5, 1.0);
            for demosaic in [Demosaic::Bilinear, Demosaic::HalfSize] {
                for o in 1..=8 {
                    info.orientation = Orientation::from_exif(o);
                    let p = DevelopParams::new(&info, demosaic, Rect::new(1, 2, 33, 19));
                    let d = develop(&img, &info, (3, 1), &p, true).unwrap();
                    let a = to_rgba8(&d, info.orientation, false);
                    let b = develop_rgba8(&img, &info, (3, 1), &p, true).unwrap();
                    assert_eq!(a, b, "orientation {o} {demosaic:?}");
                }
            }
        }
    }

    /// The demosaics that hand their bands over in column chunks are streamed to the encoder
    /// (`Kernel::strip_to`): the result equals developing whole strips, then encoding, for
    /// every orientation, across chunk and band borders.
    #[test]
    fn streamed_chunks_match_two_pass_for_all_orientations() {
        let (w, h) = (430u32, 150u32);
        let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
        let mut s = 12345u32;
        for v in img.u16_mut().iter_mut() {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            *v = (100 + (s >> 20) % 3900) as u16;
        }
        let xtrans = CfaPattern::parse("GGRGGB/GGBGGR/BRGRBG/GGBGGR/GGRGGB/RBGBRG").unwrap();
        for (cfa, demosaics) in [
            (CfaPattern::RGGB, &[Demosaic::Ahd, Demosaic::EdgeAware, Demosaic::Ppg][..]),
            (xtrans, &[Demosaic::EdgeAware][..]),
        ] {
            let mut info = FrameInfo::new(w, h, 12);
            info.cfa = Some(cfa);
            info.black_level = BlackLevel::uniform(64.0);
            info.white_level = WhiteLevel::uniform(4095);
            info.white_balance = WhiteBalance::from_multipliers(2.0, 1.0, 1.5, 1.0);
            for &demosaic in demosaics {
                for o in 1..=8 {
                    info.orientation = Orientation::from_exif(o);
                    let p = DevelopParams::new(&info, demosaic, Rect::new(3, 2, 421, 141));
                    let d = develop(&img, &info, (1, 0), &p, true).unwrap();
                    let a = to_rgba8(&d, info.orientation, false);
                    let b = develop_rgba8(&img, &info, (1, 0), &p, true).unwrap();
                    assert!(a == b, "orientation {o} {demosaic:?}");
                }
            }
        }
    }

    /// Naive bilinear reference: own colour = centre sample; other colours = mean of the
    /// same-colour samples in the 3x3 neighbourhood that lie inside the image.
    fn naive(img: &RawImage, info: &FrameInfo, origin: (u32, u32)) -> Vec<[f32; 3]> {
        let (w, h) = (img.width as i64, img.height as i64);
        let cfa = info.cfa.unwrap();
        let col =
            |x: i64, y: i64| cfa.color_at((x + origin.0 as i64) as usize, (y + origin.1 as i64) as usize) as usize;
        let calib = Calib::new(info, 0);
        let v = |x: i64, y: i64| {
            calib.norm(
                img.u16()[(y * w + x) as usize] as f32,
                (x + origin.0 as i64) as usize,
                (y + origin.1 as i64) as usize,
            )
        };
        let mut out = vec![];
        for y in 0..h {
            for x in 0..w {
                let own = col(x, y);
                let mut rgb = [0f32; 3];
                for (c, slot) in rgb.iter_mut().enumerate() {
                    if c == own {
                        *slot = v(x, y);
                        continue;
                    }
                    let (mut s, mut n) = (0f32, 0f32);
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            let (xx, yy) = (x + dx, y + dy);
                            if xx >= 0 && yy >= 0 && xx < w && yy < h && col(xx, yy) == c {
                                s += v(xx, yy);
                                n += 1.0;
                            }
                        }
                    }
                    *slot = if n > 0.0 { s / n } else { 0.0 };
                }
                out.push(rgb);
            }
        }
        out
    }

    #[test]
    fn bilinear_matches_naive_reference() {
        let (w, h) = (29u32, 17u32);
        let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
        for (i, v) in img.u16_mut().iter_mut().enumerate() {
            *v = ((i * 7919) % 3900 + 100) as u16;
        }
        let pats = [
            CfaPattern::RGGB,
            CfaPattern::GBRG,
            CfaPattern::GRBG,
            CfaPattern::BGGR,
            CfaPattern::parse("GGRGGB/GGBGGR/BRGRBG/GGBGGR/GGRGGB/RBGBRG").unwrap(),
        ];
        for cfa in pats {
            for origin in [(0, 0), (1, 0), (0, 1), (3, 5)] {
                let mut info = FrameInfo::new(w, h, 12);
                info.cfa = Some(cfa);
                info.black_level = BlackLevel::pattern2x2([60.0, 64.0, 62.0, 66.0]);
                info.white_level = WhiteLevel::uniform(4095);
                // Identity WB/matrix so the output is the demosaiced camera RGB.
                let mut p = DevelopParams::new(&info, Demosaic::Bilinear, Rect::full(w, h));
                p.cam_to_rgb = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
                let d = develop(&img, &info, origin, &p, false).unwrap();
                let r = naive(&img, &info, origin);
                for (i, px) in d.rgb.chunks(3).enumerate() {
                    for c in 0..3 {
                        let want = r[i][c].clamp(0.0, 1.0);
                        assert!(
                            (px[c] - want).abs() < 1e-5,
                            "{cfa} origin {origin:?} pixel {i} ch {c}: {} vs {want}",
                            px[c]
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn half_size_monochrome_is_grey() {
        let mut img = RawImage::new_u16(8, 8, 1, &Limits::default()).unwrap();
        img.u16_mut().iter_mut().for_each(|v| *v = 1000);
        let mut info = FrameInfo::new(8, 8, 12);
        info.cfa = None;
        info.layout = SensorLayout::Monochrome;
        info.white_level = WhiteLevel::uniform(4000);
        let p = DevelopParams::new(&info, Demosaic::HalfSize, Rect::full(8, 8));
        let d = develop(&img, &info, (0, 0), &p, false).unwrap();
        assert!(d.rgb.iter().all(|&v| (v - 0.25).abs() < 1e-5), "{:?}", &d.rgb[..3]);
    }

    #[test]
    fn masked_black_per_position() {
        let mut img = RawImage::new_u16(10, 6, 1, &Limits::default()).unwrap();
        for y in 0..6 {
            for x in 0..10 {
                img.u16_mut()[y * 10 + x] = (100 + (y % 2) * 20 + (x % 2) * 10) as u16;
            }
        }
        let mut info = FrameInfo::new(10, 6, 12);
        info.cfa = Some(CfaPattern::RGGB);
        info.masked_areas = vec![Rect::new(1, 0, 3, 6), Rect::new(0, 4, 10, 2)];
        let b = black_from_masked(&img, &info).unwrap();
        assert_eq!(b.values, vec![100.0, 110.0, 120.0, 130.0]);
    }

    #[test]
    fn masked_black_per_sample() {
        // Three samples per pixel (Foveon): one black per sample, rows 0 and 5 masked.
        let mut img = RawImage::new_u16(4, 6, 3, &Limits::default()).unwrap();
        for (i, px) in img.u16_mut().chunks_exact_mut(3).enumerate() {
            let masked = i / 4 == 0 || i / 4 == 5;
            px.copy_from_slice(&if masked { [10 + (i % 2) as u16 * 2, 20, 30] } else { [500, 600, 700] });
        }
        let mut info = FrameInfo::new(4, 6, 12);
        info.layout = SensorLayout::Foveon;
        info.masked_areas = vec![Rect::new(0, 0, 4, 1), Rect::new(0, 5, 4, 1)];
        let b = black_from_masked(&img, &info).unwrap();
        assert_eq!((b.samples, b.values.clone()), (3, vec![11.0, 20.0, 30.0]));
        // Region decodes: the masked rows outside the region are not counted.
        let region = img.crop(Rect::new(0, 4, 4, 2)).unwrap();
        let b = black_from_masked_at(&region, &info, (0, 4)).unwrap();
        assert_eq!(b.values, vec![11.0, 20.0, 30.0]);
        info.masked_areas.clear();
        assert!(black_from_masked(&img, &info).is_none());
    }

    #[test]
    fn multichannel_fast_normalisation_matches_reference() {
        let (w, h) = (7u32, 3u32);
        let mut img = RawImage::new_u16(w, h, 3, &Limits::default()).unwrap();
        for (i, v) in img.u16_mut().iter_mut().enumerate() {
            *v = (i * 997 % 4000) as u16;
        }
        let mut info = FrameInfo::new(w, h, 12);
        info.cfa = None;
        info.cpp = 3;
        info.layout = SensorLayout::Linear;
        info.black_level =
            BlackLevel { rows: 1, cols: 1, samples: 3, values: vec![10.0, 20.0, 30.0], ..Default::default() };
        info.white_level = WhiteLevel { values: vec![4000, 3900, 3800] };
        let p = DevelopParams::new(&info, Demosaic::Bilinear, Rect::full(w, h));
        let k = kernel(&img, &info, (0, 0), &p, Rect::full(w, h));
        let mut row = vec![0f32; (w * 3) as usize];
        for y in 0..h as usize {
            k.normalize_row(y, &mut row);
            for x in 0..w as usize {
                for c in 0..3 {
                    let want = k.calibs[c].norm(img.sample(x as u32, y as u32, c as u32), x, y);
                    assert!((row[x * 3 + c] - want).abs() < 1e-6);
                }
            }
        }
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

    /// A Bayer and an X-Trans test image with calibration, white balance and a colour matrix.
    fn test_images() -> Vec<(RawImage, FrameInfo)> {
        let (w, h) = (37u32, 23u32);
        let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
        for (i, v) in img.u16_mut().iter_mut().enumerate() {
            *v = ((i * 7919) % 4100 + 20) as u16;
        }
        let mut out = Vec::new();
        for cfa in [CfaPattern::RGGB, CfaPattern::parse("GGRGGB/GGBGGR/BRGRBG/GGBGGR/GGRGGB/RBGBRG").unwrap()] {
            let mut info = FrameInfo::new(w, h, 12);
            info.cfa = Some(cfa);
            info.black_level = BlackLevel::uniform(64.0);
            info.white_level = WhiteLevel::uniform(4095);
            info.white_balance = WhiteBalance::from_multipliers(2.0, 1.0, 1.5, 1.0);
            info.color_matrices = ColorMatrices {
                color: vec![
                    ColorMatrix::new(Illuminant::D65, 3, vec![0.6, -0.1, -0.05, -0.4, 1.2, 0.2, -0.1, 0.25, 0.6])
                        .unwrap(),
                ],
                ..Default::default()
            };
            out.push((img.clone(), info));
        }
        out
    }

    /// Reference: develop to f32, then encode each pixel with `enc` and orient like `to_rgba8`.
    fn reference<T: Copy + Default + Send, const N: usize>(
        img: &RawImage,
        info: &FrameInfo,
        p: &DevelopParams,
        o: Orientation,
        enc: impl Fn([f32; 3]) -> [T; N] + Sync,
    ) -> (u32, u32, Vec<T>) {
        let d = develop(img, info, (3, 1), p, false).unwrap();
        encode_developed(&d, Pixel(enc), o, false)
    }

    #[test]
    fn encoded_outputs_match_the_f32_develop() {
        let lut = srgb16_lut();
        for (img, mut info) in test_images() {
            for demosaic in [Demosaic::Bilinear, Demosaic::HalfSize] {
                for o in 1..=8 {
                    info.orientation = Orientation::from_exif(o);
                    let p = DevelopParams::new(&info, demosaic, Rect::new(1, 2, 33, 19));
                    let run = |enc| develop_encoded(&img, &info, (3, 1), &p, enc, true).unwrap();
                    let o = info.orientation;
                    let (w, h, c, px) = run(RgbEncoding::Srgb16);
                    let want = reference(&img, &info, &p, o, |v: [f32; 3]| v.map(|x| srgb16(x, lut)));
                    assert_eq!((w, h, c, px.as_u16().unwrap()), (want.0, want.1, 3, &want.2[..]), "rgb16 {o:?}");
                    // Linear encodings keep the sensor orientation.
                    let n = Orientation::Normal;
                    let (w, h, _, px) = run(RgbEncoding::Linear16);
                    let want = reference(&img, &info, &p, n, |v: [f32; 3]| v.map(linear16));
                    assert_eq!((w, h, px.as_u16().unwrap()), (want.0, want.1, &want.2[..]), "linear16 {o:?}");
                    let (w, h, _, px) = run(RgbEncoding::LinearF16);
                    let want = reference(&img, &info, &p, n, |v: [f32; 3]| v.map(f32_to_f16));
                    assert_eq!((w, h, px.as_f16_bits().unwrap()), (want.0, want.1, &want.2[..]), "f16 {o:?}");
                    let (w, h, c, px) = run(RgbEncoding::Rgba8);
                    let want = develop_rgba8(&img, &info, (3, 1), &p, false).unwrap();
                    assert_eq!((w, h, c, px.as_u8().unwrap()), (want.0, want.1, 4, &want.2[..]), "rgba8 {o:?}");
                    let (_, _, _, px) = run(RgbEncoding::LinearF32);
                    assert_eq!(px.as_f32().unwrap(), &develop(&img, &info, (3, 1), &p, false).unwrap().rgb[..]);
                }
            }
        }
    }

    #[test]
    fn levels_scale_linear_values_after_the_matrix() {
        assert_eq!(Levels::new(0.0, 0.0, 1.0).unwrap(), None);
        assert!(Levels::new(f32::NAN, 0.0, 1.0).is_err());
        assert!(Levels::new(40.0, 0.0, 1.0).is_err());
        assert!(Levels::new(0.0, 0.5, 0.5).is_err());
        assert!(Levels::new(0.0, 0.0, f32::INFINITY).is_err());
        let l = Levels::new(1.0, 0.1, 0.9).unwrap().unwrap();
        assert!((l.scale - 2.5).abs() < 1e-6 && (l.offset - 0.125).abs() < 1e-6, "{l:?}");
        for (img, info) in test_images() {
            let full = Rect::full(img.width, img.height);
            let p = DevelopParams::new(&info, Demosaic::Bilinear, full);
            let base = develop(&img, &info, (0, 0), &p, true).unwrap();
            let lp = DevelopParams { levels: Some(l), ..p.clone() };
            let got = develop(&img, &info, (0, 0), &lp, true).unwrap();
            for (g, b) in got.rgb.iter().zip(&base.rgb) {
                assert_eq!(*g, *b * l.scale - l.offset);
            }
            // Integer encodings clip the result.
            let (_, _, _, px) = develop_encoded(&img, &info, (0, 0), &lp, RgbEncoding::Linear16, true).unwrap();
            let want: Vec<u16> = got.rgb.iter().map(|&v| linear16(v)).collect();
            assert_eq!(px.as_u16().unwrap(), &want[..]);
        }
    }

    #[test]
    fn encoder_runs_match_their_pixels() {
        let mut v: Vec<f32> = (0..=70_000).map(|i| i as f32 / 65_536.0 - 0.03).collect();
        v.extend([f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.0, 1.0, 0.999_999_9, 1e-30]);
        while v.len() % 3 != 0 {
            v.push(0.5);
        }
        let px: Vec<[f32; 3]> = v.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        fn check<T: Copy + Default + PartialEq + std::fmt::Debug>(enc: &impl Encode<T, 3>, px: &[[f32; 3]]) {
            let mut run = vec![T::default(); px.len() * 3];
            enc.run(px, &mut run);
            let one: Vec<T> = px.iter().flat_map(|p| enc.px(*p)).collect();
            assert_eq!(run, one);
        }
        check(&Srgb16, &px);
        check(&Sample(linear16), &px);
        check(&Sample(f32_to_f16), &px);
        let lut = srgb16_lut();
        check(&Sample(|v: f32| srgb16(v, lut)), &px);
        for (p, e) in px.iter().zip(px.iter().map(|p| Srgb16.px(*p))) {
            assert_eq!(e, p.map(|v| srgb16(v, lut)), "{p:?}");
        }
    }

    #[test]
    fn srgb16_paired_matches_srgb16() {
        let (lut, pairs) = (srgb16_lut(), srgb16_pairs());
        for k in -10..=70_000 {
            let v = k as f32 / 65_536.0;
            assert_eq!(srgb16_paired(v, pairs), srgb16(v, lut), "{v}");
        }
    }

    #[test]
    fn srgb16_curve_is_accurate_and_monotonic() {
        let lut = srgb16_lut();
        let exact = |v: f64| {
            let s = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
            (s * 65535.0).round() as i64
        };
        let (mut prev, mut off) = (0u16, 0usize);
        let n = 1_000_000;
        for i in 0..=n {
            let v = i as f32 / n as f32;
            let got = srgb16(v, lut);
            assert!(got >= prev, "not monotonic at {v}");
            prev = got;
            let d = (got as i64 - exact(v as f64)).abs();
            assert!(d <= 1, "{v}: {got} vs {}", exact(v as f64));
            off += (d != 0) as usize;
        }
        // Interpolation error stays below 0.1 code: off by one only next to a rounding
        // boundary (measured: 0.19 % of samples).
        assert!(off < n / 200, "{off} samples off by one");
        assert_eq!((srgb16(-1.0, lut), srgb16(0.0, lut), srgb16(1.0, lut), srgb16(7.0, lut)), (0, 0, 65535, 65535));
        // The 8-bit curve is the same function.
        let l8 = srgb_lut();
        for i in 0..=4096 {
            let v = i as f32 / 4096.0;
            assert!((srgb16(v, lut) as f32 / 257.0 - l8[i] as f32).abs() <= 0.51, "{v}");
        }
    }

    #[test]
    fn half_floats_round_to_nearest_even_and_saturate() {
        // Nearest half by bisection over the bit patterns (monotonic for positive values).
        let half_ref = |x: f32| -> u16 {
            let s = if x.is_sign_negative() { 0x8000u16 } else { 0 };
            let a = (x as f64).abs();
            if a >= 65520.0 {
                return s | 0x7bff;
            }
            let (mut lo, mut hi) = (0u16, 0x7bffu16);
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                if f16_to_f32(mid) as f64 <= a { lo = mid } else { hi = mid }
            }
            let (dl, dh) = (a - f16_to_f32(lo) as f64, f16_to_f32(hi) as f64 - a);
            s | if dl < dh || (dl == dh && lo & 1 == 0) { lo } else { hi }
        };
        let mut x = 1u32;
        for i in 0..100_000u32 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let v = if i % 2 == 0 { f32::from_bits(x) } else { (x as f32 / u32::MAX as f32) * 4.0 - 1.0 };
            if v.is_nan() || v.is_infinite() {
                continue;
            }
            assert_eq!(f32_to_f16(v), half_ref(v), "{v:e}");
        }
        for v in [0.0f32, 1.0, -1.0, 0.5, 65504.0, 65519.0, 65520.0, 1e9, -1e9, 5.960_464_5e-8, 2.9e-8, 6.1e-5] {
            assert_eq!(f32_to_f16(v), half_ref(v), "{v:e}");
        }
        assert_eq!(f32_to_f16(f32::NAN) & 0x7e00, 0x7e00);
        // Every finite half converts back exactly.
        for h in 0..=0xffffu16 {
            let e = (h >> 10) & 0x1f;
            if e != 31 {
                assert_eq!(f32_to_f16(f16_to_f32(h)), h, "{h:#06x}");
            }
        }
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
        assert_eq!(f16_to_f32(0x8001), -(2f32.powi(-24)));
        assert!(f16_to_f32(0x7c00).is_infinite());
    }
}
