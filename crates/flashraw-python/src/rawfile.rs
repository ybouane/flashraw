//! The native `RawFile`: input handling, decoding with the GIL released, zero-copy outputs.

use crate::{convert, invalid, make_error, to_pyerr};
use flashraw::{
    Backend, CorruptionPolicy, DecodeOptions, Demosaic, FlashRaw, Limits, OutputFormat, PreviewInfo, RawFrame,
    RawPixelData, Rect,
};
use numpy::ndarray::{ArrayD, IxDyn};
use numpy::{Element, PyArray};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyMemoryView, PyString};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{RwLock, RwLockReadGuard};

/// What keeps the input bytes alive while the decoder borrows them.
enum Keep {
    /// The decoder owns its input (memory map or copied buffer).
    Nothing,
    /// An immutable `bytes` object.
    Bytes(#[allow(dead_code)] Py<PyBytes>),
}

struct Inner {
    // Field order matters: `raw` borrows from `_keep` and must be dropped first.
    raw: FlashRaw<'static>,
    _keep: Keep,
}

/// How the input reached the decoder (reported as `RawFile.source`).
#[derive(Clone, Copy)]
enum SourceKind {
    /// Borrowed from a Python object without a copy.
    Borrowed,
    /// Memory-mapped from a path.
    Mapped,
    /// Copied once (mutable buffers).
    Copied,
}

/// An opened RAW file (native half of `flashraw.RawFile`).
#[pyclass(module = "flashraw._flashraw", name = "_RawFile", frozen)]
pub struct RawFile {
    inner: RwLock<Option<Inner>>,
    limits: Limits,
    source: SourceKind,
    byte_length: usize,
}

/// A path argument: `str` or `os.PathLike`.
pub fn as_path(obj: &Bound<'_, PyAny>) -> PyResult<Option<PathBuf>> {
    if obj.is_instance_of::<PyString>() || obj.hasattr("__fspath__")? {
        return Ok(Some(obj.extract::<PathBuf>()?));
    }
    Ok(None)
}

/// The bytes of a bytes-like object as an immutable `bytes` object, and whether they were
/// borrowed without a copy: `bytes` itself, or a contiguous `memoryview` covering a whole
/// `bytes` object. Anything else that exports a buffer (`bytearray`, `mmap`, arrays, partial
/// views) is copied once into a new `bytes` object, because its memory could change while a
/// decode runs without the GIL.
pub fn as_bytes_object<'py>(obj: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyBytes>, bool)> {
    if let Ok(b) = obj.cast::<PyBytes>() {
        return Ok((b.clone(), true));
    }
    let py = obj.py();
    let mv = PyMemoryView::from(obj).map_err(|_| {
        invalid(
            py,
            format!(
                "expected a path, bytes-like object or binary file object, got {}",
                obj.get_type().name().map(|n| n.to_string()).unwrap_or_default()
            ),
        )
    })?;
    let base = mv.getattr("obj")?;
    if let Ok(b) = base.cast::<PyBytes>() {
        let whole =
            mv.getattr("c_contiguous")?.is_truthy()? && mv.getattr("nbytes")?.extract::<usize>()? == b.as_bytes().len();
        if whole {
            return Ok((b.clone(), true));
        }
    }
    Ok((mv.call_method0("tobytes")?.cast_into::<PyBytes>()?, false))
}

enum Failure {
    Closed,
    Flash(flashraw::Error),
    Panic(String),
}

impl Failure {
    fn into_pyerr(self, py: Python<'_>) -> PyErr {
        match self {
            Failure::Closed => closed_error(py),
            Failure::Flash(e) => to_pyerr(py, e),
            Failure::Panic(m) => make_error(py, "Internal", &format!("internal error: {m}"), None, None),
        }
    }
}

fn closed_error(py: Python<'_>) -> PyErr {
    make_error(py, "Closed", "the RawFile is closed", None, None)
}

/// Run `f`, turning a panic into an `Internal` failure instead of unwinding into Python.
fn guarded<T>(f: impl FnOnce() -> Result<T, Failure>) -> Result<T, Failure> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".into());
            Err(Failure::Panic(msg))
        }
    }
}

