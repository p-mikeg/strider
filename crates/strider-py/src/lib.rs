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
/// local server can serve it and stay fully offline (no CDN).  Internal
/// helper — underscore-prefixed on the Python side so it never leaks onto
/// the public `strider` surface.
#[pyfunction]
#[pyo3(name = "_viz_standalone_js")]
fn viz_standalone_js() -> &'static str {
    ::dot::viz_standalone_js()
}

#[pymodule]
fn _strider(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    force_anyhow_backtrace_capture();
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(pyo3::wrap_pyfunction!(viz_standalone_js, m)?)?;
    // The `_load_elf_*` seams stay on the top-level module so the pure-Python
    // facade (`_api.py`) reaches them via `_ext._load_elf_from_segments`; they
    // are underscore/private-intent and never enter `strider.__all__`.
    m.add_function(pyo3::wrap_pyfunction!(reader::load_elf_from_segments, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(reader::load_elf_from_sections, m)?)?;
    // StriderError is the one cross-cutting symbol kept at the top level.
    errors::register(py, m)?;

    // Every domain submodule is created here and passed into the per-domain
    // `register` fns, which only add their classes/functions to the module
    // they are handed — lib.rs owns the submodule graph.
    let sleigh = PyModule::new_bound(py, "sleigh")?;
    sleigh::register(py, &sleigh)?;
    arch::register(py, &sleigh)?;
    cc::register(py, &sleigh)?;
    m.add_submodule(&sleigh)?;

    let reader = PyModule::new_bound(py, "reader")?;
    reader::register(py, &reader)?;
    m.add_submodule(&reader)?;

    let cfg = PyModule::new_bound(py, "cfg")?;
    cfg::register(py, &cfg)?;
    options::register_cfg(py, &cfg)?;
    m.add_submodule(&cfg)?;

    let ir = PyModule::new_bound(py, "ir")?;
    function::register(py, &ir)?;
    node::register(py, &ir)?;
    m.add_submodule(&ir)?;

    let lift = PyModule::new_bound(py, "lift")?;
    strider_cls::register(py, &lift)?;
    options::register_lift(py, &lift)?;
    m.add_submodule(&lift)?;

    let opt = PyModule::new_bound(py, "opt")?;
    opt::register(py, &opt)?;
    m.add_submodule(&opt)?;

    let pattern = PyModule::new_bound(py, "pattern")?;
    pattern::register(py, &pattern)?;
    matcher::register(py, &pattern)?;
    m.add_submodule(&pattern)?;

    let template = PyModule::new_bound(py, "template")?;
    template::register(py, &template)?;
    m.add_submodule(&template)?;
    Ok(())
}
