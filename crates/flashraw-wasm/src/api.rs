//! Target-independent implementation of the JS API (unit-testable natively).
//!
//! Options arrive as one JSON string per call (cheap, and new options — e.g. GPU knobs — do
//! not change the WASM ABI). Results are pixel buffers that stay in WASM memory plus a JSON
//! description; the JS layer decides whether to copy the pixels out or hand back a view.

use crate::json;
use flashraw::{
    Backend, CorruptionPolicy, DecodeOptions, Demosaic, Error, FlashRaw, Limits, OutputFormat, PreviewInfo, Rect,
};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

/// Default limits on wasm32: the address space is 4 GiB, so the native 1.2 G-sample default
/// would only turn a hostile header into an abort. 400 M samples covers 150 MP sensors (and
/// 100 MP three-channel linear DNGs) while keeping a u16 buffer under 800 MB.
pub const DEFAULT_MAX_SAMPLES: u64 = 400_000_000;

/// A structured error for JS: `kind` mirrors [`flashraw::Error::kind`].
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub kind: &'static str,
    pub message: String,
    /// Structured cause of a `CorruptFile` error ([`json::corrupt_reason`]), set on the JS
    /// error as `corrupt`.
    pub corrupt: Option<Value>,
}

impl ApiError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        ApiError { corrupt: None, kind: "InvalidArgument", message: msg.into() }
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError { kind: e.kind(), message: e.to_string(), corrupt: e.corrupt_reason().map(json::corrupt_reason) }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// Decoded pixels kept in WASM memory until JS copies or views them.
#[derive(Debug)]
pub enum Pixels {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
    /// IEEE half floats (as u16 bits) packed at the start of the original f32 buffer: the
    /// conversion runs in place so `sampleType: 'f16'` never needs a second buffer (WASM
    /// memory never shrinks, so a peak would stay forever). `len` counts halves.
    F16 {
        buf: Vec<f32>,
        len: usize,
    },
    /// IEEE half floats (as u16 bits) produced by the decode itself (`linear-rgb-f16`).
    F16Bits(Vec<u16>),
}

impl Pixels {
    pub fn kind(&self) -> &'static str {
        match self {
            Pixels::U8(_) => "u8",
            Pixels::U16(_) => "u16",
            Pixels::F32(_) => "f32",
            Pixels::F16 { .. } | Pixels::F16Bits(_) => "f16",
        }
    }
    /// Number of elements.
    pub fn len(&self) -> usize {
        match self {
            Pixels::U8(v) => v.len(),
            Pixels::U16(v) => v.len(),
            Pixels::F32(v) => v.len(),
            Pixels::F16 { len, .. } => *len,
            Pixels::F16Bits(v) => v.len(),
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
            Pixels::F16 { buf, .. } => buf.as_ptr() as usize,
            Pixels::F16Bits(v) => v.as_ptr() as usize,
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

fn f32_opt(o: &Value, key: &str) -> ApiResult<Option<f32>> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|x| x.is_finite())
            .map(|x| Some(x as f32))
            .ok_or_else(|| ApiError::invalid(format!("option `{key}` must be a finite number"))),
    }
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

/// Options handled by this layer on top of [`DecodeOptions`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ApiExtras {
    /// `sampleType: 'f16'` on `normalized` (converted to half floats after the decode;
    /// `linear-rgb` with `sampleType: 'f16'` becomes the `linear-rgb-f16` output instead).
    pub f16: bool,
    /// `crop: 'active'`: raw outputs cropped to the active area.
    pub active: bool,
    /// `linear-rgb` with `sampleType: 'f16'`: decoded as `linear-rgb-f16`, reported as
    /// `linear-rgb` with `dataType: 'f16'` (like `normalized`).
    pub rgb_f16: bool,
}

/// Parse decode options. Returns the options and the requested backend name.
pub fn parse_decode_options(s: &str) -> ApiResult<(DecodeOptions, String)> {
    parse_decode_request(s).map(|(o, b, _)| (o, b))
}

