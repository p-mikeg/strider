//! Phase 4 Task 4.0 — reference hand-written PyO3 pattern type
//! demonstrating the exact emission shape Task 4.1's
//! `#[strider_pattern]` proc-macro must produce.
//!
//! See `crates/strider-pattern-macros/EMISSION_SPEC.md` for the
//! canonical attribute order and field-method shape derived from this
//! module.  When this file diverges from the macro output, the macro
//! is at fault — this is the test oracle.
//!
//! ## Design choices (V4 verification + Phase 4 prompt)
//!
//! 1. `#[gen_stub_pyclass]` MUST precede `#[pyclass]`; the proc-macro
//!    walks the inner `#[pyclass]` attribute to discover the Python
//!    name and module.
//! 2. `#[gen_stub_pymethods]` MUST precede `#[pymethods]` for the same
//!    reason.
//! 3. Builder state lives inside `Arc<Mutex<...>>` so each `&self`
//!    Python method can mutate without `&mut self`.  This shape
//!    survives `pyo3-stub-gen`'s typed signature emission unchanged
//!    (the proc-macro reads parameter / return types off the function
//!    signature; the `Mutex` indirection is invisible to it).
//! 4. The v1 mirror in `pattern.rs` uses `RefCell<Option<...>>`.  We
//!    use `Arc<Mutex<...>>` here for the V2 reference because (a) the
//!    macro-generated form must be `Send + Sync`-safe to play nicely
//!    with future `Py<Self>::send` ergonomics PyO3 0.23+ wants, and
//!    (b) the v1 `.when(f: PyObject)` builder already needs `Mutex`-
//!    backed storage (see V4 verification note in the plan) — using
//!    `Mutex` uniformly removes one cross-builder rule the macro would
//!    have to encode.
//!
//! ## Coexistence with v1
//!
//! This module exposes the type as `StackStorePatV2` in
//! `strider.pattern` so the existing `strider.pattern.StackStorePat`
//! (v1 hand-mirror in `pattern.rs`) continues to pass every existing
//! test.  Phase 4 Task 4.2 swaps the v1 type out for a
//! macro-generated one of identical shape.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::pattern::{intern_str, wrap_when, PatLike, PyCapture, PyPat};
use crate::sleigh::PyVnSpace;

/// Inner state of [`PyStackStorePatV2`].  Held inside an `Arc<Mutex<…>>`
/// on the `#[pyclass]` so every Python-facing `&self` method can lock
/// + mutate.  The `Send + Sync` boundary is honoured automatically:
/// `PyVnSpace::inner` is `Copy + Send`, `strider_analyze::pattern::Pat` is already used
/// from multiple threads in the v1 mirror, and `PyObject` (the closure
/// for `.when(f)`) is `Send + Sync` per PyO3's contract.
#[derive(Default)]
struct StackStoreInner {
    offset: Option<i64>,
    offset_any: Option<BTreeSet<i64>>,
    data: Option<strider_analyze::pattern::Pat>,
    space: Option<rsleigh::VnSpace>,
    /// `.when(f)` closure storage.  Held as a raw `PyObject` because
    /// the predicate is invoked via `wrap_when_for_reference` at
    /// finalise time, after the builder has accumulated every other
    /// field.  `None` means no predicate; one predicate per builder
    /// (the v1 mirror exposes `.when` as a method on every typed
    /// builder via `pat_builder_finalise!`, and the macro-generated
    /// form will inherit the same contract).
    when: Option<PyObject>,
    /// `.capture(c)` storage.  Honoured at `finalise()` time by
    /// wrapping the assembled pattern in `Pat::capture(c)`.  The
    /// macro-generated form will emit identical wiring.
    capture: Option<strider_analyze::pattern::Capture>,
}

/// Macro-emission reference for `StackStorePat`.  Hand-authored to
/// pin the exact `#[gen_stub_*]`/`#[pyclass]`/`#[pymethods]` attribute
/// stacking and field-method body shape that Task 4.1's
/// `#[strider_pattern]` proc-macro must replicate.
///
/// Exposed to Python as `strider.pattern.StackStorePatV2` (V2 suffix
/// so the v1 `StackStorePat` continues to pass every existing test
/// during the migration).
#[gen_stub_pyclass]
#[pyclass(name = "StackStorePatV2", module = "strider.pattern")]
pub struct PyStackStorePatV2 {
    inner: Arc<Mutex<StackStoreInner>>,
}

