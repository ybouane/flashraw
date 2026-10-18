//! Output colour spaces: primaries, transfer curves, encoder tables and the matching ICC
//! profiles (written by a small matrix/TRC profile writer, no binary blobs).
//!
//! The develop pass produces linear sRGB; an output space is one more 3x3 matrix that is
//! folded into the camera matrix once per decode (see
//! [`crate::process::DevelopParams::with_color_space`]), so it costs nothing per pixel.
//! Display outputs then apply the space's transfer curve through a lookup table.

use std::sync::{Arc, OnceLock};

/// Colour space of the RGB outputs ([`crate::OutputFormat::LinearRgbF32`],
/// [`crate::OutputFormat::Rgba8`], [`crate::OutputFormat::Rgb16`], ...).
///
/// The linear outputs carry the primaries of the space with a linear transfer function;
/// the display outputs ([`crate::OutputFormat::Rgba8`], [`crate::OutputFormat::Rgb16`])
/// additionally apply its transfer curve. Matrices are adapted to a common D50 connection
/// space with the Bradford transform, the same way the matching ICC profiles
/// ([`ColorSpace::icc_profile`]) describe them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum ColorSpace {
    /// sRGB (IEC 61966-2-1): Rec.709 primaries, D65, the piecewise sRGB curve. The default;
    /// outputs are identical to a decode that predates colour spaces.
    #[default]
    Srgb,
    /// Display P3: DCI-P3 primaries, D65 white, the sRGB curve.
    DisplayP3,
    /// Adobe RGB (1998): wider greens, D65 white, pure gamma 563/256 (about 2.2).
    AdobeRgb,
    /// ProPhoto RGB (ROMM RGB): very wide gamut, D50 white, gamma 1.8 with a linear toe
    /// (slope 16 below 1/512).
    ProPhoto,
    /// ITU-R BT.2020 primaries, D65 white, with the BT.2020/BT.709 OETF (the 12-bit
    /// constants: 4.5 L below 0.018053968..., else 1.09929682... L^0.45 - 0.09929682...).
    Rec2020,
    /// A colour space described only by an ICC profile ([`crate::OutputProfile`]). Only
    /// reported by [`crate::RawFrame::color_space`]; not a valid
    /// [`crate::DecodeOptions::color_space`].
    Custom,
}

type Mat = [[f64; 3]; 3];

/// ICC D50 PCS illuminant.
const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

const SRGB_PRIMARIES: [[f64; 2]; 3] = [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]];
const D65_XY: [f64; 2] = [0.3127, 0.3290];

fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn mul_vec(a: &Mat, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| (0..3).map(|k| a[i][k] * v[k]).sum())
}

fn inv(m: &Mat) -> Mat {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let s = 1.0 / det;
    [
        [(e * i - f * h) * s, (c * h - b * i) * s, (b * f - c * e) * s],
        [(f * g - d * i) * s, (a * i - c * g) * s, (c * d - a * f) * s],
        [(d * h - e * g) * s, (b * g - a * h) * s, (a * e - b * d) * s],
    ]
}

fn xy_to_xyz(xy: [f64; 2]) -> [f64; 3] {
    [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]]
}

/// RGB → XYZ matrix (columns are the primaries) for `primaries` and `white` (XYZ, Y = 1).
fn rgb_to_xyz(primaries: &[[f64; 2]; 3], white: [f64; 3]) -> Mat {
    let p: Vec<[f64; 3]> = primaries.iter().map(|&xy| xy_to_xyz(xy)).collect();
    let cols: Mat = [[p[0][0], p[1][0], p[2][0]], [p[0][1], p[1][1], p[2][1]], [p[0][2], p[1][2], p[2][2]]];
    let s = mul_vec(&inv(&cols), white);
    [
        [cols[0][0] * s[0], cols[0][1] * s[1], cols[0][2] * s[2]],
        [cols[1][0] * s[0], cols[1][1] * s[1], cols[1][2] * s[2]],
        [cols[2][0] * s[0], cols[2][1] * s[1], cols[2][2] * s[2]],
    ]
}

