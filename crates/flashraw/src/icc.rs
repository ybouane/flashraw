//! Output profiles: resolving the colour management request of a decode into develop
//! parameters, and arbitrary ICC profiles (cargo feature `icc`).
//!
//! A built-in [`ColorSpace`] and a matrix/TRC ICC profile are both folded into the develop
//! pass (one 3x3 matrix and a table per channel), so they cost nothing per pixel. A
//! profile with lookup tables (A2B/B2A) cannot be folded: the image is developed to linear
//! ProPhoto RGB, converted by a second parallel pass, then quantised. That is slower and
//! needs a float image in memory; see `docs/color-management.md`.

use crate::color::{ColorSpace, OutputCurve};
use crate::options::{DecodeOptions, OutputFormat};
use crate::process::{DevelopParams, RgbEncoding};
use flashraw_core::{Error, Result};
use std::sync::Arc;

/// ICC rendering intent of a profile conversion (only profiles with lookup tables choose
/// between them; matrix/TRC profiles convert the same way for every intent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum RenderingIntent {
    /// Perceptual: the profile's `A2B0`/`B2A0` tables (the usual default for photographs).
    #[default]
    Perceptual,
    /// Relative colorimetric: white point mapped to the white of the output medium.
    RelativeColorimetric,
    /// Saturation: the profile's `B2A2` table.
    Saturation,
    /// Absolute colorimetric: whites are not adapted (proofing).
    AbsoluteColorimetric,
}

impl RenderingIntent {
    /// Parse an intent name: `perceptual`, `relative`/`relative-colorimetric`, `saturation`,
    /// `absolute`/`absolute-colorimetric` (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "perceptual" | "p" => RenderingIntent::Perceptual,
            "relative" | "relative-colorimetric" | "rel" | "r" => RenderingIntent::RelativeColorimetric,
            "saturation" | "s" => RenderingIntent::Saturation,
            "absolute" | "absolute-colorimetric" | "abs" | "a" => RenderingIntent::AbsoluteColorimetric,
            _ => return None,
        })
    }

    /// Canonical name, accepted by [`RenderingIntent::parse`].
    pub fn name(self) -> &'static str {
        match self {
            RenderingIntent::Perceptual => "perceptual",
            RenderingIntent::RelativeColorimetric => "relative",
            RenderingIntent::Saturation => "saturation",
            RenderingIntent::AbsoluteColorimetric => "absolute",
        }
    }
}

/// An ICC profile to convert RGB outputs to ([`DecodeOptions::output_profile`]).
///
/// Needs the `icc` cargo feature; a decode with a profile fails with
/// [`Error::InvalidArgument`] in a build without it, and for profiles that are not RGB
/// output profiles or are malformed. Matrix/TRC profiles (most display profiles) are folded
/// into the develop pass; profiles with lookup tables convert in a second pass.
#[derive(Clone)]
pub struct OutputProfile {
    bytes: Arc<[u8]>,
    /// Rendering intent for profiles with lookup tables. Default: perceptual.
    pub intent: RenderingIntent,
    /// Black point compensation for profiles with lookup tables: the darkest colour of the
    /// image maps to the darkest colour of the profile (applied in linear light). No effect
    /// on matrix/TRC profiles, whose black is zero. Default: false.
    pub black_point_compensation: bool,
    /// Tests: run matrix profiles through the second pass.
    #[cfg(test)]
    force_lut: bool,
}

impl OutputProfile {
    /// A profile from its ICC bytes (validated when the decode starts).
    pub fn new(icc: impl Into<Arc<[u8]>>) -> Self {
        OutputProfile {
            bytes: icc.into(),
            intent: RenderingIntent::Perceptual,
            black_point_compensation: false,
            #[cfg(test)]
            force_lut: false,
        }
    }

    /// Set the rendering intent.
    pub fn with_intent(mut self, intent: RenderingIntent) -> Self {
        self.intent = intent;
        self
    }

    /// Enable or disable black point compensation.
    pub fn with_black_point_compensation(mut self, on: bool) -> Self {
        self.black_point_compensation = on;
        self
    }

    /// The ICC profile bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[cfg(all(test, feature = "icc"))]
    fn force_lut_for_test(&self) -> bool {
        self.force_lut
    }

    #[cfg(all(not(test), feature = "icc"))]
    fn force_lut_for_test(&self) -> bool {
        false
    }
}

