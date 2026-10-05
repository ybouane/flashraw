//! `flashraw_file`: opening files and reading metadata.

use crate::calib;
use crate::error::{FfiError, FfiResult, cstring_lossy, flashraw_status, guard, guard_value};
use crate::types::*;
use flashraw::{FlashRaw, Limits, RawMetadata};
use std::ffi::{CStr, CString, c_char};

/// An opened RAW file (opaque). Metadata and previews are available immediately; pixels are
/// decoded with `flashraw_decode()`.
///
/// Thread safety: all functions taking a `const flashraw_file*` may be called concurrently
/// from any number of threads (including several concurrent decodes of the same file).
/// `flashraw_file_free()` must not race with any other use of the handle.
pub struct flashraw_file {
    pub(crate) raw: FlashRaw<'static>,
    /// Limits the file was opened with; the default for its decodes.
    pub(crate) limits: Limits,
    strings: [Option<CString>; STRING_KEYS],
}

impl flashraw_file {
    fn new(raw: FlashRaw<'static>, limits: Limits) -> Self {
        let m: &RawMetadata = raw.metadata();
        let s = |v: &str| (!v.is_empty()).then(|| cstring_lossy(v));
        let o = |v: &Option<String>| v.as_deref().and_then(s);
        let strings = [
            s(&m.make),
            s(&m.model),
            s(&m.canonical_make),
            s(&m.canonical_model),
            s(&m.camera()),
            s(raw.format().name()),
            s(raw.frame_info().compression.name()),
            o(&m.software),
            o(&m.firmware),
            o(&m.serial),
            o(&m.artist),
            o(&m.copyright),
            o(&m.capture_time),
            o(&m.lens.make),
            o(&m.lens.model),
            o(&m.lens.serial),
            o(&m.unique_camera_model),
        ];
        flashraw_file { raw, limits, strings }
    }
}

/// Borrow a file handle.
///
/// # Safety
/// `f` must be NULL or a live handle returned by an open function.
pub(crate) unsafe fn file_ref<'a>(f: *const flashraw_file) -> FfiResult<&'a flashraw_file> {
    // SAFETY: per the contract, a non-null `f` points to a live `flashraw_file`.
    unsafe { f.as_ref() }.ok_or_else(|| FfiError::null("file"))
}

fn limits_from(o: &flashraw_open_options) -> Limits {
    let mut l = Limits::default();
    if o.max_samples != 0 {
        l = l.with_max_samples(o.max_samples);
    }
    if o.max_dimension != 0 {
        l = l.with_max_dimension(o.max_dimension);
    }
    l
}

fn default_open_options() -> flashraw_open_options {
    flashraw_open_options {
        size: std::mem::size_of::<flashraw_open_options>() as u32,
        max_dimension: 0,
        max_samples: 0,
    }
}

/// Initialise `opts` with the defaults and `opts->size = sizeof(flashraw_open_options)`.
///
/// # Safety
/// `opts` must be NULL or point to a writable `flashraw_open_options`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_open_options_init(opts: *mut flashraw_open_options) -> flashraw_status {
    guard(|| {
        // SAFETY: a non-null `opts` points to writable memory of the struct's size.
        let o = unsafe { opts.as_mut() }.ok_or_else(|| FfiError::null("opts"))?;
        *o = default_open_options();
        Ok(())
    })
}

/// Store a new handle in `*out`.
///
/// # Safety
/// `out` must be NULL or writable.
unsafe fn open_into(
    out: *mut *mut flashraw_file,
    opts: *const flashraw_open_options,
    open: impl FnOnce(Limits) -> FfiResult<FlashRaw<'static>>,
) -> flashraw_status {
    guard(|| {
        if out.is_null() {
            return Err(FfiError::null("out"));
        }
        // SAFETY: `out` is non-null and writable per the contract. Cleared first so callers
        // never see a stale pointer after an error.
        unsafe { out.write(std::ptr::null_mut()) };
        // SAFETY: `opts` is NULL or points to a versioned options struct (contract).
        let o = unsafe { read_versioned(opts, default_open_options(), "flashraw_open_options") }?;
        let limits = limits_from(&o);
        let raw = open(limits)?;
        let handle = Box::into_raw(Box::new(flashraw_file::new(raw, limits)));
        // SAFETY: as above.
        unsafe { out.write(handle) };
        Ok(())
    })
}

