//! Target-independent implementation of the JS API (unit-testable natively).
//!
//! Options arrive as one JSON string per call (cheap, and new options — e.g. GPU knobs — do
//! not change the WASM ABI). Results are pixel buffers that stay in WASM memory plus a JSON
//! description; the JS layer decides whether to copy the pixels out or hand back a view.

use crate::json;
use flashraw::{Backend, DecodeOptions, Demosaic, Error, FlashRaw, Limits, OutputFormat, PreviewInfo, Rect};
use serde_json::Value;

/// Default limits on wasm32: the address space is 4 GiB, so the native 1.2 G-sample default
/// would only turn a hostile header into an abort. 400 M samples covers 150 MP sensors (and
/// 100 MP three-channel linear DNGs) while keeping a u16 buffer under 800 MB.
pub const DEFAULT_MAX_SAMPLES: u64 = 400_000_000;

/// A structured error for JS: `kind` mirrors [`flashraw::Error::kind`].
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub kind: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        ApiError { kind: "InvalidArgument", message: msg.into() }
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError { kind: e.kind(), message: e.to_string() }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// Decoded pixels kept in WASM memory until JS copies or views them.
#[derive(Debug)]
pub enum Pixels {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
}

impl Pixels {
    pub fn kind(&self) -> &'static str {
        match self {
            Pixels::U8(_) => "u8",
            Pixels::U16(_) => "u16",
            Pixels::F32(_) => "f32",
        }
    }
    /// Number of elements.
    pub fn len(&self) -> usize {
        match self {
            Pixels::U8(v) => v.len(),
            Pixels::U16(v) => v.len(),
            Pixels::F32(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Address of the first element (aligned for the element type).
    pub fn ptr(&self) -> usize {
        match self {
            Pixels::U8(v) => v.as_ptr() as usize,
            Pixels::U16(v) => v.as_ptr() as usize,
            Pixels::F32(v) => v.as_ptr() as usize,
        }
    }
}

/// A decode result: pixels plus the JSON description (see `json::frame`).
#[derive(Debug)]
pub struct Decoded {
    pub pixels: Pixels,
    pub info: String,
}

fn limits_from(v: Option<&Value>) -> ApiResult<Limits> {
    let mut l = Limits::default().with_max_samples(DEFAULT_MAX_SAMPLES);
    if let Some(v) = v.filter(|v| !v.is_null()) {
        let o = v.as_object().ok_or_else(|| ApiError::invalid("limits must be an object"))?;
        if let Some(x) = o.get("maxSamples").filter(|x| !x.is_null()) {
            l.max_samples = x
                .as_f64()
                .filter(|x| *x >= 1.0 && x.fract() == 0.0)
                .ok_or_else(|| ApiError::invalid("limits.maxSamples must be a positive integer"))?
                as u64;
        }
        if let Some(x) = o.get("maxDimension").filter(|x| !x.is_null()) {
            l.max_dimension = x
                .as_u64()
                .filter(|x| *x >= 1 && *x <= u32::MAX as u64)
                .ok_or_else(|| ApiError::invalid("limits.maxDimension must be a positive integer"))?
                as u32;
        }
    }
    Ok(l)
}

fn parse_json(s: &str) -> ApiResult<Value> {
    if s.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    let v: Value =
        serde_json::from_str(s).map_err(|e| ApiError::invalid(format!("options are not valid JSON: {e}")))?;
    if !(v.is_object() || v.is_null()) {
        return Err(ApiError::invalid("options must be an object"));
    }
    Ok(v)
}

fn str_opt<'a>(o: &'a Value, key: &str) -> ApiResult<Option<&'a str>> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(ApiError::invalid(format!("option `{key}` must be a string"))),
    }
}

fn u32_field(o: &Value, key: &str) -> ApiResult<u32> {
    o.get(key)
        .and_then(|v| v.as_u64())
        .filter(|v| *v <= u32::MAX as u64)
        .map(|v| v as u32)
        .ok_or_else(|| ApiError::invalid(format!("region.{key} must be a non-negative integer")))
}

/// Parse a rectangle `{x, y, width, height}`.
pub fn parse_rect(v: &Value) -> ApiResult<Rect> {
    if !v.is_object() {
        return Err(ApiError::invalid("region must be an object {x, y, width, height}"));
    }
    let r = Rect::new(u32_field(v, "x")?, u32_field(v, "y")?, u32_field(v, "width")?, u32_field(v, "height")?);
    if r.is_empty() {
        return Err(ApiError::invalid(format!("region {r} is empty")));
    }
    Ok(r)
}