impl PyStackStorePatV2 {
    /// Build the underlying `strider_analyze::pattern::Pat` from the accumulated
    /// builder state.  Called by `.into_pat()` (Python) and by
    /// `PatLike::StackStorePatV2` (the `PatLike` enum the v1 mirror
    /// dispatches on for nested-builder arguments).  Locks the
    /// `Mutex` once and reads every field; non-set fields fall back
    /// to the v1 default (`strider_analyze::pattern::stack_store()` with no
    /// constraints).
    pub(crate) fn finalise(&self) -> strider_analyze::pattern::Pat {
        // PoisonError recovery: lock could only be poisoned if a
        // previous `&self` method panicked mid-mutation, which is
        // unreachable under the current implementations (every
        // method is panic-free up to the `.replace(...)` call).
        // Recover via `into_inner()` for parity with the v1
        // `intern_table` recovery — keeps the type usable even after
        // a future panicking method is added.
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut b = strider_analyze::pattern::stack_store();
        if let Some(s) = guard.space {
            b = b.space(s);
        }
        if let Some(o) = guard.offset {
            b = b.offset(o);
        }
        if let Some(ref set) = guard.offset_any {
            // The v1 builder takes a `Vec<i64>`; collect from the
            // BTreeSet so the canonical-iteration-order matches the
            // sorted form the Python user expects.
            b = b.offset_any(set.iter().copied().collect::<Vec<_>>());
        }
        if let Some(ref p) = guard.data {
            b = b.data(p.clone());
        }
        let mut pat: strider_analyze::pattern::Pat = b.into();
        if let Some(c) = guard.capture {
            use strider_analyze::pattern::IntoPat;
            pat = pat.capture(c);
        }
        if let Some(ref f) = guard.when {
            // Clone the PyObject ref under the GIL — `clone_ref`
            // requires `Python<'_>` per pyo3 0.22.  `wrap_when`
            // takes the closure as an owned `PyObject` and stores it
            // inside the resulting `Pat`'s `when_match` callback.
            let f_clone = Python::with_gil(|py| f.clone_ref(py));
            pat = wrap_when(pat, f_clone);
        }
        pat
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PyStackStorePatV2 {
    /// Construct an empty builder.  All fields default to `None`;
    /// `finalise()` produces the unconstrained `stack_store()` pattern
    /// until a field is set.
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(StackStoreInner::default())),
        }
    }

    /// Match only stack-stores whose SP-relative offset equals `k`.
    /// Returns `self` so calls chain: `p.offset(8).data(...)`.
    fn offset(slf: PyRef<'_, Self>, k: i64) -> PyRef<'_, Self> {
        // PoisonError recovery: parity with `finalise()` above.
        // `replace`ing a `Copy` field cannot panic, so the inner
        // state is consistent on entry even after recovery.
        let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.offset = Some(k);
        drop(guard);
        slf
    }

    /// Match only stack-stores whose offset is in `offsets`.  Empty
    /// set vacuously fails (matches nothing) — mirrors the contract
    /// of `int_const_any_of` and the v1 `.offset_any(...)` method.
    fn offset_any(slf: PyRef<'_, Self>, offsets: BTreeSet<i64>) -> PyRef<'_, Self> {
        let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.offset_any = Some(offsets);
        drop(guard);
        slf
    }

    /// Match only stack-stores whose `data` operand satisfies `p`.
    /// Accepts every `PatLike` variant (str, Capture, Pat, typed
    /// builder).  Fails with `PatternError` if the inner pattern
    /// fails to finalise (e.g. a reserved capture name).
    fn data<'py>(
        slf: PyRef<'py, Self>,
        p: PatLike<'py>,
    ) -> PyResult<PyRef<'py, Self>> {
        let pat = p.into_pat()?;
        let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.data = Some(pat);
        drop(guard);
        Ok(slf)
    }

    /// Match only stack-stores in the given address space.  Spaces
    /// are produced by `VnSpace.ram()` / `VnSpace.register()` /
    /// `VnSpace.from_id(...)`.
    fn space(slf: PyRef<'_, Self>, s: PyVnSpace) -> PyRef<'_, Self> {
        let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.space = Some(s.inner);
        drop(guard);
        slf
    }

    /// Capture the matched stack-store node under the given
    /// [`Capture`].  Mirrors the v1 `pat_builder_finalise!`-emitted
    /// `.capture(c)`.
    fn capture<'py>(
        slf: PyRef<'py, Self>,
        c: PyRef<'py, PyCapture>,
    ) -> PyRef<'py, Self> {
        let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.capture = Some(c.inner);
        drop(guard);
        slf
    }

    /// Capture under a string name (auto-interned).  Reserved names
    /// (`"_"`, `"any_"`) raise `PatternError`.
    fn cap<'py>(
        slf: PyRef<'py, Self>,
        name: &'py str,
    ) -> PyResult<PyRef<'py, Self>> {
        let c = intern_str(name)?;
        let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.capture = Some(c);
        drop(guard);
        Ok(slf)
    }

    /// Attach a Python predicate that runs after the match.
    /// See `PyPat::when` for the full predicate contract; the
    /// predicate receives a `PartialMatch` proxy and returns a bool.
    fn when(slf: PyRef<'_, Self>, f: PyObject) -> PyRef<'_, Self> {
        let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.when = Some(f);
        drop(guard);
        slf
    }

    /// Finalise into a [`Pat`].  Most call sites accept this builder
    /// directly via `PatLike`, so explicit `.into_pat()` is rarely
    /// needed.
    fn into_pat(&self) -> PyPat {
        PyPat::from_pat(self.finalise())
    }

}

