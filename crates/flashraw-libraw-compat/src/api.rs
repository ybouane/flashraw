//! The exported `libraw_*` functions.
//!
//! Every function catches panics (a panic becomes `LIBRAW_UNSPECIFIED_ERROR`, `NULL` or
//! `-999.0`), and NULL handles are rejected as LibRaw documents (`EINVAL` for functions
//! returning `int`, `-999.0f` for `float`, `NULL` for pointers).

use crate::develop;
use crate::processor::{self, State};
use crate::types::*;
use flashraw::FlashRaw;
use std::alloc::Layout;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};

const EINVAL: c_int = 22;

/// Borrow the data and private state of a handle.
///
/// # Safety
/// `lr` must be NULL or a live handle from `libraw_init` whose `parent_class` was not
/// modified by the caller.
unsafe fn handle<'a>(lr: *mut libraw_data_t) -> Option<(&'a mut libraw_data_t, &'a mut State)> {
    // SAFETY: per the contract, a non-null `lr` is a live, exclusively used handle.
    let d = unsafe { lr.as_mut() }?;
    // SAFETY: `parent_class` points to the `State` boxed in `libraw_init` (a separate
    // allocation, so the two references do not alias).
    let st = unsafe { (d.parent_class as *mut State).as_mut() }?;
    Some((d, st))
}

/// After a panic (a bug) the handle may hold pointers into buffers that were being replaced:
/// drop the file entirely so no dangling pointer stays visible to the caller.
fn reset_after_panic(lr: *mut libraw_data_t) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: as in the caller; the panicking borrow has ended.
        if let Some((d, st)) = unsafe { handle(lr) } {
            processor::recycle(d, st);
            st.set_error(&"internal error (please report): the handle was reset");
        }
    }));
}

/// Run `f` on a handle: EINVAL for NULL, `LIBRAW_UNSPECIFIED_ERROR` on a panic.
fn with<F: FnOnce(&mut libraw_data_t, &mut State) -> c_int>(lr: *mut libraw_data_t, f: F) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: exported functions document `lr` as NULL or a live handle.
        match unsafe { handle(lr) } {
            Some((d, st)) => f(d, st),
            None => EINVAL,
        }
    }))
    .unwrap_or_else(|_| {
        reset_after_panic(lr);
        err::LIBRAW_UNSPECIFIED_ERROR
    })
}

/// Like `with` for functions without a result.
fn with_void<F: FnOnce(&mut libraw_data_t, &mut State)>(lr: *mut libraw_data_t, f: F) {
    with(lr, |d, st| {
        f(d, st);
        0
    });
}

/// Like `with` for value getters: `fallback` for NULL handles and panics.
fn get<T, F: FnOnce(&mut libraw_data_t, &mut State) -> T>(lr: *mut libraw_data_t, fallback: T, f: F) -> T {
    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: exported functions document `lr` as NULL or a live handle.
        unsafe { handle(lr) }.map(|(d, st)| f(d, st))
    })) {
        Ok(v) => v.unwrap_or(fallback),
        Err(_) => {
            reset_after_panic(lr);
            fallback
        }
    }
}

fn interned(s: String) -> *const c_char {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static NAMES: OnceLock<Mutex<HashMap<String, &'static CStr>>> = OnceLock::new();
    let mut m = match NAMES.get_or_init(Default::default).lock() {
        Ok(m) => m,
        Err(p) => p.into_inner(),
    };
    let c = m
        .entry(s)
        .or_insert_with_key(|k| Box::leak(CString::new(k.replace('\0', "")).unwrap_or_default().into_boxed_c_str()));
    c.as_ptr()
}

// ---------------------------------------------------------------------------------------
// Lifetime
// ---------------------------------------------------------------------------------------

/// Create a processor handle (LibRaw defaults in `params`). NULL if out of memory.
#[unsafe(no_mangle)]
pub extern "C" fn libraw_init(_flags: c_uint) -> *mut libraw_data_t {
    catch_unwind(|| {
        let layout = Layout::new::<libraw_data_t>();
        // SAFETY: the layout has a non-zero size; all-zero is a valid `libraw_data_t` (Pod).
        let p = unsafe { std::alloc::alloc_zeroed(layout) } as *mut libraw_data_t;
        if p.is_null() {
            return std::ptr::null_mut();
        }
        let st = Box::into_raw(Box::<State>::default());
        // SAFETY: `p` is a fresh, zeroed, exclusively owned allocation.
        let d = unsafe { &mut *p };
        d.parent_class = st.cast();
        processor::default_params(d);
        p
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Release a handle and everything it owns (NULL is ignored). Images returned by
/// `libraw_dcraw_make_mem_*` stay valid until `libraw_dcraw_clear_mem`.
///
/// # Safety
/// `lr` must be NULL or a live handle; it must not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_close(lr: *mut libraw_data_t) {
    if lr.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `lr` is a live handle from `libraw_init` (contract).
        let st = unsafe { (*lr).parent_class } as *mut State;
        if !st.is_null() {
            // SAFETY: allocated with `Box::into_raw` in `libraw_init`, freed exactly once.
            drop(unsafe { Box::from_raw(st) });
        }
        // SAFETY: allocated in `libraw_init` with this layout.
        unsafe { std::alloc::dealloc(lr.cast(), Layout::new::<libraw_data_t>()) };
    }));
}

