use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

create_exception!(
    strider,
    StriderError,
    PyException,
    "Every analysis failure strider raises.  The message is the error and its \
     causes, and `.backtrace` is that message followed by the Rust backtrace; \
     `STRIDER_BACKTRACE=1` folds the backtrace into the message too.  Argument \
     validation still raises the built-in `ValueError` / `TypeError`, and a \
     missing file `FileNotFoundError`."
);

thread_local! {
    /// The Python exception a user callback raised, kept so the synthesized
    /// `StriderError` can chain it as `__cause__`.  Cleared on entry to every
    /// callback so a swallowed failure cannot attach to a later error.
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
        // A user callback's own exception carries the traceback into their
        // code, which the message text alone does not.
        if let Some(cause) = take_callback_cause() {
            err.set_cause(py, Some(cause));
        }
    });
    err
}

fn backtrace_requested() -> bool {
    std::env::var_os("STRIDER_BACKTRACE").is_some_and(|v| !v.is_empty() && v != "0")
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let ty = py.get_type_bound::<StriderError>();
    // Class-level default so `.backtrace` always reads: `into_strider_err`
    // shadows it per instance, but the pending-exception path above returns
    // the caller's own exception and never sets one.
    ty.setattr("backtrace", "")?;
    parent.add("StriderError", ty)?;
    Ok(())
}