/// [`parse_decode_options`] plus the options applied by this layer ([`ApiExtras`]).
pub fn parse_decode_request(s: &str) -> ApiResult<(DecodeOptions, String, ApiExtras)> {
    let v = parse_json(s)?;
    let mut extras = ApiExtras::default();
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
                "unknown output `{out}` (expected raw-u16, raw-f32, normalized, linear-rgb, linear-rgb16, \
                 linear-rgb-f16, rgba8 or rgb16)"
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
        // RGB outputs: the default crop as with `true`; raw outputs: the active area.
        Some(Value::String(c)) if c == "active" => extras.active = true,
        Some(_) => return Err(ApiError::invalid("option `crop` must be a boolean or \"active\"")),
    }
    match str_opt(&v, "sampleType")? {
        None | Some("f32") => {}
        Some("f16") => match o.output {
            OutputFormat::NormalizedF32 => extras.f16 = true,
            // Half floats straight from the fused develop (no f32 frame).
            OutputFormat::LinearRgbF32 => {
                o.output = OutputFormat::LinearRgbF16;
                extras.rgb_f16 = true;
            }
            OutputFormat::LinearRgbF16 => {}
            _ => {
                return Err(ApiError::invalid(
                    "sampleType \"f16\" requires output \"normalized\", \"linear-rgb\" or \"linear-rgb-f16\"",
                ));
            }
        },
        Some(t) => return Err(ApiError::invalid(format!("unknown sampleType `{t}` (expected f32 or f16)"))),
    }
    if let Some(p) = str_opt(&v, "corruptionPolicy")? {
        o.corruption_policy = CorruptionPolicy::parse(p)
            .ok_or_else(|| ApiError::invalid(format!("unknown corruptionPolicy `{p}` (expected recover or strict)")))?;
    }
    match v.get("applyOpcodes") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(b)) => o.apply_opcodes = *b,
        Some(_) => return Err(ApiError::invalid("option `applyOpcodes` must be a boolean")),
    }
    o.exposure = f32_opt(&v, "exposure")?.unwrap_or(0.0);
    o.black_point = f32_opt(&v, "blackPoint")?.unwrap_or(0.0);
    o.white_point = f32_opt(&v, "whitePoint")?.unwrap_or(1.0);
    match v.get("previewFallback") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(b)) => o.preview_fallback = *b,
        Some(_) => return Err(ApiError::invalid("option `previewFallback` must be a boolean")),
    }
    if let Some(r) = v.get("region").filter(|r| !r.is_null()) {
        o.region = Some(parse_rect(r)?);
    }
    Ok((o, backend.to_ascii_lowercase(), extras))
}

fn is_raw_output(o: OutputFormat) -> bool {
    matches!(o.host_equivalent(), OutputFormat::RawU16 | OutputFormat::RawF32 | OutputFormat::NormalizedF32)
}

/// `crop: 'active'` on a raw output: decode the active area as a region (an explicit
/// `region` wins). The frame's `origin` and CFA phase then describe the cropped buffer.
fn apply_active_crop(raw: &FlashRaw<'_>, o: &mut DecodeOptions, x: ApiExtras) {
    if x.active && o.region.is_none() && is_raw_output(o.output) {
        let f = raw.frame_info();
        let a = f.active_area;
        if a.width > 0 && a.height > 0 && (a.width, a.height) != (f.width, f.height) {
            o.region = Some(a);
        }
    }
}

/// f32 → IEEE half bits (round to nearest even, saturating at ±65504), shared with the
/// fused develop's `linear-rgb-f16` output.
pub use flashraw::process::f32_to_f16;