/// Bradford chromatic adaptation matrix from white `src` to white `dst` (XYZ).
fn bradford(src: [f64; 3], dst: [f64; 3]) -> Mat {
    const B: Mat = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    let (s, d) = (mul_vec(&B, src), mul_vec(&B, dst));
    let scale: Mat = [[d[0] / s[0], 0.0, 0.0], [0.0, d[1] / s[1], 0.0], [0.0, 0.0, d[2] / s[2]]];
    mul(&inv(&B), &mul(&scale, &B))
}

/// Transfer function family of a space (encoding: linear → encoded).
#[derive(Clone, Copy)]
enum Transfer {
    Srgb,
    Gamma(f64),
    ProPhoto,
    Bt2020,
}

const BT2020_ALPHA: f64 = 1.099_296_826_809_44;
const BT2020_BETA: f64 = 0.018_053_968_510_807;

impl Transfer {
    fn encode(self, x: f64) -> f64 {
        match self {
            Transfer::Srgb => {
                if x <= 0.003_130_8 {
                    12.92 * x
                } else {
                    1.055 * x.powf(1.0 / 2.4) - 0.055
                }
            }
            Transfer::Gamma(g) => x.powf(1.0 / g),
            Transfer::ProPhoto => {
                if x < 1.0 / 512.0 {
                    16.0 * x
                } else {
                    x.powf(1.0 / 1.8)
                }
            }
            Transfer::Bt2020 => {
                if x < BT2020_BETA {
                    4.5 * x
                } else {
                    BT2020_ALPHA * x.powf(0.45) - (BT2020_ALPHA - 1.0)
                }
            }
        }
    }

