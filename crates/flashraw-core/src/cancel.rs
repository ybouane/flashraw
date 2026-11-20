//! Cooperative cancellation of a decode.
//!
//! A [`CancelToken`] is a shared flag. Whoever starts a decode hands the token to it
//! (`DecodeOptions::cancel`), and any other thread can call [`CancelToken::cancel`] while the
//! decode runs. The decode notices at its next check, stops handing out work, and returns
//! [`Error::Cancelled`]; the partial result is dropped.
//!
//! Checks sit between units of work (the chunks, strips, tiles and bands the parallel helpers in
//! [`crate::par`] hand out, and the rows of the long sequential loops), so a cancelled decode
//! stops within one unit rather than at once. Nothing is interrupted from the outside: no thread
//! is killed and no memory is left in an undefined state.
//!
//! The token a decode runs under is kept per thread ([`scope`]); the parallel helpers carry it
//! onto the pool workers they use, so code deep inside a decoder reads it with [`check`] or
//! [`is_cancelled`] without any parameter plumbing.

use crate::{Error, Result};
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A flag that asks a running decode to stop. Cloning gives another handle to the same flag.
///
/// A token that has been cancelled stays cancelled; use a new one for the next decode.
#[derive(Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// A token that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask every decode running under this token (or started with it later) to stop. Safe to
    /// call from any thread, any number of times.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    /// Whether [`cancel`](Self::cancel) has been called.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// The flag itself, for hosts that share it with another runtime.
    pub fn as_atomic(&self) -> &AtomicBool {
        &self.flag
    }

    /// A token around an existing flag (the flag is shared, not copied).
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        CancelToken { flag }
    }
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken").field("cancelled", &self.is_cancelled()).finish()
    }
}

std::thread_local! {
    /// The token of the decode running on this thread.
    static CURRENT: RefCell<Option<CancelToken>> = const { RefCell::new(None) };
}

/// Restores the previous token of the thread when dropped.
pub struct Scope(Option<CancelToken>);

impl Drop for Scope {
    fn drop(&mut self) {
        let prev = self.0.take();
        CURRENT.with(|c| *c.borrow_mut() = prev);
    }
}

/// Make `token` the one [`check`] and [`is_cancelled`] read on this thread until the returned
/// guard is dropped (`None` clears it).
pub fn enter(token: Option<CancelToken>) -> Scope {
    Scope(CURRENT.with(|c| c.replace(token)))
}

/// Run `f` with `token` as this thread's token.
pub fn scope<R>(token: Option<&CancelToken>, f: impl FnOnce() -> R) -> R {
    let _guard = enter(token.cloned());
    f()
}

/// The token of the decode running on this thread, if it has one.
pub fn current() -> Option<CancelToken> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Whether the decode running on this thread has been cancelled (false when it has no token).
#[inline]
pub fn is_cancelled() -> bool {
    CURRENT.with(|c| c.borrow().as_ref().is_some_and(CancelToken::is_cancelled))
}

/// `Err(Error::Cancelled)` when the decode running on this thread has been cancelled. Call it
/// between units of work in a loop that can run for more than a few milliseconds.
#[inline]
pub fn check() -> Result<()> {
    if is_cancelled() { Err(Error::Cancelled) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_token_never_cancels() {
        assert!(!is_cancelled());
        assert!(check().is_ok());
        assert!(current().is_none());
    }

    #[test]
    fn scope_sets_and_restores() {
        let outer = CancelToken::new();
        let inner = CancelToken::new();
        scope(Some(&outer), || {
            assert!(check().is_ok());
            scope(Some(&inner), || {
                inner.cancel();
                assert!(matches!(check(), Err(Error::Cancelled)));
            });
            // Back under the outer token, which was not cancelled.
            assert!(check().is_ok());
            outer.cancel();
            assert!(is_cancelled());
        });
        assert!(!is_cancelled());
    }

    #[test]
    fn clones_share_the_flag() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled());
        assert!(format!("{b:?}").contains("true"));
    }

    #[test]
    fn another_thread_can_cancel() {
        let t = CancelToken::new();
        let t2 = t.clone();
        scope(Some(&t), || {
            let h = std::thread::spawn(move || t2.cancel());
            h.join().unwrap();
            assert!(is_cancelled());
        });
    }
}