fn open_raw(source: impl Into<flashraw::Source<'static>>, limits: Limits) -> Result<FlashRaw<'static>, Failure> {
    let source = source.into();
    guarded(move || FlashRaw::open_with(source, limits).map_err(Failure::Flash))
}

fn limits(max_samples: Option<u64>, max_dimension: Option<u32>) -> Limits {
    let mut l = Limits::default();
    if let Some(s) = max_samples {
        l = l.with_max_samples(s);
    }
    if let Some(d) = max_dimension {
        l = l.with_max_dimension(d);
    }
    l
}

/// Shape of a frame's pixel array: `(h, w)` for one sample per pixel, else `(h, w, c)`.
fn shape(f: &RawFrame) -> Vec<usize> {
    let (h, w, c) = (f.height as usize, f.width as usize, f.cpp as usize);
    if c == 1 { vec![h, w] } else { vec![h, w, c] }
}

/// Move `v` into a NumPy array of `shape` without copying: the array's base object owns the
/// Rust allocation and frees it when the array is collected.
fn into_array<'py, T: Element>(py: Python<'py>, shape: &[usize], v: Vec<T>) -> PyResult<Bound<'py, PyAny>> {
    let a = ArrayD::from_shape_vec(IxDyn(shape), v)
        .map_err(|e| make_error(py, "Internal", &format!("pixel buffer does not match {shape:?}: {e}"), None, None))?;
    Ok(PyArray::from_owned_array(py, a).into_any())
}

/// Address of the first sample (for zero-copy checks).
fn data_address(d: &RawPixelData) -> usize {
    match d {
        RawPixelData::U8(v) => v.as_ptr() as usize,
        RawPixelData::U16(v) | RawPixelData::F16(v) => v.as_ptr() as usize,
        RawPixelData::F32(v) => v.as_ptr() as usize,
        #[allow(unreachable_patterns)]
        _ => 0,
    }
}

/// Run a decode with `opts.threads` worker threads in a pool owned by the calling thread.
///
/// The library shares one pool per thread count between all callers, so concurrent decodes
/// with the same explicit `threads` from several Python threads would queue on one pool (with
/// `threads=1`, run one at a time). A per-calling-thread pool keeps `threads` a per-decode
/// setting: each Python thread decoding with `threads=n` gets its own `n` workers. `None`
/// (all cores) uses the global pool shared by every decode.
fn with_pool<T: Send>(opts: &mut DecodeOptions, f: impl FnOnce(DecodeOptions) -> T + Send) -> T {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::Arc;
    thread_local! {
        static POOLS: RefCell<HashMap<usize, Arc<rayon::ThreadPool>>> = RefCell::new(HashMap::new());
    }
    // `threads=1` runs on the calling thread inside the library; no pool is needed.
    let Some(n) = opts.threads.filter(|&n| n > 1) else { return f(opts.clone()) };
    let pool = POOLS.with(|p| {
        let mut p = p.borrow_mut();
        if let Some(pool) = p.get(&n) {
            return Some(pool.clone());
        }
        let pool = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(n).build().ok()?);
        p.insert(n, pool.clone());
        Some(pool)
    });
    match pool {
        // Inside `install` the library sees `n` threads and needs no pool of its own.
        Some(pool) => {
            let o = DecodeOptions { threads: None, ..opts.clone() };
            pool.install(|| f(o))
        }
        None => f(opts.clone()),
    }
}

/// Decode-time options parsed from Python arguments.
struct Request {
    opts: DecodeOptions,
    backend: String,
    /// `crop="active"`: raw outputs cropped to the active area.
    active: bool,
    tile: Option<usize>,
}

fn is_raw_output(o: OutputFormat) -> bool {
    matches!(o, OutputFormat::RawU16 | OutputFormat::RawF32 | OutputFormat::NormalizedF32)
}