/// Free the per-file data, keeping `params` for the next file.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_recycle(lr: *mut libraw_data_t) {
    with_void(lr, processor::recycle)
}

/// Close the input (file mapping or caller buffer). Decoded data stays available; calls
/// that need the input (`libraw_unpack`, `libraw_unpack_thumb`) then fail.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_recycle_datastream(lr: *mut libraw_data_t) {
    with_void(lr, |_, st| st.raw = None)
}

// ---------------------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------------------

fn open_with(
    d: &mut libraw_data_t,
    st: &mut State,
    open: impl FnOnce() -> flashraw::Result<FlashRaw<'static>>,
) -> c_int {
    processor::recycle(d, st);
    let raw = match open() {
        Ok(r) => r,
        Err(e) => {
            st.set_error(&e);
            return processor::open_error(&e);
        }
    };
    st.raw = Some(raw);
    let rc = processor::identify(d, st);
    if rc != err::LIBRAW_SUCCESS {
        let msg = std::mem::take(&mut st.last_error);
        processor::recycle(d, st);
        st.last_error = msg;
    }
    rc
}

/// Open a file (memory-mapped). The path is passed to the OS as bytes on Unix and as UTF-8
/// elsewhere.
///
/// # Safety
/// `lr` must be NULL or a live handle; `path` NULL or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_open_file(lr: *mut libraw_data_t, path: *const c_char) -> c_int {
    if path.is_null() {
        return EINVAL;
    }
    // SAFETY: `path` is a NUL-terminated string (contract).
    let bytes = unsafe { CStr::from_ptr(path) }.to_bytes().to_vec();
    with(lr, |d, st| {
        #[cfg(unix)]
        let p = {
            use std::os::unix::ffi::OsStrExt;
            std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&bytes))
        };
        #[cfg(not(unix))]
        let p = std::path::PathBuf::from(String::from_utf8_lossy(&bytes).into_owned());
        open_with(d, st, || FlashRaw::open_path(&p))
    })
}

/// Open a file from a wide-character path (Windows).
///
/// # Safety
/// `lr` must be NULL or a live handle; `path` NULL or a NUL-terminated wide string.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_open_wfile(lr: *mut libraw_data_t, path: *const u16) -> c_int {
    use std::os::windows::ffi::OsStringExt;
    if path.is_null() {
        return EINVAL;
    }
    let mut n = 0usize;
    // SAFETY: `path` is NUL-terminated (contract); read up to the terminator.
    while unsafe { *path.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: `n` readable code units precede the terminator.
    let p = std::path::PathBuf::from(std::ffi::OsString::from_wide(unsafe { std::slice::from_raw_parts(path, n) }));
    with(lr, |d, st| open_with(d, st, || FlashRaw::open_path(&p)))
}

/// Open a file held in memory. As with LibRaw the buffer is not copied: it must stay valid
/// and unmodified until `libraw_recycle`, `libraw_recycle_datastream`, the next open or
/// `libraw_close`.
///
/// # Safety
/// `lr` must be NULL or a live handle; `buffer` must point to `size` readable bytes with the
/// lifetime above.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_open_buffer(lr: *mut libraw_data_t, buffer: *const c_void, size: usize) -> c_int {
    if buffer.is_null() || size == 0 {
        return if buffer.is_null() { EINVAL } else { err::LIBRAW_FILE_UNSUPPORTED };
    }
    // SAFETY: the caller guarantees `size` readable bytes that outlive the input stream
    // (documented contract, identical to LibRaw's); the 'static borrow lives only in the
    // handle's `State::raw`, which is dropped before the buffer may be released.
    let bytes: &'static [u8] = unsafe { std::slice::from_raw_parts(buffer as *const u8, size) };
    with(lr, |d, st| open_with(d, st, || FlashRaw::open(bytes)))
}

/// Not implemented: raw Bayer buffers without a container.
///
/// # Safety
/// Any arguments; nothing is dereferenced.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn libraw_open_bayer(
    _lr: *mut libraw_data_t,
    _data: *mut u8,
    _datalen: c_uint,
    _raw_width: u16,
    _raw_height: u16,
    _left_margin: u16,
    _top_margin: u16,
    _right_margin: u16,
    _bottom_margin: u16,
    _procflags: u8,
    _bayer_pattern: u8,
    _unused_bits: c_uint,
    _otherflags: c_uint,
    _black_level: c_uint,
) -> c_int {
    err::LIBRAW_NOT_IMPLEMENTED
}

