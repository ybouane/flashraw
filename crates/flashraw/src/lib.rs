//! # FlashRaw
//!
//! Fast, safe RAW image decoding.
//!
//! ```no_run
//! use flashraw::{FlashRaw, DecodeOptions, Backend, OutputFormat};
//!
//! # fn main() -> flashraw::Result<()> {
//! let raw = FlashRaw::open_path("photo.NEF")?;
//! println!("{} {}x{}", raw.metadata().camera(), raw.metadata().width, raw.metadata().height);
//!
//! // Embedded JPEG preview without touching the sensor data:
//! if let Some(p) = raw.preview() {
//!     std::fs::write("preview.jpg", raw.preview_bytes(p)?)?;
//! }
//!
//! let frame = raw.decode(DecodeOptions {
//!     backend: Backend::Auto,
//!     output: OutputFormat::RawU16,
//!     ..Default::default()
//! })?;
//! println!("{}x{} via {}", frame.width(), frame.height(), frame.stats().backend.decode);
//! # Ok(()) }
//! ```
//!
//! The decode result is sensor-domain data ([`RawFrame`]) plus everything needed to
//! interpret it (CFA, black/white levels, white balance, colour matrices, crops).
//! Convenience RGB outputs are available via [`OutputFormat`].

#![deny(unsafe_op_in_unsafe_fn)]

mod frame;
// GPU backend (see crate::gpu and docs/gpu.md).
#[cfg(feature = "gpu")]
pub mod gpu;
mod options;
pub mod process;
mod source;

pub use flashraw_core::{
    BlackLevel, CfaColor, CfaPattern, ColorMatrices, ColorMatrix, ColorSource, Compression, DecodeUnit, Error,
    ExposureInfo, FrameInfo, GpsInfo, Illuminant, LensInfo, Limits, Orientation, PreviewEncoding, PreviewInfo,
    PreviewKind, RandomAccess, RawFormat, RawMetadata, Rect, Result, SampleFormat, SensorLayout, Stage, WhiteBalance,
    WhiteLevel,
};
pub use frame::{BackendReport, DecodeStatistics, RawFrame, RawPixelData};
pub use options::{Backend, DecodeOptions, Demosaic, OutputFormat};
pub use source::Source;

/// Low-level access for advanced users and tooling.
pub mod low_level {
    pub use flashraw_codecs as codecs;
    pub use flashraw_core as core;
    pub use flashraw_formats as formats;
}

/// Alias matching the conceptual name used in documentation.
pub type RawFile<'a> = FlashRaw<'a>;

use flashraw_core::{DecodeContext, PixelData, Profiler, RawDecoder, RawImage};

/// An opened RAW file. Metadata and previews are available immediately; sensor
/// data is decoded on demand with [`FlashRaw::decode`].
pub struct FlashRaw<'a> {
    // Field order matters: `decoder` borrows from `source` and must be dropped first
    // (Rust drops fields in declaration order).
    decoder: Box<dyn RawDecoder + 'a>,
    source: Source<'a>,
    parse_ms: f64,
}

impl std::fmt::Debug for FlashRaw<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlashRaw").field("format", &self.format()).field("camera", &self.metadata().camera()).finish()
    }
}

impl<'a> FlashRaw<'a> {
    /// Open from any [`Source`]: `&[u8]` (zero copy), `Vec<u8>`, `Arc<[u8]>`, or an mmap.
    pub fn open(source: impl Into<Source<'a>>) -> Result<Self> {
        Self::open_with(source, Limits::default())
    }