/// `crop="active"` on a raw output: decode the active area as a region (an explicit region
/// wins). The frame's `origin` and CFA phase then describe the cropped buffer.
fn apply_active_crop(raw: &FlashRaw<'_>, r: &mut Request) {
    let o = &mut r.opts;
    if r.active && o.region.is_none() && is_raw_output(o.output) {
        let f = raw.frame_info();
        let a = f.active_area;
        if a.width > 0 && a.height > 0 && (a.width, a.height) != (f.width, f.height) {
            o.region = Some(a);
        }
    }
}

impl RawFile {
    /// Open from a path, bytes-like object or binary file object.
    pub fn open(
        py: Python<'_>,
        source: &Bound<'_, PyAny>,
        max_samples: Option<u64>,
        max_dimension: Option<u32>,
    ) -> PyResult<Self> {
        let limits = limits(max_samples, max_dimension);
        let path = if source.is_instance_of::<PyBytes>() { None } else { as_path(source)? };
        if let Some(path) = path {
            let raw = py
                .detach(|| {
                    let f = std::fs::File::open(&path).map_err(|e| Failure::Flash(e.into()))?;
                    // SAFETY: a read-only mapping that FlashRaw never writes through; as for
                    // `FlashRaw::open_path`, parsing is bounds-checked against the length
                    // captured here (a concurrent truncation by another process could fault).
                    let m = unsafe { memmap2::Mmap::map(&f) }.map_err(|e| Failure::Flash(e.into()))?;
                    open_raw(m, limits)
                })
                .map_err(|f| f.into_pyerr(py))?;
            return Ok(Self::new(raw, Keep::Nothing, limits, SourceKind::Mapped));
        }
        let is_buffer = source.is_instance_of::<PyBytes>() || PyMemoryView::from(source).is_ok();
        if !is_buffer && source.hasattr("read")? {
            let data = source.call_method0("read")?;
            if data.hasattr("read")? || as_path(&data)?.is_some() {
                return Err(invalid(py, "read() must return a bytes-like object"));
            }
            return Self::open_bytes(py, &data, limits);
        }
        Self::open_bytes(py, source, limits)
    }

    fn open_bytes(py: Python<'_>, source: &Bound<'_, PyAny>, limits: Limits) -> PyResult<Self> {
        let (b, borrowed) = as_bytes_object(source)?;
        let bytes = b.as_bytes();
        // SAFETY: a `bytes` object is immutable and kept alive by `Keep::Bytes`, stored next to
        // the decoder and dropped after it (see `Inner`); its data never moves.
        let slice: &'static [u8] = unsafe { std::slice::from_raw_parts(bytes.as_ptr(), bytes.len()) };
        let raw = py.detach(|| open_raw(slice, limits)).map_err(|f| f.into_pyerr(py))?;
        let kind = if borrowed { SourceKind::Borrowed } else { SourceKind::Copied };
        Ok(Self::new(raw, Keep::Bytes(b.unbind()), limits, kind))
    }

    fn new(raw: FlashRaw<'static>, keep: Keep, limits: Limits, source: SourceKind) -> Self {
        let byte_length = raw.bytes().len();
        RawFile { inner: RwLock::new(Some(Inner { raw, _keep: keep })), limits, source, byte_length }
    }