// ---------------------------------------------------------------------------------------
// Unpacking
// ---------------------------------------------------------------------------------------

/// Decode the sensor data into `imgdata.rawdata` (`raw_image` for one sample per pixel,
/// `color4_image` otherwise) and refresh the black/white levels.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_unpack(lr: *mut libraw_data_t) -> c_int {
    with(lr, processor::unpack)
}

fn unpack_thumb_index(d: &mut libraw_data_t, st: &mut State, index: Option<usize>) -> c_int {
    let Some(raw) = st.raw.as_ref() else {
        return if st.info.is_some() { err::LIBRAW_INPUT_CLOSED } else { err::LIBRAW_OUT_OF_ORDER_CALL };
    };
    let previews = raw.previews();
    let p = match index {
        Some(i) => match previews.get(i) {
            Some(p) => p,
            None => return err::LIBRAW_REQUEST_FOR_NONEXISTENT_THUMBNAIL,
        },
        None => match raw.preview() {
            Some(p) => p,
            None => return err::LIBRAW_NO_THUMBNAIL,
        },
    };
    let tformat = processor::thumb_format(p.encoding);
    if tformat == fmt::LIBRAW_THUMBNAIL_UNKNOWN {
        return err::LIBRAW_UNSUPPORTED_THUMBNAIL;
    }
    let bytes = match raw.preview_bytes(p) {
        Ok(b) => b.to_vec(),
        Err(e) => {
            st.set_error(&e);
            return err::LIBRAW_DATA_ERROR;
        }
    };
    let (w, h) = (p.width.min(65535) as u16, p.height.min(65535) as u16);
    if tformat == fmt::LIBRAW_THUMBNAIL_BITMAP && bytes.len() < w as usize * h as usize * 3 {
        return err::LIBRAW_DATA_ERROR;
    }
    st.thumb_buf = bytes;
    let t = &mut d.thumbnail;
    t.tformat = tformat;
    t.twidth = w;
    t.theight = h;
    t.tlength = st.thumb_buf.len().min(u32::MAX as usize) as u32;
    t.tcolors = 3;
    t.thumb = st.thumb_buf.as_mut_ptr().cast();
    d.progress_flags |= progress::THUMB_LOAD;
    err::LIBRAW_SUCCESS
}

/// Load the largest embedded preview into `imgdata.thumbnail` (JPEG data as stored; RGB
/// previews as 8-bit RGB).
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_unpack_thumb(lr: *mut libraw_data_t) -> c_int {
    with(lr, |d, st| unpack_thumb_index(d, st, None))
}

/// Load preview `i` of `imgdata.thumbs_list`.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_unpack_thumb_ex(lr: *mut libraw_data_t, i: c_int) -> c_int {
    with(lr, |d, st| match usize::try_from(i) {
        Ok(i) => unpack_thumb_index(d, st, Some(i)),
        Err(_) => err::LIBRAW_REQUEST_FOR_NONEXISTENT_THUMBNAIL,
    })
}

/// Copy the raw data into `imgdata.image` (4 samples per pixel).
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_raw2image(lr: *mut libraw_data_t) -> c_int {
    with(lr, processor::raw2image)
}

/// Release `imgdata.image`.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_free_image(lr: *mut libraw_data_t) {
    with_void(lr, |d, st| {
        st.image_buf = Vec::new();
        d.image = std::ptr::null_mut();
    })
}

/// Subtract the black level from `imgdata.image` (after `libraw_raw2image`).
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_subtract_black(lr: *mut libraw_data_t) {
    with_void(lr, processor::subtract_black)
}

// ---------------------------------------------------------------------------------------
// Processing and output
// ---------------------------------------------------------------------------------------

/// Develop the unpacked raw data into `imgdata.image` (see docs/libraw-compat.md for how
/// this differs from LibRaw's pipeline).
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_dcraw_process(lr: *mut libraw_data_t) -> c_int {
    with(lr, develop::dcraw_process)
}

