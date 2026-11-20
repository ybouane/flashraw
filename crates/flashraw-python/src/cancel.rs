//! `flashraw.CancelToken`: stops a running decode from another thread.

use pyo3::prelude::*;

/// A flag that asks a running decode to stop. Pass it as `cancel=` to `RawFile.decode()`, then
/// call :meth:`cancel` from any thread: the decode raises :class:`flashraw.CancelledError` soon
/// after and its cores are free again. A cancelled token stays cancelled; make a new one for
/// the next decode, or share one between decodes that should stop together.
#[pyclass(name = "CancelToken", module = "flashraw", frozen)]
pub struct PyCancelToken {
    pub(crate) token: flashraw::CancelToken,
}

#[pymethods]
impl PyCancelToken {
    #[new]
    fn new() -> Self {
        PyCancelToken { token: flashraw::CancelToken::new() }
    }

    /// Ask every decode using this token to stop (safe from any thread, any number of times).
    fn cancel(&self) {
        self.token.cancel();
    }

    /// Whether :meth:`cancel` has been called.
    #[getter]
    fn cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    fn __repr__(&self) -> String {
        format!("<flashraw.CancelToken cancelled={}>", if self.token.is_cancelled() { "True" } else { "False" })
    }
}