    fn read(&self) -> RwLockReadGuard<'_, Option<Inner>> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Run `f` on the open file (under the GIL; for cheap accessors).
    fn with<T>(&self, py: Python<'_>, f: impl FnOnce(&FlashRaw<'static>) -> PyResult<T>) -> PyResult<T> {
        let g = self.read();
        let inner = g.as_ref().ok_or_else(|| closed_error(py))?;
        f(&inner.raw)
    }

    pub fn metadata<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let v = self.with(py, |raw| Ok(convert::metadata(raw)))?;
        convert::to_py(py, &v)
    }

    #[allow(clippy::too_many_arguments)] // one per Python keyword argument
    fn parse_request(
        py: Python<'_>,
        output: &str,
        backend: &str,
        threads: Option<usize>,
        region: Option<(u32, u32, u32, u32)>,
        crop: Option<&Bound<'_, PyAny>>,
        demosaic: &str,
        apply_opcodes: bool,
        corruption_policy: &str,
        levels: (f32, f32, f32),
        preview_fallback: bool,
        limits: Limits,
    ) -> PyResult<Request> {
        let out = OutputFormat::parse(output).ok_or_else(|| {
            invalid(
                py,
                format!(
                    "unknown output {output:?} (expected raw-u16, raw-f32, normalized, linear-rgb, linear-rgb16, \
                     linear-rgb-f16, rgba8 or rgb16)"
                ),
            )
        })?;
        if out.is_gpu_resident() {
            return Err(invalid(
                py,
                format!(
                    "GPU-resident output {output:?} is not available from Python; use backend=\"gpu\" with a host output"
                ),
            ));
        }
        let be = Backend::parse(backend)
            .ok_or_else(|| invalid(py, format!("unknown backend {backend:?} (expected auto, cpu, simd or gpu)")))?;
        let dm = Demosaic::parse(demosaic)
            .ok_or_else(|| invalid(py, format!("unknown demosaic {demosaic:?} (expected bilinear or half-size)")))?;
        let policy = CorruptionPolicy::parse(corruption_policy).ok_or_else(|| {
            invalid(py, format!("unknown corruption_policy {corruption_policy:?} (expected recover or strict)"))
        })?;
        if threads == Some(0) {
            return Err(invalid(py, "threads must be at least 1 (or None for all cores)"));
        }
        let (mut crop_rgb, mut active) = (true, false);
        if let Some(c) = crop.filter(|c| !c.is_none()) {
            if let Ok(b) = c.extract::<bool>() {
                crop_rgb = b;
            } else if c.extract::<String>().is_ok_and(|s| s == "active") {
                active = true;
            } else {
                return Err(invalid(py, "crop must be True, False or \"active\""));
            }
        }
        let region = match region {
            None => None,
            Some((x, y, w, h)) => {
                let r = Rect::new(x, y, w, h);
                if r.is_empty() {
                    return Err(invalid(py, format!("region {r} is empty")));
                }
                Some(r)
            }
        };
        let opts = DecodeOptions {
            backend: be,
            output: out,
            threads,
            region,
            crop: crop_rgb,
            demosaic: dm,
            apply_opcodes,
            limits,
            corruption_policy: policy,
            exposure: levels.0,
            black_point: levels.1,
            white_point: levels.2,
            preview_fallback,
        };
        Ok(Request { opts, backend: be.name().to_string(), active, tile: None })
    }

    /// Decode with the GIL released; returns `(array, info)`.
    fn run_decode<'py>(&self, py: Python<'py>, mut req: Request) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
        let (mut frame, call_ms) = py
            .detach(|| {
                let g = self.read();
                let inner = g.as_ref().ok_or(Failure::Closed)?;
                let raw = &inner.raw;
                guarded(|| {
                    apply_active_crop(raw, &mut req);
                    let t0 = std::time::Instant::now();
                    let frame = with_pool(&mut req.opts, |opts| match req.tile {
                        Some(i) => raw.decode_tile(i, opts),
                        None => raw.decode(opts),
                    })
                    .map_err(Failure::Flash)?;
                    Ok((frame, t0.elapsed().as_secs_f64() * 1e3))
                })
            })
            .map_err(|f| f.into_pyerr(py))?;
        #[cfg(feature = "gpu")]
        if frame.as_gpu().is_some() {
            frame = py.detach(|| frame.download()).map_err(|e| to_pyerr(py, e))?;
        }
        let mut info = convert::frame(&frame, &req.backend);
        info["data_address"] = serde_json::json!(data_address(&frame.data));
        info["stats"]["call_ms"] = serde_json::json!(call_ms);
        let shape = shape(&frame);
        let data = std::mem::replace(&mut frame.data, RawPixelData::U8(Vec::new()));
        let array = match data {
            RawPixelData::U8(v) => into_array(py, &shape, v)?,
            RawPixelData::U16(v) => into_array(py, &shape, v)?,
            // Half floats: the u16 bit patterns viewed as float16 (no copy).
            RawPixelData::F16(v) => into_array(py, &shape, v)?.call_method1("view", ("float16",))?,
            RawPixelData::F32(v) => into_array(py, &shape, v)?,
            #[allow(unreachable_patterns)]
            _ => return Err(make_error(py, "NotAvailable", "the frame is not in host memory", None, None)),
        };
        Ok((array, convert::to_py(py, &info)?))
    }

    /// Resolve a preview selector: `"largest"`, `"smallest"` or an index.
    fn select_preview<'r>(
        py: Python<'_>,
        raw: &'r FlashRaw<'_>,
        which: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(usize, &'r PreviewInfo)> {
        let all = raw.previews();
        let none = || make_error(py, "NotAvailable", "the file has no embedded preview", None, None);
        let pick = |p: Option<&PreviewInfo>| -> PyResult<(usize, &'r PreviewInfo)> {
            let p = p.ok_or_else(none)?;
            let i = all.iter().position(|x| std::ptr::eq(x, p)).unwrap_or(0);
            Ok((i, &all[i]))
        };
        let Some(which) = which.filter(|w| !w.is_none()) else { return pick(raw.preview()) };
        if let Ok(i) = which.extract::<i64>() {
            let n = all.len() as i64;
            let j = if i < 0 { i + n } else { i };
            if j < 0 || j >= n {
                return Err(make_error(
                    py,
                    "NotAvailable",
                    &format!("preview {i} out of range ({n} previews)"),
                    None,
                    None,
                ));
            }
            return Ok((j as usize, &all[j as usize]));
        }
        match which.extract::<String>()?.as_str() {
            "largest" | "preview" => pick(raw.preview()),
            "smallest" | "thumbnail" => pick(raw.thumbnail()),
            s => Err(invalid(
                py,
                format!("unknown preview selector {s:?} (expected \"largest\", \"smallest\" or an index)"),
            )),
        }
    }
}