/// Output geometry: what `libraw_adjust_sizes_info_only` computes without processing.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_adjust_sizes_info_only(lr: *mut libraw_data_t) -> c_int {
    with(lr, |d, st| {
        if st.info.is_none() {
            return err::LIBRAW_OUT_OF_ORDER_CALL;
        }
        d.sizes = d.rawdata.sizes;
        let cfa = st.cfa(&d.sizes);
        processor::set_output_size(d, cfa.filters != 0);
        let s = &mut d.sizes;
        if s.pixel_aspect < 0.999 && s.pixel_aspect > 0.0 {
            s.iheight = (s.iheight as f64 / s.pixel_aspect + 0.5).min(65535.0) as u16;
        } else if s.pixel_aspect > 1.001 {
            s.iwidth = (s.iwidth as f64 * s.pixel_aspect + 0.5).min(65535.0) as u16;
        }
        if d.params.user_flip >= 0 {
            s.flip = d.params.user_flip;
        }
        if s.flip & 4 != 0 {
            std::mem::swap(&mut s.width, &mut s.height);
            std::mem::swap(&mut s.iwidth, &mut s.iheight);
        }
        err::LIBRAW_SUCCESS
    })
}

#[repr(C)]
struct ImageHeader {
    type_: c_int,
    height: u16,
    width: u16,
    colors: u16,
    bits: u16,
    data_size: c_uint,
}

/// A `libraw_processed_image_t` allocated with `malloc` (released by
/// `libraw_dcraw_clear_mem` or `free`).
fn processed_image(
    type_: c_int,
    w: usize,
    h: usize,
    colors: usize,
    bits: usize,
    data: &[u8],
) -> *mut libraw_processed_image_t {
    let header = std::mem::offset_of!(libraw_processed_image_t, data);
    let total = std::mem::size_of::<libraw_processed_image_t>() + data.len();
    // SAFETY: plain C allocation of `total` bytes, released with `free`.
    let p = unsafe { libc::malloc(total) } as *mut u8;
    if p.is_null() {
        return std::ptr::null_mut();
    }
    let hdr = ImageHeader {
        type_,
        height: h.min(65535) as u16,
        width: w.min(65535) as u16,
        colors: colors as u16,
        bits: bits as u16,
        data_size: data.len().min(u32::MAX as usize) as c_uint,
    };
    // SAFETY: `p` has `total >= header + data.len()` bytes; `ImageHeader` has the layout of
    // the leading fields of `libraw_processed_image_t` (checked by the layout test), and
    // malloc memory is suitably aligned.
    unsafe {
        std::ptr::write(p as *mut ImageHeader, hdr);
        std::ptr::copy_nonoverlapping(data.as_ptr(), p.add(header), data.len());
    }
    p.cast()
}

/// Render the processed image into a newly allocated RGB (or grey) bitmap.
///
/// # Safety
/// `lr` must be NULL or a live handle; `errc` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_dcraw_make_mem_image(
    lr: *mut libraw_data_t,
    errc: *mut c_int,
) -> *mut libraw_processed_image_t {
    let mut code = EINVAL;
    let img = get(lr, std::ptr::null_mut(), |d, st| match develop::render(d, st) {
        Ok((w, h, colors, bits, data)) => {
            let p = processed_image(fmt::LIBRAW_IMAGE_BITMAP, w, h, colors, bits, &data);
            code = if p.is_null() { err::LIBRAW_UNSUFFICIENT_MEMORY } else { err::LIBRAW_SUCCESS };
            p
        }
        Err(c) => {
            code = c;
            std::ptr::null_mut()
        }
    });
    if !errc.is_null() {
        // SAFETY: `errc` is writable (contract).
        unsafe { errc.write(code) };
    }
    img
}

/// Copy the loaded thumbnail into a newly allocated `libraw_processed_image_t`.
///
/// # Safety
/// `lr` must be NULL or a live handle; `errc` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_dcraw_make_mem_thumb(
    lr: *mut libraw_data_t,
    errc: *mut c_int,
) -> *mut libraw_processed_image_t {
    let mut code = EINVAL;
    let img = get(lr, std::ptr::null_mut(), |d, st| {
        if st.thumb_buf.is_empty() {
            code = err::LIBRAW_OUT_OF_ORDER_CALL;
            return std::ptr::null_mut();
        }
        let t = &d.thumbnail;
        let (ty, colors) = match t.tformat {
            fmt::LIBRAW_THUMBNAIL_JPEG => (fmt::LIBRAW_IMAGE_JPEG, 3),
            fmt::LIBRAW_THUMBNAIL_BITMAP => (fmt::LIBRAW_IMAGE_BITMAP, 3),
            fmt::LIBRAW_THUMBNAIL_JPEGXL => (fmt::LIBRAW_IMAGE_JPEGXL, 3),
            fmt::LIBRAW_THUMBNAIL_H265 => (fmt::LIBRAW_IMAGE_H265, 3),
            _ => {
                code = err::LIBRAW_UNSUPPORTED_THUMBNAIL;
                return std::ptr::null_mut();
            }
        };
        let p = processed_image(ty, t.twidth as usize, t.theight as usize, colors, 8, &st.thumb_buf);
        code = if p.is_null() { err::LIBRAW_UNSUFFICIENT_MEMORY } else { err::LIBRAW_SUCCESS };
        p
    });
    if !errc.is_null() {
        // SAFETY: `errc` is writable (contract).
        unsafe { errc.write(code) };
    }
    img
}

