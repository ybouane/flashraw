//! Decode options and backend selection.

use crate::color::ColorSpace;
use crate::icc::{OutputProfile, RenderingIntent};
use flashraw_core::{CameraProfile, Limits, Rect};

/// Execution backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Backend {
    /// Pick the fastest available backend for this file and output.
    #[default]
    Auto,
    /// Portable scalar CPU code (reference implementations).
    Cpu,
    /// CPU with explicit SIMD kernels (runtime dispatched).
    CpuSimd,
    /// GPU compute (requires the `gpu` feature and an adapter).
    Gpu,
}

impl WhiteBalanceMode {
    /// Parse a white balance mode name (`as-shot`, `auto`; case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "as-shot" | "asshot" | "camera" => WhiteBalanceMode::AsShot,
            "auto" => WhiteBalanceMode::Auto,
            "daylight" | "d65" => WhiteBalanceMode::Daylight,
            _ => return None,
        })
    }

    /// Canonical name, accepted by [`WhiteBalanceMode::parse`].
    pub fn name(self) -> &'static str {
        match self {
            WhiteBalanceMode::AsShot => "as-shot",
            WhiteBalanceMode::Auto => "auto",
            WhiteBalanceMode::Daylight => "daylight",
        }
    }
}

impl NoiseReduction {
    /// Parse a level name (`auto`, `off`, `low`, `medium`, `high`; case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "auto" => NoiseReduction::Auto,
            "off" | "none" => NoiseReduction::Off,
            "low" => NoiseReduction::Low,
            "medium" | "med" => NoiseReduction::Medium,
            "high" => NoiseReduction::High,
            _ => return None,
        })
    }

    /// Canonical name, accepted by [`NoiseReduction::parse`].
    pub fn name(self) -> &'static str {
        match self {
            NoiseReduction::Auto => "auto",
            NoiseReduction::Off => "off",
            NoiseReduction::Low => "low",
            NoiseReduction::Medium => "medium",
            NoiseReduction::High => "high",
        }
    }

    /// Smoothing weight of this level (module `edgeaware`, step 5): `None` keeps the one
    /// estimated from the frame.
    pub(crate) fn floor(self) -> Option<f32> {
        match self {
            NoiseReduction::Auto => None,
            NoiseReduction::Off => Some(0.0),
            NoiseReduction::Low => Some(0.25),
            NoiseReduction::Medium => Some(0.5),
            NoiseReduction::High => Some(0.8),
        }
    }
}

impl Backend {
    /// Parse a backend name (`auto`, `cpu`/`scalar`, `simd`/`cpu-simd`, `gpu`/`webgpu`;
    /// case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "auto" => Backend::Auto,
            "cpu" | "scalar" => Backend::Cpu,
            "simd" | "cpu-simd" | "cpusimd" => Backend::CpuSimd,
            "gpu" | "webgpu" => Backend::Gpu,
            _ => return None,
        })
    }
}

