//! `flashraw_frame`: decoding and decoded-frame accessors.

use crate::calib;
use crate::error::{FfiError, FfiResult, cstring_lossy, flashraw_status, guard, guard_value};
use crate::file::{file_ref, flashraw_file};
use crate::types::*;
use flashraw::{DecodeOptions, RawFrame, RawPixelData};
use std::ffi::{CString, c_char, c_void};

/// A decoded frame (opaque): samples plus everything needed to interpret them. Independent
/// of the file it was decoded from (it may outlive it). Immutable: any number of threads
/// may read it concurrently.
pub struct flashraw_frame {
    frame: RawFrame,
    warnings: Vec<CString>,
    backend_decode: CString,
    backend_postprocess: Option<CString>,
    simd: CString,
}

const _: () = {
    const fn is_send_sync<T: Send + Sync>() {}
    is_send_sync::<flashraw_frame>();
};

/// # Safety
/// `f` must be NULL or a live frame handle.
unsafe fn frame_ref<'a>(f: *const flashraw_frame) -> FfiResult<&'a flashraw_frame> {
    // SAFETY: per the contract, a non-null `f` points to a live `flashraw_frame`.
    unsafe { f.as_ref() }.ok_or_else(|| FfiError::null("frame"))
}

fn with_frame<T>(f: *const flashraw_frame, fallback: T, g: impl FnOnce(&flashraw_frame) -> FfiResult<T>) -> T {
    guard_value(fallback, || {
        // SAFETY: every exported frame getter documents `f` as NULL or a live handle.
        let f = unsafe { frame_ref(f) }?;
        g(f)
    })
}

pub(crate) fn default_decode_options() -> flashraw_decode_options {
    flashraw_decode_options {
        size: std::mem::size_of::<flashraw_decode_options>() as u32,
        backend: flashraw_backend::FLASHRAW_BACKEND_AUTO as i32,
        output: flashraw_output_format::FLASHRAW_OUTPUT_RAW_U16 as i32,
        demosaic: flashraw_demosaic::FLASHRAW_DEMOSAIC_BILINEAR as i32,
        corruption_policy: flashraw_corruption_policy::FLASHRAW_CORRUPTION_RECOVER as i32,
        threads: 0,
        crop: 1,
        apply_opcodes: 1,
        use_region: 0,
        region: flashraw_rect::default(),
        max_dimension: 0,
        max_samples: 0,
        exposure: 0.0,
        black_point: 0.0,
        white_point: 1.0,
        preview_fallback: 0,
        color_space: flashraw_color_space::FLASHRAW_COLOR_SPACE_SRGB as i32,
        intent: flashraw_rendering_intent::FLASHRAW_INTENT_PERCEPTUAL as i32,
        black_point_compensation: 0,
        reserved: 0,
        output_profile: std::ptr::null(),
        output_profile_len: 0,
    }
}

/// Initialise `opts` with the defaults and `opts->size = sizeof(flashraw_decode_options)`.
/// Always call this before setting individual fields.
///
/// # Safety
/// `opts` must be NULL or point to a writable `flashraw_decode_options`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_decode_options_init(opts: *mut flashraw_decode_options) -> flashraw_status {
    guard(|| {
        // SAFETY: a non-null `opts` points to writable memory of the struct's size.
        let o = unsafe { opts.as_mut() }.ok_or_else(|| FfiError::null("opts"))?;
        *o = default_decode_options();
        Ok(())
    })
}

pub(crate) fn to_decode_options(o: &flashraw_decode_options, base: flashraw::Limits) -> FfiResult<DecodeOptions> {
    let mut d = DecodeOptions {
        backend: backend_from(o.backend)?,
        output: output_from(o.output)?,
        demosaic: demosaic_from(o.demosaic)?,
        corruption_policy: policy_from(o.corruption_policy)?,
        threads: (o.threads != 0).then_some(o.threads as usize),
        crop: o.crop != 0,
        apply_opcodes: o.apply_opcodes != 0,
        region: (o.use_region != 0).then(|| o.region.into()),
        limits: base,
        exposure: o.exposure,
        black_point: o.black_point,
        white_point: o.white_point,
        preview_fallback: o.preview_fallback != 0,
        color_space: color_space_from(o.color_space)?,
        output_profile: None,
    };
    if !o.output_profile.is_null() {
        // SAFETY: the caller guarantees `output_profile_len` readable bytes (documented on the field).
        let bytes = unsafe { std::slice::from_raw_parts(o.output_profile, o.output_profile_len) };
        d.output_profile = Some(
            flashraw::OutputProfile::new(bytes)
                .with_intent(intent_from(o.intent)?)
                .with_black_point_compensation(o.black_point_compensation != 0),
        );
    }
    if o.max_samples != 0 {
        d.limits = d.limits.with_max_samples(o.max_samples);
    }
    if o.max_dimension != 0 {
        d.limits = d.limits.with_max_dimension(o.max_dimension);
    }
    if let Some(r) = d.region {
        if r.is_empty() {
            return Err(FfiError::invalid(format!("empty region {r}")));
        }
    }
    Ok(d)
}

