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
#![warn(missing_docs)]

mod frame;
// GPU backend (see crate::gpu and docs/gpu.md).
#[cfg(feature = "gpu")]
pub mod gpu;
mod opcodes;
mod options;
// Host-side develop kernels, shared with the GPU crate's equivalence tests. Not part
// of the stable API.
#[doc(hidden)]
pub mod process;
mod source;

pub use flashraw_core::{
    BlackLevel, CfaColor, CfaPattern, ColorMatrices, ColorMatrix, ColorSource, Compression, CorruptReason, DataUnit,
    DecodeUnit, DecodeWarning, DngExtras, DngProfileInfo, Error, ExposureInfo, FrameInfo, GpsInfo, Illuminant,
    LensInfo, Limits, Orientation, OrientationMetadata, OrientationPolicy, OrientationSource, PreviewEncoding,
    PreviewInfo, PreviewKind, RandomAccess, RawFormat, RawMetadata, Rect, Result, SampleFormat, SensorLayout, Stage,
    Truncation, WhiteBalance, WhiteLevel,
};
pub use frame::{BackendReport, DecodeStatistics, RawFrame, RawPixelData};
pub use options::{Backend, CorruptionPolicy, DecodeOptions, Demosaic, OutputFormat, ParseOptionError};
pub use source::Source;