/// Convert `v` to halves in place (half `i` lands in bytes `2i..2i+2`, which only overlap
/// floats `< i` that are already converted). Blocks `[m, 2m)` read bytes `[4m, 8m)` and write
/// `[2m, 4m)`: disjoint, so each block runs in parallel once the blocks below it are done.
fn to_f16_in_place(mut v: Vec<f32>) -> Pixels {
    let n = v.len();
    let base = v.as_mut_ptr() as usize;
    // SAFETY: indices stay below `n`; reads of float `i` happen before any write to the bytes
    // of float `i` (writes of halves `j <= 2i+1` happen after their sources are read, see above).
    // Blocks of 64 are read into registers/stack first, so a block's writes never clobber its
    // own unread inputs (writes `[2lo, 2hi)` vs reads `[4lo, 4hi)` overlap only when
    // `hi > 2lo`, i.e. inside the sequential prefix, which is converted one element at a time).
    let run = move |lo: usize, hi: usize| unsafe {
        let src = base as *const f32;
        let dst = base as *mut u16;
        let mut i = lo;
        if lo >= 64 {
            while i + 64 <= hi {
                let mut tmp = [0u16; 64];
                let s = std::slice::from_raw_parts(src.add(i), 64);
                for (t, &f) in tmp.iter_mut().zip(s) {
                    *t = f32_to_f16(f);
                }
                std::ptr::copy_nonoverlapping(tmp.as_ptr(), dst.add(i), 64);
                i += 64;
            }
        }
        for j in i..hi {
            let h = f32_to_f16(src.add(j).read());
            dst.add(j).write(h);
        }
    };
    const SEQ: usize = 1 << 16;
    run(0, n.min(SEQ));
    let mut m = SEQ;
    while m < n {
        let hi = (2 * m).min(n);
        const CHUNK: usize = 1 << 16;
        let (lo0, len) = (m, hi - m);
        flashraw::low_level::core::par::for_each_index(len.div_ceil(CHUNK), cfg!(feature = "threads"), |c| {
            let lo = lo0 + c * CHUNK;
            run(lo, (lo + CHUNK).min(hi))
        });
        m = hi;
    }
    Pixels::F16 { buf: v, len: n }
}

/// Open from an owned buffer (the single copy of the input made by the JS layer).
pub fn open(bytes: Vec<u8>, options_json: &str) -> ApiResult<FlashRaw<'static>> {
    let limits = parse_open_options(options_json)?;
    Ok(FlashRaw::open_with(bytes, limits)?)
}

#[cfg_attr(not(feature = "gpu"), allow(dead_code))]
fn into_decoded(frame: flashraw::RawFrame, backend: &str) -> ApiResult<Decoded> {
    into_decoded_x(frame, backend, ApiExtras::default())
}

fn into_decoded_x(mut frame: flashraw::RawFrame, backend: &str, x: ApiExtras) -> ApiResult<Decoded> {
    if x.rgb_f16 {
        frame.output = OutputFormat::LinearRgbF32;
    }
    let info = json::frame(&frame, backend).to_string();
    let pixels = match frame.data {
        flashraw::RawPixelData::U8(v) => Pixels::U8(v),
        flashraw::RawPixelData::U16(v) => Pixels::U16(v),
        flashraw::RawPixelData::F16(v) => Pixels::F16Bits(v),
        flashraw::RawPixelData::F32(v) if x.f16 => to_f16_in_place(v),
        flashraw::RawPixelData::F32(v) => Pixels::F32(v),
        #[allow(unreachable_patterns)]
        _ => {
            return Err(ApiError {
                corrupt: None,
                kind: "NotAvailable",
                message: "GPU-resident outputs require the asynchronous GPU decode (WebGPU build)".into(),
            });
        }
    };
    Ok(Decoded { pixels, info })
}