/// What the decode produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum OutputFormat {
    /// Sensor samples as stored (u16, or f32 for floating-point DNG). No processing.
    #[default]
    RawU16,
    /// Sensor samples converted to f32 without normalisation.
    RawF32,
    /// Black-subtracted, white-normalised sensor samples in `[0, 1]` (f32, CFA layout kept).
    NormalizedF32,
    /// Demosaiced, white-balanced linear camera RGB → linear sRGB primaries (or those of
    /// [`DecodeOptions::color_space`]), f32 RGB.
    /// Sensor orientation (see [`crate::RawFrame::orientation`]); values are not clipped.
    LinearRgbF32,
    /// Display-ready 8-bit sRGB RGBA (orientation applied), e.g. for `ImageData`. Uses the
    /// colour space and transfer curve of [`DecodeOptions::color_space`] (sRGB by default).
    Rgba8,
    /// Display-ready 16-bit sRGB RGB (orientation applied, no alpha): the transfer curve of
    /// [`OutputFormat::Rgba8`] at 16-bit precision, e.g. for 16-bit PNG/TIFF.
    Rgb16,
    /// [`OutputFormat::LinearRgbF32`] as 16-bit integers: linear sRGB primaries, `[0, 1]`
    /// clipped and scaled to `[0, 65535]`. Sensor orientation, like the f32 output.
    LinearRgb16,
    /// [`OutputFormat::LinearRgbF32`] as IEEE half floats ([`crate::RawPixelData::F16`]),
    /// half the memory. Rounded to nearest; values beyond ±65504 saturate. Sensor
    /// orientation, like the f32 output.
    LinearRgbF16,
    // GPU-resident outputs (feature `gpu`): same content as the host variant, returned
    // as `RawPixelData::Gpu` without downloading (see `crate::gpu` and `RawFrame::download`).
    /// [`OutputFormat::RawU16`] kept on the GPU (u16 pairs per 32-bit word).
    GpuRawU16,
    /// [`OutputFormat::NormalizedF32`] kept on the GPU.
    GpuNormalizedF32,
    /// [`OutputFormat::LinearRgbF32`] kept on the GPU.
    GpuLinearRgbF32,
    /// [`OutputFormat::Rgba8`] kept on the GPU (one packed RGBA8 pixel per 32-bit word).
    GpuRgba8,
}

impl OutputFormat {
    /// True for the `Gpu*` variants (result stays on the device).
    pub fn is_gpu_resident(self) -> bool {
        matches!(
            self,
            OutputFormat::GpuRawU16
                | OutputFormat::GpuNormalizedF32
                | OutputFormat::GpuLinearRgbF32
                | OutputFormat::GpuRgba8
        )
    }
    /// True for the outputs that have the EXIF orientation applied (the display outputs
    /// [`OutputFormat::Rgba8`], [`OutputFormat::Rgb16`] and [`OutputFormat::GpuRgba8`]);
    /// other outputs keep the sensor orientation and report it in
    /// [`crate::RawFrame::orientation`].
    pub fn applies_orientation(self) -> bool {
        matches!(self.host_equivalent(), OutputFormat::Rgba8 | OutputFormat::Rgb16)
    }
    /// True for the demosaiced RGB outputs (host or GPU-resident).
    pub fn is_rgb(self) -> bool {
        self.rgb_encoding().is_some()
    }
    /// Sample encoding of an RGB output.
    pub(crate) fn rgb_encoding(self) -> Option<crate::process::RgbEncoding> {
        use crate::process::RgbEncoding as E;
        Some(match self.host_equivalent() {
            OutputFormat::Rgba8 => E::Rgba8,
            OutputFormat::Rgb16 => E::Srgb16,
            OutputFormat::LinearRgb16 => E::Linear16,
            OutputFormat::LinearRgbF16 => E::LinearF16,
            OutputFormat::LinearRgbF32 => E::LinearF32,
            _ => return None,
        })
    }
    /// The host-memory variant with the same content (identity for host variants).
    pub fn host_equivalent(self) -> Self {
        match self {
            OutputFormat::GpuRawU16 => OutputFormat::RawU16,
            OutputFormat::GpuNormalizedF32 => OutputFormat::NormalizedF32,
            OutputFormat::GpuLinearRgbF32 => OutputFormat::LinearRgbF32,
            OutputFormat::GpuRgba8 => OutputFormat::Rgba8,
            o => o,
        }
    }
}