    /// Open with explicit safety limits.
    pub fn open_with(source: impl Into<Source<'a>>, limits: Limits) -> Result<Self> {
        let source = source.into();
        let t0 = flashraw_core::profile::now_ms();
        let bytes: &[u8] = source.bytes();
        // SAFETY: `bytes` points into memory owned by `source` (a borrowed slice living
        // for 'a, or a Vec/Arc/Mmap whose heap data or mapping does not move when the
        // `Source` value moves). That memory is never mutated or freed while the decoder
        // exists: `source` is stored next to the decoder in `FlashRaw`, declared after
        // it so the decoder is dropped first, and no `&mut` access to `source` is ever
        // exposed. Extending the slice lifetime to 'a is therefore sound.
        let bytes: &'a [u8] = unsafe { std::slice::from_raw_parts(bytes.as_ptr(), bytes.len()) };
        let decoder = flashraw_formats::open(bytes, &flashraw_formats::OpenOptions { limits })?;
        let parse_ms = flashraw_core::profile::now_ms() - t0;
        Ok(FlashRaw { decoder, source, parse_ms })
    }

    /// Normalised metadata (no pixel decoding involved).
    pub fn metadata(&self) -> &RawMetadata {
        self.decoder.metadata()
    }

    /// Geometry and calibration of the sensor buffer (known before decoding).
    pub fn frame_info(&self) -> &FrameInfo {
        self.decoder.frame_info()
    }

    pub fn format(&self) -> RawFormat {
        self.decoder.format()
    }

    /// The input bytes.
    pub fn bytes(&self) -> &[u8] {
        self.source.bytes()
    }

    /// Time spent parsing the container and metadata in `open`.
    pub fn parse_ms(&self) -> f64 {
        self.parse_ms
    }