impl std::fmt::Debug for OutputProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputProfile")
            .field("bytes", &self.bytes.len())
            .field("intent", &self.intent)
            .field("black_point_compensation", &self.black_point_compensation)
            .finish()
    }
}

/// The resolved colour management of one decode.
pub(crate) struct OutputSetup {
    /// What the frame reports.
    pub color_space: ColorSpace,
    kind: Kind,
}

#[cfg_attr(not(feature = "icc"), allow(dead_code))]
enum Kind {
    Builtin(ColorSpace),
    /// Matrix/TRC profile: folded into the develop pass.
    Matrix(MatrixPlan),
    /// Profile with lookup tables: second pass.
    Lut(LutPlan),
}

#[cfg_attr(not(feature = "icc"), allow(dead_code))]
pub(crate) struct MatrixPlan {
    /// Linear sRGB → linear profile RGB.
    matrix: [[f64; 3]; 3],
    curve: OutputCurve,
    profile: Arc<[u8]>,
    /// The same primaries with a linear curve.
    linear: Arc<[u8]>,
}

impl OutputSetup {
    /// Built-in colour space (`Custom` is rejected).
    pub fn builtin(cs: ColorSpace) -> Result<Self> {
        if cs == ColorSpace::Custom {
            return Err(Error::InvalidArgument(
                "ColorSpace::Custom describes a profile; set `output_profile` instead".into(),
            ));
        }
        Ok(OutputSetup { color_space: cs, kind: Kind::Builtin(cs) })
    }

    /// Resolve the options (sRGB, a built-in space, or a validated profile).
    pub fn resolve(opts: &DecodeOptions) -> Result<Self> {
        match &opts.output_profile {
            Some(p) => plan(p),
            None => Self::builtin(opts.color_space),
        }
    }

    /// Fold the output matrix into `params` and select its curve.
    pub fn apply(&self, params: DevelopParams) -> DevelopParams {
        match &self.kind {
            Kind::Builtin(cs) => params.with_color_space(*cs),
            Kind::Matrix(m) => params.with_output(m.matrix, m.curve.clone()),
            // Developed wide, converted afterwards.
            Kind::Lut(_) => params.with_color_space(ColorSpace::ProPhoto),
        }
    }

    /// The second-pass converter, for profiles with lookup tables.
    pub fn lut(&self) -> Option<&LutPlan> {
        match &self.kind {
            Kind::Lut(l) => Some(l),
            _ => None,
        }
    }

    /// ICC profile describing the output samples (display profile for the display
    /// outputs, linear-curve variant for the linear ones).
    pub fn profile_for(&self, output: OutputFormat) -> Option<Arc<[u8]>> {
        let enc = output.rgb_encoding()?;
        let display = matches!(enc, RgbEncoding::Rgba8 | RgbEncoding::Srgb16);
        Some(match &self.kind {
            Kind::Builtin(cs) => (if display { cs.icc_profile() } else { cs.icc_profile_linear() }).into(),
            Kind::Matrix(m) => {
                if display {
                    m.profile.clone()
                } else {
                    m.linear.clone()
                }
            }
            Kind::Lut(l) => l.profile.clone(),
        })
    }
}

/// A profile converted by the colour engine in a second pass (see the module docs).
#[cfg_attr(not(feature = "icc"), allow(dead_code))]
pub(crate) struct LutPlan {
    profile: Arc<[u8]>,
    #[cfg(feature = "icc")]
    imp: imp::LutTransform,
}

#[cfg(feature = "icc")]
impl LutPlan {
    /// Convert developed linear ProPhoto RGB to the profile's device values in `[0, 1]`.
    pub fn apply(&self, d: &mut crate::process::Developed, parallel: bool) -> Result<()> {
        self.imp.apply(d, parallel)
    }
}

#[cfg(not(feature = "icc"))]
fn no_icc() -> Error {
    Error::InvalidArgument("built without ICC support (enable the `icc` cargo feature)".into())
}

#[cfg(not(feature = "icc"))]
fn plan(_p: &OutputProfile) -> Result<OutputSetup> {
    Err(no_icc())
}

#[cfg(feature = "icc")]
fn plan(p: &OutputProfile) -> Result<OutputSetup> {
    imp::plan(p)
}

