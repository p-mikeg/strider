//! Python bindings for the Strider binary analysis pipeline.
//!
//! See `docs/superpowers/specs/2026-05-01-strider-py-design.md`.

use pyo3::prelude::*;

mod arch;
mod errors;

#[pymodule]
fn strider(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    errors::register(py, m)?;
    arch::register(py, m)?;
    Ok(())
}
