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

/// Convert an `anyhow::Error` into the most specific `StriderError`
/// subclass we can recover from its typed inner error.  Falls back to
/// the generic `StriderError` when the inner error is opaque.
///
/// Formatted with `{:?}` (Debug) so the anyhow Caused-by chain is in
/// the exception message; under `RUST_BACKTRACE=1` the Rust backtrace
/// is included as well. Python's `__cause__` chain is left empty —
/// PyO3-anyhow does not synthesize a separate exception per Rust
/// error link.
///
/// **Pending-PyErr passthrough.**  If a Python callback (e.g. a
/// subclassed `MemReader.read`) raised `KeyboardInterrupt` or
/// `SystemExit` and `restore`d the `PyErr` before propagating its
/// failure as an `anyhow::Error`, that `PyErr` is still pending on
/// the Python interpreter.  Take it here so the original control-flow
/// exception wins over the synthesized `StriderError` — Ctrl-C
/// interrupts a long lift instead of being absorbed.
pub fn into_strider_err(e: anyhow::Error) -> PyErr {
    if let Some(pending) = Python::with_gil(PyErr::take) {
        return pending;
    }
    if e.downcast_ref::<strider_analyze::UnresolvedIndirectBranch>().is_some() {
        return UnresolvedIndirectBranchError::new_err(format!("{e:?}"));
    }
    if e.downcast_ref::<strider_analyze::UnknownCallOtherError>().is_some() {
        return UnknownCallOtherError::new_err(format!("{e:?}"));
    }
    // String-match heuristic for lift failures.  The orchestrator path
    // (`strider_analyze::run`) folds every error through this converter, so plain
    // pcode-lift / sleigh / cfg failures arrive as bare `anyhow::Error`
    // chains with no typed root.  Until `pcode-lift` exposes a public
    // `LiftError` type we can downcast, recognise the failure family by
    // scanning the formatted chain for canonical lift-stage substrings
    // and route to `LiftError`.  Keep the substring set narrow so an
    // unrelated `StriderError` whose message happens to contain "lift"
    // doesn't get mis-classified.
    let chain = format!("{e:?}").to_lowercase();
    const LIFT_MARKERS: &[&str] = &[
        "lift",
        "sleigh",
        "decode",
        "pcode",
        "unsupported instruction",
    ];
    if LIFT_MARKERS.iter().any(|m| chain.contains(m)) {
        return LiftError::new_err(format!("{e:?}"));
    }
    StriderError::new_err(format!("{e:?}"))
}

pub fn into_lift_err(e: anyhow::Error) -> PyErr {
    if let Some(pending) = Python::with_gil(PyErr::take) {
        return pending;
    }
    if e.downcast_ref::<strider_analyze::UnknownCallOtherError>().is_some() {
        return UnknownCallOtherError::new_err(format!("{e:?}"));
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