/// Parse open-time options (`{ limits }`).
pub fn parse_open_options(s: &str) -> ApiResult<Limits> {
    let v = parse_json(s)?;
    limits_from(v.get("limits"))
}

/// Parse decode options. Returns the options and the requested backend name.
pub fn parse_decode_options(s: &str) -> ApiResult<(DecodeOptions, String)> {
    let v = parse_json(s)?;
    // Single-threaded builds decode on the calling thread; the threaded build (feature `threads`)
    // uses rayon's global pool, which the JS layer starts in workers (`threads: None`).
    let threads = if cfg!(feature = "threads") { None } else { Some(1) };
    let mut o = DecodeOptions { threads, limits: limits_from(v.get("limits"))?, ..Default::default() };
    let backend = str_opt(&v, "backend")?.unwrap_or("auto");
    o.backend = Backend::parse(backend).ok_or_else(|| {
        ApiError::invalid(format!("unknown backend `{backend}` (expected auto, cpu, cpu-simd, scalar or gpu)"))
    })?;
    if let Some(out) = str_opt(&v, "output")? {
        o.output = OutputFormat::parse(out).ok_or_else(|| {
            ApiError::invalid(format!(
                "unknown output `{out}` (expected raw-u16, raw-f32, normalized, linear-rgb or rgba8)"
            ))
        })?;
    }
    if let Some(d) = str_opt(&v, "demosaic")? {
        o.demosaic = match d.to_ascii_lowercase().as_str() {
            "bilinear" => Demosaic::Bilinear,
            "half" | "half-size" | "halfsize" => Demosaic::HalfSize,
            _ => return Err(ApiError::invalid(format!("unknown demosaic `{d}` (expected bilinear or half-size)"))),
        };
    }
    match v.get("crop") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(b)) => o.crop = *b,
        Some(_) => return Err(ApiError::invalid("option `crop` must be a boolean")),
    }
    match v.get("applyOpcodes") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(b)) => o.apply_opcodes = *b,
        Some(_) => return Err(ApiError::invalid("option `applyOpcodes` must be a boolean")),
    }
    if let Some(r) = v.get("region").filter(|r| !r.is_null()) {
        o.region = Some(parse_rect(r)?);
    }
    Ok((o, backend.to_ascii_lowercase()))
}

/// Open from an owned buffer (the single copy of the input made by the JS layer).
pub fn open(bytes: Vec<u8>, options_json: &str) -> ApiResult<FlashRaw<'static>> {
    let limits = parse_open_options(options_json)?;
    Ok(FlashRaw::open_with(bytes, limits)?)
}

fn into_decoded(frame: flashraw::RawFrame, backend: &str) -> ApiResult<Decoded> {
    let info = json::frame(&frame, backend).to_string();
    let pixels = match frame.data {
        flashraw::RawPixelData::U8(v) => Pixels::U8(v),
        flashraw::RawPixelData::U16(v) => Pixels::U16(v),
        flashraw::RawPixelData::F32(v) => Pixels::F32(v),
        #[allow(unreachable_patterns)]
        _ => {
            return Err(ApiError {
                kind: "NotAvailable",
                message: "GPU-resident outputs require the asynchronous GPU decode (WebGPU build)".into(),
            });
        }
    };
    Ok(Decoded { pixels, info })
}

/// Decode the sensor data (full frame or `options.region`).
pub fn decode(raw: &FlashRaw<'_>, options_json: &str) -> ApiResult<Decoded> {
    let (opts, backend) = parse_decode_options(options_json)?;
    into_decoded(raw.decode(opts)?, &backend)
}

/// Result of [`decode_gpu`]: host pixels, or an image left on the GPU (`gpu-*` outputs).
#[cfg(feature = "gpu")]
#[derive(Debug)]
pub enum GpuDecoded {
    Host(Decoded),
    /// The frame (its data is `RawPixelData::Gpu`), its JSON description and the requested
    /// backend name (for [`download_gpu`]).
    Resident {
        frame: Box<flashraw::RawFrame>,
        info: String,
        backend: String,
    },
}

/// Read a GPU-resident frame back into WASM memory.
#[cfg(feature = "gpu")]
pub async fn download_gpu(frame: &flashraw::RawFrame, backend: &str) -> ApiResult<Decoded> {
    into_decoded(frame.download_async().await?, backend)
}

