//! # FlashRaw C API
//!
//! A stable C ABI over the [`flashraw`] crate, plus a header-only C++17 wrapper
//! (`include/flashraw.hpp`). The C header `include/flashraw.h` is generated from this crate
//! with cbindgen and checked in; see `docs/c-api.md` for usage, building, linking, memory
//! ownership, threading and the ABI stability policy.
//!
//! Design rules for every exported function:
//!
//! - `extern "C"`, unmangled, `flashraw_` prefix; only C types cross the boundary (opaque
//!   handles, fixed-width integers, `float`/`double`, `bool`, `#[repr(C)]` structs of those).
//! - Enums are passed as `int32_t` and validated on the way in.
//! - Every body runs inside `catch_unwind`: a panic becomes `FLASHRAW_ERROR_INTERNAL`, never
//!   an unwind into C. NULL pointers are reported as `FLASHRAW_ERROR_INVALID_ARGUMENT`.
//! - Errors set a thread-local last error (`flashraw_last_error_message()`).
//! - Structs that may grow carry their size in their first field (`size`).

#![allow(non_camel_case_types)]
#![deny(unsafe_op_in_unsafe_fn)]

mod calib;
mod error;
mod file;
mod frame;
mod json;
mod types;

pub use error::*;
pub use file::*;
pub use frame::*;
pub use types::*;

use std::ffi::{CStr, CString, c_char};

/// Version of the C ABI described by `flashraw.h`. Incremented only for incompatible
/// changes (which the stability policy rules out within a major version); additions do not
/// change it.
pub const FLASHRAW_ABI_VERSION: u32 = 1;

/// Library version string, e.g. `"0.1.0"` (static).
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_version() -> *const c_char {
    const V: &CStr = match CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
        Ok(v) => v,
        Err(_) => c"unknown",
    };
    V.as_ptr()
}

/// Library version as `major << 16 | minor << 8 | patch`.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_version_number() -> u32 {
    let p = |s: &str| s.parse::<u32>().unwrap_or(0);
    (p(env!("CARGO_PKG_VERSION_MAJOR")) << 16)
        | (p(env!("CARGO_PKG_VERSION_MINOR")) << 8)
        | p(env!("CARGO_PKG_VERSION_PATCH"))
}

/// ABI version implemented by the loaded library (`FLASHRAW_ABI_VERSION` of the header it
/// was built with). Compare with the header's `FLASHRAW_ABI_VERSION` at startup.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_abi_version() -> u32 {
    FLASHRAW_ABI_VERSION
}

/// Cheap probe: true when the bytes look like a RAW file FlashRaw can open (magic numbers
/// and container structure only; nothing is decoded). A few KB from the start of the file
/// are usually enough, but some formats are recognised only from the full file.
///
/// # Safety
/// `data` must point to `len` readable bytes (or be NULL with `len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_supports(data: *const u8, len: usize) -> bool {
    // SAFETY: forwarded caller contract.
    !unsafe { flashraw_detect_format(data, len) }.is_null()
}

/// Like `flashraw_supports()`, returning the detected format name ("DNG", "NEF", ...) as a
/// static string, or NULL when unrecognised.
///
/// # Safety
/// `data` must point to `len` readable bytes (or be NULL with `len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_detect_format(data: *const u8, len: usize) -> *const c_char {
    guard_value(std::ptr::null(), || {
        if data.is_null() {
            return if len == 0 { Ok(std::ptr::null()) } else { Err(FfiError::null("data")) };
        }
        // SAFETY: the caller guarantees `len` readable bytes at `data`.
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        Ok(flashraw::supports(bytes).map_or(std::ptr::null(), |f| static_cstr(f.name())))
    })
}

/// A NUL-terminated copy of a static string, interned for the life of the process (the set
/// of names is small and fixed).
fn static_cstr(s: &'static str) -> *const c_char {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static NAMES: OnceLock<Mutex<HashMap<&'static str, &'static CStr>>> = OnceLock::new();
    let mut m = match NAMES.get_or_init(Default::default).lock() {
        Ok(m) => m,
        Err(p) => p.into_inner(),
    };
    m.entry(s).or_insert_with(|| Box::leak(cstring_lossy(s).into_boxed_c_str())).as_ptr()
}

/// True when the GPU backend is compiled in and an adapter is available.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_gpu_available() -> bool {
    error::guard_silent(false, flashraw::gpu_available)
}

/// Release a string returned by a `*_json()` function (NULL is ignored).
///
/// # Safety
/// `s` must be NULL or a string returned by this library that has not been freed yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_string_free(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: `s` came from `CString::into_raw` in this library and is freed once.
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Name of a `flashraw_stage` value (e.g. "Entropy decode"); static, "Unknown" for unknown
/// values.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_stage_name(stage: i32) -> *const c_char {
    static NAMES: [&CStr; 11] = [
        c"Container parse",
        c"Metadata",
        c"Entropy decode",
        c"Pixel unpack",
        c"Pixel reconstruction",
        c"Calibration",
        c"Post-process",
        c"GPU upload",
        c"GPU compute",
        c"GPU download",
        c"Preview",
    ];
    usize::try_from(stage).ok().and_then(|i| NAMES.get(i)).map_or(c"Unknown".as_ptr(), |s| s.as_ptr())
}

/// Canonical name of a `flashraw_output_format` value ("raw-u16", "rgba8", ...); static,
/// NULL for unknown values.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_output_format_name(output: i32) -> *const c_char {
    match types::output_from(output) {
        Ok(o) => static_cstr(o.name()),
        Err(_) => std::ptr::null(),
    }
}

/// Parse an output format name as accepted by the CLI ("raw-u16", "raw-f32", "normalized",
/// "linear-rgb", "rgba8", case-insensitive) into `*out`.
///
/// # Safety
/// `name` must be NULL or a NUL-terminated string; `out` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_output_format_parse(name: *const c_char, out: *mut i32) -> flashraw_status {
    error::guard(|| {
        if name.is_null() || out.is_null() {
            return Err(FfiError::null("name/out"));
        }
        // SAFETY: `name` is a NUL-terminated string per the contract.
        let s = unsafe { CStr::from_ptr(name) }.to_string_lossy();
        let o = flashraw::OutputFormat::parse(&s)
            .filter(|o| !o.is_gpu_resident())
            .ok_or_else(|| FfiError::invalid(format!("unknown output format {s:?}")))?;
        // SAFETY: `out` is non-null and writable.
        unsafe { out.write(types::output_to(o)) };
        Ok(())
    })
}