    fn decode(self, v: f64) -> f64 {
        match self {
            Transfer::Srgb => {
                if v <= 0.040_45 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            Transfer::Gamma(g) => v.powf(g),
            Transfer::ProPhoto => {
                if v < 16.0 / 512.0 {
                    v / 16.0
                } else {
                    v.powf(1.8)
                }
            }
            Transfer::Bt2020 => {
                if v < 4.5 * BT2020_BETA {
                    v / 4.5
                } else {
                    ((v + (BT2020_ALPHA - 1.0)) / BT2020_ALPHA).powf(1.0 / 0.45)
                }
            }
        }
    }
}

impl ColorSpace {
    /// Parse a colour space name: `srgb`, `display-p3`/`p3`, `adobe-rgb`/`adobergb`,
    /// `prophoto`/`romm`, `rec2020`/`bt2020` (case-insensitive, `_` or `-`). `custom` is
    /// not parsed: it needs a profile.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().replace(['_', ' '], "-").as_str() {
            "srgb" | "s-rgb" => ColorSpace::Srgb,
            "display-p3" | "displayp3" | "p3" | "p3-d65" => ColorSpace::DisplayP3,
            "adobe-rgb" | "adobergb" | "adobe" | "adobe-rgb-1998" => ColorSpace::AdobeRgb,
            "prophoto" | "prophoto-rgb" | "romm" | "romm-rgb" => ColorSpace::ProPhoto,
            "rec2020" | "rec-2020" | "bt2020" | "bt-2020" => ColorSpace::Rec2020,
            _ => return None,
        })
    }

    /// Canonical name, accepted by [`ColorSpace::parse`] (except `custom`).
    pub fn name(self) -> &'static str {
        match self {
            ColorSpace::Srgb => "srgb",
            ColorSpace::DisplayP3 => "display-p3",
            ColorSpace::AdobeRgb => "adobe-rgb",
            ColorSpace::ProPhoto => "prophoto",
            ColorSpace::Rec2020 => "rec2020",
            ColorSpace::Custom => "custom",
        }
    }

    /// Every built-in space, in declaration order (without [`ColorSpace::Custom`]).
    pub const ALL: [ColorSpace; 5] =
        [ColorSpace::Srgb, ColorSpace::DisplayP3, ColorSpace::AdobeRgb, ColorSpace::ProPhoto, ColorSpace::Rec2020];

    /// Chromaticities `[[xr, yr], [xg, yg], [xb, yb]]` of the primaries (CIE xy); `None`
    /// for [`ColorSpace::Custom`].
    pub fn primaries(self) -> Option<[[f64; 2]; 3]> {
        Some(match self {
            ColorSpace::Srgb => SRGB_PRIMARIES,
            ColorSpace::DisplayP3 => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]],
            ColorSpace::AdobeRgb => [[0.64, 0.33], [0.21, 0.71], [0.15, 0.06]],
            ColorSpace::ProPhoto => [[0.7347, 0.2653], [0.1596, 0.8404], [0.0366, 0.0001]],
            ColorSpace::Rec2020 => [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
            ColorSpace::Custom => return None,
        })
    }

    /// Chromaticity of the native white point (D65, or D50 for ProPhoto); `None` for
    /// [`ColorSpace::Custom`].
    pub fn white_point(self) -> Option<[f64; 2]> {
        match self {
            ColorSpace::ProPhoto => Some([0.3457, 0.3585]),
            ColorSpace::Custom => None,
            _ => Some(D65_XY),
        }
    }

    fn transfer(self) -> Transfer {
        match self {
            ColorSpace::Srgb | ColorSpace::DisplayP3 | ColorSpace::Custom => Transfer::Srgb,
            ColorSpace::AdobeRgb => Transfer::Gamma(563.0 / 256.0),
            ColorSpace::ProPhoto => Transfer::ProPhoto,
            ColorSpace::Rec2020 => Transfer::Bt2020,
        }
    }

    /// The transfer curve: linear `x` in `[0, 1]` → encoded value (the display outputs
    /// quantise this). [`ColorSpace::Custom`] reports the sRGB curve.
    pub fn encode(self, x: f64) -> f64 {
        self.transfer().encode(x)
    }

    /// Inverse of [`ColorSpace::encode`]: encoded value → linear.
    pub fn decode(self, v: f64) -> f64 {
        self.transfer().decode(v)
    }

    /// Native-white XYZ (Y = 1) of the space as used for its matrix.
    fn native_white(self) -> [f64; 3] {
        match self {
            // ICC D50, so white maps to exactly (1, 1, 1) in the profile.
            ColorSpace::ProPhoto => D50,
            _ => xy_to_xyz(D65_XY),
        }
    }

    /// RGB → XYZ matrix relative to the D50 connection space (the matrix of the ICC
    /// profile: Bradford-adapted for D65 spaces). `None` for [`ColorSpace::Custom`].
    pub(crate) fn colorants_d50(self) -> Option<[[f64; 3]; 3]> {
        let p = self.primaries()?;
        let w = self.native_white();
        let m = rgb_to_xyz(&p, w);
        Some(if self == ColorSpace::ProPhoto { m } else { mul(&bradford(w, D50), &m) })
    }

    /// The 3x3 matrix taking linear sRGB to linear RGB of this space (identity for sRGB and
    /// [`ColorSpace::Custom`]). Folded into the camera matrix at decode time.
    pub fn matrix_from_srgb(self) -> [[f64; 3]; 3] {
        if matches!(self, ColorSpace::Srgb | ColorSpace::Custom) {
            return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        }
        let src = ColorSpace::Srgb.colorants_d50().expect("sRGB");
        let dst = self.colorants_d50().expect("built-in space");
        mul(&inv(&dst), &src)
    }

    /// Encoder tables for the display outputs; [`OutputCurve::Srgb`] keeps the original
    /// sRGB tables (and therefore bit-identical output).
    pub(crate) fn output_curve(self) -> OutputCurve {
        static ADOBE: OnceLock<CurveTables> = OnceLock::new();
        static PROPHOTO: OnceLock<CurveTables> = OnceLock::new();
        static REC2020: OnceLock<CurveTables> = OnceLock::new();
        let (cell, t) = match self {
            ColorSpace::Srgb | ColorSpace::DisplayP3 | ColorSpace::Custom => return OutputCurve::Srgb,
            ColorSpace::AdobeRgb => (&ADOBE, self.transfer()),
            ColorSpace::ProPhoto => (&PROPHOTO, self.transfer()),
            ColorSpace::Rec2020 => (&REC2020, self.transfer()),
        };
        OutputCurve::Shared(cell.get_or_init(|| CurveTables::build(|x| t.encode(x))))
    }

    /// A standard ICC v4 matrix/TRC profile for this space ("FlashRaw Display P3", ...),
    /// for tagging display outputs (RGBA8, 16-bit). Empty for [`ColorSpace::Custom`].
    pub fn icc_profile(self) -> Vec<u8> {
        self.profile(false)
    }

    /// The same primaries with a linear transfer curve, for tagging the linear outputs
    /// ([`crate::OutputFormat::LinearRgbF32`], [`crate::OutputFormat::LinearRgb16`],
    /// [`crate::OutputFormat::LinearRgbF16`]). Empty for [`ColorSpace::Custom`].
    pub fn icc_profile_linear(self) -> Vec<u8> {
        self.profile(true)
    }

    fn profile(self, linear: bool) -> Vec<u8> {
        let Some(colorants) = self.colorants_d50() else { return Vec::new() };
        let w = self.native_white();
        let name = match self {
            ColorSpace::Srgb => "sRGB",
            ColorSpace::DisplayP3 => "Display P3",
            ColorSpace::AdobeRgb => "Adobe RGB (1998)",
            ColorSpace::ProPhoto => "ProPhoto RGB",
            ColorSpace::Rec2020 => "Rec. 2020",
            ColorSpace::Custom => "",
        };
        let trc = if linear {
            IccCurve::Linear
        } else {
            match self {
                ColorSpace::AdobeRgb => IccCurve::Gamma(563.0 / 256.0),
                ColorSpace::ProPhoto => IccCurve::Para([1.8, 1.0, 0.0, 1.0 / 16.0, 1.0 / 32.0]),
                ColorSpace::Rec2020 => IccCurve::Para([
                    1.0 / 0.45,
                    1.0 / BT2020_ALPHA,
                    (BT2020_ALPHA - 1.0) / BT2020_ALPHA,
                    1.0 / 4.5,
                    4.5 * BT2020_BETA,
                ]),
                _ => IccCurve::Para([2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045]),
            }
        };
        let description = if linear { format!("FlashRaw {name} (linear)") } else { format!("FlashRaw {name}") };
        let adapt = (self != ColorSpace::ProPhoto).then(|| bradford(w, D50));
        write_matrix_profile(&IccProfileSpec { description: &description, colorants, adapt, curves: [trc, trc, trc] })
    }
}

