// These three fire on PyO3 0.22 macro expansions.
#![allow(unsafe_op_in_unsafe_fn)]
#![allow(unexpected_cfgs)]
#![allow(clippy::useless_conversion)]
// `#[pymethods]` receivers are `&self` / `&mut self`, so `into_pat` can't
// take `self`.
#![allow(clippy::wrong_self_convention)]

use pyo3::prelude::*;

mod arch;
mod bigint;
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

/// Makes anyhow capture a backtrace at every error site, unless the user set
/// `RUST_LIB_BACKTRACE` or `RUST_BACKTRACE`.
fn force_anyhow_backtrace_capture() {
    if std::env::var_os("RUST_LIB_BACKTRACE").is_none()
        && std::env::var_os("RUST_BACKTRACE").is_none()
    {
        // SAFETY: races a concurrent `getenv` if strider is imported from a
        // threaded process; exporting either variable beforehand avoids it.
        unsafe {
            std::env::set_var("RUST_LIB_BACKTRACE", "1");
        }
        // std caches the decision on the first capture; removing the variable
        // keeps it from children.
        let _ = std::backtrace::Backtrace::capture();
        // SAFETY: as above.
        unsafe {
            std::env::remove_var("RUST_LIB_BACKTRACE");
        }
    }
}

/// Vendored viz.js (Graphviz-in-Wasm).
#[pyfunction]
#[pyo3(name = "_viz_standalone_js")]
fn viz_standalone_js() -> &'static str {
    ::dot::VIZ_STANDALONE_JS
}

#[pymodule]
fn _strider(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    force_anyhow_backtrace_capture();
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(pyo3::wrap_pyfunction!(viz_standalone_js, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(reader::load_elf, m)?)?;
    errors::register(py, m)?;

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