    /// What this file supports (random access, tiles, previews, GPU unpack).
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: self.decoder.random_access(),
            units: self.decoder.units().len(),
            previews: self.metadata().previews.len(),
            gpu_unpack: self.decoder.packed_payload().is_some(),
            float: self.frame_info().sample_format == SampleFormat::F32,
        }
    }

    /// Independently decodable tiles/strips.
    pub fn units(&self) -> Vec<DecodeUnit> {
        self.decoder.units()
    }

    /// All embedded previews.
    pub fn previews(&self) -> &[PreviewInfo] {
        &self.metadata().previews
    }

    /// Largest embedded preview.
    pub fn preview(&self) -> Option<&PreviewInfo> {
        self.metadata().largest_preview()
    }

    /// Smallest embedded preview (thumbnail).
    pub fn thumbnail(&self) -> Option<&PreviewInfo> {
        self.metadata().smallest_preview()
    }

    /// Raw bytes of an embedded preview (e.g. a JPEG), zero-copy.
    pub fn preview_bytes(&self, p: &PreviewInfo) -> Result<&[u8]> {
        flashraw_core::bytes::slice_at(self.bytes(), p.offset as usize, p.length as usize)
    }

    /// Decode a preview to 8-bit RGB (`(width, height, rgb)`).
    pub fn decode_preview_rgb(&self, p: &PreviewInfo) -> Result<(u32, u32, Vec<u8>)> {
        let b = self.preview_bytes(p)?;
        match p.encoding {
            PreviewEncoding::Jpeg => {
                let d = flashraw_codecs::jpeg::decode(b)?;
                let rgb = if d.components == 1 { d.data.iter().flat_map(|&v| [v, v, v]).collect() } else { d.data };
                Ok((d.width as u32, d.height as u32, rgb))
            }
            PreviewEncoding::Rgb8 => Ok((p.width, p.height, b.to_vec())),
            e => Err(Error::NotAvailable(format!("preview encoding {e:?} cannot be decoded"))),
        }
    }

    /// Decode the sensor data.
    pub fn decode(&self, opts: DecodeOptions) -> Result<RawFrame> {
        let pool = thread_pool(opts.threads);
        match pool {
            #[cfg(feature = "rayon")]
            Some(p) => p.install(|| self.decode_inner(&opts)),
            _ => self.decode_inner(&opts),
        }
    }

    /// Decode only `region` (sensor-buffer coordinates). Formats with tiles or
    /// row-addressable payloads decode only the data needed; others decode fully
    /// and crop (see [`FlashRaw::capabilities`]).
    pub fn decode_region(&self, region: Rect, mut opts: DecodeOptions) -> Result<RawFrame> {
        opts.region = Some(region);
        self.decode(opts)
    }

    /// Decode a single tile/strip by index (see [`FlashRaw::units`]).
    pub fn decode_tile(&self, index: usize, opts: DecodeOptions) -> Result<RawFrame> {
        let u = self.units();
        let unit = u
            .get(index)
            .ok_or_else(|| Error::InvalidArgument(format!("tile {index} out of range ({} tiles)", u.len())))?;
        self.decode_region(unit.rect, opts)
    }

    fn decode_inner(&self, opts: &DecodeOptions) -> Result<RawFrame> {
        let t0 = flashraw_core::profile::now_ms();
        let profiler = Profiler::new();
        profiler.record(Stage::ContainerParse, self.parse_ms);
        let info = self.decoder.frame_info();
        let simd_level = flashraw_core::simd::kernel_level();
        let use_simd = !matches!(opts.backend, Backend::Cpu) && cfg!(feature = "simd");
        let threads = flashraw_core::par::threads();
        if opts.backend == Backend::Gpu && !gpu_available() {
            return Err(Error::GpuUnavailable(
                "FlashRaw was built without the `gpu` feature or no adapter was found".into(),
            ));
        }
        // ---- GPU backend hook (crate::gpu): GPU/Auto selection and GPU-resident outputs ----
        #[cfg(feature = "gpu")]
        if let Some(frame) = gpu::decode(self, opts, t0, &profiler)? {
            return Ok(frame);
        }
        if opts.output.is_gpu_resident() {
            return Err(Error::GpuUnavailable(format!(
                "{:?} requires the `gpu` feature and a GPU backend",
                opts.output
            )));
        }
        // ---- end GPU hook ----
        let ctx = DecodeContext { limits: opts.limits, parallel: threads > 1, simd: use_simd, profiler: &profiler };
        let full = Rect::full(info.width, info.height);
        let region = match opts.region {
            Some(r) => {
                Some(r.intersect(&full).ok_or_else(|| Error::InvalidArgument(format!("region {r} outside {full}")))?)
            }
            None => None,
        };
        let img = match region {
            Some(r) if r != full => self.decoder.decode_region(&ctx, r)?,
            _ => self.decoder.decode(&ctx)?,
        };
        let origin = region.map(|r| (r.x, r.y)).unwrap_or((0, 0));
        let frame_rect = Rect::new(origin.0, origin.1, img.width, img.height);
        let translate = |r: Rect| -> Rect {
            r.intersect(&frame_rect)
                .map(|i| Rect::new(i.x - origin.0, i.y - origin.1, i.width, i.height))
                .unwrap_or_default()
        };
        let mut black = info.black_level.clone();
        if info.black_from_masked {
            if let Some(b) = profiler.time(Stage::Calibration, || process::black_from_masked(&img, info)) {
                black = b;
            }
        }
        let decode_backend = format!(
            "CPU {} x{} thread{}",
            if use_simd { simd_level.name() } else { "scalar" },
            threads,
            if threads == 1 { "" } else { "s" }
        );
        let mut postprocess = None;
        let mut frame_info_for_post = info.clone();
        frame_info_for_post.black_level = black.clone();
        let mut float_scaled: Option<f32> = None;
        let (width, height, cpp, data) = match opts.output {
            OutputFormat::RawU16 => match img.data {
                PixelData::U16(v) => (img.width, img.height, img.cpp, RawPixelData::U16(v)),
                PixelData::F32(v) => {
                    let (v, scale) = float_to_u16(&v, info.white_level.max());
                    if scale != 1.0 {
                        float_scaled = Some(scale);
                    }
                    (img.width, img.height, img.cpp, RawPixelData::U16(v))
                }
            },
            OutputFormat::RawF32 => {
                let v = match img.data {
                    PixelData::U16(v) => v.into_iter().map(|x| x as f32).collect(),
                    PixelData::F32(v) => v,
                };
                (img.width, img.height, img.cpp, RawPixelData::F32(v))
            }
            OutputFormat::NormalizedF32 => {
                postprocess = Some(format!("CPU x{threads}"));
                let v = profiler
                    .time(Stage::Postprocess, || process::normalize(&img, &frame_info_for_post, origin, ctx.parallel));
                (img.width, img.height, img.cpp, RawPixelData::F32(v))
            }
            // GPU-resident outputs are handled (or rejected) by the GPU hook above.
            OutputFormat::GpuRawU16
            | OutputFormat::GpuNormalizedF32
            | OutputFormat::GpuLinearRgbF32
            | OutputFormat::GpuRgba8 => return Err(Error::GpuUnavailable("GPU-resident output".into())),
            OutputFormat::LinearRgbF32 | OutputFormat::Rgba8 => {
                postprocess = Some(format!("CPU x{threads}"));
                let crop = if opts.crop { translate(info.crop) } else { Rect::full(img.width, img.height) };
                let crop = if crop.is_empty() { Rect::full(img.width, img.height) } else { crop };
                let params = process::DevelopParams::new(&frame_info_for_post, opts.demosaic, crop);
                if opts.output == OutputFormat::Rgba8 {
                    let (w, h, px) = profiler.time(Stage::Postprocess, || {
                        process::develop_rgba8(&img, &frame_info_for_post, origin, &params, ctx.parallel)
                    })?;
                    (w, h, 4, RawPixelData::U8(px))
                } else {
                    let dev = profiler.time(Stage::Postprocess, || {
                        process::develop(&img, &frame_info_for_post, origin, &params, ctx.parallel)
                    })?;
                    (dev.width, dev.height, 3, RawPixelData::F32(dev.rgb))
                }
            }
        };
        let raw_output =
            matches!(opts.output, OutputFormat::RawU16 | OutputFormat::RawF32 | OutputFormat::NormalizedF32);
        let total = flashraw_core::profile::now_ms() - t0 + self.parse_ms;
        let output_bytes = data.byte_len() as u64;
        let stats = DecodeStatistics {
            backend: BackendReport {
                decode: decode_backend,
                postprocess,
                simd: if use_simd { simd_level.name().to_string() } else { "scalar".into() },
                threads,
                gpu_adapter: None,
            },
            stages: profiler.stages(),
            total_ms: total,
            input_bytes: self.bytes().len() as u64,
            output_bytes,
        };
        Ok(RawFrame {
            width,
            height,
            cpp,
            active_area: if raw_output { translate(info.active_area) } else { Rect::full(width, height) },
            crop: if raw_output { translate(info.crop) } else { Rect::full(width, height) },
            origin,
            data,
            output: opts.output,
            bits_per_sample: info.bits_per_sample,
            cfa: if raw_output { info.cfa } else { None },
            sensor_layout: if raw_output { info.layout } else { SensorLayout::Linear },
            masked_areas: if raw_output {
                info.masked_areas.iter().map(|r| translate(*r)).filter(|r| !r.is_empty()).collect()
            } else {
                vec![]
            },
            black_level: match float_scaled {
                Some(m) => BlackLevel { values: black.values.iter().map(|v| (v * m).trunc()).collect(), ..black },
                None => black,
            },
            white_level: match float_scaled {
                Some(_) => WhiteLevel::uniform(FLOAT_TARGET as u32),
                None => info.white_level.clone(),
            },
            white_balance: info.white_balance,
            color_matrices: info.color_matrices.clone(),
            orientation: if opts.output == OutputFormat::Rgba8 { Orientation::Normal } else { info.orientation },
            metadata: self.metadata().clone(),
            stats,
        })
    }

    /// Decode into a caller-provided [`RawImage`]-compatible u16 buffer is not exposed;
    /// use [`FlashRaw::decode`]. This helper decodes and returns the raw image without
    /// building a [`RawFrame`] (used by benchmarks).
    pub fn decode_raw_image(&self, opts: &DecodeOptions) -> Result<RawImage> {
        let profiler = Profiler::new();
        let ctx = DecodeContext {
            limits: opts.limits,
            parallel: flashraw_core::par::threads() > 1,
            simd: !matches!(opts.backend, Backend::Cpu),
            profiler: &profiler,
        };
        self.decoder.decode(&ctx)
    }
}