/// Decode the sensor data of `file` into a new frame (`*out`; NULL on failure). `opts` may
/// be NULL for the defaults (raw 16-bit samples, all cores, recover policy).
///
/// Safe to call concurrently on the same file from several threads.
///
/// # Safety
/// `file` must be NULL or a live handle; `opts` NULL or initialised with
/// `flashraw_decode_options_init()`; `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_decode(
    file: *const flashraw_file,
    opts: *const flashraw_decode_options,
    out: *mut *mut flashraw_frame,
) -> flashraw_status {
    guard(|| {
        if out.is_null() {
            return Err(FfiError::null("out"));
        }
        // SAFETY: `out` is non-null and writable (contract); cleared before any failure.
        unsafe { out.write(std::ptr::null_mut()) };
        // SAFETY: caller contract on `file` and `opts`.
        let f = unsafe { file_ref(file) }?;
        // SAFETY: as above.
        let o = unsafe { read_versioned(opts, default_decode_options(), "flashraw_decode_options") }?;
        let opts = to_decode_options(&o, f.limits)?;
        let frame = f.raw.decode(opts)?;
        let s = &frame.stats.backend;
        let h = flashraw_frame {
            warnings: frame.warnings.iter().map(|w| cstring_lossy(&w.to_string())).collect(),
            backend_decode: cstring_lossy(&s.decode),
            backend_postprocess: s.postprocess.as_deref().map(cstring_lossy),
            simd: cstring_lossy(&s.simd),
            frame,
        };
        // SAFETY: as above.
        unsafe { out.write(Box::into_raw(Box::new(h))) };
        Ok(())
    })
}

/// Free a frame (NULL is ignored). Its data pointer becomes invalid.
///
/// # Safety
/// `f` must be NULL or a frame from `flashraw_decode()` that has not been freed, and no other
/// thread may be using it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_free(f: *mut flashraw_frame) {
    if f.is_null() {
        return;
    }
    // SAFETY: `f` came from `Box::into_raw` in `flashraw_decode` and is freed exactly once.
    let b = unsafe { Box::from_raw(f) };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(b)));
}

/// Width of the sample buffer in pixels (region width for region decodes, crop width for
/// cropped RGB outputs, after orientation for RGBA8). 0 on error.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_width(frame: *const flashraw_frame) -> u32 {
    with_frame(frame, 0, |f| Ok(f.frame.width))
}

/// Height of the sample buffer in pixels. 0 on error.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_height(frame: *const flashraw_frame) -> u32 {
    with_frame(frame, 0, |f| Ok(f.frame.height))
}

/// Samples per pixel (1 for CFA/mono, 3 for linear RGB, 4 for RGBA8). 0 on error.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_cpp(frame: *const flashraw_frame) -> u32 {
    with_frame(frame, 0, |f| Ok(f.frame.cpp))
}

/// Significant bits per sample of the sensor data as stored in the file.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_bits_per_sample(frame: *const flashraw_frame) -> u32 {
    with_frame(frame, 0, |f| Ok(f.frame.bits_per_sample as u32))
}

/// EXIF orientation (1-8) still to apply for display (1 for RGBA8, which has it applied).
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_orientation(frame: *const flashraw_frame) -> u32 {
    with_frame(frame, 0, |f| Ok(f.frame.orientation.exif()))
}

/// Element type of the samples (`flashraw_sample_type`); 0 on error.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_sample_type(frame: *const flashraw_frame) -> i32 {
    with_frame(frame, 0, |f| {
        Ok(match &f.frame.data {
            RawPixelData::U8(_) => flashraw_sample_type::FLASHRAW_SAMPLE_U8 as i32,
            RawPixelData::U16(_) => flashraw_sample_type::FLASHRAW_SAMPLE_U16 as i32,
            RawPixelData::F32(_) => flashraw_sample_type::FLASHRAW_SAMPLE_F32 as i32,
            RawPixelData::F16(_) => flashraw_sample_type::FLASHRAW_SAMPLE_F16 as i32,
            #[allow(unreachable_patterns)]
            _ => 0,
        })
    })
}

