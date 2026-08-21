use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

create_exception!(
    strider,
    StriderError,
    PyException,
    "The single exception type raised by strider.  The message is the error \
     and its causes; set `STRIDER_BACKTRACE=1` for the Rust backtrace too."
);

/// Convert an error into a `StriderError` carrying its Caused-by chain.
///
/// A pending Python exception wins: it is returned as-is rather than buried
/// under a synthesized `StriderError`, with `e` chained onto it as the
/// `__cause__` so the Rust side of the failure is not lost.
pub fn into_strider_err(e: anyhow::Error) -> PyErr {
    if let Some(pending) = Python::with_gil(PyErr::take) {
        Python::with_gil(|py| pending.set_cause(py, Some(StriderError::new_err(format!("{e:#}")))));
        return pending;
    }
    // `{e:?}` appends anyhow's backtrace, burying the actionable line; the
    // trace stays reachable on `.backtrace`.
    let verbose = format!("{e:?}");
    let err = StriderError::new_err(if backtrace_requested() {
        verbose.clone()
    } else {
        format!("{e:#}")
    });
    Python::with_gil(|py| {
        let _ = err.value_bound(py).setattr("backtrace", verbose);
    });
    err
}

fn backtrace_requested() -> bool {
    std::env::var_os("STRIDER_BACKTRACE").is_some_and(|v| !v.is_empty() && v != "0")
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add("StriderError", py.get_type_bound::<StriderError>())?;
    Ok(())
}
