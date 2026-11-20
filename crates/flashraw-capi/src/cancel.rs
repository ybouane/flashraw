//! Cancel tokens: stop a running `flashraw_decode()` from another thread.

/// A flag that asks a running decode to stop (see `flashraw_decode_options.cancel`).
///
/// Thread safety: `flashraw_cancel_token_cancel()` and `flashraw_cancel_token_is_cancelled()` may
/// be called from any thread, also while a decode is using the token. The token must not be freed
/// while another thread may still call into it or while a decode holds it.
#[allow(non_camel_case_types)]
pub struct flashraw_cancel_token {
    pub(crate) token: flashraw::CancelToken,
}

/// Create a token that is not cancelled. Free it with `flashraw_cancel_token_free()`.
#[unsafe(no_mangle)]
pub extern "C" fn flashraw_cancel_token_new() -> *mut flashraw_cancel_token {
    Box::into_raw(Box::new(flashraw_cancel_token { token: flashraw::CancelToken::new() }))
}

/// Ask every decode that uses the token (now or later) to stop. Does nothing for NULL. Safe to
/// call from any thread, any number of times; the decode returns `FLASHRAW_ERROR_CANCELLED`.
///
/// # Safety
/// `t` must be NULL or a live token.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_cancel_token_cancel(t: *const flashraw_cancel_token) {
    // SAFETY: a non-null `t` is a live token (contract).
    if let Some(t) = unsafe { t.as_ref() } {
        t.token.cancel();
    }
}

/// Non-zero once the token has been cancelled (0 for NULL).
///
/// # Safety
/// `t` must be NULL or a live token.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_cancel_token_is_cancelled(t: *const flashraw_cancel_token) -> i32 {
    // SAFETY: a non-null `t` is a live token (contract).
    unsafe { t.as_ref() }.is_some_and(|t| t.token.is_cancelled()) as i32
}

/// Free a token. Does nothing for NULL.
///
/// # Safety
/// `t` must be NULL or a token from `flashraw_cancel_token_new()` that has not been freed, and no
/// other thread may be using it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flashraw_cancel_token_free(t: *mut flashraw_cancel_token) {
    if !t.is_null() {
        // SAFETY: allocated by `flashraw_cancel_token_new()` and not freed yet (contract).
        drop(unsafe { Box::from_raw(t) });
    }
}
