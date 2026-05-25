//! Python exception hierarchy for strider-py.
//!
//! All Rust errors propagate through the analysis as `anyhow::Error` and
//! land in Python as `StriderError`.  Errors carry an informative message
//! and (with `RUST_BACKTRACE=1`) a Rust backtrace.

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

create_exception!(strider.errors, StriderError, PyException);

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

/// Register the `strider.errors` submodule on the parent module.
pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new_bound(py, "errors")?;
    m.add("StriderError", py.get_type_bound::<StriderError>())?;
    parent.add_submodule(&m)?;
    parent.add("StriderError", py.get_type_bound::<StriderError>())?;
    // Allow `from strider import errors` and `from strider.errors import StriderError`.
    let sys = py.import_bound("sys")?;
    let modules = sys.getattr("modules")?;
    modules.set_item("strider.errors", &m)?;
    Ok(())
}