/// Release an image from `libraw_dcraw_make_mem_image` / `_thumb` (NULL is ignored).
///
/// # Safety
/// `p` must be NULL or an image returned by this library and not yet released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_dcraw_clear_mem(p: *mut libraw_processed_image_t) {
    if !p.is_null() {
        // SAFETY: allocated with `malloc` in `processed_image`.
        unsafe { libc::free(p.cast()) };
    }
}

fn write_file(path: *const c_char, data: &[u8]) -> c_int {
    if path.is_null() {
        return EINVAL;
    }
    // SAFETY: `path` is a NUL-terminated string (contract of the exported callers).
    let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
    let r = if bytes == b"-" {
        use std::io::Write;
        std::io::stdout().write_all(data)
    } else {
        #[cfg(unix)]
        let p = {
            use std::os::unix::ffi::OsStrExt;
            std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
        };
        #[cfg(not(unix))]
        let p = std::path::PathBuf::from(String::from_utf8_lossy(bytes).into_owned());
        std::fs::write(p, data)
    };
    match r {
        Ok(()) => err::LIBRAW_SUCCESS,
        Err(e) => e.raw_os_error().unwrap_or(err::LIBRAW_IO_ERROR),
    }
}

/// Write the processed image as PPM/PGM (`params.output_tiff` = 0). TIFF output is not
/// implemented.
///
/// # Safety
/// `lr` must be NULL or a live handle; `filename` NULL or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_dcraw_ppm_tiff_writer(lr: *mut libraw_data_t, filename: *const c_char) -> c_int {
    with(lr, |d, st| {
        if d.params.output_tiff != 0 {
            return err::LIBRAW_NOT_IMPLEMENTED;
        }
        match develop::render(d, st) {
            Ok((w, h, colors, bits, data)) => write_file(filename, &develop::ppm(w, h, colors, bits, &data)),
            Err(c) => c,
        }
    })
}

/// Write the loaded thumbnail: JPEG (and other compressed previews) as stored, RGB bitmaps
/// as PPM.
///
/// # Safety
/// `lr` must be NULL or a live handle; `fname` NULL or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_dcraw_thumb_writer(lr: *mut libraw_data_t, fname: *const c_char) -> c_int {
    with(lr, |d, st| {
        if st.thumb_buf.is_empty() {
            return err::LIBRAW_OUT_OF_ORDER_CALL;
        }
        let t = &d.thumbnail;
        if t.tformat == fmt::LIBRAW_THUMBNAIL_BITMAP {
            let n = t.twidth as usize * t.theight as usize * 3;
            write_file(fname, &develop::ppm(t.twidth as usize, t.theight as usize, 3, 8, &st.thumb_buf[..n]))
        } else {
            write_file(fname, &st.thumb_buf)
        }
    })
}

/// FlashRaw extension: the geometry of the bitmap `libraw_dcraw_make_mem_image` returns.
///
/// # Safety
/// `lr` must be NULL or a live handle; the out-pointers NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_flashraw_mem_image_format(
    lr: *mut libraw_data_t,
    width: *mut c_int,
    height: *mut c_int,
    colors: *mut c_int,
    bps: *mut c_int,
) {
    let (w, h, c, b) = get(lr, (0, 0, 0, 0), |d, _| develop::mem_image_format(d));
    for (p, v) in [(width, w), (height, h), (colors, c), (bps, b)] {
        if !p.is_null() {
            // SAFETY: non-null out-pointers are writable (contract).
            unsafe { p.write(v as c_int) };
        }
    }
}

// ---------------------------------------------------------------------------------------
// Information
// ---------------------------------------------------------------------------------------

/// Description of a LibRaw error code (static string).
#[unsafe(no_mangle)]
pub extern "C" fn libraw_strerror(code: c_int) -> *const c_char {
    let s: &'static CStr = match code {
        err::LIBRAW_SUCCESS => c"No error",
        err::LIBRAW_UNSPECIFIED_ERROR => c"Unspecified error",
        err::LIBRAW_FILE_UNSUPPORTED => c"Unsupported file format or not a RAW file",
        err::LIBRAW_REQUEST_FOR_NONEXISTENT_IMAGE => c"Requested image does not exist",
        err::LIBRAW_OUT_OF_ORDER_CALL => c"Functions called in the wrong order",
        err::LIBRAW_NO_THUMBNAIL => c"The file has no thumbnail",
        err::LIBRAW_UNSUPPORTED_THUMBNAIL => c"Unsupported thumbnail format",
        err::LIBRAW_INPUT_CLOSED => c"Input closed",
        err::LIBRAW_NOT_IMPLEMENTED => c"Not implemented in this library",
        err::LIBRAW_REQUEST_FOR_NONEXISTENT_THUMBNAIL => c"Requested thumbnail does not exist",
        err::LIBRAW_UNSUFFICIENT_MEMORY => c"Out of memory",
        err::LIBRAW_DATA_ERROR => c"Corrupt or truncated data",
        err::LIBRAW_IO_ERROR => c"Input/output error",
        err::LIBRAW_CANCELLED_BY_CALLBACK => c"Cancelled by a callback",
        err::LIBRAW_BAD_CROP => c"Bad crop box",
        err::LIBRAW_TOO_BIG => c"Image too big",
        err::LIBRAW_MEMPOOL_OVERFLOW => c"Memory pool overflow",
        _ => c"Unknown error code",
    };
    s.as_ptr()
}

