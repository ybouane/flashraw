//! Python bindings for FlashRaw: the native `flashraw._flashraw` extension module.
//!
//! The public Python API lives in `python/flashraw/` and wraps this module:
//!
//! - **Input**: `bytes` (and read-only views of `bytes` or a read-only `mmap`) are borrowed
//!   without a copy for the lifetime of the [`RawFile`]; paths are memory-mapped; mutable
//!   buffers (`bytearray`, writable views) are copied once, because Python code could change
//!   them while a decode runs on another thread.
//! - **Output**: decoded pixels are moved into a NumPy array without a copy (the array's base
//!   object owns the Rust allocation); the frame description is a plain dict.
//! - **Threads**: decoding runs with the GIL released, so Python threads decode in parallel;
//!   each decode still uses FlashRaw's own thread pool.
//! - **Errors**: raised as `flashraw.FlashRawError` subclasses (one per `Error::kind`),
//!   constructed by `flashraw._errors._make`.

#![deny(unsafe_op_in_unsafe_fn)]

mod convert;
mod rawfile;

use numpy::{PyArrayDescrMethods, PyUntypedArray, PyUntypedArrayMethods};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::PyBytes;

pub(crate) use rawfile::RawFile;

/// Construct a `flashraw` exception through the Python layer (`flashraw._errors._make`).
/// `corrupt` is the structured cause of a `CorruptFile` error, `errno` the OS error code of
/// an `Io` error.
pub(crate) fn make_error(
    py: Python<'_>,
    kind: &str,
    message: &str,
    corrupt: Option<&serde_json::Value>,
    errno: Option<i32>,
) -> PyErr {
    static MAKE: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
    let build = || -> PyResult<PyErr> {
        let make = MAKE.get_or_try_init(py, || -> PyResult<Py<PyAny>> {
            Ok(py.import("flashraw._errors")?.getattr("_make")?.unbind())
        })?;
        let corrupt = match corrupt {
            Some(c) => convert::to_py(py, c)?,
            None => py.None().into_bound(py),
        };
        let exc = make.bind(py).call1((kind, message, corrupt, errno))?;
        Ok(PyErr::from_value(exc))
    };
    build().unwrap_or_else(|e| e)
}

/// Convert a FlashRaw error into the matching Python exception.
pub(crate) fn to_pyerr(py: Python<'_>, e: flashraw::Error) -> PyErr {
    let corrupt = e.corrupt_reason().map(convert::corrupt_reason);
    let errno = match &e {
        flashraw::Error::Io(io) => io.raw_os_error(),
        _ => None,
    };
    let message = match &e {
        // OSError formats "[Errno n] message" itself.
        flashraw::Error::Io(io) => {
            let s = io.to_string();
            match errno {
                Some(n) => s.trim_end_matches(&format!(" (os error {n})")).to_string(),
                None => s,
            }
        }
        _ => e.to_string(),
    };
    make_error(py, e.kind(), &message, corrupt.as_ref(), errno)
}

pub(crate) fn invalid(py: Python<'_>, msg: impl AsRef<str>) -> PyErr {
    make_error(py, "InvalidArgument", msg.as_ref(), None, None)
}

/// Format detection: the container format name (e.g. `"NEF"`) when `data` looks like a
/// supported RAW file, else `None`. `data` is any bytes-like object (not copied) or a path.
#[pyfunction]
fn detect_format(py: Python<'_>, data: &Bound<'_, PyAny>) -> PyResult<Option<&'static str>> {
    if let Some(path) = rawfile::as_path(data)? {
        let f = std::fs::File::open(&path).map_err(|e| to_pyerr(py, e.into()))?;
        // SAFETY: read-only mapping, only read for the duration of this call (the same
        // assumption as `FlashRaw::open_path`: the file is not truncated concurrently).
        let m = unsafe { memmap2::Mmap::map(&f) }.map_err(|e| to_pyerr(py, e.into()))?;
        return Ok(py.detach(|| flashraw::supports(&m).map(|f| f.name())));
    }
    let (b, _) = rawfile::as_bytes_object(data)?;
    Ok(flashraw::supports(b.as_bytes()).map(|f| f.name()))
}

/// Metadata of a RAW file as a dict (no sensor decoding).
#[pyfunction]
#[pyo3(signature = (source, max_samples=None, max_dimension=None))]
fn read_metadata<'py>(
    py: Python<'py>,
    source: &Bound<'py, PyAny>,
    max_samples: Option<u64>,
    max_dimension: Option<u32>,
) -> PyResult<Bound<'py, PyAny>> {
    let f = RawFile::open(py, source, max_samples, max_dimension)?;
    f.metadata(py)
}

/// FNV-1a 64 hash of the bytes of a `bytes` object or a C-contiguous NumPy array. Over a
/// little-endian `uint16` array this equals the `hash_full` reported by `flashraw verify`.
#[pyfunction]
fn fnv1a64(py: Python<'_>, data: &Bound<'_, PyAny>) -> PyResult<u64> {
    let hash = |bytes: &[u8]| {
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
        h
    };
    if let Ok(b) = data.cast::<PyBytes>() {
        let bytes = b.as_bytes();
        return Ok(py.detach(|| hash(bytes)));
    }
    let a = data.cast::<PyUntypedArray>().map_err(|_| invalid(py, "fnv1a64 expects bytes or a NumPy array"))?;
    if !a.is_c_contiguous() {
        return Err(invalid(py, "fnv1a64 needs a C-contiguous array"));
    }
    let n = a.len() * a.dtype().itemsize();
    // SAFETY: a C-contiguous array holds `len * itemsize` bytes at `data`; the array is
    // referenced by `a` for the duration of this call and only read here.
    let bytes = unsafe { std::slice::from_raw_parts((*a.as_array_ptr()).data as *const u8, n) };
    Ok(py.detach(|| hash(bytes)))
}

/// Whether a GPU backend is compiled in and an adapter is available.
#[pyfunction]
fn gpu_available(py: Python<'_>) -> bool {
    py.detach(flashraw::gpu_available)
}

/// Build information: version, compiled features, SIMD level and default thread count.
#[pyfunction]
fn build_info(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let v = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "simd": flashraw::low_level::core::simd::kernel_level().name(),
        "threads": threads,
        "gpu": cfg!(feature = "gpu"),
        "jpegxl": cfg!(feature = "jpegxl"),
        "outputs": [
            "raw-u16",
            "raw-f32",
            "normalized",
            "linear-rgb",
            "linear-rgb16",
            "linear-rgb-f16",
            "rgba8",
            "rgb16"
        ],
        "backends": ["auto", "cpu", "simd", "gpu"],
    });
    convert::to_py(py, &v)
}

/// The native extension module (`flashraw._flashraw`).
#[pymodule(gil_used = false)]
fn _flashraw(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<RawFile>()?;
    m.add_function(wrap_pyfunction!(detect_format, m)?)?;
    m.add_function(wrap_pyfunction!(read_metadata, m)?)?;
    m.add_function(wrap_pyfunction!(fnv1a64, m)?)?;
    m.add_function(wrap_pyfunction!(gpu_available, m)?)?;
    m.add_function(wrap_pyfunction!(build_info, m)?)?;
    Ok(())
}