impl OutputFormat {
    /// Parse an output name as accepted by the CLI and the JS bindings (`raw-u16`,
    /// `raw-f32`, `normalized`, `linear-rgb`, `rgba8`, `rgb16`, `linear-rgb16`,
    /// `linear-rgb-f16`, `gpu-raw-u16`, `gpu-normalized`, `gpu-linear-rgb`, `gpu-rgba8`,
    /// plus a few aliases; case-insensitive, `_` or `-`).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "raw" | "raw-u16" | "u16" => OutputFormat::RawU16,
            "raw-f32" | "f32" => OutputFormat::RawF32,
            "normalized" | "normalized-f32" => OutputFormat::NormalizedF32,
            "linear-rgb" | "linear-rgb-f32" | "rgb-f32" => OutputFormat::LinearRgbF32,
            "rgba8" | "rgba" | "imagedata" => OutputFormat::Rgba8,
            "rgb16" | "srgb16" | "rgb-u16" => OutputFormat::Rgb16,
            "linear-rgb16" | "linear-rgb-u16" => OutputFormat::LinearRgb16,
            "linear-rgb-f16" | "rgb-f16" | "linear-rgb-half" => OutputFormat::LinearRgbF16,
            "gpu-raw-u16" | "gpu-raw" | "gpu-u16" => OutputFormat::GpuRawU16,
            "gpu-normalized" | "gpu-normalized-f32" => OutputFormat::GpuNormalizedF32,
            "gpu-linear-rgb" | "gpu-linear-rgb-f32" => OutputFormat::GpuLinearRgbF32,
            "gpu-rgba8" | "gpu-rgba" => OutputFormat::GpuRgba8,
            _ => return None,
        })
    }
}

/// Demosaic algorithm for RGB outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Demosaic {
    /// Bilinear interpolation (Bayer), nearest-same-colour average (other CFAs). The fastest
    /// full-resolution demosaic.
    Bilinear,
    /// 2x2 superpixel binning: half resolution, fastest, no interpolation artefacts.
    HalfSize,
    /// Directionally weighted 7x7 filters for Bayer and X-Trans sensors: far fewer zipper
    /// and false-colour artefacts than [`Demosaic::Bilinear`], several times slower. Other
    /// sensors use bilinear. The GPU backend runs it too (about 6x its bilinear kernel time);
    /// the GPU result equals the CPU's within rounding. It may change between minor versions
    /// as the filters improve.
    EdgeAware,
    /// Homogeneity-directed interpolation (Hirakawa and Parks) for Bayer sensors: two
    /// candidate images, interpolated along rows and along columns, of which each pixel takes the
    /// one that is locally the more homogeneous in a perceptual colour space (blended by their
    /// homogeneity). The fewest false-colour artefacts on real Bayer files, at the cost of fine
    /// edges against [`Demosaic::EdgeAware`] (lower PSNR on ground-truth edges), and it does not
    /// smooth noise; what [`Demosaic::Auto`] picks on Bayer sensors. Other sensors use the
    /// edge-aware demosaic where it exists, else bilinear. The GPU backend runs it too: it decides
    /// with thresholds, so GPU rounding can change a homogeneity count and the GPU result can
    /// differ from the CPU's on a small fraction of pixels (well under 1 % of the 8-bit samples of
    /// the test frames on an Apple M1 Ultra).
    Ahd,
    /// Patterned pixel grouping for Bayer sensors: green interpolated along the row or column of
    /// smaller gradient, red and blue from colour differences (along the diagonal of smaller
    /// gradient where the colour is not measured on the row or column). Cleaner than bilinear
    /// and a little cheaper than [`Demosaic::Ahd`]. Bayer only: other sensors are an
    /// error ([`crate::Error::InvalidArgument`]).
    Ppg,
    /// Ratio-corrected demosaic for Bayer sensors (experimental): green from ratio-corrected
    /// directional estimates weighted by local gradients, red and blue from colour differences
    /// with soft diagonal weights. Bayer only. May change or be removed between minor versions.
    Rcd,
    /// The best validated algorithm for the sensor, chosen when the file is decoded and the same
    /// for the same sensor type on every machine: [`Demosaic::Ahd`] on Bayer sensors (the fewest
    /// false-colour artefacts on real files in the benchmarks, and the fastest of the
    /// interpolating ones after bilinear), [`Demosaic::EdgeAware`] on X-Trans sensors (closest
    /// to ground truth and the fewest false-colour artefacts there) and on Bayer sensors when
    /// [`DecodeOptions::noise_reduction`] asks for a smoothing level, bilinear on every other
    /// pattern. The default. The algorithm behind it may
    /// change between minor versions as better ones are validated; pin an explicit algorithm
    /// (for example [`Demosaic::Bilinear`], the fastest) to keep the output stable.
    #[default]
    Auto,
}