/// Open a file held in caller memory **without copying it** (zero-copy).
///
/// The `len` bytes at `data` must stay valid and unmodified until `flashraw_file_free()`
/// has returned, and every frame or preview pointer obtained from the handle must be
/// treated as invalid after that. `opts` may be NULL (defaults). On success `*out` receives
/// the handle; on failure it is set to NULL.
///
/// # Safety
/// `data` must point to `len` readable bytes that outlive the returned handle; `out` must be
/// writable; `opts` must be NULL or initialised with `flashraw_open_options_init()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_open_memory(
    data: *const u8,
    len: usize,
    opts: *const flashraw_open_options,
    out: *mut *mut flashraw_file,
) -> flashraw_status {
    // SAFETY: forwarded caller contract on `out` and `opts`.
    unsafe {
        open_into(out, opts, |limits| {
            if data.is_null() && len > 0 {
                return Err(FfiError::null("data"));
            }
            let bytes: &'static [u8] = if len == 0 {
                &[]
            } else {
                // SAFETY: the caller guarantees `len` readable bytes at `data` that stay
                // valid and unmodified for the lifetime of the handle (documented contract);
                // the 'static lifetime never escapes the handle.
                std::slice::from_raw_parts(data, len)
            };
            Ok(FlashRaw::open_with(bytes, limits)?)
        })
    }
}

/// Open a file held in caller memory, copying it first: the caller may free `data` as soon
/// as the call returns. Otherwise like `flashraw_open_memory()`.
///
/// # Safety
/// `data` must point to `len` readable bytes; `out` must be writable; `opts` must be NULL or
/// initialised with `flashraw_open_options_init()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_open_memory_copy(
    data: *const u8,
    len: usize,
    opts: *const flashraw_open_options,
    out: *mut *mut flashraw_file,
) -> flashraw_status {
    // SAFETY: forwarded caller contract on `out` and `opts`.
    unsafe {
        open_into(out, opts, |limits| {
            if data.is_null() && len > 0 {
                return Err(FfiError::null("data"));
            }
            let v = if len == 0 {
                Vec::new()
            } else {
                // SAFETY: the caller guarantees `len` readable bytes at `data` for the
                // duration of this call; they are copied before returning.
                std::slice::from_raw_parts(data, len).to_vec()
            };
            Ok(FlashRaw::open_with(v, limits)?)
        })
    }
}

/// Open a file from disk (memory-mapped read-only). `path` is a NUL-terminated UTF-8
/// path. Otherwise like `flashraw_open_memory()`.
///
/// As with any memory-mapped reader, the file must not be truncated by another process
/// while the handle is alive.
///
/// # Safety
/// `path` must be a NUL-terminated string; `out` must be writable; `opts` must be NULL or
/// initialised with `flashraw_open_options_init()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_open_path(
    path: *const c_char,
    opts: *const flashraw_open_options,
    out: *mut *mut flashraw_file,
) -> flashraw_status {
    // SAFETY: forwarded caller contract on `out` and `opts`.
    unsafe {
        open_into(out, opts, |limits| {
            if path.is_null() {
                return Err(FfiError::null("path"));
            }
            // SAFETY: `path` is a NUL-terminated string per the contract.
            let p = CStr::from_ptr(path).to_str().map_err(|_| FfiError::invalid("path is not valid UTF-8"))?;
            let file = std::fs::File::open(p).map_err(flashraw::Error::from)?;
            let len = file.metadata().map_err(flashraw::Error::from)?.len();
            let source = if len == 0 {
                // Empty files cannot be mapped everywhere; they are rejected by the parser.
                flashraw::Source::Owned(Vec::new())
            } else {
                // SAFETY: read-only mapping; FlashRaw never writes through it and
                // bounds-checks every access against the length captured at map time.
                // Concurrent truncation by another process is excluded by the documented
                // contract (as for every memory-mapped reader).
                flashraw::Source::Mmap(memmap2::Mmap::map(&file).map_err(flashraw::Error::from)?)
            };
            Ok(FlashRaw::open_with(source, limits)?)
        })
    }
}

/// Free a file handle (NULL is ignored). Frames decoded from it stay valid; pointers
/// returned by the file's getters (strings, preview bytes, JSON excluded) do not.
///
/// # Safety
/// `f` must be NULL or a handle from an open function that has not been freed yet, and no
/// other thread may be using it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_free(f: *mut flashraw_file) {
    if f.is_null() {
        return;
    }
    // SAFETY: `f` came from `Box::into_raw` in `open_into` and is freed exactly once.
    let b = unsafe { Box::from_raw(f) };
    // Dropping runs decoder destructors only; contain any panic so it cannot unwind into C.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(b)));
}

