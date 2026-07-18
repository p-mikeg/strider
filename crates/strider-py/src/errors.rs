//! Python exception hierarchy for strider-py.
//!
//! All Rust errors propagate through the analysis as `anyhow::Error` and
//! land in Python as `StriderError`.  Errors carry an informative message
//! and (with `RUST_BACKTRACE=1`) a Rust backtrace.

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

/// Convert an `anyhow::Error` into a `StriderError`.
///
/// Formatted with `{:?}` (Debug) so the anyhow Caused-by chain is in
/// the exception message; under `RUST_BACKTRACE=1` the Rust backtrace
/// is included as well.
///
/// If a Python callback raised an exception (e.g. `KeyboardInterrupt`
/// inside a `MemReader.read` implementation), the pending Python
/// exception is taken and returned as-is so the original
/// control-flow exception wins over the synthesized `StriderError`.
pub fn into_strider_err(e: anyhow::Error) -> PyErr {
    if let Some(pending) = Python::with_gil(PyErr::take) {
        return pending;
    }
    StriderError::new_err(format!("{e:?}"))
}

/// Register `StriderError` on the top-level `strider` module.
pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add("StriderError", py.get_type_bound::<StriderError>())?;
    Ok(())
}
