//! Calibration getters shared by files (before decoding) and frames (after decoding).

use crate::error::{FfiError, FfiResult};
use crate::types::*;
use flashraw::{BlackLevel, CfaPattern, ColorMatrices, WhiteBalance, WhiteLevel};

pub(crate) fn cfa(p: Option<CfaPattern>, out: *mut flashraw_cfa) -> FfiResult<bool> {
    if out.is_null() {
        return Err(FfiError::null("out"));
    }
    let Some(p) = p else { return Ok(false) };
    let mut c = flashraw_cfa { width: p.width() as u32, height: p.height() as u32, colors: [0; 64] };
    for (d, s) in c.colors.iter_mut().zip(p.colors()) {
        *d = cfa_color_to(s);
    }
    // SAFETY: `out` is non-null and points to a caller-owned `flashraw_cfa`.
    unsafe { out.write(c) };
    Ok(true)
}

/// Copy `src` into the caller buffer (`dst`, `capacity` elements, may be NULL when 0);
/// returns the full count.
pub(crate) fn copy_out<T: Copy>(src: &[T], dst: *mut T, capacity: usize) -> FfiResult<usize> {
    if dst.is_null() && capacity > 0 {
        return Err(FfiError::null("values"));
    }
    let n = src.len().min(capacity);
    if n > 0 {
        // SAFETY: `dst` is non-null with room for `capacity >= n` elements (caller
        // contract); `src` has at least `n` elements and cannot overlap caller memory.
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), dst, n) };
    }
    Ok(src.len())
}

pub(crate) fn black(
    b: &BlackLevel,
    info: *mut flashraw_black_level,
    values: *mut f32,
    capacity: usize,
) -> FfiResult<usize> {
    if !info.is_null() {
        let i = flashraw_black_level {
            rows: b.rows.max(1) as u32,
            cols: b.cols.max(1) as u32,
            samples: b.samples.max(1) as u32,
            count: b.values.len() as u32,
            mean: b.mean(),
            has_deltas: (!b.row_deltas.is_empty() || !b.col_deltas.is_empty()) as u32,
        };
        // SAFETY: `info` is non-null and points to a caller-owned struct.
        unsafe { info.write(i) };
    }
    copy_out(&b.values, values, capacity)
}

pub(crate) fn white(w: &WhiteLevel, values: *mut u32, capacity: usize) -> FfiResult<usize> {
    copy_out(&w.values, values, capacity)
}

pub(crate) fn white_balance(wb: Option<WhiteBalance>, out: *mut f32) -> FfiResult<bool> {
    if out.is_null() {
        return Err(FfiError::null("coeffs"));
    }
    let Some(wb) = wb else { return Ok(false) };
    // SAFETY: `out` points to 4 writable floats (documented contract).
    unsafe { std::ptr::copy_nonoverlapping(wb.coeffs.as_ptr(), out, 4) };
    Ok(true)
}

fn list(m: &ColorMatrices, kind: i32) -> FfiResult<&[flashraw::ColorMatrix]> {
    Ok(match kind {
        0 => &m.color,
        1 => &m.forward,
        2 => &m.calibration,
        _ => return Err(FfiError::invalid(format!("unknown matrix kind {kind}"))),
    })
}

pub(crate) fn matrix_count(m: &ColorMatrices, kind: i32) -> FfiResult<usize> {
    Ok(list(m, kind)?.len())
}

pub(crate) fn matrix(m: &ColorMatrices, kind: i32, index: usize, out: *mut flashraw_color_matrix) -> FfiResult<()> {
    if out.is_null() {
        return Err(FfiError::null("out"));
    }
    let l = list(m, kind)?;
    let x = l.get(index).ok_or_else(|| FfiError::not_available(format!("matrix {index} of {}", l.len())))?;
    let rows = x.rows.max(1) as usize;
    let cols = x.values.len() / rows;
    if x.values.len() > 16 || rows * cols != x.values.len() {
        return Err(FfiError::not_available(format!(
            "{rows}-row matrix with {} values does not fit `flashraw_color_matrix` (see the metadata JSON)",
            x.values.len()
        )));
    }
    let mut c = flashraw_color_matrix {
        illuminant: x.illuminant.0 as u32,
        rows: rows as u32,
        cols: cols as u32,
        values: [0.0; 16],
    };
    c.values[..x.values.len()].copy_from_slice(&x.values);
    // SAFETY: `out` is non-null and points to a caller-owned struct.
    unsafe { out.write(c) };
    Ok(())
}