#[pymethods]
impl RawFile {
    #[new]
    #[pyo3(signature = (source, max_samples=None, max_dimension=None))]
    fn py_new(
        py: Python<'_>,
        source: &Bound<'_, PyAny>,
        max_samples: Option<u64>,
        max_dimension: Option<u32>,
    ) -> PyResult<Self> {
        RawFile::open(py, source, max_samples, max_dimension)
    }

    /// Normalised metadata as a dict.
    #[pyo3(name = "metadata")]
    fn py_metadata<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.metadata(py)
    }

    /// Sensor-buffer geometry and calibration as a dict.
    fn frame_info<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let v = self.with(py, |raw| Ok(convert::frame_info(raw.frame_info())))?;
        convert::to_py(py, &v)
    }

    /// What the file supports, as a dict.
    fn capabilities<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let v = self.with(py, |raw| Ok(convert::capabilities(raw)))?;
        convert::to_py(py, &v)
    }

    /// Independently decodable tiles/strips, as a list of dicts.
    fn tiles<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let v = self.with(py, |raw| Ok(convert::units(&raw.units())))?;
        convert::to_py(py, &v)
    }

    /// Container format name (e.g. `"NEF"`).
    #[getter]
    fn format(&self, py: Python<'_>) -> PyResult<&'static str> {
        self.with(py, |raw| Ok(raw.format().name()))
    }

    /// Size of the input in bytes.
    #[getter]
    fn byte_length(&self) -> usize {
        self.byte_length
    }

    /// Milliseconds spent parsing the container in `open`.
    #[getter]
    fn parse_ms(&self, py: Python<'_>) -> PyResult<f64> {
        self.with(py, |raw| Ok(raw.parse_ms()))
    }

    /// How the input is held: `"borrowed"` (zero-copy view of a Python object), `"mapped"`
    /// (memory-mapped path) or `"copied"` (a mutable buffer copied once).
    #[getter]
    fn source(&self) -> &'static str {
        match self.source {
            SourceKind::Borrowed => "borrowed",
            SourceKind::Mapped => "mapped",
            SourceKind::Copied => "copied",
        }
    }

    /// Address of the first input byte (for zero-copy checks).
    #[getter]
    fn source_address(&self, py: Python<'_>) -> PyResult<usize> {
        self.with(py, |raw| Ok(raw.bytes().as_ptr() as usize))
    }

    #[getter]
    fn closed(&self) -> bool {
        self.read().is_none()
    }

    /// Release the input and the decoder (waits for running decodes; idempotent).
    fn close(&self, py: Python<'_>) {
        let taken = py.detach(|| self.inner.write().unwrap_or_else(|e| e.into_inner()).take());
        drop(taken);
    }

    /// Decode the sensor data. Returns `(array, info)`.
    #[pyo3(signature = (
        output="raw-u16", backend="auto", threads=None, region=None, crop=None, demosaic="bilinear",
        apply_opcodes=true, corruption_policy="recover", tile=None, exposure=0.0, black_point=0.0,
        white_point=1.0, preview_fallback=false
    ))]
    #[allow(clippy::too_many_arguments)]
    fn decode<'py>(
        &self,
        py: Python<'py>,
        output: &str,
        backend: &str,
        threads: Option<usize>,
        region: Option<(u32, u32, u32, u32)>,
        crop: Option<&Bound<'py, PyAny>>,
        demosaic: &str,
        apply_opcodes: bool,
        corruption_policy: &str,
        tile: Option<usize>,
        exposure: f32,
        black_point: f32,
        white_point: f32,
        preview_fallback: bool,
    ) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
        let mut req = Self::parse_request(
            py,
            output,
            backend,
            threads,
            region,
            crop,
            demosaic,
            apply_opcodes,
            corruption_policy,
            (exposure, black_point, white_point),
            preview_fallback,
            self.limits,
        )?;
        if tile.is_some() && req.opts.region.is_some() {
            return Err(invalid(py, "tile and region cannot be combined"));
        }
        req.tile = tile;
        self.run_decode(py, req)
    }

    /// The encoded bytes of an embedded preview (usually JPEG) and its description.
    #[pyo3(signature = (which=None))]
    fn preview_bytes<'py>(
        &self,
        py: Python<'py>,
        which: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<(Bound<'py, PyBytes>, Bound<'py, PyAny>)> {
        let (bytes, info) = self.with(py, |raw| {
            let (i, p) = Self::select_preview(py, raw, which)?;
            let b = raw.preview_bytes(p).map_err(|e| to_pyerr(py, e))?;
            let mut info = convert::preview(i, p);
            info["byte_length"] = serde_json::json!(b.len());
            Ok((PyBytes::new(py, b), info))
        })?;
        Ok((bytes, convert::to_py(py, &info)?))
    }

    /// Decode a JPEG or uncompressed RGB preview to an `(h, w, 3)` uint8 array (orientation
    /// not applied). Returns `(array, info)`.
    #[pyo3(signature = (which=None))]
    fn decode_preview<'py>(
        &self,
        py: Python<'py>,
        which: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
        let (index, info) = self.with(py, |raw| {
            let (i, p) = Self::select_preview(py, raw, which)?;
            Ok((i, convert::preview(i, p)))
        })?;
        let t0 = std::time::Instant::now();
        let img = py
            .detach(|| {
                let g = self.read();
                let inner = g.as_ref().ok_or(Failure::Closed)?;
                let raw = &inner.raw;
                guarded(|| raw.decode_preview_rgb(&raw.previews()[index]).map_err(Failure::Flash))
            })
            .map_err(|f| f.into_pyerr(py))?;
        let (w, h) = (img.width as usize, img.height as usize);
        if img.rgb.len() != w * h * 3 {
            return Err(make_error(
                py,
                "CorruptFile",
                &format!("preview has {} bytes for {w}x{h} RGB", img.rgb.len()),
                None,
                None,
            ));
        }
        let mut info = info;
        info["width"] = serde_json::json!(w);
        info["height"] = serde_json::json!(h);
        info["decode_ms"] = serde_json::json!(t0.elapsed().as_secs_f64() * 1e3);
        let array = into_array(py, &[h, w, 3], img.rgb)?;
        Ok((array, convert::to_py(py, &info)?))
    }
}
