//! Status codes, the thread-local last error, and the panic/argument guard every exported
//! function runs its body in.

use std::cell::RefCell;
use std::ffi::{CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Result of a fallible call. `FLASHRAW_OK` (0) is success; every other value is an error
/// whose details are available from `flashraw_last_error_message()` on the same thread.
///
/// The error codes `1..=11` correspond one-to-one to the error kinds of the Rust library
/// (`flashraw_error_kind_name()` returns the kind name, e.g. `"CorruptFile"`). Values are
/// stable: new kinds get new numbers, existing numbers never change meaning.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum flashraw_status {
    /// Success.
    FLASHRAW_OK = 0,
    /// The bytes do not look like any supported RAW format.
    FLASHRAW_ERROR_UNSUPPORTED_FORMAT = 1,
    /// The container is understood but its compression scheme is not supported.
    FLASHRAW_ERROR_UNSUPPORTED_COMPRESSION = 2,
    /// The file is structurally invalid (truncated, bad offsets, corrupt entropy data...).
    FLASHRAW_ERROR_CORRUPT_FILE = 3,
    /// Metadata required for decoding is missing or nonsensical.
    FLASHRAW_ERROR_INVALID_METADATA = 4,
    /// The format is supported but this camera or mode is not.
    FLASHRAW_ERROR_CAMERA_UNSUPPORTED = 5,
    /// The GPU backend was requested but is not compiled in or has no adapter.
    FLASHRAW_ERROR_GPU_UNAVAILABLE = 6,
    /// An allocation failed or would exceed the configured limits.
    FLASHRAW_ERROR_OUT_OF_MEMORY = 7,
    /// Dimensions or other values exceed the configured safety limits.
    FLASHRAW_ERROR_LIMIT_EXCEEDED = 8,
    /// The requested item does not exist for this file (no such preview, ...).
    FLASHRAW_ERROR_NOT_AVAILABLE = 9,
    /// Invalid argument: NULL pointer, unknown enum value, bad region, bad struct size...
    FLASHRAW_ERROR_INVALID_ARGUMENT = 10,
    /// I/O failure while reading the input.
    FLASHRAW_ERROR_IO = 11,
    /// Internal error: a bug in FlashRaw (a caught panic). The handle involved is still
    /// safe to free.
    FLASHRAW_ERROR_INTERNAL = 12,
    /// An error kind added to the library after this ABI version; see the message and
    /// `flashraw_last_error_kind()`.
    FLASHRAW_ERROR_OTHER = 13,
}

use flashraw_status::*;