/// True when this build can use arbitrary ICC profiles (cargo feature `icc`).
pub const fn icc_supported() -> bool {
    cfg!(feature = "icc")
}

#[cfg(feature = "icc")]
mod imp {
    use super::*;
    use crate::color::{CurveTables, IccCurve, IccProfileSpec, write_matrix_profile};
    use moxcms::{
        ColorProfile, DataColorSpace, Layout, ProfileClass, ToneReprCurve, TransformExecutor, TransformOptions,
    };

    pub struct LutTransform {
        transform: Arc<dyn TransformExecutor<f32> + Send + Sync>,
        /// Black point compensation: linear ProPhoto value of the profile's black.
        black: Option<[f32; 3]>,
    }

    fn bad(e: impl std::fmt::Display) -> Error {
        Error::InvalidArgument(format!("invalid output profile: {e}"))
    }

    fn to_intent(i: RenderingIntent) -> moxcms::RenderingIntent {
        match i {
            RenderingIntent::Perceptual => moxcms::RenderingIntent::Perceptual,
            RenderingIntent::RelativeColorimetric => moxcms::RenderingIntent::RelativeColorimetric,
            RenderingIntent::Saturation => moxcms::RenderingIntent::Saturation,
            RenderingIntent::AbsoluteColorimetric => moxcms::RenderingIntent::AbsoluteColorimetric,
        }
    }

