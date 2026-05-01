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

/// Convert an `anyhow::Error` into a generic `StriderError`. Use the
/// boundary-specific `into_*_err` helpers below when you know which
/// stage raised the error.
///
/// Formatted with `{:?}` (Debug) so the anyhow Caused-by chain is in
/// the exception message; under `RUST_BACKTRACE=1` the Rust backtrace
/// is included as well. Python's `__cause__` chain is left empty —
/// PyO3-anyhow does not synthesize a separate exception per Rust
/// error link.
#[allow(dead_code)]
pub fn into_strider_err(e: anyhow::Error) -> PyErr {
    StriderError::new_err(format!("{e:?}"))
}

#[allow(dead_code)]
pub fn into_lift_err(e: anyhow::Error) -> PyErr {
    LiftError::new_err(format!("{e:?}"))
}

#[allow(dead_code)]
pub fn into_reader_err(e: anyhow::Error) -> PyErr {
    ReaderError::new_err(format!("{e:?}"))
}

#[allow(dead_code)]
pub fn into_pattern_err(e: anyhow::Error) -> PyErr {
    PatternError::new_err(format!("{e:?}"))
}

#[allow(dead_code)]
pub fn into_rewrite_err(e: anyhow::Error) -> PyErr {
    RewriteError::new_err(format!("{e:?}"))
}

/// Register the `strider.errors` submodule on the parent module.
pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new_bound(py, "errors")?;
    m.add("StriderError", py.get_type_bound::<StriderError>())?;
    m.add("LiftError", py.get_type_bound::<LiftError>())?;
    m.add("ReaderError", py.get_type_bound::<ReaderError>())?;
    m.add("PatternError", py.get_type_bound::<PatternError>())?;
    m.add("RewriteError", py.get_type_bound::<RewriteError>())?;
    parent.add_submodule(&m)?;
    parent.add("StriderError", py.get_type_bound::<StriderError>())?;
    // Allow `from strider import errors` and `from strider.errors import X`.
    let sys = py.import_bound("sys")?;
    let modules = sys.getattr("modules")?;
    modules.set_item("strider.errors", &m)?;
    Ok(())
}