/// Free-function constructor mirroring v1's `stack_store(offset, data)`
/// helper.  Demonstrates the `#[gen_stub_pyfunction]` + `#[pyfunction]`
/// stacking that Task 4.1's macro will emit for each top-level
/// constructor (one per pattern).
///
/// **Emission-spec note (V4 verification)**: `#[gen_stub_pyfunction]`
/// at this version of `pyo3-stub-gen` (0.7) cannot translate
/// `#[pyo3(signature = (arg = default))]` annotations because the
/// generated code references `pyo3::IntoPyObjectExt` (added in pyo3
/// 0.23).  Task 4.1's proc-macro must therefore emit the keyword-
/// default handling itself rather than relying on `#[pyo3(signature
/// = ...)]`, or upgrade the pyo3-stub-gen pin to a release that's
/// built against pyo3 0.23+.  Until then, this reference uses
/// `#[pyo3(signature = (offset=None, data=None))]` to silence the
/// pyo3 implicit-defaults deprecation warning AND skips
/// `#[gen_stub_pyfunction]` — the v1 hand-written `pattern.pyi`
/// covers the `stack_store(...)` free-function shape; the V2 method
/// surface on `PyStackStorePatV2` is what this reference proves out.
#[pyfunction]
#[pyo3(signature = (offset=None, data=None))]
pub fn stack_store_v2(
    offset: Option<i64>,
    data: Option<PatLike<'_>>,
) -> PyResult<PyStackStorePatV2> {
    let b = PyStackStorePatV2::new();
    {
        let mut guard = b.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(o) = offset {
            guard.offset = Some(o);
        }
        if let Some(v) = data {
            guard.data = Some(v.into_pat()?);
        }
    }
    Ok(b)
}

/// Register the v2 reference type into the `strider.pattern`
/// submodule.  Called from `strider_analyze::pattern::register` once both v1 and v2
/// shapes have been added.
pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStackStorePatV2>()?;
    m.add_function(pyo3::wrap_pyfunction!(stack_store_v2, m)?)?;
    Ok(())
}

// `pyo3-stub-gen` discovers `inventory::submit!`-emitted entries at
// load time of the rlib.  The `define_stub_info_gatherer!` invocation
// lives in `lib.rs` so a single gather call covers the whole crate.