/// Decode the sensor data (full frame or `options.region`).
pub fn decode(raw: &FlashRaw<'_>, options_json: &str) -> ApiResult<Decoded> {
    let (mut opts, backend, x) = parse_decode_request(options_json)?;
    apply_active_crop(raw, &mut opts, x);
    into_decoded_x(raw.decode(opts)?, &backend, x)
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
    let (mut opts, backend, x) = parse_decode_request(options_json)?;
    if tile.is_none() {
        apply_active_crop(raw, &mut opts, x);
    }
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
        return into_decoded_x(raw.decode(opts)?, &backend, x).map(GpuDecoded::Host);
    }
    let frame = raw.decode_gpu_async(opts).await?;
    if frame.as_gpu().is_some() {
        let info = json::frame(&frame, &backend).to_string();
        return Ok(GpuDecoded::Resident { frame: Box::new(frame), info, backend });
    }
    into_decoded_x(frame, &backend, x).map(GpuDecoded::Host)
}

/// Decode one independently decodable tile/strip.
pub fn decode_tile(raw: &FlashRaw<'_>, index: u32, options_json: &str) -> ApiResult<Decoded> {
    let (opts, backend, x) = parse_decode_request(options_json)?;
    into_decoded_x(raw.decode_tile(index as usize, opts)?, &backend, x)
}