/// Output format the frame was decoded to (`flashraw_output_format`).
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_output_format(frame: *const flashraw_frame) -> i32 {
    with_frame(frame, -1, |f| Ok(output_to(f.frame.output)))
}

/// Pointer to the samples: row-major, `cpp` interleaved samples per pixel, element type per
/// `flashraw_frame_sample_type()`, rows of `width * cpp` elements with no padding. Owned by
/// the frame (valid until `flashraw_frame_free()`), suitably aligned for its element type.
/// NULL on error or for an empty frame.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_data(frame: *const flashraw_frame) -> *const c_void {
    with_frame(frame, std::ptr::null(), |f| {
        Ok(match &f.frame.data {
            RawPixelData::U8(v) if !v.is_empty() => v.as_ptr() as *const c_void,
            RawPixelData::U16(v) | RawPixelData::F16(v) if !v.is_empty() => v.as_ptr() as *const c_void,
            RawPixelData::F32(v) if !v.is_empty() => v.as_ptr() as *const c_void,
            _ => std::ptr::null(),
        })
    })
}

/// Size of the sample buffer in bytes.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_data_len(frame: *const flashraw_frame) -> usize {
    with_frame(frame, 0, |f| Ok(f.frame.data.byte_len()))
}

/// Number of samples (elements) in the buffer: `width * height * cpp`.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_sample_count(frame: *const flashraw_frame) -> usize {
    with_frame(frame, 0, |f| Ok(f.frame.data.len()))
}

fn rect_out(
    frame: *const flashraw_frame,
    out: *mut flashraw_rect,
    g: impl FnOnce(&RawFrame) -> flashraw::Rect,
) -> flashraw_status {
    guard(|| {
        // SAFETY: exported callers document `frame` as NULL or live.
        let f = unsafe { frame_ref(frame) }?;
        // SAFETY: a non-null `out` is writable per the contract.
        let o = unsafe { out.as_mut() }.ok_or_else(|| FfiError::null("out"))?;
        *o = g(&f.frame).into();
        Ok(())
    })
}

/// Region of the buffer holding valid image pixels (frame coordinates).
///
/// # Safety
/// `frame` must be NULL or a live frame; `out` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_active_area(
    frame: *const flashraw_frame,
    out: *mut flashraw_rect,
) -> flashraw_status {
    rect_out(frame, out, |f| f.active_area)
}

/// Recommended default crop (frame coordinates).
///
/// # Safety
/// `frame` must be NULL or a live frame; `out` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_crop(frame: *const flashraw_frame, out: *mut flashraw_rect) -> flashraw_status {
    rect_out(frame, out, |f| f.crop)
}

/// Origin of the frame in sensor-buffer coordinates (non-zero for region decodes).
///
/// # Safety
/// `frame` must be NULL or a live frame; `x` and `y` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_origin(
    frame: *const flashraw_frame,
    x: *mut u32,
    y: *mut u32,
) -> flashraw_status {
    guard(|| {
        // SAFETY: caller contract.
        let f = unsafe { frame_ref(frame) }?;
        if x.is_null() || y.is_null() {
            return Err(FfiError::null("x/y"));
        }
        // SAFETY: both pointers are non-null and writable (contract).
        unsafe {
            x.write(f.frame.origin.0);
            y.write(f.frame.origin.1);
        }
        Ok(())
    })
}

/// Sensor layout of the samples (`flashraw_sensor_layout`; LINEAR for RGB outputs).
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_sensor_layout(frame: *const flashraw_frame) -> i32 {
    with_frame(frame, flashraw_sensor_layout::FLASHRAW_LAYOUT_UNKNOWN as i32, |f| {
        Ok(layout_to(f.frame.sensor_layout) as i32)
    })
}

/// CFA pattern anchored at sensor-buffer (0, 0) (shift by the origin for region frames).
/// False for RGB outputs and sensors without a CFA.
///
/// # Safety
/// `frame` must be NULL or a live frame; `out` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_cfa(frame: *const flashraw_frame, out: *mut flashraw_cfa) -> bool {
    with_frame(frame, false, |f| calib::cfa(f.frame.cfa, out))
}

/// CFA colour (`flashraw_cfa_color`) at frame coordinates (accounts for the origin), or -1
/// when the frame has no CFA.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_color_at(frame: *const flashraw_frame, x: u32, y: u32) -> i32 {
    with_frame(frame, -1, |f| Ok(f.frame.color_at(x, y).map_or(-1, |c| cfa_color_to(c) as i32)))
}