/// A metadata string, or NULL when the file does not record it. Owned by the handle
/// (valid until `flashraw_file_free()`).
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_string(f: *const flashraw_file, key: i32) -> *const c_char {
    guard_value(std::ptr::null(), || {
        // SAFETY: caller contract.
        let f = unsafe { file_ref(f) }?;
        let i = usize::try_from(key)
            .ok()
            .filter(|&i| i < STRING_KEYS)
            .ok_or_else(|| FfiError::invalid(format!("unknown string key {key}")))?;
        Ok(f.strings[i].as_ref().map_or(std::ptr::null(), |s| s.as_ptr()))
    })
}

/// Make as recorded in the file (NULL if absent). Same lifetime as `flashraw_file_string()`.
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_make(f: *const flashraw_file) -> *const c_char {
    // SAFETY: caller contract.
    unsafe { flashraw_file_string(f, flashraw_string_key::FLASHRAW_STRING_MAKE as i32) }
}

/// Model as recorded in the file (NULL if absent).
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_model(f: *const flashraw_file) -> *const c_char {
    // SAFETY: caller contract.
    unsafe { flashraw_file_string(f, flashraw_string_key::FLASHRAW_STRING_MODEL as i32) }
}

/// Normalised make, e.g. "Nikon" (NULL if unknown).
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_canonical_make(f: *const flashraw_file) -> *const c_char {
    // SAFETY: caller contract.
    unsafe { flashraw_file_string(f, flashraw_string_key::FLASHRAW_STRING_CANONICAL_MAKE as i32) }
}

/// Normalised model (NULL if unknown).
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_canonical_model(f: *const flashraw_file) -> *const c_char {
    // SAFETY: caller contract.
    unsafe { flashraw_file_string(f, flashraw_string_key::FLASHRAW_STRING_CANONICAL_MODEL as i32) }
}

/// Container format name: "DNG", "NEF", "CR3", ...
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_format_name(f: *const flashraw_file) -> *const c_char {
    // SAFETY: caller contract.
    unsafe { flashraw_file_string(f, flashraw_string_key::FLASHRAW_STRING_FORMAT as i32) }
}

/// Run a getter on a file handle; NULL handles yield `fallback` and an error.
fn with_file<T>(f: *const flashraw_file, fallback: T, g: impl FnOnce(&flashraw_file) -> FfiResult<T>) -> T {
    guard_value(fallback, || {
        // SAFETY: every exported getter documents that `f` is NULL or a live handle.
        let f = unsafe { file_ref(f) }?;
        g(f)
    })
}

/// Width of the full sensor buffer in pixels (including masked borders); what a raw
/// output decode returns. 0 on error.
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_raw_width(f: *const flashraw_file) -> u32 {
    with_file(f, 0, |f| Ok(f.raw.frame_info().width))
}

/// Height of the full sensor buffer in pixels (including masked borders). 0 on error.
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_raw_height(f: *const flashraw_file) -> u32 {
    with_file(f, 0, |f| Ok(f.raw.frame_info().height))
}

/// Width after the default crop (the size of a cropped RGB output before orientation).
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_width(f: *const flashraw_file) -> u32 {
    with_file(f, 0, |f| Ok(f.raw.metadata().width))
}

/// Height after the default crop.
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_height(f: *const flashraw_file) -> u32 {
    with_file(f, 0, |f| Ok(f.raw.metadata().height))
}

/// Samples per pixel of the sensor data (1 for CFA/monochrome, 3+ for linear data).
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_cpp(f: *const flashraw_file) -> u32 {
    with_file(f, 0, |f| Ok(f.raw.frame_info().cpp))
}

/// Significant bits per stored sample (e.g. 12 or 14).
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_bits_per_sample(f: *const flashraw_file) -> u32 {
    with_file(f, 0, |f| Ok(f.raw.frame_info().bits_per_sample as u32))
}

/// Non-zero when the sensor data is floating point (e.g. floating-point DNG).
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_is_float(f: *const flashraw_file) -> bool {
    with_file(f, false, |f| Ok(f.raw.capabilities().float))
}