/// What RGB outputs do with clipped highlights (samples at the sensor's saturation level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Highlights {
    /// Clamp each channel to the white level after white balance. Partly clipped pixels keep
    /// a colour cast (usually magenta or yellow). The default.
    #[default]
    Clip,
    /// Pixels with a channel at or near saturation fade smoothly to a neutral white, which
    /// removes the colour cast around blown highlights. CPU only (the GPU backend decodes on
    /// the CPU for it).
    Neutral,
    /// Hue-preserving: pixels that exceed the white level after white balance (a channel with a
    /// large multiplier, not yet at sensor saturation) are scaled down together instead of
    /// clamped channel by channel, which keeps their colour (a bright red stays red, where
    /// `Clip` turns it pink or white); pixels at or near sensor saturation fade to white as in
    /// `Neutral`. Nothing changes below the white level. CPU only (the GPU backend decodes on
    /// the CPU for it).
    Blend,
    /// Rebuilds clipped channels from the unclipped ones: a model fitted on the unclipped
    /// blocks of the frame predicts a clipped channel (alone, or with one more clipped) from the
    /// others, the pixel keeps its colour above white as in `Blend`, and only pixels clipped in
    /// every channel fade to white. The model is global to the frame, so it works for highlights
    /// whose colour resembles the rest of the scene (skies, white or grey objects, skin) and
    /// not for a saturated coloured light source; frames it cannot fit (too few unclipped
    /// blocks, four-colour sensors, files without a colour filter array) behave as `Blend`.
    /// CPU only (the GPU backend decodes on the CPU for it).
    Reconstruct,
}

/// Where the white balance of RGB outputs comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum WhiteBalanceMode {
    /// The camera's balance recorded in the file (the default).
    #[default]
    AsShot,
    /// Starts from the camera's balance and refines it over the near-neutral areas of the
    /// frame (6x6 blocks that are mid-tone and have red and blue within 30 % of green, after
    /// the current balance), so that their means are equal; coloured areas that dominate the
    /// scene do not take part and the result stays within a factor of two of the camera's. A
    /// simple estimate, not a substitute for a grey card. Frames the camera already balanced,
    /// four-colour and Foveon sensors, files without a colour filter array and frames with too
    /// few near-neutral areas keep the as-shot balance. CPU only (the GPU backend decodes on
    /// the CPU for it).
    Auto,
    /// Daylight (D65) from the file's colour matrix: the neutral that the matrix maps to
    /// equal red, green and blue. Files without a usable matrix, frames the camera already
    /// balanced and files without a colour filter array keep the as-shot balance.
    Daylight,
}

/// How strongly the edge-aware demosaic smooths noise (`DecodeOptions::noise_reduction`).
///
/// The smoothing is part of [`Demosaic::EdgeAware`]: each pixel moves towards the mean of its
/// 3x3 window, weighted so that edges and detail are kept. Bilinear and half-size decodes do not
/// smooth. It is a light, colour-preserving filter, not a substitute for a dedicated denoiser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum NoiseReduction {
    /// Estimated from the noise of the frame: none on clean frames (the default).
    #[default]
    Auto,
    /// No smoothing at all.
    Off,
    /// At least a quarter of the way to the local mean, more on noisy frames.
    Low,
    /// At least half way.
    Medium,
    /// At least four fifths of the way.
    High,
}