/// Name of a processing stage (static string).
#[unsafe(no_mangle)]
pub extern "C" fn libraw_strprogress(p: c_int) -> *const c_char {
    let s: &'static CStr = match p {
        0 => c"Starting",
        1 => c"Opening file",
        2 => c"Reading metadata",
        4 => c"Adjusting size",
        8 => c"Reading RAW data",
        16 => c"Preparing image",
        32 => c"Removing zero values",
        64 => c"Fixing bad pixels",
        128 => c"Subtracting dark frame",
        256 => c"Interpolating layered data",
        512 => c"Scaling colours",
        1024 => c"Pre-interpolating",
        2048 => c"Interpolating",
        4096 => c"Mixing green channels",
        8192 => c"Median filter",
        16384 => c"Highlight recovery",
        32768 => c"Rotating",
        65536 => c"Flipping",
        131072 => c"Applying colour profile",
        262144 => c"Converting to RGB",
        524288 => c"Stretching",
        268435456 => c"Loading thumbnail",
        _ => c"Unknown stage",
    };
    s.as_ptr()
}

/// LibRaw API level implemented, e.g. "0.22.0-FlashRaw" (static string).
#[unsafe(no_mangle)]
pub extern "C" fn libraw_version() -> *const c_char {
    c"0.22.0-FlashRaw".as_ptr()
}

/// LibRaw API level as `major << 16 | minor << 8 | patch`.
#[unsafe(no_mangle)]
pub extern "C" fn libraw_versionNumber() -> c_int {
    22 << 8
}

/// FlashRaw extension: version of the FlashRaw library underneath.
#[unsafe(no_mangle)]
pub extern "C" fn libraw_flashraw_version() -> *const c_char {
    const V: &CStr = match CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
        Ok(v) => v,
        Err(_) => c"unknown",
    };
    V.as_ptr()
}

/// FlashRaw extension: message of the last FlashRaw error on this handle (empty if none).
/// Valid until the next call on the handle.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_flashraw_last_error(lr: *mut libraw_data_t) -> *const c_char {
    get(lr, c"".as_ptr(), |_, st| st.last_error.as_ptr())
}

struct CameraList([*const c_char; 1]);
// SAFETY: the list holds only a NULL pointer and is never mutated.
unsafe impl Sync for CameraList {}
static CAMERA_LIST: CameraList = CameraList([std::ptr::null()]);

/// Supported-camera list. FlashRaw identifies cameras from file metadata, so the list is
/// empty (a single NULL terminator).
#[unsafe(no_mangle)]
pub extern "C" fn libraw_cameraList() -> *const *const c_char {
    CAMERA_LIST.0.as_ptr()
}

/// Number of entries in `libraw_cameraList()`: 0.
#[unsafe(no_mangle)]
pub extern "C" fn libraw_cameraCount() -> c_int {
    0
}

/// Capability flags: none of the optional LibRaw build options apply.
#[unsafe(no_mangle)]
pub extern "C" fn libraw_capabilities() -> c_uint {
    0
}

/// Decoder description: `"flashraw_<format>"` and flags.
///
/// # Safety
/// `lr` must be NULL or a live handle; `out` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_get_decoder_info(lr: *mut libraw_data_t, out: *mut libraw_decoder_info_t) -> c_int {
    if out.is_null() {
        return EINVAL;
    }
    with(lr, |_, st| {
        if st.info.is_none() {
            return err::LIBRAW_OUT_OF_ORDER_CALL;
        }
        let flags = if st.cpp == 1 { 1 << 12 } else { 1 << 11 };
        let info = libraw_decoder_info_t {
            decoder_name: interned(format!("flashraw_{}", st.format.to_ascii_lowercase())),
            decoder_flags: flags,
        };
        // SAFETY: `out` is writable (contract).
        unsafe { out.write(info) };
        err::LIBRAW_SUCCESS
    })
}

