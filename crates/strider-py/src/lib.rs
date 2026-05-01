//! Python bindings for the Strider binary analysis pipeline.
//!
//! See `docs/superpowers/specs/2026-05-01-strider-py-design.md`.

use pyo3::prelude::*;

mod arch;
mod cc;
mod cfg;
mod dot;
mod errors;
mod graph;
mod matcher;
mod opt;
mod pattern;
mod reader;
mod run;
mod sleigh;
mod strider_cls;

#[pymodule]
fn strider(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    errors::register(py, m)?;
    arch::register(py, m)?;
    cc::register(py, m)?;
    reader::register(py, m)?;
    sleigh::register(py, m)?;
    cfg::register(py, m)?;
    graph::register(py, m)?;
    strider_cls::register(py, m)?;
    opt::register(py, m)?;
    run::register(py, m)?;
    pattern::register(py, m)?;
    matcher::register(py, m)?;
    Ok(())
}
