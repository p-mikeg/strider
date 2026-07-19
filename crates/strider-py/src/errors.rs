use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

create_exception!(
    strider,
    StriderError,
    PyException,
    "The single exception type raised by strider.  Every error lands here \
     carrying an informative message (and a backtrace under \
     `RUST_BACKTRACE=1`).  The hierarchy is flat: there are no subclasses."
);

/// Convert an error into a `StriderError` carrying its Caused-by chain.
///
/// A pending Python exception wins: it is returned as-is rather than buried
/// under a synthesized `StriderError`.
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