/// Name of the unpacking function (`"flashraw_<format>"`).
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_unpack_function_name(lr: *mut libraw_data_t) -> *const c_char {
    get(lr, std::ptr::null(), |_, st| {
        if st.info.is_none() {
            c"Function not set".as_ptr()
        } else {
            interned(format!("flashraw_{}", st.format.to_ascii_lowercase()))
        }
    })
}

/// Colour index (into `idata.cdesc`) of visible-area pixel (row, col); 6 without a CFA.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_COLOR(lr: *mut libraw_data_t, row: c_int, col: c_int) -> c_int {
    with(lr, |d, st| {
        st.cfa(&d.sizes).color(row as i64, col as i64, d.sizes.left_margin as u32, d.sizes.top_margin as u32)
    })
}

/// Use `sizes.raw_inset_crops[]` as the visible area: bit 1 of `mask` tries crop [1],
/// bit 0 crop [0]; a crop smaller than `maxcrop` of the current size is ignored. Returns
/// the index used plus one, or 0 when nothing changed.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_adjust_to_raw_inset_crop(lr: *mut libraw_data_t, mask: c_uint, maxcrop: f32) -> c_int {
    with(lr, |d, st| {
        if st.info.is_none() {
            return err::LIBRAW_OUT_OF_ORDER_CALL;
        }
        let s = d.sizes;
        for i in [1usize, 0] {
            if mask & (1 << i) == 0 {
                continue;
            }
            let c = s.raw_inset_crops[i];
            let fits = c.cwidth > 0
                && c.cheight > 0
                && c.cleft as u32 + c.cwidth as u32 <= s.raw_width as u32
                && c.ctop as u32 + c.cheight as u32 <= s.raw_height as u32
                && c.cwidth as f32 >= s.width as f32 * maxcrop
                && c.cheight as f32 >= s.height as f32 * maxcrop;
            if !fits {
                continue;
            }
            for sizes in [&mut d.sizes, &mut d.rawdata.sizes] {
                sizes.left_margin = c.cleft;
                sizes.top_margin = c.ctop;
                sizes.width = c.cwidth;
                sizes.height = c.cheight;
            }
            // Colour indices and black patterns are anchored at the visible area.
            let cfa = st.cfa(&d.sizes);
            d.idata.filters = cfa.filters;
            let (xt, xa) = cfa.xtrans(c.cleft as u32, c.ctop as u32);
            d.idata.xtrans = xt.map(|r| r.map(|v| v as c_char));
            d.idata.xtrans_abs = xa.map(|r| r.map(|v| v as c_char));
            d.rawdata.iparams = d.idata;
            processor::set_output_size(d, cfa.filters != 0);
            d.rawdata.sizes = d.sizes;
            return i as c_int + 1;
        }
        0
    })
}

// ---------------------------------------------------------------------------------------
// Callbacks (accepted, never invoked)
// ---------------------------------------------------------------------------------------

/// No-op data-error handler.
///
/// # Safety
/// Any arguments; nothing is dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn default_data_callback(_data: *mut c_void, _file: *const c_char, _offset: i64) {}

/// Accepted for compatibility; never invoked.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_set_exifparser_handler(
    lr: *mut libraw_data_t,
    _cb: exif_parser_callback,
    _data: *mut c_void,
) {
    with_void(lr, |_, _| {})
}

/// Accepted for compatibility; never invoked.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_set_makernotes_handler(
    lr: *mut libraw_data_t,
    _cb: exif_parser_callback,
    _data: *mut c_void,
) {
    with_void(lr, |_, _| {})
}

/// Accepted for compatibility; never invoked (data errors are recovered and reported
/// through `libraw_flashraw_last_error`).
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_set_dataerror_handler(lr: *mut libraw_data_t, _cb: data_callback, _data: *mut c_void) {
    with_void(lr, |_, _| {})
}

/// Accepted for compatibility; never invoked.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_set_progress_handler(
    lr: *mut libraw_data_t,
    _cb: progress_callback,
    _data: *mut c_void,
) {
    with_void(lr, |_, _| {})
}

// ---------------------------------------------------------------------------------------
// Parameter setters and getters
// ---------------------------------------------------------------------------------------