/// Asynchronous GPU decode (the only way to use WebGPU from WASM: the browser cannot block on
/// the GPU). `tile` selects one decode unit. Raw outputs of entropy-coded files are decoded on
/// the CPU, exactly like the native `Backend::Gpu` (the CPU decode *is* the result there).
#[cfg(feature = "gpu")]
pub async fn decode_gpu(raw: &FlashRaw<'_>, tile: Option<u32>, options_json: &str) -> ApiResult<GpuDecoded> {
    let (mut opts, backend) = parse_decode_options(options_json)?;
    if let Some(i) = tile {
        let units = raw.units();
        let unit = units
            .get(i as usize)
            .ok_or_else(|| ApiError::invalid(format!("tile {i} out of range ({} tiles)", units.len())))?;
        opts.region = Some(unit.rect);
    }
    let base = opts.output.host_equivalent();
    let resident = opts.output.is_gpu_resident();
    if !resident && (base == OutputFormat::RawF32 || (base == OutputFormat::RawU16 && !raw.capabilities().gpu_unpack)) {
        opts.backend = Backend::Auto;
        return into_decoded(raw.decode(opts)?, &backend).map(GpuDecoded::Host);
    }
    let frame = raw.decode_gpu_async(opts).await?;
    if frame.as_gpu().is_some() {
        let info = json::frame(&frame, &backend).to_string();
        return Ok(GpuDecoded::Resident { frame: Box::new(frame), info, backend });
    }
    into_decoded(frame, &backend).map(GpuDecoded::Host)
}

/// Decode one independently decodable tile/strip.
pub fn decode_tile(raw: &FlashRaw<'_>, index: u32, options_json: &str) -> ApiResult<Decoded> {
    let (opts, backend) = parse_decode_options(options_json)?;
    into_decoded(raw.decode_tile(index as usize, opts)?, &backend)
}

/// Resolve a preview selector: `"largest"`, `"smallest"` or an index.
pub fn select_preview<'r>(raw: &'r FlashRaw<'_>, which: &str) -> ApiResult<(usize, &'r PreviewInfo)> {
    let all = raw.previews();
    let none = || ApiError { kind: "NotAvailable", message: "the file has no embedded preview".into() };
    let pick = |p: Option<&PreviewInfo>| -> ApiResult<(usize, &'r PreviewInfo)> {
        let p = p.ok_or_else(none)?;
        let i = all.iter().position(|x| std::ptr::eq(x, p)).unwrap_or(0);
        Ok((i, &all[i]))
    };
    match which {
        "largest" | "preview" | "" => pick(raw.preview()),
        "smallest" | "thumbnail" => pick(raw.thumbnail()),
        s => {
            let i: usize = s.parse().map_err(|_| ApiError::invalid(format!("unknown preview selector `{s}`")))?;
            let p = all.get(i).ok_or_else(|| ApiError {
                kind: "NotAvailable",
                message: format!("preview {i} out of range ({} previews)", all.len()),
            })?;
            Ok((i, p))
        }
    }
}

/// Location of a preview's encoded bytes inside the input buffer: `(offset, length, info json)`.
pub fn preview_span(raw: &FlashRaw<'_>, which: &str) -> ApiResult<(usize, usize, String)> {
    let (i, p) = select_preview(raw, which)?;
    let bytes = raw.preview_bytes(p)?;
    let offset = bytes.as_ptr() as usize - raw.bytes().as_ptr() as usize;
    let mut info = json::previews(raw.metadata())[i].clone();
    if let Some(o) = info.as_object_mut() {
        o.insert("byteLength".into(), bytes.len().into());
    }
    Ok((offset, bytes.len(), info.to_string()))
}

/// Decode a preview to RGBA8 (orientation not applied; reported in the info).
pub fn decode_preview_rgba(raw: &FlashRaw<'_>, which: &str) -> ApiResult<Decoded> {
    let t0 = flashraw::low_level::core::profile::now_ms();
    let (i, p) = select_preview(raw, which)?;
    let (w, h, rgb) = raw.decode_preview_rgb(p)?;
    let n = w as usize * h as usize;
    if rgb.len() < n * 3 {
        return Err(Error::corrupt(format!("preview has {} bytes for {w}x{h} RGB", rgb.len())).into());
    }
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(n * 4).map_err(|_| ApiError { kind: "OutOfMemory", message: "preview buffer".into() })?;
    for px in rgb[..n * 3].chunks_exact(3) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    let mut info = json::previews(raw.metadata())[i].clone();
    if let Some(o) = info.as_object_mut() {
        o.insert("width".into(), w.into());
        o.insert("height".into(), h.into());
        o.insert("decodeMs".into(), (flashraw::low_level::core::profile::now_ms() - t0).into());
    }
    Ok(Decoded { pixels: Pixels::U8(rgba), info: info.to_string() })
}