/// Low-level access for advanced users and tooling: the internal crates. Not covered
/// by semver guarantees; may change in any release.
#[doc(hidden)]
pub mod low_level {
    pub use flashraw_codecs as codecs;
    pub use flashraw_core as core;
    pub use flashraw_formats as formats;
}

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

    /// Container format of the file.
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

    /// Decode a JPEG or uncompressed RGB preview to 8-bit RGB. Orientation is not
    /// applied (see [`PreviewInfo::orientation`]).
    pub fn decode_preview_rgb(&self, p: &PreviewInfo) -> Result<PreviewImage> {
        let b = self.preview_bytes(p)?;
        match p.encoding {
            PreviewEncoding::Jpeg => {
                let d = flashraw_codecs::jpeg::decode(b)?;
                let rgb = if d.components == 1 { d.data.iter().flat_map(|&v| [v, v, v]).collect() } else { d.data };
                Ok(PreviewImage { width: d.width as u32, height: d.height as u32, rgb })
            }
            PreviewEncoding::Rgb8 => Ok(PreviewImage { width: p.width, height: p.height, rgb: b.to_vec() }),
            e => Err(Error::NotAvailable(format!("preview encoding {e:?} cannot be decoded"))),
        }
    }

    /// Decode the sensor data.
    pub fn decode(&self, opts: DecodeOptions) -> Result<RawFrame> {
        // One thread: run on the calling thread rather than on a one-worker pool, so the
        // decode keeps the caller's scheduling class (a lone pool worker may be placed on
        // an efficiency core).
        if opts.threads == Some(1) {
            return flashraw_core::par::sequential(|| self.decode_inner(&opts));
        }
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

    /// Decode `win` (sensor-buffer coordinates; the whole buffer or a region) and, for
    /// formats whose black level comes from masked (optical black) areas, measure it. Those
    /// areas usually lie outside a cropped window (e.g. the active area), so the decode then
    /// covers the window plus the masked areas and is cropped back afterwards: a region must
    /// normalise exactly like the same pixels of a full decode.
    pub(crate) fn decode_window(
        &self,
        ctx: &DecodeContext,
        win: Rect,
        profiler: &Profiler,
    ) -> Result<(RawImage, Option<flashraw_core::BlackLevel>)> {
        let info = self.decoder.frame_info();
        let full = Rect::full(info.width, info.height);
        let decode = |r: Rect| if r == full { self.decoder.decode(ctx) } else { self.decoder.decode_region(ctx, r) };
        if !info.black_from_masked || info.masked_areas.is_empty() {
            return Ok((decode(win)?, None));
        }
        let span = info.masked_areas.iter().fold(win, |a, m| {
            Rect::from_ltrb(a.x.min(m.x), a.y.min(m.y), a.right().max(m.right()), a.bottom().max(m.bottom()))
                .unwrap_or(a)
                .intersect(&full)
                .unwrap_or(a)
        });
        let img = decode(span)?;
        let black = profiler.time(Stage::Calibration, || process::black_from_masked_at(&img, info, (span.x, span.y)));
        let img =
            if span == win { img } else { img.crop(Rect::new(win.x - span.x, win.y - span.y, win.width, win.height))? };
        Ok((img, black))
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
        let ctx = decode_context(opts, &profiler, threads > 1, use_simd);
        let full = Rect::full(info.width, info.height);
        let region = match opts.region {
            Some(r) => {
                Some(r.intersect(&full).ok_or_else(|| Error::InvalidArgument(format!("region {r} outside {full}")))?)
            }
            None => None,
        };
        let (mut img, measured) = self.decode_window(&ctx, region.unwrap_or(full), &profiler)?;
        let origin = region.map(|r| (r.x, r.y)).unwrap_or((0, 0));
        let mut lists = opcodes::Lists::of(info);
        if let Some(l) = &mut lists {
            if opts.apply_opcodes {
                profiler.time(Stage::Calibration, || l.apply_list1(&mut img, info, origin, ctx.parallel));
            } else {
                (0..3).for_each(|i| l.record_unused(i, "disabled"));
            }
        }
        let frame_rect = Rect::new(origin.0, origin.1, img.width, img.height);
        let translate = |r: Rect| -> Rect {
            r.intersect(&frame_rect)
                .map(|i| Rect::new(i.x - origin.0, i.y - origin.1, i.width, i.height))
                .unwrap_or_default()
        };
        let black = measured.unwrap_or_else(|| info.black_level.clone());
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
                    let (v, scale) = float_to_u16(&v, info.white_level.max(), img.float_max);
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
                let mut v = profiler
                    .time(Stage::Postprocess, || process::normalize(&img, &frame_info_for_post, origin, ctx.parallel));
                if let Some(l) = lists.as_mut().filter(|_| opts.apply_opcodes) {
                    profiler.time(Stage::Postprocess, || {
                        l.apply_list2(&mut v, img.width, img.cpp, &frame_info_for_post, origin, ctx.parallel)
                    });
                    l.record_unused(2, "applies after demosaicing");
                }
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
                let staged =
                    opts.apply_opcodes && lists.as_ref().is_some_and(|l| l.has_pixel_ops(1) || l.has_pixel_ops(2));
                if let Some(l) = lists.as_mut().filter(|_| staged) {
                    profiler.time(Stage::Postprocess, || {
                        l.develop(
                            &img,
                            &frame_info_for_post,
                            origin,
                            opts.demosaic,
                            crop,
                            opts.output == OutputFormat::Rgba8,
                            ctx.parallel,
                        )
                    })?
                } else if opts.output == OutputFormat::Rgba8 {
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
        if let Some(l) = lists.as_mut().filter(|_| opts.apply_opcodes) {
            if matches!(opts.output, OutputFormat::RawU16 | OutputFormat::RawF32) {
                (1..3).for_each(|i| l.record_unused(i, "sensor-domain output"));
            }
        }
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
            opcodes: lists.map(|l| l.into_notes()).unwrap_or_default(),
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
            warnings: ctx.take_warnings(),
        })
    }

    /// Decode and return the internal raw image without building a [`RawFrame`] (used by
    /// benchmarks). Not part of the stable API; use [`FlashRaw::decode`].
    #[doc(hidden)]
    pub fn decode_raw_image(&self, opts: &DecodeOptions) -> Result<RawImage> {
        let profiler = Profiler::new();
        let ctx =
            decode_context(opts, &profiler, flashraw_core::par::threads() > 1, !matches!(opts.backend, Backend::Cpu));
        self.decoder.decode(&ctx)
    }
}