/// Black level of the frame (measured from masked areas when the format requires it),
/// anchored at sensor-buffer (0, 0). Like `flashraw_file_black_level()`.
///
/// # Safety
/// `frame` must be NULL or a live frame; `info` NULL or writable; `values` NULL or writable
/// for `capacity` floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_black_level(
    frame: *const flashraw_frame,
    info: *mut flashraw_black_level,
    values: *mut f32,
    capacity: usize,
) -> usize {
    with_frame(frame, 0, |f| calib::black(&f.frame.black_level, info, values, capacity))
}

/// White levels of the frame. Like `flashraw_file_white_level()`.
///
/// # Safety
/// `frame` must be NULL or a live frame; `values` NULL or writable for `capacity` elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_white_level(
    frame: *const flashraw_frame,
    values: *mut u32,
    capacity: usize,
) -> usize {
    with_frame(frame, 0, |f| calib::white(&f.frame.white_level, values, capacity))
}

/// As-shot white balance `[R, G, B, G2]` (G = 1); false when unknown.
///
/// # Safety
/// `frame` must be NULL or a live frame; `coeffs` must point to 4 writable floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_white_balance(frame: *const flashraw_frame, coeffs: *mut f32) -> bool {
    with_frame(frame, false, |f| calib::white_balance(f.frame.white_balance, coeffs))
}

/// True when the camera already applied the as-shot white balance to the samples (see
/// `flashraw_file_white_balance_applied`). False for NULL.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_white_balance_applied(frame: *const flashraw_frame) -> bool {
    with_frame(frame, false, |f| Ok(f.frame.white_balance_applied))
}

/// Number of matrices of `kind` (`flashraw_matrix_kind`).
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_color_matrix_count(frame: *const flashraw_frame, kind: i32) -> usize {
    with_frame(frame, 0, |f| calib::matrix_count(&f.frame.color_matrices, kind))
}

/// Matrix `index` of `kind` (`flashraw_matrix_kind`).
///
/// # Safety
/// `frame` must be NULL or a live frame; `out` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_color_matrix(
    frame: *const flashraw_frame,
    kind: i32,
    index: usize,
    out: *mut flashraw_color_matrix,
) -> flashraw_status {
    guard(|| {
        // SAFETY: caller contract.
        let f = unsafe { frame_ref(frame) }?;
        calib::matrix(&f.frame.color_matrices, kind, index, out)
    })
}

/// True when some samples hold no decoded data (truncated or corrupt input recovered under
/// `FLASHRAW_CORRUPTION_RECOVER`; see the warnings).
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_is_incomplete(frame: *const flashraw_frame) -> bool {
    with_frame(frame, false, |f| Ok(f.frame.is_incomplete()))
}

/// Where the frame's pixels come from (`flashraw_pixel_source`): the sensor data, or the
/// embedded preview substituted by `flashraw_decode_options.preview_fallback`.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_source(frame: *const flashraw_frame) -> i32 {
    with_frame(frame, flashraw_pixel_source::FLASHRAW_SOURCE_SENSOR as i32, |f| {
        Ok(match f.frame.source {
            flashraw::FrameSource::Preview => flashraw_pixel_source::FLASHRAW_SOURCE_PREVIEW,
            _ => flashraw_pixel_source::FLASHRAW_SOURCE_SENSOR,
        } as i32)
    })
}

/// Colour space of the RGB samples (`flashraw_color_space`; `FLASHRAW_COLOR_SPACE_CUSTOM`
/// when an output profile was used, sRGB for sensor-domain outputs). 0 on error.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_color_space(frame: *const flashraw_frame) -> i32 {
    with_frame(frame, 0, |f| Ok(color_space_to(f.frame.color_space)))
}

/// ICC profile describing the RGB samples (for tagging files): the profile of the colour
/// space (display curve for RGBA8 and RGB16, linear curve for the linear outputs) or the
/// output profile that was used. Writes the size to `*len` (if non-NULL). Owned by the
/// frame. NULL for sensor-domain outputs and preview frames.
///
/// # Safety
/// `frame` must be NULL or a live frame; `len` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_icc_profile(frame: *const flashraw_frame, len: *mut usize) -> *const u8 {
    with_frame(frame, std::ptr::null(), |f| {
        let p = f.frame.icc_profile();
        // SAFETY: a non-null `len` is writable per the contract.
        if let Some(l) = unsafe { len.as_mut() } {
            *l = p.map_or(0, <[u8]>::len);
        }
        Ok(p.map_or(std::ptr::null(), <[u8]>::as_ptr))
    })
}