// ---- encoder tables ----

/// Segments of the 8-bit encoder table of the non-sRGB spaces (the sRGB table keeps its
/// original 4096).
pub const LUT8_SEGMENTS: usize = 16384;
/// Segments of the 16-bit curve tables (same as the sRGB table).
pub const LUT16_SEGMENTS: usize = 16384;

/// Encoder lookup tables of one transfer curve: linear → 8-bit code (`LUT8_SEGMENTS + 1`
/// entries over `[0, 1]`) and linear → 16-bit code as `f32` (`LUT16_SEGMENTS + 1` entries,
/// interpolated linearly).
pub struct CurveTables {
    /// 8-bit codes, indexed by `trunc(clamp(v, 0, 1) * (len - 1))`.
    pub lut8: Vec<u8>,
    /// 16-bit codes (as `f32`) at evenly spaced points of `[0, 1]`.
    pub lut16: Vec<f32>,
}

impl std::fmt::Debug for CurveTables {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CurveTables({}, {})", self.lut8.len(), self.lut16.len())
    }
}

impl CurveTables {
    /// Tables of the encoding function `enc` (linear `[0, 1]` → encoded `[0, 1]`, exact in f64).
    pub fn build(enc: impl Fn(f64) -> f64) -> Self {
        let lut8 = (0..=LUT8_SEGMENTS)
            .map(|i| (enc(i as f64 / LUT8_SEGMENTS as f64) * 255.0 + 0.5).clamp(0.0, 255.0) as u8)
            .collect();
        let lut16 = (0..=LUT16_SEGMENTS)
            .map(|i| (enc(i as f64 / LUT16_SEGMENTS as f64).clamp(0.0, 1.0) * 65535.0) as f32)
            .collect();
        CurveTables { lut8, lut16 }
    }
}

/// Transfer curve applied by the display encoders of a develop pass.
#[derive(Debug, Clone, Default)]
pub enum OutputCurve {
    /// The sRGB curve through the original tables (sRGB and Display P3 outputs).
    #[default]
    Srgb,
    /// One table set for all channels (built-in spaces).
    Shared(&'static CurveTables),
    /// One table set for all channels, owned (a profile whose three curves are equal).
    Owned(Arc<CurveTables>),
    /// One table set per channel (profiles whose three curves differ).
    PerChannel(Arc<[CurveTables; 3]>),
}

// ---- ICC profile writer ----

/// Tone curve of a written profile channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum IccCurve {
    /// Identity (`curv` with no entries).
    Linear,
    /// Pure gamma, as a `curv` u8Fixed8 value.
    Gamma(f64),
    /// Parametric type 3: `[g, a, b, c, d]`, `y = (a x + b)^g` for `x >= d`, else `c x`.
    Para([f64; 5]),
}

