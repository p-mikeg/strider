//! Python bindings for the Strider binary analysis pipeline.
//!
//! See `docs/superpowers/specs/2026-05-01-strider-py-design.md`.

// PyO3 0.22's attribute macros (`#[pymethods]`, `#[pyfunction]`,
// `#[pymodule]`, `create_exception!`) emit calls to `unsafe fn`s
// (`BoundRef::ref_from_ptr`, `BoundRef::downcast_unchecked`,
// `unwrap_required_argument`, raw-pointer dereferences) inside
// generated function bodies that the Rust 2024 edition's
// `unsafe_op_in_unsafe_fn` lint flags as warnings.  PyO3 0.23+ wraps
// those calls in explicit `unsafe { … }` blocks; until we cut over to
// 0.23+ we silence the lint at the crate root rather than sprinkling
// `#[allow(...)]` on every #[pymethods] impl.  The same release also
// stops emitting the legacy `gil-refs` feature gate, which fires the
// `unexpected_cfgs` lint here for the same reason.
#![allow(unsafe_op_in_unsafe_fn)]
#![allow(unexpected_cfgs)]

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