/// Copy the standard ICC profile of a built-in colour space into `buf` (`linear` non-zero:
/// the variant with a linear curve, for the linear outputs). Returns the profile size; when
/// `buf` is NULL or `capacity` is smaller nothing is written. 0 for an unknown space.
///
/// # Safety
/// `buf` must be NULL or writable for `capacity` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_icc_profile(color_space: i32, linear: bool, buf: *mut u8, capacity: usize) -> usize {
    guard_value(0, || {
        let cs = color_space_from(color_space)?;
        let p = if linear { cs.icc_profile_linear() } else { cs.icc_profile() };
        if !buf.is_null() && capacity >= p.len() {
            // SAFETY: `buf` is writable for `capacity >= p.len()` bytes (contract).
            unsafe { std::ptr::copy_nonoverlapping(p.as_ptr(), buf, p.len()) };
        }
        Ok(p.len())
    })
}

/// True when this build can convert to arbitrary ICC profiles (`output_profile`); the
/// built-in colour spaces and their profiles are always available.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_icc_supported() -> bool {
    flashraw::icc_supported()
}

/// Number of recovery warnings.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_warning_count(frame: *const flashraw_frame) -> usize {
    with_frame(frame, 0, |f| Ok(f.warnings.len()))
}

/// Message of warning `index` (owned by the frame), or NULL. Structured fields are in
/// `flashraw_frame_json()`.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_warning(frame: *const flashraw_frame, index: usize) -> *const c_char {
    with_frame(frame, std::ptr::null(), |f| {
        f.warnings
            .get(index)
            .map(|s| s.as_ptr())
            .ok_or_else(|| FfiError::not_available(format!("warning {index} of {}", f.warnings.len())))
    })
}

/// Samples zero-filled by recovery, summed over all warnings.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_samples_filled(frame: *const flashraw_frame) -> u64 {
    with_frame(frame, 0, |f| Ok(f.frame.warnings.iter().map(|w| w.samples_filled).sum()))
}

/// Timing statistics. Set `out->size = sizeof(flashraw_decode_stats)` first.
///
/// # Safety
/// `frame` must be NULL or a live frame; `out` NULL or a writable struct with `size` set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_stats(
    frame: *const flashraw_frame,
    out: *mut flashraw_decode_stats,
) -> flashraw_status {
    guard(|| {
        // SAFETY: caller contract.
        let f = unsafe { frame_ref(frame) }?;
        let s = &f.frame.stats;
        let mut v = flashraw_decode_stats {
            size: std::mem::size_of::<flashraw_decode_stats>() as u32,
            threads: s.backend.threads as u32,
            total_ms: s.total_ms,
            input_bytes: s.input_bytes,
            output_bytes: s.output_bytes,
            stage_ms: [0.0; 11],
        };
        for &(st, ms) in &s.stages {
            if let Some(i) = stage_to(st) {
                v.stage_ms[i] += ms;
            }
        }
        // SAFETY: caller contract on `out`.
        unsafe { write_versioned(out, &v, "out") }
    })
}

/// Description of the backend that decoded the frame, e.g. "CPU AVX2 x16 threads".
/// Owned by the frame.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_backend(frame: *const flashraw_frame) -> *const c_char {
    with_frame(frame, std::ptr::null(), |f| Ok(f.backend_decode.as_ptr()))
}

/// Description of the post-processing backend, or NULL when none ran (raw outputs).
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_postprocess_backend(frame: *const flashraw_frame) -> *const c_char {
    with_frame(frame, std::ptr::null(), |f| Ok(f.backend_postprocess.as_ref().map_or(std::ptr::null(), |s| s.as_ptr())))
}

/// SIMD kernel level used ("AVX2", "NEON", ..., or "scalar").
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_simd(frame: *const flashraw_frame) -> *const c_char {
    with_frame(frame, std::ptr::null(), |f| Ok(f.simd.as_ptr()))
}

/// The frame's description as JSON (geometry, calibration, statistics, warnings with their
/// structured fields; no pixel data). Release with `flashraw_string_free()`.
///
/// # Safety
/// `frame` must be NULL or a live frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_frame_json(frame: *const flashraw_frame) -> *mut c_char {
    with_frame(frame, std::ptr::null_mut(), |f| Ok(cstring_lossy(&crate::json::frame(&f.frame).to_string()).into_raw()))
}