/// Cheap format detection.
pub fn detect(bytes: &[u8]) -> Option<&'static str> {
    flashraw::supports(bytes).map(|f| f.name())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus_file(rel: &str) -> Option<Vec<u8>> {
        let root = std::env::var("FLASHRAW_CORPUS").unwrap_or_else(|_| "/var/projects/flashraw/corpus".into());
        std::fs::read(std::path::Path::new(&root).join(rel)).ok()
    }

    #[test]
    fn options_parse() {
        let (o, b) = parse_decode_options(
            r#"{"output":"rgba8","backend":"GPU","demosaic":"half-size","crop":false,"region":{"x":2,"y":4,"width":8,"height":6}}"#,
        )
        .unwrap();
        assert_eq!(o.output, OutputFormat::Rgba8);
        assert_eq!(o.backend, Backend::Gpu);
        assert_eq!(b, "gpu");
        assert_eq!(o.demosaic, Demosaic::HalfSize);
        assert!(!o.crop);
        assert_eq!(o.region, Some(Rect::new(2, 4, 8, 6)));
        assert_eq!(o.limits.max_samples, DEFAULT_MAX_SAMPLES);
        assert_eq!(parse_decode_options("").unwrap().0.output, OutputFormat::RawU16);
        for bad in [
            r#"{"output":"png"}"#,
            r#"{"backend":"tpu"}"#,
            r#"{"crop":1}"#,
            r#"{"region":{"x":-1,"y":0,"width":1,"height":1}}"#,
            r#"{"region":{"x":0,"y":0,"width":0,"height":1}}"#,
            r#"{"limits":{"maxSamples":0}}"#,
            "[1]",
            "{",
        ] {
            assert_eq!(parse_decode_options(bad).unwrap_err().kind, "InvalidArgument", "{bad}");
        }
        assert_eq!(parse_open_options(r#"{"limits":{"maxSamples":10,"maxDimension":5}}"#).unwrap().max_dimension, 5);
    }

    #[test]
    fn garbage_is_structured_error() {
        let e = open(vec![0u8; 100], "").unwrap_err();
        assert_eq!(e.kind, "UnsupportedFormat");
        assert!(detect(&[0u8; 100]).is_none());
        let mut tiff = b"II*\0\x08\0\0\0".to_vec();
        tiff.extend_from_slice(&[0xff; 64]);
        assert!(open(tiff, "").is_err());
    }

    #[test]
    fn dng_roundtrip() {
        let Some(bytes) = corpus_file("Canon/EOS 5D Mark III/M24-1221_000202.dng") else { return };
        assert_eq!(detect(&bytes), Some("DNG"));
        let raw = open(bytes, "").unwrap();
        let meta = json::metadata(&raw);
        assert_eq!(meta["format"], "DNG");
        assert_eq!(meta["camera"]["canonicalMake"], "Canon");
        assert_eq!(meta["cfa"]["pattern"], "RGGB");
        let d = decode(&raw, r#"{"output":"raw-u16"}"#).unwrap();
        let info: Value = serde_json::from_str(&d.info).unwrap();
        assert_eq!(d.pixels.kind(), "u16");
        assert_eq!(d.pixels.len() as u64, info["width"].as_u64().unwrap() * info["height"].as_u64().unwrap());
        assert_eq!(info["backend"]["requested"], "auto");
        let (off, len, _) = preview_span(&raw, "largest").unwrap();
        assert_eq!(&raw.bytes()[off..off + 2], &[0xff, 0xd8]);
        assert!(len > 2);
        let p = decode_preview_rgba(&raw, "smallest").unwrap();
        let pi: Value = serde_json::from_str(&p.info).unwrap();
        assert_eq!(p.pixels.len() as u64, pi["width"].as_u64().unwrap() * pi["height"].as_u64().unwrap() * 4);
        // Without an adapter (or the `gpu` feature) a GPU request fails cleanly.
        if !flashraw::gpu_available() {
            assert_eq!(decode(&raw, r#"{"backend":"gpu"}"#).unwrap_err().kind, "GpuUnavailable");
        }
        assert_eq!(select_preview(&raw, "9").unwrap_err().kind, "NotAvailable");
        let r = decode(&raw, r#"{"region":{"x":16,"y":8,"width":64,"height":32}}"#).unwrap();
        assert_eq!(r.pixels.len(), 64 * 32);
    }
}
