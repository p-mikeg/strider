// All three fire on PyO3 0.22 macro expansions, not on our own code, and are
// fixed upstream in 0.23.
#![allow(unsafe_op_in_unsafe_fn)]
#![allow(unexpected_cfgs)]
#![allow(clippy::useless_conversion)]
// `#[pymethods]` receivers must be `&self` / `&mut self`, so the `into_pat`
// finaliser on every pattern builder can't take `self` by value.
#![allow(clippy::wrong_self_convention)]

use pyo3::prelude::*;

mod arch;
mod call_other_abi;
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
mod value_ops;

/// Makes anyhow capture a backtrace at every error site.
fn force_anyhow_backtrace_capture() {
    // Anyhow reads `RUST_LIB_BACKTRACE`, falling back to `RUST_BACKTRACE`.
    // Seeding only when neither is set honours a user's explicit opt-out; seeding
    // only `RUST_LIB_BACKTRACE` leaves panic-time semantics alone.
    if std::env::var_os("RUST_LIB_BACKTRACE").is_none()
        && std::env::var_os("RUST_BACKTRACE").is_none()
    {
        // SAFETY: glibc `setenv` can free the environ block a concurrent
        // `getenv` is reading, handing that reader a dangling pointer.  Module
        // init is serialised against other IMPORTS, not against other threads,
        // so importing strider from a worker of an already-threaded process
        // races anything calling `getenv` there.  Accepted, and avoidable:
        // exporting `RUST_LIB_BACKTRACE` (or `RUST_BACKTRACE`) before the
        // process starts takes this branch out entirely.
        unsafe {
            std::env::set_var("RUST_LIB_BACKTRACE", "1");
        }
    }
}

/// Vendored viz.js (Graphviz-in-Wasm).
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
    // Top-level so the pure-Python facade (`_api.py`) can reach them as
    // `_ext._load_elf_from_segments`.
    m.add_function(pyo3::wrap_pyfunction!(reader::load_elf_from_segments, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(reader::load_elf_from_sections, m)?)?;
    // StriderError is the one cross-cutting symbol kept at the top level.
    errors::register(py, m)?;

    // lib.rs owns the submodule graph: each `register` fn only populates the
    // module it is handed.
    let sleigh = PyModule::new_bound(py, "sleigh")?;
    sleigh::register(py, &sleigh)?;
    arch::register(py, &sleigh)?;
    cc::register(py, &sleigh)?;
    call_other_abi::register(py, &sleigh)?;
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