/// What a decode does when the sensor data is corrupt or truncated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum CorruptionPolicy {
    /// Decode every complete sample that is present, zero-fill the rest, and report each
    /// repair in [`crate::RawFrame::warnings`] ([`crate::RawFrame::is_incomplete`] is then
    /// true): never silent. Applies to truncated strips, tiles and other uncompressed or
    /// packed payloads, to compressed DNG strips/tiles (a unit that is truncated or
    /// fails to decode is zero-filled) and to Nikon and Pentax Huffman and Samsung SRW
    /// streams cut short by the end of the file (the rows decoded from real data are kept).
    /// Other compressed streams (single entropy-coded streams such as CR2/CR3, ARW, RAF
    /// compressed data) fail as under `Strict`.
    /// The default.
    #[default]
    Recover,
    /// Fail with [`crate::Error::CorruptFile`] (with a structured
    /// [`crate::CorruptReason`]) when any sensor data needed for the output is missing.
    Strict,
}

impl CorruptionPolicy {
    /// Parse a policy name (`strict`, `recover`; case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "strict" => CorruptionPolicy::Strict,
            "recover" => CorruptionPolicy::Recover,
            _ => return None,
        })
    }

    /// Canonical name, accepted by [`CorruptionPolicy::parse`].
    pub fn name(self) -> &'static str {
        match self {
            CorruptionPolicy::Strict => "strict",
            CorruptionPolicy::Recover => "recover",
        }
    }
}

