use pyo3::PyErrArguments;
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyString;

create_exception!(
    strider,
    StriderError,
    PyException,
    "Raised when an analysis fails, a pattern is malformed, or an argument \
     has a bad value, a malformed ELF included. A wrong type raises \
     `TypeError`, an int out of range `OverflowError` (an `entry` raises \
     `StriderError`), a range-checked option `ValueError`, a file \
     `load_elf` finds is not an ELF or targets an unsupported arch or ABI \
     `ValueError`, and a missing path `FileNotFoundError`.\n\n\
     `.backtrace` is the message followed by the Rust backtrace (empty when \
     raised from Python); `STRIDER_BACKTRACE=1` appends it to the message."
);

thread_local! {
    /// The Python exception a user callback raised, chained as the
    /// `StriderError`'s `__cause__`. Cleared on entry to every callback and
    /// when the calling operation finishes, so it never outlives that call.
    static CALLBACK_CAUSE: std::cell::RefCell<Option<PyErr>> =
        const { std::cell::RefCell::new(None) };
}

/// Remembers `e` as the cause of whatever `StriderError` this call produces.
pub fn stash_callback_cause(e: PyErr) {
    CALLBACK_CAUSE.with(|c| *c.borrow_mut() = Some(e));
}

/// Drops any remembered cause; call before invoking a user callback.
pub fn clear_callback_cause() {
    CALLBACK_CAUSE.with(|c| *c.borrow_mut() = None);
}

fn take_callback_cause() -> Option<PyErr> {
    CALLBACK_CAUSE.with(|c| c.borrow_mut().take())
}

/// The error a `StriderError` was raised from, whose frames `.backtrace`
/// resolves when read.
#[pyclass(frozen)]
struct ErrorOrigin(anyhow::Error);

#[pymethods]
impl ErrorOrigin {
    /// Pickles as its rendered text.
    fn __reduce__(&self, py: Python<'_>) -> (Py<PyAny>, (String,)) {
        (
            py.get_type_bound::<PyString>().into_any().unbind(),
            (format!("{:?}", self.0),),
        )
    }
}

/// The error and its causes followed by the Rust backtrace, resolved when
/// first read; empty on a `StriderError` raised from Python.
#[pyfunction]
fn backtrace(err: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = err.py();
    let origin = err.getattr(intern!(py, "_origin"))?;
    Ok(match origin.downcast::<ErrorOrigin>() {
        Ok(origin) => format!("{:?}", origin.get().0).into_py(py),
        Err(_) => origin.unbind(),
    })
}

#[pyfunction]
fn set_backtrace(err: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<()> {
    err.setattr(intern!(err.py(), "_origin"), value)
}

#[pyfunction]
fn del_backtrace(err: &Bound<'_, PyAny>) -> PyResult<()> {
    err.delattr(intern!(err.py(), "_origin"))
        .map_err(|_| pyo3::exceptions::PyAttributeError::new_err("backtrace"))
}

/// A `StriderError` instantiated lazily, once the raising stack has unwound,
/// as the error site may have too little stack to format it.
struct Raised {
    origin: anyhow::Error,
    cause: Option<PyErr>,
}

impl PyErrArguments for Raised {
    fn arguments(self, py: Python<'_>) -> PyObject {
        // `{:?}` appends anyhow's backtrace, burying the actionable line.
        let msg = if backtrace_requested() {
            format!("{:?}", self.origin)
        } else {
            format!("{:#}", self.origin)
        };
        let err = || -> PyResult<_> {
            let err = py.get_type_bound::<StriderError>().call1((msg,))?;
            err.setattr(
                intern!(py, "_origin"),
                Py::new(py, ErrorOrigin(self.origin))?,
            )?;
            if let Some(cause) = self.cause {
                err.setattr(intern!(py, "__cause__"), cause.into_value(py))?;
            }
            Ok(err.unbind())
        };
        err().unwrap_or_else(|e| e.into_value(py).into_any())
    }
}

/// Convert an error into a `StriderError` carrying its Caused-by chain.
///
/// A pending Python exception is returned instead, with `e` chained as its
/// `__cause__`.
pub fn into_strider_err(e: anyhow::Error) -> PyErr {
    let raised = |cause| PyErr::new::<StriderError, _>(Raised { origin: e, cause });
    Python::with_gil(|py| match PyErr::take(py) {
        Some(pending) => {
            pending.set_cause(py, Some(raised(None)));
            pending
        }
        None => raised(take_callback_cause()),
    })
}

/// The message of a failing `MemReader.read`, stashed so a failure the
/// decoder swallows still raises.
#[derive(Debug)]
struct ReaderFailure(String);

impl std::fmt::Display for ReaderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReaderFailure {}

/// The `StriderError` a failing `MemReader.read` stashes, `cause` chained.
pub fn reader_failure(msg: String, cause: Option<PyErr>) -> PyErr {
    PyErr::new::<StriderError, _>(Raised {
        origin: ReaderFailure(msg).into(),
        cause,
    })
}

/// What a call raises given the `pending` error a callback stashed and the
/// error `out` it returned: `out` when `pending` is a [`reader_failure`] and
/// `out` the `StriderError` it propagated as, which names where the decode
/// failed, with `pending`'s cause; else `pending`.
pub fn pending_over(py: Python<'_>, pending: PyErr, out: PyErr) -> PyErr {
    let is_reader_failure = pending
        .value_bound(py)
        .getattr(intern!(py, "_origin"))
        .ok()
        .and_then(|o| {
            o.downcast::<ErrorOrigin>()
                .ok()
                .map(|o| o.get().0.is::<ReaderFailure>())
        })
        .unwrap_or(false);
    if !is_reader_failure || !out.is_instance_of::<StriderError>(py) {
        return pending;
    }
    out.set_cause(py, pending.cause(py));
    out
}

fn backtrace_requested() -> bool {
    std::env::var_os("STRIDER_BACKTRACE").is_some_and(|v| !v.is_empty() && v != "0")
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let ty = py.get_type_bound::<StriderError>();
    // A `StriderError` raised from Python has no origin.
    ty.setattr("_origin", "")?;
    let property = py.import_bound("builtins")?.getattr("property")?;
    ty.setattr(
        "backtrace",
        property.call1((
            wrap_pyfunction_bound!(backtrace, py)?,
            wrap_pyfunction_bound!(set_backtrace, py)?,
            wrap_pyfunction_bound!(del_backtrace, py)?,
        ))?,
    )?;
    parent.add("StriderError", ty)?;
    Ok(())
}
