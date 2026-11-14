//! Rendering with a DNG camera profile ([`crate::CameraProfile`]).
//!
//! Implemented from the public DNG specification (chapter "Camera Profiles" and the
//! sections on mapping camera colour space to CIE XYZ and on applying the profile tables).
//! The input is the white balanced, clipped camera RGB that the develop pass produces with
//! an identity colour matrix; the output is linear RGB in the output colour space:
//!
//! 1. Camera RGB to XYZ (D50): the interpolated `ForwardMatrix` when the profile has one,
//!    else `ColorMatrix` inverted at the illuminant of the as-shot neutral and adapted to
//!    D50 with the Bradford transform. Interpolation between the two calibration
//!    illuminants is linear in reciprocal colour temperature, with the temperature of the
//!    neutral found by iterating the colour matrix.
//! 2. XYZ to linear ProPhoto RGB (the working space of the tables), scaled by
//!    `BaselineExposureOffset`.
//! 3. `ProfileHueSatMap` then `ProfileLookTable`: RGB to HSV (optionally on the sRGB-encoded
//!    values), trilinear lookup of `(hue shift, saturation scale, value scale)`, HSV to RGB.
//! 4. `ProfileToneCurve` on each channel.
//! 5. ProPhoto to the output colour space.
//!
//! Not applied: `DefaultBlackRender`, `BaselineExposure` of the file (the `exposure` option
//! controls brightness), the third calibration of a three-illuminant DNG.

use crate::color::{ColorSpace, Mat, bradford, inv, mul, mul_vec};
use crate::process::{Developed, Levels};
use flashraw_core::{CameraProfile, ColorMatrix, Error, Result, TableEncoding};

const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

fn det(m: &Mat) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

fn invertible(m: &Mat) -> Option<Mat> {
    let d = det(m);
    (d.is_finite() && d.abs() > 1e-9).then(|| inv(m))
}

fn to_mat(m: &ColorMatrix) -> Option<Mat> {
    (m.rows == 3 && m.values.len() >= 9)
        .then(|| std::array::from_fn(|i| std::array::from_fn(|j| m.values[i * 3 + j] as f64)))
}

/// McCamy's approximation of the correlated colour temperature of chromaticity `(x, y)`.
fn cct(x: f64, y: f64) -> f64 {
    let n = (x - 0.3320) / (0.1858 - y);
    449.0 * n * n * n + 3525.0 * n * n + 6823.3 * n + 5520.33
}

/// Weight of `items[0]` in a blend of the first two entries at temperature `t` (linear in
/// reciprocal temperature, clamped to the two calibrations). 1 for a single entry; 0.5 when
/// an illuminant is unknown.
pub(crate) fn weight(items: &[ColorMatrix], t: f64) -> f64 {
    if items.len() < 2 {
        return 1.0;
    }
    match (items[0].illuminant.temperature(), items[1].illuminant.temperature()) {
        (Some(a), Some(b)) if (a - b).abs() > 1.0 && t > 0.0 => {
            let (a, b) = (a as f64, b as f64);
            ((1.0 / t - 1.0 / b) / (1.0 / a - 1.0 / b)).clamp(0.0, 1.0)
        }
        _ => 0.5,
    }
}

/// The matrices of `items` (first two) blended at temperature `t`.
pub(crate) fn blend(items: &[ColorMatrix], t: f64) -> Option<Mat> {
    let a = to_mat(items.first()?)?;
    let Some(b) = items.get(1).and_then(to_mat) else { return Some(a) };
    let w = weight(items, t);
    Some(std::array::from_fn(|i| std::array::from_fn(|j| w * a[i][j] + (1.0 - w) * b[i][j])))
}