/// Options for [`crate::FlashRaw::decode`].
#[derive(Debug, Clone)]
pub struct DecodeOptions {
    /// Execution backend. Default: [`Backend::Auto`].
    pub backend: Backend,
    /// What the decode produces. Default: [`OutputFormat::RawU16`].
    pub output: OutputFormat,
    /// Worker threads. `None` (the default) uses all cores (the global thread pool);
    /// `Some(1)` decodes single-threaded on the calling thread; `Some(n)` uses a pool of `n`
    /// workers (shared by decodes with the same count; on Apple platforms its threads run
    /// with the user-initiated quality of service). `Some(0)` means the same as `None`.
    /// `n` is clamped to 256, at most 8 distinct pool sizes are kept alive (the oldest is
    /// released when a ninth is requested), and a pool that cannot be started falls back to
    /// the global pool.
    pub threads: Option<usize>,
    /// Decode only this region (sensor-buffer coordinates).
    pub region: Option<Rect>,
    /// Crop RGB outputs to the default crop (raw outputs are never cropped implicitly).
    pub crop: bool,
    /// Demosaic algorithm for RGB outputs. Default: [`Demosaic::Auto`].
    pub demosaic: Demosaic,
    /// Clipped highlights of RGB outputs. Default: [`Highlights::Clip`].
    pub highlights: Highlights,
    /// White balance of RGB outputs. Default: [`WhiteBalanceMode::AsShot`].
    pub white_balance: WhiteBalanceMode,
    /// Explicit red, green and blue multipliers of the camera RGB (green is normalised to 1),
    /// replacing `white_balance`. Values must be positive and finite or the decode fails with
    /// `Error::InvalidArgument`. Frames the camera already balanced, four-colour sensors and
    /// Foveon keep their own balance. CPU only. Default: none.
    pub white_balance_multipliers: Option<[f32; 3]>,
    /// Smoothing of the edge-aware demosaic. Default: [`NoiseReduction::Auto`].
    pub noise_reduction: NoiseReduction,
    /// Correct the lateral chromatic aberration of Bayer frames (16-bit, three colours) before
    /// demosaicing: radial scale factors of the red and blue planes against green are estimated
    /// from the frame and the red and blue samples resampled. Frames without enough edges, and
    /// other sensors, are left as they are. Applies to the RGB outputs; CPU only (the GPU backend
    /// decodes on the CPU for it). Default: false.
    pub chromatic_aberration: bool,
    /// Apply DNG opcode lists (bad pixels, lens-shading gain maps, ...) in their
    /// specified stages. Geometric opcodes are never applied. Default: true.
    pub apply_opcodes: bool,
    /// Safety limits for untrusted input.
    pub limits: Limits,
    /// Truncated or corrupt sensor data: decode what is there and report it in
    /// [`crate::RawFrame::warnings`] ([`CorruptionPolicy::Recover`], the default) or fail
    /// ([`CorruptionPolicy::Strict`]).
    pub corruption_policy: CorruptionPolicy,
    /// Exposure of RGB outputs in EV (stops): linear values are multiplied by
    /// `2^exposure` after the colour conversion, before the black/white points and the
    /// output encoding. A convenience control; tone curves and highlight recovery are
    /// left to editors. Range `[-32, 32]`. Default: 0.
    pub exposure: f32,
    /// Black point of RGB outputs in normalised linear units (after exposure): this value
    /// maps to 0. Integer outputs clip below it; float outputs keep the (negative) values.
    /// Default: 0.
    pub black_point: f32,
    /// White point of RGB outputs in normalised linear units (after exposure): this value
    /// maps to 1. Integer outputs clip above it; float outputs keep larger values. Must be
    /// above `black_point`. Default: 1.
    pub white_point: f32,
    /// When the sensor data cannot be decoded ([`crate::Error::UnsupportedCompression`],
    /// e.g. Nikon High Efficiency NEF), return the embedded full-size preview for the RGB
    /// outputs ([`OutputFormat::Rgba8`], [`OutputFormat::LinearRgbF32`]) instead of
    /// failing. The frame is marked [`crate::FrameSource::Preview`] and carries a
    /// [`crate::CorruptReason::PreviewFallback`] warning. Needs a preview covering at
    /// least 90% of the default crop in each dimension; never applies to sensor-domain
    /// outputs or region decodes. Default: false (the error is returned).
    pub preview_fallback: bool,
    /// Colour space of the RGB outputs ([`ColorSpace::Srgb`] by default). The primaries are
    /// part of the develop pass; the display outputs ([`OutputFormat::Rgba8`],
    /// [`OutputFormat::Rgb16`]) also use the space's transfer curve. Applies to every
    /// backend, region and tile decode. [`ColorSpace::Custom`] is rejected: use
    /// [`DecodeOptions::output_profile`].
    pub color_space: ColorSpace,
    /// Convert the RGB outputs to this ICC profile instead of `color_space` (needs the
    /// `icc` cargo feature). Matrix/TRC profiles cost nothing per pixel; profiles with
    /// lookup tables run a second, slower pass on the CPU. GPU decodes of such profiles
    /// fall back to the CPU. Default: none.
    pub output_profile: Option<OutputProfile>,
    /// Render the RGB outputs with this DNG camera profile ([`CameraProfile`]): its colour
    /// matrices (the forward matrix when it has one) replace the file's, and its hue/sat map,
    /// look table and tone curve change the colours and the tone. Take one from a `.dcp` file
    /// ([`CameraProfile::from_dcp`]) or from the DNG being decoded
    /// ([`crate::FlashRaw::embedded_camera_profile`]). Applies to [`OutputFormat::Rgba8`],
    /// [`OutputFormat::Rgb16`] and the linear RGB outputs of three-colour sensors; `exposure`
    /// and the black and white points are applied after it. CPU only: GPU decodes fall back
    /// to the CPU. Default: none, which leaves the output bit-identical to a decode without
    /// the option.
    pub camera_profile: Option<CameraProfile>,
}

impl DecodeOptions {
    /// Set the output colour space.
    pub fn with_color_space(mut self, cs: ColorSpace) -> Self {
        self.color_space = cs;
        self
    }

    /// Set the output ICC profile.
    pub fn with_output_profile(mut self, p: OutputProfile) -> Self {
        self.output_profile = Some(p);
        self
    }
}

impl Default for DecodeOptions {
    fn default() -> Self {
        DecodeOptions {
            backend: Backend::Auto,
            output: OutputFormat::RawU16,
            threads: None,
            region: None,
            crop: true,
            demosaic: Demosaic::Bilinear,
            highlights: Highlights::Clip,
            white_balance: WhiteBalanceMode::AsShot,
            white_balance_multipliers: None,
            noise_reduction: NoiseReduction::Auto,
            chromatic_aberration: false,
            apply_opcodes: true,
            limits: Limits::default(),
            corruption_policy: CorruptionPolicy::Recover,
            exposure: 0.0,
            black_point: 0.0,
            white_point: 1.0,
            preview_fallback: false,
            color_space: ColorSpace::Srgb,
            output_profile: None,
            camera_profile: None,
        }
    }
}

