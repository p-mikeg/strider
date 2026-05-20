//! Phase 4 Task 4.1 — macro-driven re-emission of the V2 reference
//! type via `#[strider_pattern]`.
//!
//! This file is the **smoking-gun test** for the proc-macro: the
//! generated `StackStorePatV3` `.pyi` MUST be byte-identical (modulo
//! the V2 -> V3 rename) to the hand-written `StackStorePatV2` `.pyi`
//! emitted by `pattern_reference.rs`.
//!
//! If the two stub outputs diverge in any way other than the class
//! name, the macro is violating `EMISSION_SPEC.md` and the fix
//! belongs in `strider-pattern-macros`, not here.
//!
//! ## Why "V3" and not replace V2 directly
//!
//! The hand-written `PyStackStorePatV2` is the *test oracle*.  We
//! intentionally keep it next to its macro-generated twin so the
//! `cargo run --example stub_gen` output contains both side-by-side
//! and a regression in either pyo3-stub-gen or this macro is
//! immediately visible in the diff.  Task 4.2 retires the
//! hand-written V2 by replacing the v1 `StackStorePat` with the
//! macro-emitted form; only then do we collapse to one type.

use std::collections::BTreeSet;

// IMPORTANT: `#[strider_pattern]` emits bare `#[pyclass]` /
// `#[pymethods]` / `#[gen_stub_pyclass]` / `#[gen_stub_pymethods]`
// attributes; pyo3-stub-gen's derive walks attributes by ident, so
// the bare form is required.  Bring the necessary names into scope.
use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use strider_pattern_macros::strider_pattern;

/// Macro-emission reference for `StackStorePat`.  Hand-authored to
/// pin the exact `#[gen_stub_*]`/`#[pyclass]`/`#[pymethods]` attribute
/// stacking and field-method body shape that Task 4.1's
/// `#[strider_pattern]` proc-macro must replicate.
///
/// Exposed to Python as `strider.pattern.StackStorePatV2` (V2 suffix
/// so the v1 `StackStorePat` continues to pass every existing test
/// during the migration).
//
// NOTE: the `///` docstring above is INTENTIONALLY a verbatim copy
// of the V2 reference's struct docstring — the smoking-gun test
// requires the V3 macro-emitted `.pyi` to be byte-identical (modulo
// class-name suffix) to the V2 hand-written one.  Replacing this
// doc with "V3" / "macro-driven" wording would be correct in
// isolation but break the validation, so the `.pyi` lies about
// which form is which.  Fine — Task 4.2 retires V2.
#[strider_pattern(
    rust_name = "PyStackStorePatV3",
    py_name = "StackStorePatV3",
    py_module = "strider.pattern",
    base_builder = "stack_store",
    node_phrase = "stack-store node",
)]
pub struct StackStorePatDef {
    /// Match only stack-stores whose SP-relative offset equals `k`.
    /// Returns `self` so calls chain: `p.offset(8).data(...)`.
    #[field(arg = "k")]
    offset: Option<i64>,

    /// Match only stack-stores whose offset is in `offsets`.  Empty
    /// set vacuously fails (matches nothing) — mirrors the contract
    /// of `int_const_any_of` and the v1 `.offset_any(...)` method.
    #[field(arg = "offsets")]
    offset_any: Option<BTreeSet<i64>>,

    /// Match only stack-stores whose `data` operand satisfies `p`.
    /// Accepts every `PatLike` variant (str, Capture, Pat, typed
    /// builder).  Fails with `PatternError` if the inner pattern
    /// fails to finalise (e.g. a reserved capture name).
    #[field(accepts = "Pat", arg = "p")]
    data: Option<strider_analyze::pattern::Pat>,

    /// Match only stack-stores in the given address space.  Spaces
    /// are produced by `VnSpace.ram()` / `VnSpace.register()` /
    /// `VnSpace.from_id(...)`.
    #[field(accepts = "VnSpace", arg = "s")]
    space: Option<rsleigh::VnSpace>,
}

/// Register the V3 (macro-driven) type into the `strider.pattern`
/// submodule.  Called from `strider_analyze::pattern::register` next to the V2
/// reference registration.
pub(crate) fn register(m: &pyo3::Bound<'_, pyo3::types::PyModule>) -> pyo3::PyResult<()> {
    use pyo3::prelude::*;
    m.add_class::<PyStackStorePatV3>()?;
    Ok(())
}