/// Correlated colour temperature of the camera neutral `n` (the camera response to white):
/// iterate colour matrix interpolation and `XYZ = M^-1 n`, as the DNG specification
/// describes. `None` without colour matrices or when the matrices are not invertible.
pub(crate) fn neutral_temperature(color: &[ColorMatrix], n: [f64; 3]) -> Option<f64> {
    let mut t = 5000.0;
    let mut last = None;
    for _ in 0..30 {
        let m = invertible(&blend(color, t)?)?;
        let xyz = mul_vec(&m, n);
        let sum = xyz[0] + xyz[1] + xyz[2];
        if !(sum.is_finite() && sum > 1e-9) {
            return last;
        }
        let next = cct(xyz[0] / sum, xyz[1] / sum).clamp(1800.0, 25000.0);
        if !next.is_finite() {
            return last;
        }
        if (next - t).abs() < 0.5 {
            return Some(next);
        }
        t = 0.5 * (t + next);
        last = Some(t);
    }
    last
}

/// A monotone cubic interpolation (Fritsch and Carlson) through ascending points.
struct Curve {
    x: Vec<f32>,
    y: Vec<f32>,
    m: Vec<f32>,
}

impl Curve {
    fn new(points: &[[f32; 2]]) -> Self {
        let n = points.len();
        let x: Vec<f32> = points.iter().map(|p| p[0]).collect();
        let y: Vec<f32> = points.iter().map(|p| p[1]).collect();
        let d: Vec<f32> = (0..n - 1).map(|k| (y[k + 1] - y[k]) / (x[k + 1] - x[k])).collect();
        let mut m = vec![0f32; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for k in 1..n - 1 {
            m[k] = if d[k - 1] * d[k] <= 0.0 { 0.0 } else { 0.5 * (d[k - 1] + d[k]) };
        }
        for k in 0..n - 1 {
            if d[k] == 0.0 {
                m[k] = 0.0;
                m[k + 1] = 0.0;
            } else {
                let (a, b) = (m[k] / d[k], m[k + 1] / d[k]);
                let r = a * a + b * b;
                if r > 9.0 {
                    let s = 3.0 / r.sqrt();
                    m[k] = s * a * d[k];
                    m[k + 1] = s * b * d[k];
                }
            }
        }
        Curve { x, y, m }
    }

    fn eval(&self, v: f32) -> f32 {
        let n = self.x.len();
        if v <= self.x[0] {
            return self.y[0];
        }
        if v >= self.x[n - 1] {
            return self.y[n - 1];
        }
        let k = self.x.partition_point(|&p| p <= v) - 1;
        let h = self.x[k + 1] - self.x[k];
        let t = (v - self.x[k]) / h;
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * self.y[k]
            + (t3 - 2.0 * t2 + t) * h * self.m[k]
            + (-2.0 * t3 + 3.0 * t2) * self.y[k + 1]
            + (t3 - t2) * h * self.m[k + 1]
    }
}

/// A hue/saturation/value table ready for lookup.
struct Table {
    dims: [usize; 3],
    data: Vec<f32>,
    srgb: bool,
}

impl Table {
    fn delta(&self, h: f32, s: f32, v: f32) -> [f32; 3] {
        let [hn, sn, vn] = self.dims;
        // Hue wraps: the last division interpolates to the first.
        let hf = (h / 6.0).rem_euclid(1.0) * hn as f32;
        let h0 = (hf.floor() as usize).min(hn - 1);
        let fh = hf - hf.floor();
        let h1 = (h0 + 1) % hn;
        let (s0, s1, fs) = split(s, sn);
        let (v0, v1, fv) = split(v, vn);
        let at = |vi: usize, hi: usize, si: usize| -> &[f32] {
            let i = ((vi * hn + hi) * sn + si) * 3;
            &self.data[i..i + 3]
        };
        let mut out = [0f32; 3];
        for (k, o) in out.iter_mut().enumerate() {
            let plane = |vi: usize| -> f32 {
                let a = at(vi, h0, s0)[k] * (1.0 - fs) + at(vi, h0, s1)[k] * fs;
                let b = at(vi, h1, s0)[k] * (1.0 - fs) + at(vi, h1, s1)[k] * fs;
                a * (1.0 - fh) + b * fh
            };
            *o = plane(v0) * (1.0 - fv) + plane(v1) * fv;
        }
        out
    }