impl Demosaic {
    /// True for the algorithms that need a Bayer sensor and are rejected with
    /// [`crate::Error::InvalidArgument`] on other sensors when an RGB output is requested.
    pub fn requires_bayer(self) -> bool {
        matches!(self, Demosaic::Ppg | Demosaic::Rcd)
    }

    /// Parse a demosaic name (`bilinear`, `half`/`half-size`, `edge-aware`, `ahd`, `auto`; case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "bilinear" => Demosaic::Bilinear,
            "half" | "half-size" | "halfsize" => Demosaic::HalfSize,
            "edge-aware" | "edgeaware" => Demosaic::EdgeAware,
            "ahd" => Demosaic::Ahd,
            "ppg" => Demosaic::Ppg,
            "rcd" => Demosaic::Rcd,
            "auto" => Demosaic::Auto,
            _ => return None,
        })
    }

    /// Canonical name, accepted by [`Demosaic::parse`].
    pub fn name(self) -> &'static str {
        match self {
            Demosaic::Bilinear => "bilinear",
            Demosaic::HalfSize => "half-size",
            Demosaic::EdgeAware => "edge-aware",
            Demosaic::Ahd => "ahd",
            Demosaic::Ppg => "ppg",
            Demosaic::Rcd => "rcd",
            Demosaic::Auto => "auto",
        }
    }

    /// The concrete algorithm [`Demosaic::Auto`] stands for on a sensor with this pattern
    /// (any other value is returned as is). `None` is a sensor without a colour filter array.
    pub fn resolve(self, cfa: Option<&flashraw_core::CfaPattern>) -> Demosaic {
        match (self, cfa) {
            (Demosaic::Auto, Some(c)) if c.is_bayer() => Demosaic::Ahd,
            (Demosaic::Auto, Some(c)) if (c.width(), c.height()) == (6, 6) => Demosaic::EdgeAware,
            (Demosaic::Auto, _) => Demosaic::Bilinear,
            (d, _) => d,
        }
    }

    /// [`Demosaic::resolve`] for a decode with `noise` smoothing: an explicit smoothing level
    /// (`Low`, `Medium`, `High`) selects the edge-aware demosaic, the one that smooths, for
    /// [`Demosaic::Auto`] on Bayer sensors too.
    pub fn resolve_for(self, cfa: Option<&flashraw_core::CfaPattern>, noise: NoiseReduction) -> Demosaic {
        let smoothing = matches!(noise, NoiseReduction::Low | NoiseReduction::Medium | NoiseReduction::High);
        match self.resolve(cfa) {
            Demosaic::Ahd if self == Demosaic::Auto && smoothing => Demosaic::EdgeAware,
            d => d,
        }
    }
}

impl Highlights {
    /// Parse a highlight mode name (`clip`, `neutral`, `blend`, `reconstruct`; case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "clip" => Highlights::Clip,
            "neutral" => Highlights::Neutral,
            "blend" => Highlights::Blend,
            "reconstruct" => Highlights::Reconstruct,
            _ => return None,
        })
    }

    /// Canonical name, accepted by [`Highlights::parse`].
    pub fn name(self) -> &'static str {
        match self {
            Highlights::Clip => "clip",
            Highlights::Neutral => "neutral",
            Highlights::Blend => "blend",
            Highlights::Reconstruct => "reconstruct",
        }
    }
}

impl Backend {
    /// Canonical name, accepted by [`Backend::parse`].
    pub fn name(self) -> &'static str {
        match self {
            Backend::Auto => "auto",
            Backend::Cpu => "cpu",
            Backend::CpuSimd => "simd",
            Backend::Gpu => "gpu",
        }
    }
}