impl flashraw_status {
    /// Kind name (as returned by the Rust library's `Error::kind()`).
    pub fn kind_name(self) -> &'static str {
        match self {
            FLASHRAW_OK => "Ok",
            FLASHRAW_ERROR_UNSUPPORTED_FORMAT => "UnsupportedFormat",
            FLASHRAW_ERROR_UNSUPPORTED_COMPRESSION => "UnsupportedCompression",
            FLASHRAW_ERROR_CORRUPT_FILE => "CorruptFile",
            FLASHRAW_ERROR_INVALID_METADATA => "InvalidMetadata",
            FLASHRAW_ERROR_CAMERA_UNSUPPORTED => "CameraUnsupported",
            FLASHRAW_ERROR_GPU_UNAVAILABLE => "GpuUnavailable",
            FLASHRAW_ERROR_OUT_OF_MEMORY => "OutOfMemory",
            FLASHRAW_ERROR_LIMIT_EXCEEDED => "LimitExceeded",
            FLASHRAW_ERROR_NOT_AVAILABLE => "NotAvailable",
            FLASHRAW_ERROR_INVALID_ARGUMENT => "InvalidArgument",
            FLASHRAW_ERROR_IO => "Io",
            FLASHRAW_ERROR_INTERNAL => "Internal",
            FLASHRAW_ERROR_OTHER => "Other",
        }
    }

    fn from_kind(kind: &str) -> Self {
        match kind {
            "UnsupportedFormat" => FLASHRAW_ERROR_UNSUPPORTED_FORMAT,
            "UnsupportedCompression" => FLASHRAW_ERROR_UNSUPPORTED_COMPRESSION,
            "CorruptFile" => FLASHRAW_ERROR_CORRUPT_FILE,
            "InvalidMetadata" => FLASHRAW_ERROR_INVALID_METADATA,
            "CameraUnsupported" => FLASHRAW_ERROR_CAMERA_UNSUPPORTED,
            "GpuUnavailable" => FLASHRAW_ERROR_GPU_UNAVAILABLE,
            "OutOfMemory" => FLASHRAW_ERROR_OUT_OF_MEMORY,
            "LimitExceeded" => FLASHRAW_ERROR_LIMIT_EXCEEDED,
            "NotAvailable" => FLASHRAW_ERROR_NOT_AVAILABLE,
            "InvalidArgument" => FLASHRAW_ERROR_INVALID_ARGUMENT,
            "Io" => FLASHRAW_ERROR_IO,
            _ => FLASHRAW_ERROR_OTHER,
        }
    }

    /// The status for an `i32` received from C, if it is a known value.
    pub fn from_i32(v: i32) -> Option<Self> {
        Some(match v {
            0 => FLASHRAW_OK,
            1 => FLASHRAW_ERROR_UNSUPPORTED_FORMAT,
            2 => FLASHRAW_ERROR_UNSUPPORTED_COMPRESSION,
            3 => FLASHRAW_ERROR_CORRUPT_FILE,
            4 => FLASHRAW_ERROR_INVALID_METADATA,
            5 => FLASHRAW_ERROR_CAMERA_UNSUPPORTED,
            6 => FLASHRAW_ERROR_GPU_UNAVAILABLE,
            7 => FLASHRAW_ERROR_OUT_OF_MEMORY,
            8 => FLASHRAW_ERROR_LIMIT_EXCEEDED,
            9 => FLASHRAW_ERROR_NOT_AVAILABLE,
            10 => FLASHRAW_ERROR_INVALID_ARGUMENT,
            11 => FLASHRAW_ERROR_IO,
            12 => FLASHRAW_ERROR_INTERNAL,
            13 => FLASHRAW_ERROR_OTHER,
            _ => return None,
        })
    }
}

/// An error on its way to C.
#[derive(Debug)]
pub(crate) struct FfiError {
    pub status: flashraw_status,
    pub kind: &'static str,
    pub message: String,
    /// Structured detail (JSON), e.g. which strip is truncated.
    pub detail: Option<String>,
}

impl FfiError {
    pub fn invalid(message: impl Into<String>) -> Self {
        FfiError {
            status: FLASHRAW_ERROR_INVALID_ARGUMENT,
            kind: "InvalidArgument",
            message: format!("invalid argument: {}", message.into()),
            detail: None,
        }
    }

    pub fn null(what: &str) -> Self {
        Self::invalid(format!("`{what}` is NULL"))
    }

    pub fn not_available(message: impl Into<String>) -> Self {
        FfiError {
            status: FLASHRAW_ERROR_NOT_AVAILABLE,
            kind: "NotAvailable",
            message: format!("not available: {}", message.into()),
            detail: None,
        }
    }
}

impl From<flashraw::Error> for FfiError {
    fn from(e: flashraw::Error) -> Self {
        let kind = e.kind();
        let detail = e.corrupt_reason().map(|r| crate::json::corrupt_reason(r).to_string());
        FfiError { status: flashraw_status::from_kind(kind), kind, message: e.to_string(), detail }
    }
}

pub(crate) type FfiResult<T> = Result<T, FfiError>;

struct LastError {
    status: flashraw_status,
    kind: CString,
    message: CString,
    detail: Option<CString>,
}

thread_local! {
    static LAST_ERROR: RefCell<Option<LastError>> = const { RefCell::new(None) };
}