    /// Apply to a linear ProPhoto colour in `[0, 1]`.
    fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let e = if self.srgb { c.map(srgb_encode) } else { c };
        let (h, s, v) = rgb_to_hsv(e);
        let [dh, ds, dv] = self.delta(h, s, v);
        let h = (h + dh / 60.0).rem_euclid(6.0);
        let s = (s * ds).clamp(0.0, 1.0);
        let v = (v * dv).clamp(0.0, 1.0);
        let o = hsv_to_rgb(h, s, v);
        if self.srgb { o.map(srgb_decode) } else { o }
    }
}

/// Division indices and fraction for `x` in `[0, 1]` over `n` divisions.
fn split(x: f32, n: usize) -> (usize, usize, f32) {
    if n < 2 {
        return (0, 0, 0.0);
    }
    let f = x.clamp(0.0, 1.0) * (n - 1) as f32;
    let i = (f.floor() as usize).min(n - 2);
    (i, i + 1, f - i as f32)
}

fn srgb_encode(x: f32) -> f32 {
    if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
}

fn srgb_decode(x: f32) -> f32 {
    if x <= 0.040_45 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}

/// RGB in `[0, 1]` to `(hue in [0, 6), saturation, value)`.
pub(crate) fn rgb_to_hsv(c: [f32; 3]) -> (f32, f32, f32) {
    let [r, g, b] = c;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if max <= 0.0 || d <= 0.0 {
        return (0.0, 0.0, max.max(0.0));
    }
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h, d / max, max)
}

/// `(hue in [0, 6), saturation, value)` to RGB.
pub(crate) fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(6.0);
    let i = (h.floor() as i32).min(5);
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// A camera profile resolved for one image: matrices at the image's illuminant, blended
/// tables and the output conversion.
pub(crate) struct ProfileRender {
    /// White balanced camera RGB to linear ProPhoto, including the exposure offset.
    cam_to_pp: [[f32; 3]; 3],
    /// Linear ProPhoto to the output colour space.
    pp_to_out: [[f32; 3]; 3],
    tables: Vec<Table>,
    tone: Option<Curve>,
    /// Whether any stage after the matrix needs values clamped to `[0, 1]`.
    clamp: bool,
}

impl ProfileRender {
    /// Resolve `profile` for a white balanced camera whose neutral (the camera response to
    /// white, the reciprocal of the white balance multipliers) is `neutral`. `file_color` are
    /// the colour matrices of the file, used when the profile has none. `out_from_srgb` takes
    /// linear sRGB to the output colour space.
    pub fn new(
        profile: &CameraProfile,
        file_color: &[ColorMatrix],
        neutral: [f64; 3],
        out_from_srgb: &Mat,
    ) -> Result<Self> {
        let color: &[ColorMatrix] = if profile.color_matrices.iter().any(|m| to_mat(m).is_some()) {
            &profile.color_matrices
        } else {
            file_color
        };
        let color: Vec<ColorMatrix> = color.iter().filter(|m| to_mat(m).is_some()).cloned().collect();
        let neutral = if neutral.iter().all(|v| v.is_finite() && *v > 1e-6) { neutral } else { [1.0; 3] };
        let temp = neutral_temperature(&color, neutral).unwrap_or(5000.0);

        let to_xyz: Mat = if let Some(fm) = blend(&profile.forward_matrices, temp) {
            fm
        } else if let Some(cm) = blend(&color, temp) {
            let cm_inv = invertible(&cm)
                .ok_or_else(|| Error::InvalidArgument("camera profile colour matrix is not invertible".into()))?;
            let w = mul_vec(&cm_inv, neutral);
            if !(w[1].is_finite() && w[1] > 1e-9) {
                return Err(Error::InvalidArgument("camera profile colour matrix has no usable white point".into()));
            }
            let white = [w[0] / w[1], 1.0, w[2] / w[1]];
            let scale = 1.0 / w[1];
            let diag: Mat =
                [[neutral[0] * scale, 0.0, 0.0], [0.0, neutral[1] * scale, 0.0], [0.0, 0.0, neutral[2] * scale]];
            mul(&bradford(white, D50), &mul(&cm_inv, &diag))
        } else {
            return Err(Error::InvalidArgument(
                "the camera profile and the file have no colour matrices to map the camera to XYZ".into(),
            ));
        };

        let pp = ColorSpace::ProPhoto.colorants_d50().expect("ProPhoto");
        let pp_inv = invertible(&pp).expect("ProPhoto primaries");
        let gain = 2f64.powf(profile.baseline_exposure_offset.clamp(-32.0, 32.0));
        let cam_to_pp = mul(&pp_inv, &to_xyz).map(|r| r.map(|v| v * gain));

        let srgb = ColorSpace::Srgb.colorants_d50().expect("sRGB");
        // ProPhoto -> XYZ D50 -> linear sRGB -> output.
        let pp_to_out = mul(out_from_srgb, &mul(&invertible(&srgb).expect("sRGB primaries"), &pp));

        let mut tables = Vec::new();
        if let Some(t) = &profile.hue_sat_map {
            let dims = t.dims.map(|d| d as usize);
            let data = match &t.data2 {
                Some(d2) => {
                    // The tables belong to the profile's own calibration illuminants.
                    let ills = if profile.color_matrices.len() >= 2 {
                        &profile.color_matrices
                    } else {
                        &profile.forward_matrices
                    };
                    let w = weight(ills, temp) as f32;
                    t.data.iter().zip(d2).map(|(a, b)| w * a + (1.0 - w) * b).collect()
                }
                None => t.data.clone(),
            };
            tables.push(Table { dims, data, srgb: t.encoding == TableEncoding::Srgb });
        }
        if let Some(t) = &profile.look_table {
            tables.push(Table {
                dims: t.dims.map(|d| d as usize),
                data: t.data.clone(),
                srgb: t.encoding == TableEncoding::Srgb,
            });
        }
        let tone = profile.tone_curve.as_deref().filter(|c| c.len() >= 2).map(Curve::new);
        let clamp = !tables.is_empty() || tone.is_some();
        let f32m = |m: Mat| -> [[f32; 3]; 3] { m.map(|r| r.map(|v| v as f32)) };
        Ok(ProfileRender { cam_to_pp: f32m(cam_to_pp), pp_to_out: f32m(pp_to_out), tables, tone, clamp })
    }