impl FlashRaw<'static> {
    /// Open a file from disk (memory-mapped when the `mmap` feature is enabled).
    pub fn open_path(path: impl AsRef<std::path::Path>) -> Result<Self> {
        #[cfg(feature = "mmap")]
        {
            let f = std::fs::File::open(path.as_ref())?;
            // SAFETY: the file is mapped read-only. As with every mmap-based reader, a
            // concurrent truncation by another process could fault; FlashRaw never
            // writes through the mapping and all parsing is bounds-checked against the
            // length captured at map time.
            let m = unsafe { memmap2::Mmap::map(&f)? };
            Self::open(m)
        }
        #[cfg(not(feature = "mmap"))]
        {
            Self::open(std::fs::read(path.as_ref())?)
        }
    }

    /// Read everything from `reader` and open it.
    pub fn from_reader(mut reader: impl std::io::Read) -> Result<Self> {
        let mut v = Vec::new();
        reader.read_to_end(&mut v)?;
        Self::open(v)
    }
}

const FLOAT_TARGET: f32 = 16383.0;

/// integer output): if the larger of the white level and the data maximum lies
/// outside [4096, 32767], samples are scaled so that maximum maps to 16383; values
/// are clamped at 0 and truncated. Returns the samples and the scale applied.
fn float_to_u16(v: &[f32], white: u32) -> (Vec<u16>, f32) {
    let data_max = v.iter().copied().fold(0f32, f32::max);
    let tmax = (white.max(1) as f32).max(data_max).max(1.0);
    let scale = if !(4096.0..=32767.0).contains(&tmax) { FLOAT_TARGET / tmax } else { 1.0 };
    (v.iter().map(|&x| (x.max(0.0) * scale) as u16).collect(), scale)
}