/// What a matrix/TRC profile contains.
pub(crate) struct IccProfileSpec<'a> {
    pub description: &'a str,
    /// RGB → XYZ (D50 connection space) matrix: columns are the colorants.
    pub colorants: [[f64; 3]; 3],
    /// Chromatic adaptation from the native white to D50 (`chad`), if any.
    pub adapt: Option<[[f64; 3]; 3]>,
    pub curves: [IccCurve; 3],
}

fn s15(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn xyz_tag(v: [f64; 3]) -> Vec<u8> {
    let mut d = b"XYZ \0\0\0\0".to_vec();
    v.iter().for_each(|&x| d.extend_from_slice(&s15(x)));
    d
}

fn text_tag(s: &str) -> Vec<u8> {
    let u: Vec<u8> = s.encode_utf16().flat_map(|c| c.to_be_bytes()).collect();
    let mut d = b"mluc\0\0\0\0".to_vec();
    d.extend_from_slice(&1u32.to_be_bytes());
    d.extend_from_slice(&12u32.to_be_bytes());
    d.extend_from_slice(b"enUS");
    d.extend_from_slice(&(u.len() as u32).to_be_bytes());
    d.extend_from_slice(&28u32.to_be_bytes());
    d.extend_from_slice(&u);
    d
}

fn curve_tag(c: IccCurve) -> Vec<u8> {
    match c {
        IccCurve::Linear => {
            let mut d = b"curv\0\0\0\0".to_vec();
            d.extend_from_slice(&0u32.to_be_bytes());
            d
        }
        IccCurve::Gamma(g) => {
            let mut d = b"curv\0\0\0\0".to_vec();
            d.extend_from_slice(&1u32.to_be_bytes());
            d.extend_from_slice(&((g * 256.0).round() as u16).to_be_bytes());
            d
        }
        IccCurve::Para(p) => {
            let mut d = b"para\0\0\0\0".to_vec();
            d.extend_from_slice(&3u16.to_be_bytes());
            d.extend_from_slice(&0u16.to_be_bytes());
            p.iter().for_each(|&x| d.extend_from_slice(&s15(x)));
            d
        }
    }
}

/// Serialise a v4 display profile with a colorant matrix and per-channel tone curves.
pub(crate) fn write_matrix_profile(spec: &IccProfileSpec<'_>) -> Vec<u8> {
    let c = &spec.colorants;
    let col = |j: usize| [c[0][j], c[1][j], c[2][j]];
    let mut tags: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"desc", text_tag(spec.description)),
        (*b"cprt", text_tag("Public domain, generated by FlashRaw")),
        (*b"wtpt", xyz_tag(D50)),
        (*b"rXYZ", xyz_tag(col(0))),
        (*b"gXYZ", xyz_tag(col(1))),
        (*b"bXYZ", xyz_tag(col(2))),
    ];
    if let Some(a) = spec.adapt {
        let mut d = b"sf32\0\0\0\0".to_vec();
        a.iter().flatten().for_each(|&x| d.extend_from_slice(&s15(x)));
        tags.push((*b"chad", d));
    }
    for (sig, curve) in [*b"rTRC", *b"gTRC", *b"bTRC"].into_iter().zip(spec.curves) {
        tags.push((sig, curve_tag(curve)));
    }
    let table_len = 4 + 12 * tags.len();
    let mut offset = 128 + table_len;
    let mut table = Vec::with_capacity(table_len);
    let mut body: Vec<u8> = Vec::new();
    table.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    // Identical tag data (equal curves) is stored once and shared.
    let mut placed: Vec<(usize, usize, &[u8])> = Vec::new();
    for (sig, data) in &tags {
        let (off, len) = match placed.iter().find(|(_, _, d)| *d == data.as_slice()) {
            Some(&(o, l, _)) => (o, l),
            None => {
                let o = offset + body.len();
                body.extend_from_slice(data);
                while body.len() % 4 != 0 {
                    body.push(0);
                }
                placed.push((o, data.len(), data.as_slice()));
                (o, data.len())
            }
        };
        table.extend_from_slice(sig);
        table.extend_from_slice(&(off as u32).to_be_bytes());
        table.extend_from_slice(&(len as u32).to_be_bytes());
    }
    offset = 128 + table.len() + body.len();
    let mut out = Vec::with_capacity(offset);
    out.extend_from_slice(&(offset as u32).to_be_bytes());
    out.extend_from_slice(&[0; 4]); // preferred CMM
    out.extend_from_slice(&[4, 0x30, 0, 0]); // version 4.3
    out.extend_from_slice(b"mntr");
    out.extend_from_slice(b"RGB ");
    out.extend_from_slice(b"XYZ ");
    // Fixed creation date (2024-01-01 00:00:00) so profiles are reproducible.
    for v in [2024u16, 1, 1, 0, 0, 0] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(b"acsp");
    out.extend_from_slice(&[0; 4]); // platform
    out.extend_from_slice(&[0; 4]); // flags
    out.extend_from_slice(&[0; 4]); // manufacturer
    out.extend_from_slice(&[0; 4]); // model
    out.extend_from_slice(&[0; 8]); // attributes
    out.extend_from_slice(&0u32.to_be_bytes()); // rendering intent: perceptual
    D50.iter().for_each(|&x| out.extend_from_slice(&s15(x)));
    out.extend_from_slice(&[0; 4]); // creator
    out.extend_from_slice(&[0; 16]); // profile ID (not computed)
    out.extend_from_slice(&[0; 28]);
    debug_assert_eq!(out.len(), 128);
    out.extend_from_slice(&table);
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ColorSpace; 5] = ColorSpace::ALL;

    #[test]
    fn names_round_trip_and_aliases() {
        for cs in ALL {
            assert_eq!(ColorSpace::parse(cs.name()), Some(cs));
        }
        assert_eq!(ColorSpace::parse("P3"), Some(ColorSpace::DisplayP3));
        assert_eq!(ColorSpace::parse("AdobeRGB"), Some(ColorSpace::AdobeRgb));
        assert_eq!(ColorSpace::parse("romm"), Some(ColorSpace::ProPhoto));
        assert_eq!(ColorSpace::parse("display_p3"), Some(ColorSpace::DisplayP3));
        assert_eq!(ColorSpace::parse("nope"), None);
    }

    #[test]
    fn white_maps_to_white_and_srgb_is_identity() {
        assert_eq!(ColorSpace::Srgb.matrix_from_srgb(), [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        for cs in ALL {
            let w = mul_vec(&cs.matrix_from_srgb(), [1.0; 3]);
            for v in w {
                assert!((v - 1.0).abs() < 1e-9, "{cs:?} white -> {w:?}");
            }
        }
    }

    #[test]
    fn matrices_round_trip_through_every_space() {
        for a in ALL {
            for b in ALL {
                // a-linear -> sRGB -> b-linear equals the direct b/a matrix.
                let to_b = mul(&b.matrix_from_srgb(), &inv(&a.matrix_from_srgb()));
                let back = mul(&a.matrix_from_srgb(), &inv(&b.matrix_from_srgb()));
                let id = mul(&to_b, &back);
                for i in 0..3 {
                    for j in 0..3 {
                        assert!((id[i][j] - if i == j { 1.0 } else { 0.0 }).abs() < 1e-9);
                    }
                }
            }
        }
    }

    #[test]
    fn matrices_match_published_values() {
        // sRGB -> Display P3 (D65): the widely published matrix.
        let m = ColorSpace::DisplayP3.matrix_from_srgb();
        let want = [[0.8224621, 0.1775380, 0.0], [0.0331941, 0.9668058, 0.0], [0.0170827, 0.0723974, 0.9105199]];
        for i in 0..3 {
            for j in 0..3 {
                assert!((m[i][j] - want[i][j]).abs() < 2e-4, "p3 {i}{j}: {} vs {}", m[i][j], want[i][j]);
            }
        }
        // sRGB -> Adobe RGB (same red/blue primaries, wider green): the green row is unchanged.
        let m = ColorSpace::AdobeRgb.matrix_from_srgb();
        let want = [[0.7152, 0.2848, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0412, 0.9588]];
        for i in 0..3 {
            for j in 0..3 {
                assert!((m[i][j] - want[i][j]).abs() < 2e-3, "adobe {i}{j}: {} vs {}", m[i][j], want[i][j]);
            }
        }
        // Rec.2020 and ProPhoto: first row of the sRGB -> space matrix.
        let m = ColorSpace::Rec2020.matrix_from_srgb();
        assert!((m[0][0] - 0.6274).abs() < 1e-3 && (m[0][1] - 0.3293).abs() < 1e-3);
        let m = ColorSpace::ProPhoto.matrix_from_srgb();
        assert!((m[0][0] - 0.5293).abs() < 2e-3 && (m[0][1] - 0.3300).abs() < 2e-3, "{m:?}");
    }

    #[test]
    fn curves_match_reference_values() {
        // sRGB mid grey 0.18 -> 0.4614, P3 shares it.
        assert!((ColorSpace::Srgb.encode(0.18) - 0.461_356).abs() < 1e-5);
        assert_eq!(ColorSpace::DisplayP3.encode(0.18), ColorSpace::Srgb.encode(0.18));
        // Adobe RGB: pure power 256/563.
        assert!((ColorSpace::AdobeRgb.encode(0.5) - 0.5f64.powf(256.0 / 563.0)).abs() < 1e-12);
        assert!((ColorSpace::AdobeRgb.encode(0.18) - 0.4585).abs() < 1e-3);
        // ProPhoto: linear toe, then gamma 1.8.
        assert!((ColorSpace::ProPhoto.encode(0.001) - 0.016).abs() < 1e-12);
        assert!((ColorSpace::ProPhoto.encode(0.5) - 0.5f64.powf(1.0 / 1.8)).abs() < 1e-12);
        // BT.2020 / BT.709: 4.5 L below the knee, 0.5 -> 0.7052.
        assert!((ColorSpace::Rec2020.encode(0.01) - 0.045).abs() < 1e-12);
        assert!((ColorSpace::Rec2020.encode(0.5) - 0.7052).abs() < 1e-3);
        for cs in ALL {
            assert!(cs.encode(0.0).abs() < 1e-12 && (cs.encode(1.0) - 1.0).abs() < 1e-9, "{cs:?}");
            for i in 0..=100 {
                let x = i as f64 / 100.0;
                assert!((cs.decode(cs.encode(x)) - x).abs() < 1e-9, "{cs:?} {x}");
            }
        }
    }

    #[test]
    fn tables_follow_the_curves() {
        for cs in [ColorSpace::AdobeRgb, ColorSpace::ProPhoto, ColorSpace::Rec2020] {
            let OutputCurve::Shared(t) = cs.output_curve() else { panic!("{cs:?}") };
            assert_eq!((t.lut8.len(), t.lut16.len()), (LUT8_SEGMENTS + 1, LUT16_SEGMENTS + 1));
            assert_eq!((t.lut8[0], t.lut8[LUT8_SEGMENTS]), (0, 255));
            assert_eq!(t.lut16[LUT16_SEGMENTS], 65535.0);
            for i in (0..=LUT16_SEGMENTS).step_by(97) {
                let want = cs.encode(i as f64 / LUT16_SEGMENTS as f64) * 65535.0;
                assert!((t.lut16[i] as f64 - want).abs() < 1.0, "{cs:?} {i}");
            }
        }
        assert!(matches!(ColorSpace::Srgb.output_curve(), OutputCurve::Srgb));
        assert!(matches!(ColorSpace::DisplayP3.output_curve(), OutputCurve::Srgb));
    }

    #[test]
    fn written_profiles_are_well_formed() {
        for cs in ALL {
            for p in [cs.icc_profile(), cs.icc_profile_linear()] {
                assert_eq!(u32::from_be_bytes(p[0..4].try_into().unwrap()) as usize, p.len());
                assert_eq!(&p[36..40], b"acsp");
                assert_eq!(&p[12..16], b"mntr");
                let n = u32::from_be_bytes(p[128..132].try_into().unwrap()) as usize;
                for i in 0..n {
                    let e = 132 + i * 12;
                    let off = u32::from_be_bytes(p[e + 4..e + 8].try_into().unwrap()) as usize;
                    let len = u32::from_be_bytes(p[e + 8..e + 12].try_into().unwrap()) as usize;
                    assert!(off % 4 == 0 && off + len <= p.len());
                }
            }
        }
        assert!(ColorSpace::Custom.icc_profile().is_empty());
    }
}