    /// One pixel: white balanced camera RGB to linear RGB of the output colour space.
    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let m = &self.cam_to_pp;
        let mut c = [
            m[0][0] * rgb[0] + m[0][1] * rgb[1] + m[0][2] * rgb[2],
            m[1][0] * rgb[0] + m[1][1] * rgb[1] + m[1][2] * rgb[2],
            m[2][0] * rgb[0] + m[2][1] * rgb[1] + m[2][2] * rgb[2],
        ];
        if self.clamp {
            c = c.map(|v| v.clamp(0.0, 1.0));
            for t in &self.tables {
                c = t.apply(c);
            }
            if let Some(tc) = &self.tone {
                c = c.map(|v| tc.eval(v.clamp(0.0, 1.0)));
            }
        }
        let m = &self.pp_to_out;
        [
            m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
            m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
            m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
        ]
    }

    /// Convert a developed image (white balanced camera RGB, identity colour matrix) in place
    /// and apply the exposure / black / white points.
    pub fn apply_image(&self, d: &mut Developed, levels: Option<Levels>, parallel: bool) {
        const PIXELS: usize = 1024;
        flashraw_core::par::for_each_row_chunk(&mut d.rgb, PIXELS * 3, 4, parallel, |_, chunk| {
            for p in chunk.chunks_exact_mut(3) {
                let mut o = self.apply([p[0], p[1], p[2]]);
                if let Some(l) = levels {
                    for v in &mut o {
                        *v = *v * l.scale - l.offset;
                    }
                }
                p.copy_from_slice(&o);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: Mat = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    use flashraw_core::{Illuminant, ProfileTable};

    fn cm(ill: Illuminant, v: [f32; 9]) -> ColorMatrix {
        ColorMatrix::new(ill, 3, v.to_vec()).unwrap()
    }

    /// Forward matrix under which white balanced camera RGB is already linear ProPhoto.
    fn prophoto_forward(ill: Illuminant) -> ColorMatrix {
        let pp = ColorSpace::ProPhoto.colorants_d50().unwrap();
        ColorMatrix::new(ill, 3, pp.iter().flatten().map(|&v| v as f32).collect()).unwrap()
    }

    /// A profile in which camera RGB is ProPhoto and the output is ProPhoto too: without
    /// tables the whole pipeline is the identity, so table effects can be computed by hand.
    fn identity_render(mut p: CameraProfile) -> ProfileRender {
        p.forward_matrices = vec![prophoto_forward(Illuminant::D65)];
        ProfileRender::new(&p, &[], [1.0; 3], &ColorSpace::ProPhoto.matrix_from_srgb()).unwrap()
    }

    fn close(a: [f32; 3], b: [f32; 3], tol: f32) {
        assert!((0..3).all(|i| (a[i] - b[i]).abs() <= tol), "{a:?} vs {b:?}");
    }

    fn table(dims: [u32; 3], data: Vec<f32>) -> ProfileTable {
        ProfileTable::new(dims, data, TableEncoding::Linear).unwrap()
    }

    #[test]
    fn matrix_only_profile_is_the_identity_in_its_working_space() {
        let r = identity_render(CameraProfile::default());
        for px in [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.6, 0.3, 0.2], [0.05, 0.9, 0.4]] {
            close(r.apply(px), px, 1e-5);
        }
    }

    #[test]
    fn identity_tables_leave_pixels_unchanged() {
        let ident = |dims: [u32; 3]| {
            let n = (dims[0] * dims[1] * dims[2]) as usize;
            table(dims, (0..n).flat_map(|_| [0.0, 1.0, 1.0]).collect())
        };
        let mut p = CameraProfile::default();
        p.hue_sat_map = Some(ident([6, 4, 2]));
        p.look_table = Some(ident([3, 3, 5]));
        let r = identity_render(p.clone());
        let mut srgb = p;
        srgb.hue_sat_map.as_mut().unwrap().encoding = TableEncoding::Srgb;
        let rs = identity_render(srgb);
        for px in
            [[0.6, 0.3, 0.2], [0.05, 0.9, 0.4], [0.2, 0.2, 0.2], [0.0, 0.0, 0.0], [1.0, 0.5, 0.0], [0.3, 0.1, 0.9]]
        {
            close(r.apply(px), px, 2e-5);
            close(rs.apply(px), px, 2e-4);
        }
    }

    #[test]
    fn hue_shift_and_saturation_scale_match_the_hand_computation() {
        // One table entry: shift hue by +60 degrees, halve the saturation.
        let mut p = CameraProfile::default();
        p.hue_sat_map = Some(table([1, 1, 1], vec![60.0, 0.5, 1.0]));
        let r = identity_render(p);
        // (0.6, 0.3, 0.2): V = 0.6, S = 0.4/0.6, H = 0.25 (sixths of 60 degrees).
        // New H = 1.25, S = 1/3, V = 0.6 -> sector 1: (V (1 - S f), V, V (1 - S)).
        let got = r.apply([0.6, 0.3, 0.2]);
        close(got, [0.6 * (1.0 - 0.25 / 3.0), 0.6, 0.4], 1e-5);
        // Greys have no saturation to scale and stay put.
        close(r.apply([0.5, 0.5, 0.5]), [0.5, 0.5, 0.5], 1e-6);
    }

    #[test]
    fn value_scale_multiplies_value() {
        let mut p = CameraProfile::default();
        p.look_table = Some(table([1, 1, 1], vec![0.0, 1.0, 0.5]));
        let r = identity_render(p);
        close(r.apply([0.6, 0.3, 0.2]), [0.3, 0.15, 0.1], 1e-5);
    }

    #[test]
    fn hue_interpolation_wraps_around() {
        // Four hue divisions at 0, 90, 180, 270 degrees; only the last shifts.
        let mut d = vec![0f32; 4 * 3];
        for i in 0..4 {
            d[i * 3 + 1] = 1.0;
            d[i * 3 + 2] = 1.0;
        }
        d[3 * 3] = 60.0;
        let t = Table { dims: [4, 1, 1], data: d, srgb: false };
        // Hue 5.5 sixths = 330 degrees, two thirds of the way from division 3 to division 0.
        let dl = t.delta(5.5, 0.5, 0.5);
        assert!((dl[0] - 60.0 * (1.0 - (5.5 / 6.0 * 4.0 - 3.0))).abs() < 1e-4, "{dl:?}");
        // Exactly on the first division: unshifted.
        assert!(t.delta(0.0, 0.5, 0.5)[0].abs() < 1e-6);
        // Just below 360 degrees is almost the unshifted first division again.
        assert!(t.delta(5.9999, 0.5, 0.5)[0] < 1e-2);
        // A shift past red wraps the result's hue: red (H 0) shifted by -30 degrees is H 5.5.
        let mut p = CameraProfile::default();
        p.hue_sat_map = Some(table([1, 1, 1], vec![-30.0, 1.0, 1.0]));
        let r = identity_render(p);
        close(r.apply([1.0, 0.0, 0.0]), hsv_to_rgb(5.5, 1.0, 1.0), 1e-5);
        close(r.apply([1.0, 0.0, 0.0]), [1.0, 0.0, 0.5], 1e-5);
    }

    #[test]
    fn table_lookup_is_trilinear() {
        // 1 hue, 2 sat, 2 val divisions: the scale grows with saturation and value.
        let mut d = Vec::new();
        for v in 0..2 {
            for s in 0..2 {
                d.extend([0.0, 1.0 + s as f32 + 2.0 * v as f32, 1.0]);
            }
        }
        let t = Table { dims: [1, 2, 2], data: d, srgb: false };
        assert!((t.delta(1.0, 0.0, 0.0)[1] - 1.0).abs() < 1e-6);
        assert!((t.delta(1.0, 1.0, 0.0)[1] - 2.0).abs() < 1e-6);
        assert!((t.delta(1.0, 0.0, 1.0)[1] - 3.0).abs() < 1e-6);
        assert!((t.delta(1.0, 1.0, 1.0)[1] - 4.0).abs() < 1e-6);
        assert!((t.delta(1.0, 0.5, 0.5)[1] - 2.5).abs() < 1e-6);
    }

    #[test]
    fn tone_curve_maps_through_its_points_and_is_monotonic() {
        let mut p = CameraProfile::default();
        p.tone_curve = Some(vec![[0.0, 0.0], [0.1, 0.3], [0.5, 0.7], [0.9, 0.95], [1.0, 1.0]]);
        let r = identity_render(p);
        for (x, y) in [(0.0, 0.0), (0.1, 0.3), (0.5, 0.7), (0.9, 0.95), (1.0, 1.0)] {
            close(r.apply([x, x, x]), [y, y, y], 1e-5);
        }
        let mut last = -1.0;
        for i in 0..=1000 {
            let x = i as f32 / 1000.0;
            let y = r.apply([x, x, x])[0];
            assert!(y >= last - 1e-6, "not monotonic at {x}: {y} < {last}");
            assert!((0.0..=1.0 + 1e-6).contains(&y));
            last = y;
        }
        // Values above 1 clamp to the last point.
        close(r.apply([4.0, 4.0, 4.0]), [1.0; 3], 1e-5);
    }

    #[test]
    fn baseline_exposure_offset_scales_linear_values() {
        let mut p = CameraProfile::default();
        p.baseline_exposure_offset = 1.0;
        let r = identity_render(p);
        close(r.apply([0.1, 0.2, 0.3]), [0.2, 0.4, 0.6], 1e-5);
    }

    #[test]
    fn forward_matrix_interpolation_endpoints() {
        let a = cm(Illuminant::STANDARD_A, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        let d = cm(Illuminant::D65, [2.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 2.0]);
        let both = [a, d];
        let at = |t: f64| blend(&both, t).unwrap()[0][0];
        assert!((at(2000.0) - 1.0).abs() < 1e-12, "warmer than A -> A");
        assert!((at(2856.0) - 1.0).abs() < 1e-12);
        assert!((at(6500.0) - 2.0).abs() < 1e-12);
        assert!((at(9000.0) - 2.0).abs() < 1e-12, "cooler than D65 -> D65");
        // In reciprocal temperature: 4000 K sits at (1/4000 - 1/6500) / (1/2856 - 1/6500) of A.
        let w = (1.0 / 4000.0 - 1.0 / 6500.0) / (1.0 / 2856.0 - 1.0 / 6500.0);
        assert!((at(4000.0) - (w + 2.0 * (1.0 - w))).abs() < 1e-9);
        assert!((weight(&both, 4000.0) - w).abs() < 1e-12);
    }

    #[test]
    fn neutral_temperature_finds_the_illuminant() {
        // Identity colour matrices: the neutral is the XYZ of the illuminant.
        let id = |ill| cm(ill, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        let color = [id(Illuminant::STANDARD_A), id(Illuminant::D65)];
        let xyz = |x: f64, y: f64| [x / y, 1.0, (1.0 - x - y) / y];
        let t65 = neutral_temperature(&color, xyz(0.3127, 0.3290)).unwrap();
        assert!((t65 - 6500.0).abs() < 100.0, "{t65}");
        let ta = neutral_temperature(&color, xyz(0.4476, 0.4074)).unwrap();
        assert!((ta - 2856.0).abs() < 100.0, "{ta}");
    }

    #[test]
    fn forward_matrix_selection_follows_the_neutral() {
        // Camera = XYZ-ish identity; forward matrices differ by illuminant. A tungsten
        // neutral must pick the first forward matrix, a daylight neutral the second.
        let id = |ill| cm(ill, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        let mut p = CameraProfile::default();
        p.color_matrices = vec![id(Illuminant::STANDARD_A), id(Illuminant::D65)];
        p.forward_matrices = vec![
            cm(Illuminant::STANDARD_A, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
            cm(Illuminant::D65, [0.5, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.5]),
        ];
        let xyz = |x: f64, y: f64| [x / y, 1.0, (1.0 - x - y) / y];
        let out = ColorSpace::Srgb.matrix_from_srgb();
        let warm = ProfileRender::new(&p, &[], xyz(0.4476, 0.4074), &out).unwrap();
        let day = ProfileRender::new(&p, &[], xyz(0.3127, 0.3290), &out).unwrap();
        let (w, d) = (warm.apply([0.2, 0.2, 0.2])[1], day.apply([0.2, 0.2, 0.2])[1]);
        assert!(w > d * 1.8, "tungsten {w} vs daylight {d}");
    }

    #[test]
    fn colour_matrix_path_maps_the_neutral_to_white() {
        // No forward matrix: ColorMatrix inverse plus Bradford. A camera whose neutral is
        // (0.5, 1, 0.7) must render white balanced (1, 1, 1) as D50 white, i.e. sRGB white.
        let mut p = CameraProfile::default();
        p.color_matrices = vec![cm(Illuminant::D65, [0.9, 0.1, 0.0, -0.2, 1.1, 0.1, 0.0, 0.1, 0.6])];
        let neutral = {
            let m = [[0.9, 0.1, 0.0], [-0.2, 1.1, 0.1], [0.0, 0.1, 0.6]];
            mul_vec(&m, [0.9505, 1.0, 1.089])
        };
        let r = ProfileRender::new(&p, &[], neutral, &IDENTITY).unwrap();
        let white = r.apply([1.0, 1.0, 1.0]);
        // The matrix is calibrated at D65, so the neutral's XYZ is D65 and Bradford takes it to D50.
        close(white, [1.0, 1.0, 1.0], 2e-3);
    }

    #[test]
    fn missing_matrices_are_an_error() {
        let p = CameraProfile::default();
        assert!(ProfileRender::new(&p, &[], [1.0; 3], &IDENTITY).is_err());
        let file = [cm(Illuminant::D65, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])];
        assert!(ProfileRender::new(&p, &file, [1.0; 3], &IDENTITY).is_ok());
    }

    #[test]
    fn hsv_round_trip() {
        for px in [[0.6, 0.3, 0.2], [0.1, 0.9, 0.4], [0.3, 0.3, 0.8], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.2, 0.2, 0.9]]
        {
            let (h, s, v) = rgb_to_hsv(px);
            close(hsv_to_rgb(h, s, v), px, 1e-6);
        }
    }
}
