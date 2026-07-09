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
// `unexpected_cfgs` lint here for the same reason.  Likewise PyO3 0.22's
// macros expand `?` over `PyResult<_>` into an `Into::<PyErr>::into(err)`
// call on a value that is already `PyErr`, which `clippy::useless_conversion`
// flags ~109 times across the binding modules; same upstream-fixed-in-0.23
// story, same crate-root suppression.
#![allow(unsafe_op_in_unsafe_fn)]
#![allow(unexpected_cfgs)]
#![allow(clippy::useless_conversion)]
// `#[pymethods]` requires `&self` / `&mut self` receivers — methods exposed
// to Python are called through a `Py<>` wrapper that can't move out.  The
// `into_pat` finaliser on every pattern builder must therefore take `&self`,
// even though the Rust convention `into_*` implies `self` by value.  The
// name is the Python-facing API contract; suppress the lint at the crate
// root rather than rename to `to_pat` and break every doc/example.
#![allow(clippy::wrong_self_convention)]

use pyo3::prelude::*;

mod arch;
mod cc;
mod cfg;
mod dot;
mod errors;
mod function;
#[macro_use]
mod macros;
mod matcher;
mod node;
mod opt;
mod options;
mod pattern;
mod pcode;
mod reader;
mod sleigh;
mod strider_cls;
mod template;

/// Forces anyhow to capture a Rust backtrace at every error
/// construction site, so the `StriderError` raised on the Python side
/// always carries source-location frames — independent of whether
/// the caller remembered to set `RUST_LIB_BACKTRACE` / `RUST_BACKTRACE`.
///
/// Anyhow checks `RUST_LIB_BACKTRACE` first and falls back to
/// `RUST_BACKTRACE`; a value of `0` disables capture.  We seed
/// `RUST_LIB_BACKTRACE=1` only when **neither** variable is set, so
/// an explicit `RUST_LIB_BACKTRACE=0` (or `RUST_BACKTRACE=0` with no
/// `RUST_LIB_BACKTRACE`) the user picked deliberately is still
/// honoured.  Setting only `RUST_LIB_BACKTRACE` keeps the panic-time
/// `RUST_BACKTRACE` semantics untouched.
fn force_anyhow_backtrace_capture() {
    if std::env::var_os("RUST_LIB_BACKTRACE").is_none()
        && std::env::var_os("RUST_BACKTRACE").is_none()
    {
        // SAFETY: called from `#[pymodule]` init, which Python's
        // import lock serialises across Python threads.  Concurrent
        // *Rust* threads spawned by other already-loaded native
        // extensions are theoretically possible — the GIL doesn't
        // gate them — but env-var mutation at import time is the
        // contract every Python native binding follows, and the
        // worst case here is a missing backtrace on a racing reader,
        // not memory unsafety.  Not worth wrapping in a Mutex.
        unsafe {
            std::env::set_var("RUST_LIB_BACKTRACE", "1");
        }
    }
}

// Register the stub-info-gathering function used by the
// `examples/stub_gen.rs` binary to emit `.pyi` files for every
// `#[gen_stub_*]`-annotated type in the crate.  Lives next to the
// pyclass definitions per pyo3-stub-gen's documentation: the
// `inventory::submit!` calls the proc-macros emit are statically
// collected per-rlib, so the gatherer must be in the same crate.
pyo3_stub_gen::define_stub_info_gatherer!(stub_info);

/// The vendored viz.js (Graphviz-in-Wasm) source, so the interactive explorer's
/// local server can serve it and stay fully offline (no CDN).
#[pyfunction]
fn viz_standalone_js() -> &'static str {
    ::dot::viz_standalone_js()
}

#[pymodule]
fn strider(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    force_anyhow_backtrace_capture();
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(pyo3::wrap_pyfunction!(viz_standalone_js, m)?)?;
    errors::register(py, m)?;
    arch::register(py, m)?;
    cc::register(py, m)?;
    reader::register(py, m)?;
    sleigh::register(py, m)?;
    cfg::register(py, m)?;
    function::register(py, m)?;
    node::register(py, m)?;
    strider_cls::register(py, m)?;
    options::register(py, m)?;
    opt::register(py, m)?;
    pattern::register(py, m)?;
    template::register(py, m)?;
    matcher::register(py, m)?;
    Ok(())
}
