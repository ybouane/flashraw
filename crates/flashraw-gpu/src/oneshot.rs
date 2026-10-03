//! Minimal one-shot channel used to turn wgpu callbacks (buffer mapping, queue
//! completion) into futures, so the same async code runs on native (driven by
//! `Device::poll`) and in the browser (driven by the JS event loop).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

struct Slot<T> {
    value: Option<T>,
    waker: Option<Waker>,
    closed: bool,
}

pub(crate) struct Sender<T>(Arc<Mutex<Slot<T>>>);
pub(crate) struct Receiver<T>(Arc<Mutex<Slot<T>>>);

pub(crate) fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let s = Arc::new(Mutex::new(Slot { value: None, waker: None, closed: false }));
    (Sender(s.clone()), Receiver(s))
}

impl<T> Sender<T> {
    pub(crate) fn send(self, v: T) {
        if let Ok(mut s) = self.0.lock() {
            s.value = Some(v);
            if let Some(w) = s.waker.take() {
                w.wake();
            }
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if let Ok(mut s) = self.0.lock() {
            s.closed = true;
            if let Some(w) = s.waker.take() {
                w.wake();
            }
        }
    }
}

impl<T> Receiver<T> {
    /// The value, if it has already been sent (native code polls the device to
    /// completion first, so it never needs to suspend).
    pub(crate) fn try_take(&self) -> Option<T> {
        self.0.lock().ok()?.value.take()
    }

    /// The value has been sent (or the sender dropped).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn is_ready(&self) -> bool {
        self.0.lock().map(|s| s.value.is_some() || s.closed).unwrap_or(true)
    }
}

impl<T> Future for Receiver<T> {
    /// `None` if the sender was dropped without sending (e.g. device lost).
    type Output = Option<T>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let Ok(mut s) = self.0.lock() else { return Poll::Ready(None) };
        if let Some(v) = s.value.take() {
            return Poll::Ready(Some(v));
        }
        if s.closed {
            return Poll::Ready(None);
        }
        s.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}