/// Resolve a preview selector: `"largest"`, `"smallest"` or an index.
pub fn select_preview<'r>(raw: &'r FlashRaw<'_>, which: &str) -> ApiResult<(usize, &'r PreviewInfo)> {
    let all = raw.previews();
    let none = || ApiError { corrupt: None, kind: "NotAvailable", message: "the file has no embedded preview".into() };
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
                corrupt: None,
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
    let img = raw.decode_preview_rgb(p)?;
    let (w, h, rgb) = (img.width, img.height, img.rgb);
    let n = w as usize * h as usize;
    if rgb.len() < n * 3 {
        return Err(Error::corrupt(format!("preview has {} bytes for {w}x{h} RGB", rgb.len())).into());
    }
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(n * 4).map_err(|_| ApiError {
        corrupt: None,
        kind: "OutOfMemory",
        message: "preview buffer".into(),
    })?;
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

    /// A 16x8 16-bit uncompressed DNG missing the last `missing` bytes of its strip.
    fn truncated_dng(missing: usize) -> Vec<u8> {
        let e: [(u16, u16, u32, u32); 12] = [
            (0x0100, 4, 1, 16),
            (0x0101, 4, 1, 8),
            (0x0102, 3, 1, 16),
            (0x0103, 3, 1, 1),
            (0x0106, 3, 1, 32803),
            (0x0111, 4, 1, 8 + 2 + 12 * 12 + 4),
            (0x0115, 3, 1, 1),
            (0x0116, 4, 1, 8),
            (0x0117, 4, 1, 256),
            (0x828D, 3, 2, 2 | (2 << 16)),
            (0x828E, 1, 4, u32::from_le_bytes([0, 1, 1, 2])),
            (0xC612, 1, 4, u32::from_le_bytes([1, 4, 0, 0])),
        ];
        let mut out = b"II*\0\x08\0\0\0".to_vec();
        out.extend((e.len() as u16).to_le_bytes());
        for (tag, typ, count, v) in e {
            out.extend(tag.to_le_bytes());
            out.extend(typ.to_le_bytes());
            out.extend(count.to_le_bytes());
            out.extend(v.to_le_bytes());
        }
        out.extend(0u32.to_le_bytes());
        out.extend((0..128u16).flat_map(|i| (i * 31 + 1).to_le_bytes()));
        out.truncate(out.len() - missing);
        out
    }

    #[test]
    fn corruption_policy() {
        assert_eq!(parse_decode_options("{}").unwrap().0.corruption_policy, CorruptionPolicy::Recover);
        let strict = parse_decode_options(r#"{"corruptionPolicy":"strict"}"#).unwrap().0;
        assert_eq!(strict.corruption_policy, CorruptionPolicy::Strict);
        assert_eq!(parse_decode_options(r#"{"corruptionPolicy":"lax"}"#).unwrap_err().kind, "InvalidArgument");

        let raw = open(truncated_dng(6), "").unwrap();
        let d = decode(&raw, "{}").unwrap();
        let info: Value = serde_json::from_str(&d.info).unwrap();
        assert_eq!(info["incomplete"], true);
        let w = &info["warnings"][0];
        assert_eq!(
            (w["kind"].as_str(), w["unit"].as_str(), w["index"].as_u64()),
            (Some("truncated"), Some("strip"), Some(0))
        );
        assert_eq!(
            (w["expected"].as_u64(), w["available"].as_u64(), w["samplesFilled"].as_u64()),
            (Some(256), Some(250), Some(3))
        );

        let e = decode(&raw, r#"{"corruptionPolicy":"strict"}"#).unwrap_err();
        assert_eq!(e.kind, "CorruptFile");
        let c = e.corrupt.unwrap();
        assert_eq!(
            (c["kind"].as_str(), c["available"].as_u64(), c["fileLength"].as_u64()),
            (Some("truncated"), Some(250), Some(408))
        );

        let ok = open(truncated_dng(0), "").unwrap();
        let info: Value = serde_json::from_str(&decode(&ok, r#"{"corruptionPolicy":"strict"}"#).unwrap().info).unwrap();
        assert_eq!(
            (info["incomplete"].as_bool(), info["warnings"].as_array().map(|a| a.len())),
            (Some(false), Some(0))
        );
    }

    #[test]
    fn preview_fallback_option() {
        assert!(!parse_decode_options("{}").unwrap().0.preview_fallback);
        assert!(parse_decode_options(r#"{"previewFallback":true}"#).unwrap().0.preview_fallback);
        assert_eq!(parse_decode_options(r#"{"previewFallback":"yes"}"#).unwrap_err().kind, "InvalidArgument");
        let nef = include_bytes!("../../../npm/tests/fixtures/nikon-he-synthetic.nef").to_vec();
        let raw = open(nef, "").unwrap();
        assert_eq!(decode(&raw, r#"{"output":"rgba8"}"#).unwrap_err().kind, "UnsupportedCompression");
        let d = decode(&raw, r#"{"output":"rgba8","previewFallback":true}"#).unwrap();
        let info: Value = serde_json::from_str(&d.info).unwrap();
        assert_eq!((info["source"].as_str(), info["incomplete"].as_bool()), (Some("preview"), Some(true)));
        assert_eq!(info["warnings"][0]["kind"].as_str(), Some("preview-fallback"));
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

    /// Reference half conversion via f64 (round to nearest even, saturating).
    fn half_ref(x: f32) -> u16 {
        let s = if x.is_sign_negative() { 0x8000u16 } else { 0 };
        let a = (x as f64).abs();
        if x.is_nan() {
            return s | 0x7e00;
        }
        if a >= 65520.0 {
            return s | 0x7bff;
        }
        // Enumerate: find nearest half by bits (monotonic for positives).
        let val = |h: u16| -> f64 {
            let e = (h >> 10) & 31;
            let m = (h & 1023) as f64;
            if e == 0 { m * 2f64.powi(-24) } else { (1.0 + m / 1024.0) * 2f64.powi(e as i32 - 15) }
        };
        let (mut lo, mut hi) = (0u16, 0x7bffu16);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if val(mid) <= a { lo = mid } else { hi = mid }
        }
        let (dl, dh) = (a - val(lo), val(hi) - a);
        let h = if dl < dh || (dl == dh && lo & 1 == 0) { lo } else { hi };
        s | if a < val(lo) { lo } else { h }
    }

    #[test]
    fn f16_conversion() {
        let mut x = 1u32;
        for i in 0..200_000u32 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            // Bias toward the interesting [0, 4) range of normalised data.
            let v = if i % 2 == 0 { f32::from_bits(x) } else { (x as f32 / u32::MAX as f32) * 4.0 };
            if v.is_infinite() {
                continue;
            }
            assert_eq!(f32_to_f16(v), half_ref(v), "{v:e}");
        }
        for v in [0.0f32, 1.0, 0.5, 65504.0, 65519.0, 65520.0, 1e9, 5.960_464_5e-8, 2.9e-8, 6.1e-5] {
            assert_eq!(f32_to_f16(v), half_ref(v), "{v:e}");
        }
        // In place, across the sequential prefix and several doubling blocks.
        let n = (1 << 18) + 12345;
        let v: Vec<f32> = (0..n).map(|i| i as f32 / 977.0).collect();
        let expect: Vec<u16> = v.iter().map(|&f| f32_to_f16(f)).collect();
        let Pixels::F16 { buf, len } = to_f16_in_place(v) else { panic!() };
        assert_eq!(len, n);
        // SAFETY: `len` halves at the start of `buf`.
        let got = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u16, len) };
        assert_eq!(got, &expect[..]);
    }

    #[test]
    fn sixteen_bit_rgb_outputs_and_levels() {
        let Some(bytes) = corpus_file("Canon/EOS 5D Mark IV/B13A0729.CR2") else { return };
        let raw = open(bytes, "").unwrap();
        let o = r#"{"output":"linear-rgb","demosaic":"half-size","exposure":0.5,"blackPoint":0.01}"#;
        let f32s = decode(&raw, o).unwrap();
        let Pixels::F32(lin) = &f32s.pixels else { panic!() };
        let want: Vec<u16> = lin.iter().map(|&v| f32_to_f16(v)).collect();
        // `sampleType: 'f16'` on linear-rgb is the fused `linear-rgb-f16` output (reported as
        // `linear-rgb` with f16 samples, like `normalized`).
        for (o, name) in [
            (
                r#"{"output":"linear-rgb","sampleType":"f16","demosaic":"half-size","exposure":0.5,"blackPoint":0.01}"#,
                "linear-rgb",
            ),
            (
                r#"{"output":"linear-rgb-f16","demosaic":"half-size","exposure":0.5,"blackPoint":0.01}"#,
                "linear-rgb-f16",
            ),
        ] {
            let h = decode(&raw, o).unwrap();
            assert_eq!(h.pixels.kind(), "f16");
            let Pixels::F16Bits(v) = &h.pixels else { panic!() };
            assert_eq!(v, &want);
            let info: Value = serde_json::from_str(&h.info).unwrap();
            assert_eq!(info["output"], name);
        }
        let r = decode(&raw, r#"{"output":"rgb16","demosaic":"half-size"}"#).unwrap();
        let info: Value = serde_json::from_str(&r.info).unwrap();
        assert_eq!(
            (r.pixels.kind(), info["channels"].as_u64(), info["orientation"].as_u64()),
            ("u16", Some(3), Some(1))
        );
        assert_eq!(decode(&raw, r#"{"output":"rgb16","whitePoint":0}"#).unwrap_err().kind, "InvalidArgument");
        assert_eq!(decode(&raw, r#"{"output":"rgb16","exposure":"x"}"#).unwrap_err().kind, "InvalidArgument");
        assert_eq!(decode(&raw, r#"{"output":"rgba8","sampleType":"f16"}"#).unwrap_err().kind, "InvalidArgument");
    }

    #[test]
    fn normalized_f16_and_active_crop() {
        let Some(bytes) = corpus_file("Canon/EOS 5D Mark IV/B13A0729.CR2") else { return };
        let raw = open(bytes, "").unwrap();
        let fi = raw.frame_info().clone();
        let a = fi.active_area;
        assert!(a.width < fi.width && a.height < fi.height);
        let full = decode(&raw, r#"{"output":"normalized"}"#).unwrap();
        let act = decode(&raw, r#"{"output":"normalized","crop":"active","sampleType":"f16"}"#).unwrap();
        let info: Value = serde_json::from_str(&act.info).unwrap();
        assert_eq!(act.pixels.kind(), "f16");
        assert_eq!(info["origin"]["x"], a.x);
        assert_eq!(info["origin"]["y"], a.y);
        assert_eq!(
            (info["width"].as_u64().unwrap(), info["height"].as_u64().unwrap()),
            (a.width as u64, a.height as u64)
        );
        assert_eq!(act.pixels.len(), (a.width * a.height) as usize);
        let Pixels::F32(f) = &full.pixels else { panic!() };
        let Pixels::F16 { buf, len } = &act.pixels else { panic!() };
        // SAFETY: `len` halves at the start of `buf`.
        let h = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u16, *len) };
        let w = fi.width as usize;
        for (y, x) in [(0usize, 0usize), (17, 33), (a.height as usize - 1, a.width as usize - 1), (1000, 2000)] {
            let fv = f[(y + a.y as usize) * w + x + a.x as usize];
            assert_eq!(h[y * a.width as usize + x], f32_to_f16(fv), "({x},{y})");
        }
        assert_eq!(decode(&raw, r#"{"output":"raw-u16","sampleType":"f16"}"#).unwrap_err().kind, "InvalidArgument");
        let r = decode(&raw, r#"{"output":"raw-u16","crop":"active"}"#).unwrap();
        assert_eq!(r.pixels.len(), (a.width * a.height) as usize);
    }
}

#[cfg(test)]
mod dng_metadata_tests {
    use super::*;

    fn read(rel: &str) -> Option<Vec<u8>> {
        std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)).ok()
    }

    #[test]
    fn dng_extras_and_opcodes() {
        let raw = open(read("npm/tests/fixtures/canon-5d3-lossy.dng").expect("fixture"), "").unwrap();
        let m = json::metadata(&raw);
        let d = &m["dng"];
        assert_eq!(d["baselineExposure"], 0.25);
        assert_eq!(d["linearResponseLimit"], 1.0);
        assert_eq!(d["profile"]["name"], "Adobe Standard");
        assert_eq!(d["profile"]["hueSatMapCount"], 2);
        assert_eq!(d["profile"]["hueSatMapDims"], json!([90, 30, 1]));
        assert_eq!(d["profile"]["lookTableDims"], json!([36, 8, 16]));
        assert_eq!(d["asShotNeutral"].as_array().unwrap().len(), 3);
        let ops = m["opcodes"].as_array().unwrap();
        assert!(ops.iter().all(|o| o["list"] == 2 && o["name"] == "MapPolynomial" && o["applied"] == true));

        let Some(bytes) = read("corpus/Google/Pixel 8 Pro/PXL_20240415_103400204.RAW-02.ORIGINAL.dng") else { return };
        let m = json::metadata(&open(bytes, "").unwrap());
        assert_eq!(m["dng"]["asShotWhiteXY"].as_array().unwrap().len(), 2);
        assert_eq!(m["dng"]["profile"]["toneCurve"], true);
        let warp = m["opcodes"].as_array().unwrap().iter().find(|o| o["name"] == "WarpRectilinear").unwrap().clone();
        assert_eq!(warp["list"], 3);
        assert_eq!(warp["applied"], false);
        assert_eq!(warp["params"]["kind"], "WarpRectilinear");
        assert_eq!(warp["params"]["planes"][0]["kr"].as_array().unwrap().len(), 4);
        assert!((warp["params"]["center"][0].as_f64().unwrap() - 0.5).abs() < 0.01);
    }

    #[test]
    fn non_dng_has_no_extras() {
        let Some(bytes) = read("corpus/Canon/EOS 5D Mark IV/B13A0729.CR2") else { return };
        let m = json::metadata(&open(bytes, "").unwrap());
        assert!(m["dng"].is_null());
        assert_eq!(m["opcodes"], json!([]));
    }
}
