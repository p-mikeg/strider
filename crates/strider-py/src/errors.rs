//! Python exception hierarchy for strider-py.
//!
//! All Rust errors propagate through the analysis as `anyhow::Error` and
//! land in Python as `StriderError` (or one of its subclasses). The
//! subclasses are produced at well-defined boundaries (lift, reader
//! construction, pattern build, rewrite). Other errors fall through to
//! plain `StriderError`.

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

create_exception!(strider.errors, StriderError, PyException);
create_exception!(strider.errors, LiftError, StriderError);
create_exception!(strider.errors, ReaderError, StriderError);
create_exception!(strider.errors, PatternError, StriderError);
create_exception!(strider.errors, RewriteError, StriderError);
create_exception!(strider.errors, UnresolvedIndirectBranchError, StriderError);
create_exception!(strider.errors, UnknownCallOtherError, StriderError);

/// Shared preamble for typed-downcast converters.  Returns `Some(PyErr)`
/// when the caller should short-circuit:
///   1. A pending Python exception was `restore`d by a callback (e.g. a
///      subclassed `MemReader.read` raising `KeyboardInterrupt` or
///      `SystemExit`).  Take it here so the original control-flow
///      exception wins over the synthesized `StriderError`.
///   2. The error downcasts to `UnknownCallOtherError`, which both
///      typed converters surface as the specific subclass.
fn take_pending_or_unknown_call_other(e: &anyhow::Error) -> Option<PyErr> {
    if let Some(pending) = Python::with_gil(PyErr::take) {
        return Some(pending);
    }
    if e.downcast_ref::<strider_analyze::UnknownCallOtherError>().is_some() {
        return Some(UnknownCallOtherError::new_err(format!("{e:?}")));
    }
    None
}

/// Convert an `anyhow::Error` into the most specific `StriderError`
/// subclass we can recover from its typed inner error.  Falls back to
/// the generic `StriderError` when the inner error is opaque.
///
/// Formatted with `{:?}` (Debug) so the anyhow Caused-by chain is in
/// the exception message; under `RUST_BACKTRACE=1` the Rust backtrace
/// is included as well. Python's `__cause__` chain is left empty —
/// PyO3-anyhow does not synthesize a separate exception per Rust
/// error link.
pub fn into_strider_err(e: anyhow::Error) -> PyErr {
    if let Some(pyerr) = take_pending_or_unknown_call_other(&e) {
        return pyerr;
    }
    if e.downcast_ref::<strider_analyze::UnresolvedIndirectBranch>().is_some() {
        return UnresolvedIndirectBranchError::new_err(format!("{e:?}"));
    }
    // Typed-downcast for lift failures.  The orchestrator's
    // `build_lift_stable` wraps every cfg-build and IR-lift failure
    // in `strider_lift::LiftError` before propagation; recovering it
    // here keeps the classification precise.  The previous
    // implementation scanned the formatted anyhow chain for
    // substrings ("lift", "sleigh", "decode", "pcode",
    // "unsupported instruction") and misclassified unrelated errors
    // whose message happened to contain one of those tokens (e.g. a
    // `ReaderError` reporting `"failed to decode section .got.plt"`
    // matched `"decode"` and surfaced as `LiftError`).
    if e.downcast_ref::<strider_lift::LiftError>().is_some() {
        return LiftError::new_err(format!("{e:?}"));
    }
    StriderError::new_err(format!("{e:?}"))
}

pub fn into_lift_err(e: anyhow::Error) -> PyErr {
    if let Some(pyerr) = take_pending_or_unknown_call_other(&e) {
        return pyerr;
    }
    LiftError::new_err(format!("{e:?}"))
}

/// Generate a converter `fn $name(e: anyhow::Error) -> PyErr` that
/// formats the anyhow chain via `{e:?}` (so RUST_BACKTRACE=1 produces
/// a backtrace) and wraps it in `$err_ty`.  Used for the converter
/// boundaries that don't downcast typed errors first
/// (reader/pattern/rewrite); `into_strider_err` and `into_lift_err`
/// stay explicit because they prefer typed-error subclasses.
macro_rules! plain_converter {
    ($name:ident, $err_ty:ident) => {
        #[allow(dead_code)]
        pub fn $name(e: anyhow::Error) -> PyErr {
            $err_ty::new_err(format!("{e:?}"))
        }
    };
}

plain_converter!(into_reader_err, ReaderError);
plain_converter!(into_pattern_err, PatternError);
plain_converter!(into_rewrite_err, RewriteError);

/// Register the `strider.errors` submodule on the parent module.
pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new_bound(py, "errors")?;
    m.add("StriderError", py.get_type_bound::<StriderError>())?;
    m.add("LiftError", py.get_type_bound::<LiftError>())?;
    m.add("ReaderError", py.get_type_bound::<ReaderError>())?;
    m.add("PatternError", py.get_type_bound::<PatternError>())?;
    m.add("RewriteError", py.get_type_bound::<RewriteError>())?;
    m.add(
        "UnresolvedIndirectBranchError",
        py.get_type_bound::<UnresolvedIndirectBranchError>(),
    )?;
    m.add(
        "UnknownCallOtherError",
        py.get_type_bound::<UnknownCallOtherError>(),
    )?;
    parent.add_submodule(&m)?;
    parent.add("StriderError", py.get_type_bound::<StriderError>())?;
    // Allow `from strider import errors` and `from strider.errors import X`.
    let sys = py.import_bound("sys")?;
    let modules = sys.getattr("modules")?;
    modules.set_item("strider.errors", &m)?;
    Ok(())
}