impl OutputFormat {
    /// Canonical name, accepted by [`OutputFormat::parse`].
    pub fn name(self) -> &'static str {
        match self {
            OutputFormat::RawU16 => "raw-u16",
            OutputFormat::RawF32 => "raw-f32",
            OutputFormat::NormalizedF32 => "normalized",
            OutputFormat::LinearRgbF32 => "linear-rgb",
            OutputFormat::Rgba8 => "rgba8",
            OutputFormat::Rgb16 => "rgb16",
            OutputFormat::LinearRgb16 => "linear-rgb16",
            OutputFormat::LinearRgbF16 => "linear-rgb-f16",
            OutputFormat::GpuRawU16 => "gpu-raw-u16",
            OutputFormat::GpuNormalizedF32 => "gpu-normalized",
            OutputFormat::GpuLinearRgbF32 => "gpu-linear-rgb",
            OutputFormat::GpuRgba8 => "gpu-rgba8",
        }
    }
}

/// Error returned when parsing an unknown option name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOptionError {
    kind: &'static str,
    value: String,
}

impl std::fmt::Display for ParseOptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown {} {:?}", self.kind, self.value)
    }
}

impl std::error::Error for ParseOptionError {}

macro_rules! name_traits {
    ($t:ty, $kind:literal) => {
        impl std::str::FromStr for $t {
            type Err = ParseOptionError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                <$t>::parse(s).ok_or_else(|| ParseOptionError { kind: $kind, value: s.to_string() })
            }
        }

        impl std::fmt::Display for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.name())
            }
        }
    };
}

name_traits!(Backend, "backend");
name_traits!(OutputFormat, "output format");
name_traits!(Demosaic, "demosaic");
name_traits!(Highlights, "highlight mode");
name_traits!(WhiteBalanceMode, "white balance mode");
name_traits!(CorruptionPolicy, "corruption policy");
name_traits!(ColorSpace, "color space");
name_traits!(RenderingIntent, "rendering intent");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for b in [Backend::Auto, Backend::Cpu, Backend::CpuSimd, Backend::Gpu] {
            assert_eq!(b.to_string().parse::<Backend>(), Ok(b));
        }
        for o in [
            OutputFormat::RawU16,
            OutputFormat::RawF32,
            OutputFormat::NormalizedF32,
            OutputFormat::LinearRgbF32,
            OutputFormat::Rgba8,
            OutputFormat::Rgb16,
            OutputFormat::LinearRgb16,
            OutputFormat::LinearRgbF16,
            OutputFormat::GpuRawU16,
            OutputFormat::GpuNormalizedF32,
            OutputFormat::GpuLinearRgbF32,
            OutputFormat::GpuRgba8,
        ] {
            assert_eq!(o.to_string().parse::<OutputFormat>(), Ok(o));
        }
        for h in [Highlights::Clip, Highlights::Neutral, Highlights::Blend, Highlights::Reconstruct] {
            assert_eq!(h.to_string().parse::<Highlights>(), Ok(h));
        }
        for m in [WhiteBalanceMode::AsShot, WhiteBalanceMode::Auto] {
            assert_eq!(m.to_string().parse::<WhiteBalanceMode>(), Ok(m));
        }
        for d in [
            Demosaic::Bilinear,
            Demosaic::HalfSize,
            Demosaic::EdgeAware,
            Demosaic::Ahd,
            Demosaic::Ppg,
            Demosaic::Rcd,
            Demosaic::Auto,
        ] {
            assert_eq!(d.to_string().parse::<Demosaic>(), Ok(d));
        }
        for p in [CorruptionPolicy::Strict, CorruptionPolicy::Recover] {
            assert_eq!(p.to_string().parse::<CorruptionPolicy>(), Ok(p));
        }
        assert_eq!("nope".parse::<Backend>().unwrap_err().to_string(), "unknown backend \"nope\"");
    }
}