    /// Run `f`, turning a panic inside the parser into an error (defence in depth: the
    /// profile is untrusted input).
    fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
            .unwrap_or_else(|_| Err(Error::InvalidArgument("invalid output profile: malformed data".into())))
    }

    pub fn plan(p: &OutputProfile) -> Result<OutputSetup> {
        guarded(|| plan_inner(p))
    }

    fn plan_inner(p: &OutputProfile) -> Result<OutputSetup> {
        let bytes = p.bytes();
        if bytes.len() < 132 {
            return Err(bad("too short for an ICC profile"));
        }
        let prof = ColorProfile::new_from_slice(bytes).map_err(bad)?;
        if prof.color_space != DataColorSpace::Rgb {
            return Err(bad("not an RGB profile"));
        }
        if !matches!(
            prof.profile_class,
            ProfileClass::DisplayDevice | ProfileClass::ColorSpace | ProfileClass::OutputDevice
        ) {
            return Err(bad("not a display, colour space or output profile"));
        }
        let has_lut = prof.lut_b_to_a_perceptual.is_some()
            || prof.lut_b_to_a_colorimetric.is_some()
            || prof.lut_b_to_a_saturation.is_some()
            || cfg!(test) && p.force_lut_for_test();
        let profile: Arc<[u8]> = bytes.into();
        if !has_lut {
            if prof.pcs != DataColorSpace::Xyz {
                return Err(bad("matrix profile with a Lab connection space"));
            }
            return matrix_plan(&prof, profile);
        }
        lut_plan(&prof, profile, p)
    }

    fn matrix_plan(prof: &ColorProfile, profile: Arc<[u8]>) -> Result<OutputSetup> {
        let col = [prof.red_colorant, prof.green_colorant, prof.blue_colorant];
        let c: [[f64; 3]; 3] =
            [[col[0].x, col[1].x, col[2].x], [col[0].y, col[1].y, col[2].y], [col[0].z, col[1].z, col[2].z]];
        let finite = c.iter().flatten().all(|v| v.is_finite() && v.abs() < 8.0);
        let det = c[0][0] * (c[1][1] * c[2][2] - c[1][2] * c[2][1]) - c[0][1] * (c[1][0] * c[2][2] - c[1][2] * c[2][0])
            + c[0][2] * (c[1][0] * c[2][1] - c[1][1] * c[2][0]);
        if !finite || det.abs() < 1e-6 {
            return Err(bad("degenerate colorant matrix"));
        }
        let trc = [&prof.red_trc, &prof.green_trc, &prof.blue_trc];
        let [Some(r), Some(g), Some(b)] = trc else { return Err(bad("missing tone curves")) };
        // Encoder tables (device value for a linear value); one set when the curves match.
        let curve = if r == g && g == b {
            OutputCurve::Owned(Arc::new(encode_tables(r)?))
        } else {
            OutputCurve::PerChannel(Arc::new([encode_tables(r)?, encode_tables(g)?, encode_tables(b)?]))
        };
        let src = ColorSpace::Srgb.colorants_d50().expect("sRGB");
        let matrix = mul3(&inv3(&c), &src);
        let linear: Arc<[u8]> = write_matrix_profile(&IccProfileSpec {
            description: "FlashRaw linear (output profile primaries)",
            colorants: c,
            adapt: None,
            curves: [IccCurve::Linear; 3],
        })
        .into();
        Ok(OutputSetup {
            color_space: ColorSpace::Custom,
            kind: Kind::Matrix(MatrixPlan { matrix, curve, profile, linear }),
        })
    }

    fn mul3(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
        std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
    }

    fn inv3(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
        let [[a, b, c], [d, e, f], [g, h, i]] = *m;
        let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
        let s = 1.0 / det;
        [
            [(e * i - f * h) * s, (c * h - b * i) * s, (b * f - c * e) * s],
            [(f * g - d * i) * s, (a * i - c * g) * s, (c * d - a * f) * s],
            [(d * h - e * g) * s, (b * g - a * h) * s, (a * e - b * d) * s],
        ]
    }

    /// Encoder tables from a profile's decoding curve (device → linear), inverted.
    fn encode_tables(curve: &ToneReprCurve) -> Result<CurveTables> {
        const S: usize = 8192;
        let ev = curve.make_linear_evaluator().map_err(bad)?;
        // Decode curve sampled on a fine device grid, forced monotonic.
        let mut dec: Vec<f64> = (0..=S).map(|j| ev.evaluate_value(j as f32 / S as f32) as f64).collect();
        if dec.iter().any(|v| !v.is_finite()) {
            return Err(bad("tone curve is not finite"));
        }
        for j in 1..dec.len() {
            if dec[j] < dec[j - 1] {
                dec[j] = dec[j - 1];
            }
        }
        let (lo, hi) = (dec[0], dec[S]);
        if hi - lo < 1e-3 {
            return Err(bad("tone curve is flat"));
        }
        let enc = |l: f64| -> f64 {
            // Inverse by bisection on the monotone samples + linear interpolation.
            if l <= lo {
                return 0.0;
            }
            if l >= hi {
                return 1.0;
            }
            let k = dec.partition_point(|&v| v < l).clamp(1, S);
            let (a, b) = (dec[k - 1], dec[k]);
            let t = if b > a { (l - a) / (b - a) } else { 0.0 };
            ((k - 1) as f64 + t) / S as f64
        };
        Ok(CurveTables::build(enc))
    }

    fn lut_plan(prof: &ColorProfile, profile: Arc<[u8]>, p: &OutputProfile) -> Result<OutputSetup> {
        let src = ColorProfile::new_from_slice(&ColorSpace::ProPhoto.icc_profile_linear()).map_err(bad)?;
        let opts = TransformOptions { rendering_intent: to_intent(p.intent), ..Default::default() };
        let transform = src.create_transform_f32(Layout::Rgb, prof, Layout::Rgb, opts).map_err(bad)?;
        // Probe: the conversion must run (rejects tables the engine cannot evaluate).
        let mut probe = [0f32; 3];
        transform.transform(&[0.18, 0.18, 0.18], &mut probe).map_err(bad)?;
        if probe.iter().any(|v| !v.is_finite()) {
            return Err(bad("profile conversion is not finite"));
        }
        let black = if p.black_point_compensation {
            // The profile's darkest colour, expressed as linear ProPhoto.
            let back = prof.create_transform_f32(Layout::Rgb, &src, Layout::Rgb, opts).map_err(bad)?;
            let mut b = [0f32; 3];
            back.transform(&[0.0, 0.0, 0.0], &mut b).map_err(bad)?;
            Some(b.map(|v| if v.is_finite() { v.clamp(0.0, 0.5) } else { 0.0 }))
        } else {
            None
        };
        Ok(OutputSetup {
            color_space: ColorSpace::Custom,
            kind: Kind::Lut(LutPlan { profile, imp: LutTransform { transform, black } }),
        })
    }

    impl LutTransform {
        pub fn apply(&self, d: &mut crate::process::Developed, parallel: bool) -> Result<()> {
            let rl = d.width as usize * 3;
            let failed = std::sync::atomic::AtomicBool::new(false);
            flashraw_core::par::for_each_row_chunk(&mut d.rgb, rl, 16, parallel, |_, chunk| {
                let mut tmp = chunk.to_vec();
                for v in tmp.iter_mut() {
                    *v = v.clamp(0.0, 1.0);
                }
                if let Some(b) = self.black {
                    for px in tmp.chunks_exact_mut(3) {
                        for c in 0..3 {
                            px[c] = b[c] + px[c] * (1.0 - b[c]);
                        }
                    }
                }
                if self.transform.transform(&tmp, chunk).is_err() {
                    failed.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            });
            if failed.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(Error::InvalidArgument("output profile conversion failed".into()));
            }
            Ok(())
        }
    }
}