/// Region of valid image pixels in sensor-buffer coordinates (excludes masked borders).
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_active_area(
    f: *const flashraw_file,
    out: *mut flashraw_rect,
) -> flashraw_status {
    rect_out(f, out, |f| f.raw.frame_info().active_area)
}

/// Recommended default crop in sensor-buffer coordinates (inside the active area).
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_crop(f: *const flashraw_file, out: *mut flashraw_rect) -> flashraw_status {
    rect_out(f, out, |f| f.raw.frame_info().crop)
}

fn rect_out(
    f: *const flashraw_file,
    out: *mut flashraw_rect,
    g: impl FnOnce(&flashraw_file) -> flashraw::Rect,
) -> flashraw_status {
    guard(|| {
        // SAFETY: exported callers document `f` as NULL or live.
        let f = unsafe { file_ref(f) }?;
        // SAFETY: a non-null `out` is writable per the exported contract.
        let o = unsafe { out.as_mut() }.ok_or_else(|| FfiError::null("out"))?;
        *o = g(f).into();
        Ok(())
    })
}

/// Number of masked (optical black) areas.
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_masked_area_count(f: *const flashraw_file) -> usize {
    with_file(f, 0, |f| Ok(f.raw.frame_info().masked_areas.len()))
}

/// Masked area `index` in sensor-buffer coordinates.
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_masked_area(
    f: *const flashraw_file,
    index: usize,
    out: *mut flashraw_rect,
) -> flashraw_status {
    guard(|| {
        // SAFETY: caller contract.
        let f = unsafe { file_ref(f) }?;
        // SAFETY: a non-null `out` is writable per the contract.
        let o = unsafe { out.as_mut() }.ok_or_else(|| FfiError::null("out"))?;
        let m = &f.raw.frame_info().masked_areas;
        *o = (*m.get(index).ok_or_else(|| FfiError::not_available(format!("masked area {index} of {}", m.len())))?)
            .into();
        Ok(())
    })
}

/// EXIF orientation (1-8) to apply for display. 1 on error.
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_orientation(f: *const flashraw_file) -> u32 {
    with_file(f, 1, |f| Ok(f.raw.metadata().orientation.exif()))
}

/// Sensor layout (`flashraw_sensor_layout`).
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_sensor_layout(f: *const flashraw_file) -> i32 {
    with_file(f, flashraw_sensor_layout::FLASHRAW_LAYOUT_UNKNOWN as i32, |f| {
        Ok(layout_to(f.raw.frame_info().layout) as i32)
    })
}

/// The CFA pattern. Returns true and fills `*out` when the sensor has one; false for
/// monochrome, linear and layered data (and on error).
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_cfa(f: *const flashraw_file, out: *mut flashraw_cfa) -> bool {
    with_file(f, false, |f| calib::cfa(f.raw.frame_info().cfa, out))
}

/// Black level as known before decoding: fills `*info` (may be NULL) and copies up to
/// `capacity` pattern values into `values` (may be NULL when `capacity` is 0). Returns the
/// total number of values. For formats that measure black from masked areas this is the
/// fallback value; the frame's black level (`flashraw_frame_black_level()`) is the measured
/// one.
///
/// # Safety
/// `f` must be NULL or a live handle; `info` NULL or writable; `values` NULL or writable for
/// `capacity` floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_black_level(
    f: *const flashraw_file,
    info: *mut flashraw_black_level,
    values: *mut f32,
    capacity: usize,
) -> usize {
    with_file(f, 0, |f| calib::black(&f.raw.frame_info().black_level, info, values, capacity))
}

/// White (saturation) levels, one per CFA colour or sample plane (a single value applies
/// to all). Copies up to `capacity` values and returns the total count.
///
/// # Safety
/// `f` must be NULL or a live handle; `values` NULL or writable for `capacity` elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_white_level(
    f: *const flashraw_file,
    values: *mut u32,
    capacity: usize,
) -> usize {
    with_file(f, 0, |f| calib::white(&f.raw.frame_info().white_level, values, capacity))
}

/// Largest white level (0 on error).
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_white_level_max(f: *const flashraw_file) -> u32 {
    with_file(f, 0, |f| Ok(f.raw.frame_info().white_level.max()))
}