pub(crate) fn cstring_lossy(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

fn set_last_error(e: FfiError) -> flashraw_status {
    let status = e.status;
    let last = LastError {
        status,
        kind: cstring_lossy(e.kind),
        message: cstring_lossy(&e.message),
        detail: e.detail.as_deref().map(cstring_lossy),
    };
    // `try_with`: never panic, even during thread teardown.
    let _ = LAST_ERROR.try_with(|l| {
        if let Ok(mut l) = l.try_borrow_mut() {
            *l = Some(last);
        }
    });
    status
}

fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn internal(p: Box<dyn std::any::Any + Send>) -> FfiError {
    FfiError {
        status: FLASHRAW_ERROR_INTERNAL,
        kind: "Internal",
        message: format!("internal error (please report): {}", panic_message(&*p)),
        detail: None,
    }
}

/// Run `f`, turning errors and panics into a status code and the thread's last error.
pub(crate) fn guard(f: impl FnOnce() -> FfiResult<()>) -> flashraw_status {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => FLASHRAW_OK,
        Ok(Err(e)) => set_last_error(e),
        Err(p) => set_last_error(internal(p)),
    }
}

/// Run `f` for a value-returning function: on error or panic, record the error and return
/// `fallback`.
pub(crate) fn guard_value<T>(fallback: T, f: impl FnOnce() -> FfiResult<T>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            set_last_error(e);
            fallback
        }
        Err(p) => {
            set_last_error(internal(p));
            fallback
        }
    }
}

/// Run `f` for a function that cannot fail except by panicking (getters on valid handles).
pub(crate) fn guard_silent<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|p| {
        set_last_error(internal(p));
        fallback
    })
}

fn with_last<T>(f: impl FnOnce(&LastError) -> T) -> Option<T> {
    LAST_ERROR.try_with(|l| l.try_borrow().ok().and_then(|l| l.as_ref().map(f))).ok().flatten()
}

/// Status code of the last failed call on this thread (`FLASHRAW_OK` if none since the
/// thread started or since `flashraw_clear_last_error()`). Successful calls do not reset it.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_last_error_code() -> flashraw_status {
    guard_silent(FLASHRAW_OK, || with_last(|l| l.status).unwrap_or(FLASHRAW_OK))
}

/// Human-readable message of the last failed call on this thread, or NULL if there is none.
///
/// The string is owned by the library and stays valid until the next failing call on this
/// thread or `flashraw_clear_last_error()`; copy it if you need it longer.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_last_error_message() -> *const c_char {
    guard_silent(std::ptr::null(), || with_last(|l| l.message.as_ptr()).unwrap_or(std::ptr::null()))
}

/// Kind name of the last failed call on this thread (e.g. `"CorruptFile"`), or NULL.
/// Same lifetime as `flashraw_last_error_message()`.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_last_error_kind() -> *const c_char {
    guard_silent(std::ptr::null(), || with_last(|l| l.kind.as_ptr()).unwrap_or(std::ptr::null()))
}

/// Structured detail of the last failed call on this thread as a JSON object, or NULL when
/// there is none. For corrupt-file errors with a known cause it describes the damaged unit,
/// e.g. `{"kind":"truncated","unit":"strip","index":3,"offset":...,"expected":...,
/// "available":...,"file_len":...}`. Same lifetime as `flashraw_last_error_message()`.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_last_error_detail_json() -> *const c_char {
    guard_silent(std::ptr::null(), || {
        with_last(|l| l.detail.as_ref().map(|d| d.as_ptr())).flatten().unwrap_or(std::ptr::null())
    })
}

/// Forget the last error of this thread.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_clear_last_error() {
    let _ = LAST_ERROR.try_with(|l| {
        if let Ok(mut l) = l.try_borrow_mut() {
            *l = None;
        }
    });
}

/// Kind name for a status code (`"CorruptFile"`, ...; `"Ok"` for `FLASHRAW_OK`). Static
/// string; `"Unknown"` for values this library does not know.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_error_kind_name(status: i32) -> *const c_char {
    static NAMES: [&std::ffi::CStr; 14] = [
        c"Ok",
        c"UnsupportedFormat",
        c"UnsupportedCompression",
        c"CorruptFile",
        c"InvalidMetadata",
        c"CameraUnsupported",
        c"GpuUnavailable",
        c"OutOfMemory",
        c"LimitExceeded",
        c"NotAvailable",
        c"InvalidArgument",
        c"Io",
        c"Internal",
        c"Other",
    ];
    match flashraw_status::from_i32(status) {
        Some(s) => {
            debug_assert_eq!(NAMES[s as usize].to_str().ok(), Some(s.kind_name()));
            NAMES[s as usize].as_ptr()
        }
        None => c"Unknown".as_ptr(),
    }
}