#[cfg(all(test, feature = "icc"))]
mod tests {
    use super::*;
    use moxcms::{ColorProfile, DataColorSpace, ProfileClass, ToneReprCurve};

    fn plan_of(bytes: Vec<u8>) -> Result<OutputSetup> {
        plan(&OutputProfile::new(bytes))
    }

    #[test]
    fn written_profiles_parse_with_the_right_primaries_and_curves() {
        for cs in ColorSpace::ALL {
            for linear in [false, true] {
                let bytes = if linear { cs.icc_profile_linear() } else { cs.icc_profile() };
                let p = ColorProfile::new_from_slice(&bytes).unwrap();
                assert_eq!(
                    (p.color_space, p.pcs, p.profile_class),
                    (DataColorSpace::Rgb, DataColorSpace::Xyz, ProfileClass::DisplayDevice)
                );
                // Colorants are the D50-adapted primaries (s15Fixed16 precision).
                let c = cs.colorants_d50().unwrap();
                for (j, col) in [p.red_colorant, p.green_colorant, p.blue_colorant].iter().enumerate() {
                    for (i, v) in [col.x, col.y, col.z].iter().enumerate() {
                        assert!((v - c[i][j]).abs() < 3e-5, "{cs:?} colorant {i}{j}");
                    }
                }
                // Equal curves, evaluating to the space's decoding function (or the identity).
                assert!(p.red_trc == p.green_trc && p.green_trc == p.blue_trc);
                let ev = p.red_trc.as_ref().unwrap().make_linear_evaluator().unwrap();
                for k in 0..=64 {
                    let v = k as f64 / 64.0;
                    let want = if linear { v } else { cs.decode(v) };
                    assert!((ev.evaluate_value(v as f32) as f64 - want).abs() < 2e-4, "{cs:?} linear={linear} v={v}");
                }
                if cs == ColorSpace::AdobeRgb && !linear {
                    assert_eq!(p.red_trc, Some(ToneReprCurve::Lut(vec![563])));
                }
                let d = p.description.as_ref().map(|d| format!("{d:?}")).unwrap_or_default();
                assert!(d.contains("FlashRaw"), "{d}");
            }
        }
    }

    #[test]
    fn matrix_profiles_fold_into_the_develop_pass() {
        for cs in ColorSpace::ALL {
            let setup = plan_of(cs.icc_profile()).unwrap();
            assert_eq!(setup.color_space, ColorSpace::Custom);
            assert!(setup.lut().is_none());
            let Kind::Matrix(m) = &setup.kind else { panic!("{cs:?} is a matrix profile") };
            let want = cs.matrix_from_srgb();
            for i in 0..3 {
                for j in 0..3 {
                    assert!(
                        (m.matrix[i][j] - want[i][j]).abs() < 2e-4,
                        "{cs:?} {i}{j}: {} vs {}",
                        m.matrix[i][j],
                        want[i][j]
                    );
                }
            }
            // Encoder tables follow the space's curve (the profile stores it at s15.16 precision).
            let t = match &m.curve {
                OutputCurve::Owned(t) => t,
                c => panic!("equal curves share one table: {c:?}"),
            };
            for i in (0..t.lut16.len()).step_by(101) {
                let x = i as f64 / (t.lut16.len() - 1) as f64;
                let tol = if x < 0.002 && cs == ColorSpace::AdobeRgb { 250.0 } else { 3.0 };
                assert!((t.lut16[i] as f64 - cs.encode(x) * 65535.0).abs() < tol, "{cs:?} x={x}");
            }
            // The linear variant keeps the primaries and is tagged for the linear outputs.
            let lin = ColorProfile::new_from_slice(&m.linear).unwrap();
            assert_eq!(lin.red_trc, lin.green_trc);
        }
    }