/// As-shot white balance multipliers `[R, G, B, G2]` normalised to G = 1 into
/// `coeffs[4]`. Returns false when the file records none.
///
/// # Safety
/// `f` must be NULL or a live handle; `coeffs` must point to 4 writable floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_white_balance(f: *const flashraw_file, coeffs: *mut f32) -> bool {
    with_file(f, false, |f| calib::white_balance(f.raw.frame_info().white_balance, coeffs))
}

/// Number of matrices of `kind` (`flashraw_matrix_kind`).
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_color_matrix_count(f: *const flashraw_file, kind: i32) -> usize {
    with_file(f, 0, |f| calib::matrix_count(&f.raw.frame_info().color_matrices, kind))
}

/// Matrix `index` of `kind` (`flashraw_matrix_kind`).
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_color_matrix(
    f: *const flashraw_file,
    kind: i32,
    index: usize,
    out: *mut flashraw_color_matrix,
) -> flashraw_status {
    guard(|| {
        // SAFETY: caller contract.
        let f = unsafe { file_ref(f) }?;
        calib::matrix(&f.raw.frame_info().color_matrices, kind, index, out)
    })
}

/// Where the colour calibration came from (`flashraw_color_source`).
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_color_source(f: *const flashraw_file) -> i32 {
    with_file(f, flashraw_color_source::FLASHRAW_COLOR_SOURCE_UNKNOWN as i32, |f| {
        Ok(color_source_to(f.raw.frame_info().color_matrices.source) as i32)
    })
}

/// Write an optional metadata value to `out`; false when absent.
fn opt_out<T>(f: *const flashraw_file, out: *mut T, g: impl FnOnce(&RawMetadata) -> Option<T>) -> bool {
    with_file(f, false, |f| {
        if out.is_null() {
            return Err(FfiError::null("out"));
        }
        match g(f.raw.metadata()) {
            Some(v) => {
                // SAFETY: `out` is non-null and writable (exported contract).
                unsafe { out.write(v) };
                Ok(true)
            }
            None => Ok(false),
        }
    })
}

/// ISO speed. Returns false (leaving `*out` untouched) when the file does not record it.
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_iso(f: *const flashraw_file, out: *mut u32) -> bool {
    opt_out(f, out, |m| m.exposure.iso)
}

/// Exposure time in seconds. Returns false (leaving `*out` untouched) when the file does not record it.
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_exposure_time(f: *const flashraw_file, out: *mut f64) -> bool {
    opt_out(f, out, |m| m.exposure.exposure_time)
}

/// Aperture as an f-number. Returns false (leaving `*out` untouched) when the file does not record it.
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_f_number(f: *const flashraw_file, out: *mut f32) -> bool {
    opt_out(f, out, |m| m.exposure.f_number)
}

/// Focal length in mm. Returns false (leaving `*out` untouched) when the file does not record it.
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_focal_length(f: *const flashraw_file, out: *mut f32) -> bool {
    opt_out(f, out, |m| m.exposure.focal_length)
}

/// 35 mm-equivalent focal length in mm. Returns false (leaving `*out` untouched) when the file does not record it.
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_focal_length_35mm(f: *const flashraw_file, out: *mut u32) -> bool {
    opt_out(f, out, |m| m.exposure.focal_length_35mm)
}

/// Exposure compensation in EV. Returns false (leaving `*out` untouched) when the file does not record it.
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_exposure_bias(f: *const flashraw_file, out: *mut f32) -> bool {
    opt_out(f, out, |m| m.exposure.exposure_bias)
}

/// Lens focal range in mm (`min`, `max`). Returns false when not recorded.
///
/// # Safety
/// `f` must be NULL or a live handle; `min` and `max` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_lens_focal_range(f: *const flashraw_file, min: *mut f32, max: *mut f32) -> bool {
    with_file(f, false, |f| {
        if min.is_null() || max.is_null() {
            return Err(FfiError::null("min/max"));
        }
        let Some((a, b)) = f.raw.metadata().lens.focal_range else { return Ok(false) };
        // SAFETY: both pointers are non-null and writable (contract).
        unsafe {
            min.write(a);
            max.write(b);
        }
        Ok(true)
    })
}

/// Number of embedded previews / thumbnails.
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_preview_count(f: *const flashraw_file) -> usize {
    with_file(f, 0, |f| Ok(f.raw.previews().len()))
}

fn preview_index(f: &flashraw_file, largest: bool) -> i64 {
    let p = if largest { f.raw.preview() } else { f.raw.thumbnail() };
    p.and_then(|p| f.raw.previews().iter().position(|x| std::ptr::eq(x, p))).map_or(-1, |i| i as i64)
}