/// File capabilities.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Capabilities {
    pub random_access: RandomAccess,
    /// Number of independently decodable units.
    pub units: usize,
    pub previews: usize,
    /// The payload is uniformly bit-packed and can be unpacked on the GPU.
    pub gpu_unpack: bool,
    pub float: bool,
}

/// Cheap check whether bytes look like a supported RAW file.
pub fn supports(data: &[u8]) -> Option<RawFormat> {
    flashraw_formats::detect(data)
}

/// Metadata only (no sensor decoding).
pub fn read_metadata(data: &[u8]) -> Result<RawMetadata> {
    Ok(FlashRaw::open(data)?.metadata().clone())
}

/// Whether a GPU backend is compiled in and an adapter is available.
pub fn gpu_available() -> bool {
    #[cfg(feature = "gpu")]
    {
        flashraw_gpu::available()
    }
    #[cfg(not(feature = "gpu"))]
    {
        false
    }
}

#[cfg(feature = "rayon")]
fn thread_pool(threads: Option<usize>) -> Option<std::sync::Arc<rayon::ThreadPool>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    let n = threads?;
    if n == 0 {
        return None;
    }
    static POOLS: OnceLock<Mutex<HashMap<usize, Arc<rayon::ThreadPool>>>> = OnceLock::new();
    let mut pools = POOLS.get_or_init(Default::default).lock().ok()?;
    if let Some(p) = pools.get(&n) {
        return Some(p.clone());
    }
    let p = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(n).build().ok()?);
    pools.insert(n, p.clone());
    Some(p)
}

#[cfg(not(feature = "rayon"))]
fn thread_pool(_threads: Option<usize>) -> Option<()> {
    None
}