macro_rules! setter {
    ($(#[$doc:meta])* $name:ident, $t:ty, |$p:ident, $v:ident| $body:expr) => {
        $(#[$doc])*
        ///
        /// # Safety
        /// `lr` must be NULL or a live handle.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(lr: *mut libraw_data_t, $v: $t) {
            with_void(lr, |d, _| {
                let $p = &mut d.params;
                $body;
            })
        }
    };
}

setter!(
    /// `params.user_qual` (the interpolation quality is always bilinear here).
    libraw_set_demosaic, c_int, |p, value| p.user_qual = value
);
setter!(
    /// `params.output_color`.
    libraw_set_output_color, c_int, |p, value| p.output_color = value
);
setter!(
    /// `params.adjust_maximum_thr`.
    libraw_set_adjust_maximum_thr, f32, |p, value| p.adjust_maximum_thr = value
);
setter!(
    /// `params.output_bps`.
    libraw_set_output_bps, c_int, |p, value| p.output_bps = value
);
setter!(
    /// `params.no_auto_bright`.
    libraw_set_no_auto_bright, c_int, |p, value| p.no_auto_bright = value
);
setter!(
    /// `params.bright`.
    libraw_set_bright, f32, |p, value| p.bright = value
);
setter!(
    /// `params.highlight` (highlights are always clipped here).
    libraw_set_highlight, c_int, |p, value| p.highlight = value
);
setter!(
    /// `params.fbdd_noiserd` (no effect here).
    libraw_set_fbdd_noiserd, c_int, |p, value| p.fbdd_noiserd = value
);
setter!(
    /// `params.output_tiff`.
    libraw_set_output_tif, c_int, |p, value| p.output_tiff = value
);

/// `params.user_mul[index]` (0-3).
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_set_user_mul(lr: *mut libraw_data_t, index: c_int, val: f32) {
    with_void(lr, |d, _| {
        if let Some(m) = usize::try_from(index).ok().and_then(|i| d.params.user_mul.get_mut(i)) {
            *m = val;
        }
    })
}

/// `params.gamm[index]` (0: power, 1: toe slope); the derived values are recomputed.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_set_gamma(lr: *mut libraw_data_t, index: c_int, value: f32) {
    with_void(lr, |d, _| {
        if let Some(g) = usize::try_from(index).ok().filter(|&i| i < 2).map(|i| &mut d.params.gamm[i]) {
            *g = value as f64;
            let derived = crate::color::gamma_params(d.params.gamm[0], d.params.gamm[1]);
            d.params.gamm[2..6].copy_from_slice(&derived);
        }
    })
}

macro_rules! int_getter {
    ($(#[$doc:meta])* $name:ident, |$d:ident| $e:expr) => {
        $(#[$doc])*
        ///
        /// # Safety
        /// `lr` must be NULL or a live handle.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(lr: *mut libraw_data_t) -> c_int {
            get(lr, EINVAL, |$d, _| $e as c_int)
        }
    };
}

int_getter!(
    /// `sizes.raw_height`.
    libraw_get_raw_height, |d| d.sizes.raw_height
);
int_getter!(
    /// `sizes.raw_width`.
    libraw_get_raw_width, |d| d.sizes.raw_width
);
int_getter!(
    /// `sizes.iheight`.
    libraw_get_iheight, |d| d.sizes.iheight
);
int_getter!(
    /// `sizes.iwidth`.
    libraw_get_iwidth, |d| d.sizes.iwidth
);
int_getter!(
    /// `color.maximum`.
    libraw_get_color_maximum, |d| d.color.maximum
);

/// `color.cam_mul[index]`; -999 for a NULL handle or bad index.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_get_cam_mul(lr: *mut libraw_data_t, index: c_int) -> f32 {
    get(lr, -999.0, |d, _| usize::try_from(index).ok().and_then(|i| d.color.cam_mul.get(i).copied()).unwrap_or(-999.0))
}

/// `color.pre_mul[index]`; -999 for a NULL handle or bad index.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_get_pre_mul(lr: *mut libraw_data_t, index: c_int) -> f32 {
    get(lr, -999.0, |d, _| usize::try_from(index).ok().and_then(|i| d.color.pre_mul.get(i).copied()).unwrap_or(-999.0))
}

/// `color.rgb_cam[index1][index2]`; -999 for a NULL handle or bad index.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_get_rgb_cam(lr: *mut libraw_data_t, index1: c_int, index2: c_int) -> f32 {
    get(lr, -999.0, |d, _| {
        let (Ok(i), Ok(j)) = (usize::try_from(index1), usize::try_from(index2)) else { return -999.0 };
        d.color.rgb_cam.get(i).and_then(|r| r.get(j)).copied().unwrap_or(-999.0)
    })
}

/// `&imgdata.idata`.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_get_iparams(lr: *mut libraw_data_t) -> *mut libraw_iparams_t {
    get(lr, std::ptr::null_mut(), |d, _| &mut d.idata as *mut _)
}

/// `&imgdata.lens`.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_get_lensinfo(lr: *mut libraw_data_t) -> *mut libraw_lensinfo_t {
    get(lr, std::ptr::null_mut(), |d, _| &mut d.lens as *mut _)
}

/// `&imgdata.other`.
///
/// # Safety
/// `lr` must be NULL or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn libraw_get_imgother(lr: *mut libraw_data_t) -> *mut libraw_imgother_t {
    get(lr, std::ptr::null_mut(), |d, _| &mut d.other as *mut _)
}
