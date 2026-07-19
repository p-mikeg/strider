//! Every Rust error travels as `anyhow::Error` and lands in Python as the
//! single flat `StriderError`.

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

create_exception!(
    strider,
    StriderError,
    PyException,
    "The single exception type raised by strider.  Every Rust error \
     (lift failure, unresolved indirect branch, bad pattern, poisoned \
     lock, …) lands here carrying an informative message (and a Rust \
     backtrace under `RUST_BACKTRACE=1`).  The hierarchy is intentionally \
     flat — there are no typed subclasses."
);

/// Formatted with `{:?}` so the anyhow Caused-by chain (and the backtrace,
/// under `RUST_BACKTRACE=1`) reaches the exception message.
///
/// A pending Python exception wins: if a callback raised, say a
/// `KeyboardInterrupt` inside a `MemReader.read`, that exception is returned
/// as-is rather than buried under a synthesized `StriderError`.
pub fn into_strider_err(e: anyhow::Error) -> PyErr {
    if let Some(pending) = Python::with_gil(PyErr::take) {
        return pending;
    }
    StriderError::new_err(format!("{e:?}"))
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add("StriderError", py.get_type_bound::<StriderError>())?;
    Ok(())
}