/// Index of the largest embedded preview, or -1 when there is none.
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_largest_preview(f: *const flashraw_file) -> i64 {
    with_file(f, -1, |f| Ok(preview_index(f, true)))
}

/// Index of the smallest embedded preview (thumbnail), or -1 when there is none.
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_smallest_preview(f: *const flashraw_file) -> i64 {
    with_file(f, -1, |f| Ok(preview_index(f, false)))
}

/// Describe preview `index`. Set `out->size = sizeof(flashraw_preview_info)` first.
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or point to a writable struct
/// whose `size` field is set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_preview_info(
    f: *const flashraw_file,
    index: usize,
    out: *mut flashraw_preview_info,
) -> flashraw_status {
    guard(|| {
        // SAFETY: caller contract.
        let f = unsafe { file_ref(f) }?;
        let ps = f.raw.previews();
        let p = ps.get(index).ok_or_else(|| FfiError::not_available(format!("preview {index} of {}", ps.len())))?;
        let v = flashraw_preview_info {
            size: std::mem::size_of::<flashraw_preview_info>() as u32,
            width: p.width,
            height: p.height,
            encoding: encoding_to(p.encoding),
            kind: kind_to(p.kind),
            orientation: p.orientation.exif(),
            offset: p.offset,
            length: p.length,
        };
        // SAFETY: caller contract on `out`.
        unsafe { write_versioned(out, &v, "out") }
    })
}

/// The bytes of preview `index` (e.g. a complete JPEG file), zero-copy: the pointer is into
/// the file's memory and stays valid until `flashraw_file_free()`. Stores the length in
/// `*len`. Returns NULL on error (no such preview, or its range lies outside the file).
///
/// # Safety
/// `f` must be NULL or a live handle; `len` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_preview_bytes(
    f: *const flashraw_file,
    index: usize,
    len: *mut usize,
) -> *const u8 {
    with_file(f, std::ptr::null(), |f| {
        if len.is_null() {
            return Err(FfiError::null("len"));
        }
        // SAFETY: `len` is non-null and writable (contract).
        unsafe { len.write(0) };
        let ps = f.raw.previews();
        let p = ps.get(index).ok_or_else(|| FfiError::not_available(format!("preview {index} of {}", ps.len())))?;
        let b = f.raw.preview_bytes(p)?;
        // SAFETY: as above.
        unsafe { len.write(b.len()) };
        Ok(b.as_ptr())
    })
}

/// Number of independently decodable units (tiles / strips) of the payload.
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_unit_count(f: *const flashraw_file) -> usize {
    with_file(f, 0, |f| Ok(f.raw.units().len()))
}

/// Rectangle (sensor-buffer coordinates) of unit `index`; decode it by passing it as the
/// decode region.
///
/// # Safety
/// `f` must be NULL or a live handle; `out` must be NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_unit(
    f: *const flashraw_file,
    index: usize,
    out: *mut flashraw_rect,
) -> flashraw_status {
    guard(|| {
        // SAFETY: caller contract.
        let f = unsafe { file_ref(f) }?;
        // SAFETY: a non-null `out` is writable per the contract.
        let o = unsafe { out.as_mut() }.ok_or_else(|| FfiError::null("out"))?;
        let u = f.raw.units();
        *o = u.get(index).ok_or_else(|| FfiError::not_available(format!("unit {index} of {}", u.len())))?.rect.into();
        Ok(())
    })
}

/// All metadata as a JSON document (camera, exposure, lens, GPS, DNG tags, previews,
/// sensor geometry and calibration, capabilities): the escape hatch for fields without a
/// getter. Returns a NUL-terminated string to release with `flashraw_string_free()`, or
/// NULL on error.
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_metadata_json(f: *const flashraw_file) -> *mut c_char {
    with_file(f, std::ptr::null_mut(), |f| Ok(cstring_lossy(&crate::json::file(&f.raw).to_string()).into_raw()))
}

/// Time spent parsing the container in milliseconds.
///
/// # Safety
/// `f` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_file_parse_ms(f: *const flashraw_file) -> f64 {
    with_file(f, 0.0, |f| Ok(f.raw.parse_ms()))
}

const _: () = {
    const fn is_send_sync<T: Send + Sync>() {}
    is_send_sync::<flashraw_file>();
};
