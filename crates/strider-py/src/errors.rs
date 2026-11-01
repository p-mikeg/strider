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
    "Raised when an analysis fails, when a pattern is built into an invalid \
     shape, and for a bad argument. The exceptions are a wrong type \
     (`TypeError`), an int out of range (`OverflowError`, though an `entry` \
     out of range raises `StriderError`), a range-checked option \
     (`ValueError`, such as `CfgOptions(function_max_size=0)`, an unknown \
     `known_targets` string, an empty `data_ranges` range or a \
     `BufferReader` region past the address space), an ELF `load_elf` cannot \
     lift (`ValueError`), and a missing `load_elf` path \
     (`FileNotFoundError`). The message carries the error and its causes; \
     `.backtrace` carries that same message followed by the Rust backtrace, \
     resolved when first read (empty on a `StriderError` raised from \
     Python), and `STRIDER_BACKTRACE=1` folds the backtrace into the message \
     too."
);

thread_local! {
    /// The Python exception a user callback raised, kept so the synthesized
    /// `StriderError` can chain it as `__cause__`.  Cleared on entry to every
    /// callback and again when the call that ran the callback drains its
    /// control-flow cell, so a swallowed failure neither outlives that call nor
    /// pins the callback frame's traceback.
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

/// `StriderError.backtrace`.
#[pyfunction]
fn backtrace(err: &Bound<'_, PyAny>) -> PyResult<String> {
    let origin = err.getattr(intern!(err.py(), "_origin"))?;
    if let Ok(origin) = origin.downcast::<ErrorOrigin>() {
        return Ok(format!("{:?}", origin.get().0));
    }
    Ok(origin.extract::<Option<String>>()?.unwrap_or_default())
}

/// Sets `StriderError.backtrace`.
#[pyfunction]
fn set_backtrace(err: &Bound<'_, PyAny>, text: String) -> PyResult<()> {
    err.setattr(intern!(err.py(), "_origin"), text)
}

/// A `StriderError` instantiated when Python raises it, once the raising
/// stack has unwound: an error can arise where a thread's stack is all but
/// spent, too little to format it or to call into Python.
struct Raised {
    origin: anyhow::Error,
    cause: Option<PyErr>,
}

impl PyErrArguments for Raised {
    fn arguments(self, py: Python<'_>) -> PyObject {
        // `{e:?}` appends anyhow's backtrace, burying the actionable line.
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
/// A pending Python exception wins: it is returned as-is rather than buried
/// under a synthesized `StriderError`, with `e` chained onto it as the
/// `__cause__` so the Rust side of the failure is not lost.
pub fn into_strider_err(e: anyhow::Error) -> PyErr {
    let raised = |cause| PyErr::new::<StriderError, _>(Raised { origin: e, cause });
    Python::with_gil(|py| match PyErr::take(py) {
        Some(pending) => {
            pending.set_cause(py, Some(raised(None)));
            pending
        }
        // A user callback's own exception carries the traceback into their
        // code, which the message text alone does not.
        None => raised(take_callback_cause()),
    })
}

fn backtrace_requested() -> bool {
    std::env::var_os("STRIDER_BACKTRACE").is_some_and(|v| !v.is_empty() && v != "0")
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let ty = py.get_type_bound::<StriderError>();
    // A `StriderError` raised from Python has no origin.
    ty.setattr("_origin", py.None())?;
    let property = py.import_bound("builtins")?.getattr("property")?;
    ty.setattr(
        "backtrace",
        property.call1((
            wrap_pyfunction_bound!(backtrace, py)?,
            wrap_pyfunction_bound!(set_backtrace, py)?,
        ))?,
    )?;
    parent.add("StriderError", ty)?;
    Ok(())
}