/// The decoder context for `opts`.
pub(crate) fn decode_context<'p>(
    opts: &DecodeOptions,
    profiler: &'p Profiler,
    parallel: bool,
    simd: bool,
) -> DecodeContext<'p> {
    let mut ctx = DecodeContext::new(profiler);
    ctx.limits = opts.limits;
    ctx.parallel = parallel;
    ctx.simd = simd;
    ctx.recover = opts.corruption_policy == CorruptionPolicy::Recover;
    ctx
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

/// Convert floating-point samples to u16 (for integer output): if the larger of the white level and the data maximum lies
/// outside [4096, 32767], samples are scaled so that maximum maps to 16383; values
/// are clamped at 0 and truncated. Returns the samples and the scale applied.
fn float_to_u16(v: &[f32], white: u32, known_max: Option<f32>) -> (Vec<u16>, f32) {
    const CHUNK: usize = 1 << 16;
    // Largest non-negative, non-NaN sample: measured by the decoder when it could, else
    // one parallel pass.
    let data_max = match known_max {
        Some(m) => m,
        None => {
            let chunks: Vec<&[f32]> = v.chunks(CHUNK).collect();
            let bits = flashraw_core::par::map(chunks, true, flashraw_core::max_nonneg_bits).into_iter().max();
            f32::from_bits(bits.unwrap_or(0))
        }
    };
    let tmax = (white.max(1) as f32).max(data_max).max(1.0);
    let scale = if !(4096.0..=32767.0).contains(&tmax) { FLOAT_TARGET / tmax } else { 1.0 };
    let mut out = flashraw_core::alloc_zeroed::<u16>(v.len()).unwrap_or_else(|_| vec![0; v.len()]);
    // Memory-bound: a few workers saturate bandwidth (see par::memory_bound_workers).
    let workers = flashraw_core::par::memory_bound_workers();
    flashraw_core::par::for_each_row_chunk_bounded(&mut out, CHUNK, workers, true, |first, o| {
        let src = &v[first * CHUNK..first * CHUNK + o.len()];
        for (d, &x) in o.iter_mut().zip(src) {
            *d = (x.max(0.0) * scale) as u16;
        }
    });
    (out, scale)
}

/// File capabilities.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct Capabilities {
    /// How the payload can be accessed independently.
    pub random_access: RandomAccess,
    /// Number of independently decodable units.
    pub units: usize,
    /// Number of embedded previews.
    pub previews: usize,
    /// The payload is uniformly bit-packed and can be unpacked on the GPU.
    pub gpu_unpack: bool,
    /// The sensor data is floating-point (e.g. floating-point DNG).
    pub float: bool,
}

/// A decoded embedded preview ([`FlashRaw::decode_preview_rgb`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PreviewImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Interleaved 8-bit RGB, `width * height * 3` bytes, row-major.
    pub rgb: Vec<u8>,
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
    let p =
        Arc::new(rayon::ThreadPoolBuilder::new().num_threads(n).start_handler(|_| raise_worker_qos()).build().ok()?);
    pools.insert(n, p.clone());
    Some(p)
}

/// Give a pool worker the scheduling class of interactive work on Apple platforms
/// (`QOS_CLASS_USER_INITIATED`), so it is scheduled like the thread that asked for the
/// decode rather than as background work on efficiency cores.
#[cfg(feature = "rayon")]
fn raise_worker_qos() {
    #[cfg(target_vendor = "apple")]
    {
        const QOS_CLASS_USER_INITIATED: u32 = 0x19;
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        // SAFETY: the call only changes the scheduling class of the calling thread; it takes
        // plain integers and has no memory-safety preconditions. Failure leaves the default.
        unsafe {
            pthread_set_qos_class_self_np(QOS_CLASS_USER_INITIATED, 0);
        }
    }
}

#[cfg(not(feature = "rayon"))]
fn thread_pool(_threads: Option<usize>) -> Option<()> {
    None
}