    #[test]
    fn invalid_profiles_are_errors_not_panics() {
        let good = ColorSpace::DisplayP3.icc_profile();
        let is_invalid = |r: Result<OutputSetup>| matches!(r, Err(Error::InvalidArgument(_)));
        assert!(is_invalid(plan_of(Vec::new())));
        assert!(is_invalid(plan_of(vec![0; 128])));
        assert!(is_invalid(plan_of(b"not an icc profile at all, just text".repeat(8))));
        // Every truncation of a valid profile, and byte flips of it, either parse or fail cleanly.
        for n in 0..good.len() {
            let _ = plan_of(good[..n].to_vec());
        }
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..3000 {
            let mut b = good.clone();
            for _ in 0..1 + next() % 4 {
                let i = (next() % b.len() as u64) as usize;
                b[i] = next() as u8;
            }
            let _ = plan_of(b);
        }
        // Other colour models are refused with a clear message.
        let gray = ColorProfile::new_gray_with_gamma(2.2).encode().unwrap();
        let Err(Error::InvalidArgument(m)) = plan_of(gray) else { panic!("gray profile accepted") };
        assert!(m.contains("not an RGB profile"), "{m}");
    }

    #[test]
    fn second_pass_matches_the_folded_pass() {
        // A matrix profile forced through the second pass (develop wide, convert, quantise)
        // must agree with the folded pass up to the engine's precision; this covers
        // orientation, quantisation and region handling of that path.
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../npm/tests/fixtures/canon-5d3-mlv.dng"),
        )
        .unwrap();
        let raw = crate::FlashRaw::open(&bytes[..]).unwrap();
        for cs in [ColorSpace::DisplayP3, ColorSpace::AdobeRgb, ColorSpace::ProPhoto] {
            for (out, tol) in [(OutputFormat::Rgba8, 4u32), (OutputFormat::Rgb16, 1000)] {
                let folded = DecodeOptions { output: out, demosaic: crate::Demosaic::HalfSize, ..Default::default() }
                    .with_output_profile(OutputProfile::new(cs.icc_profile()));
                let mut forced = folded.clone();
                if let Some(p) = forced.output_profile.as_mut() {
                    p.force_lut = true;
                }
                let (a, b) = (raw.decode(folded).unwrap(), raw.decode(forced).unwrap());
                assert_eq!((a.width, a.height, a.cpp), (b.width, b.height, b.cpp));
                let worst = match (&a.data, &b.data) {
                    (crate::RawPixelData::U8(x), crate::RawPixelData::U8(y)) => {
                        x.iter().zip(y).map(|(p, q)| p.abs_diff(*q) as u32).max().unwrap()
                    }
                    (crate::RawPixelData::U16(x), crate::RawPixelData::U16(y)) => {
                        x.iter().zip(y).map(|(p, q)| p.abs_diff(*q) as u32).max().unwrap()
                    }
                    _ => panic!("unexpected sample types"),
                };
                assert!(worst <= tol, "{cs:?} {out:?}: {worst} > {tol}");
            }
        }
        // Black point compensation does not move a profile whose black is zero by more than a code.
        let mut p = OutputProfile::new(ColorSpace::DisplayP3.icc_profile()).with_black_point_compensation(true);
        p.force_lut = true;
        let o =
            DecodeOptions { output: OutputFormat::Rgba8, demosaic: crate::Demosaic::HalfSize, ..Default::default() };
        let plain =
            raw.decode(o.clone().with_output_profile(OutputProfile::new(ColorSpace::DisplayP3.icc_profile()))).unwrap();
        let bpc = raw.decode(o.with_output_profile(p)).unwrap();
        let (x, y) = (plain.as_u8().unwrap(), bpc.as_u8().unwrap());
        assert!(x.iter().zip(y).all(|(p, q)| p.abs_diff(*q) <= 2));
        // The linear outputs have no device encoding with this pass.
        let mut p = OutputProfile::new(ColorSpace::DisplayP3.icc_profile());
        p.force_lut = true;
        let e = raw
            .decode(DecodeOptions { output: OutputFormat::LinearRgbF32, ..Default::default() }.with_output_profile(p))
            .unwrap_err();
        assert!(matches!(e, Error::InvalidArgument(_)), "{e}");
    }
}
