//! `strider.pattern` submodule.
//!
//! Wraps the `pattern` crate.  Provides:
//! - `Capture` — opaque capture-variable handle.
//! - `Pat` — opaque wrapped pattern.  Constructed via free functions
//!   (`add`, `load`, `call`, `float_add`, `cast_to_int`, …) and chained
//!   via builder methods (`.addr()`, `.arg()`, `.capture()`, `.cap()`,
//!   `.when()`, `.ordered()`, etc.).
//! - String-keyed captures: any free function that accepts a sub-pattern
//!   also accepts a string; the string is interned to a `Capture` at
//!   the point the outermost pattern is finalized, so back-references
//!   (`add("x", "x")`) work.  The intern table is global per process,
//!   so the same string in the same Python process always resolves to
//!   the same `Capture`.
//!
//! Coverage: every constructor in `strider_analyze::pattern::pat::ctor` plus the typed
//! family dispatchers (`int_binary`, `bool_binary`, `float_binary`),
//! `.when` predicate guards, `.ordered()` overrides, and the
//! variant-agnostic `*_any` constructors that bind the matched op
//! variant to a `Capture` for later inspection via `Match.*_op`
//! (`int_binary_op`, `bool_binary_op`, `float_binary_op`, etc.).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use pyo3::types::{PyString, PyTuple};
// Brought into scope so the `#[strider_pattern]` proc-macro emits bare
// `#[gen_stub_pyclass]` / `#[gen_stub_pymethods]` attributes that
// pyo3-stub-gen can recognise.
#[allow(unused_imports)]
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};
use strider_pattern_macros::strider_pattern;

use crate::errors::into_pattern_err;

// ── Pat-builder finalise macro ───────────────────────────────────────────
//
// Every typed pattern builder (`PyPhiPat`, `PyCallPat`, `PyLoadPat`, …)
// finishes by reading its in-progress builder state into a `strider_analyze::pattern::Pat`
// and wrapping it in a `PyPat`.  The four-method `capture` / `cap` /
// `when` / `into_pat` block is identical at every site (only the
// receiver type differs).
//
// `pat_builder_finalise!(BuilderTy)` emits a separate `#[pymethods] impl
// BuilderTy { … }` block carrying those four methods.  This relies on
// PyO3's `multiple-pymethods` feature so the same `#[pyclass]` can have
// more than one `#[pymethods]` block.  Each builder retains its own
// primary `#[pymethods]` block holding the builder-specific methods
// (`for_vn`, `addr`, `arg`, `at`, …) and only declares
// `pat_builder_finalise!(BuilderTy);` at module scope.

macro_rules! pat_builder_finalise {
    ($BuilderTy:ident) => {
        #[pymethods]
        impl $BuilderTy {
            /// Capture this pattern's matched node under the given
            /// [`Capture`].
            fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
                use strider_analyze::pattern::IntoPat;
                PyPat::from_pat(self.finalise().capture(c.inner))
            }
            /// Capture this pattern under a string name (auto-interned).
            fn cap(&self, name: &str) -> PyResult<PyPat> {
                use strider_analyze::pattern::IntoPat;
                let c = intern_str(name)?;
                Ok(PyPat::from_pat(self.finalise().capture(c)))
            }
            /// Attach a Python predicate that runs after the match.  See
            /// [`PyPat::when`] for the full predicate contract.
            fn when(&self, f: PyObject) -> PyPat {
                PyPat::from_pat(wrap_when(self.finalise(), f))
            }
            /// Finalise into a [`PyPat`].  Most call sites accept a
            /// builder directly via `PatLike`, so explicit `.into_pat()`
            /// is rarely needed.
            fn into_pat(&self) -> PyPat {
                PyPat::from_pat(self.finalise())
            }
        }
    };
}

// ── Capture ──────────────────────────────────────────────────────────────

// `#[gen_stub_pyclass]` derives `PyStubType` for `PyCapture` so the
// macro-emitted reference type's `.capture(c: PyRef<'_, PyCapture>)`
// signature compiles under `#[gen_stub_pymethods]`.  This only adds
// the type-info impl; the existing `#[pymethods]` block below is
// unchanged (no `#[gen_stub_pymethods]` here — the hand-written
// `pattern.pyi` already covers PyCapture's surface).
#[pyo3_stub_gen::derive::gen_stub_pyclass]
#[pyclass(name = "Capture", module = "strider.pattern", frozen)]
#[derive(Clone)]
pub struct PyCapture {
    pub(crate) inner: strider_analyze::pattern::Capture,
}

#[pymethods]
impl PyCapture {
    #[new]
    fn new() -> Self {
        Self {
            inner: strider_analyze::pattern::Capture::new(),
        }
    }

    fn __repr__(&self) -> String {
        format!("Capture({:?})", self.inner)
    }

    fn __hash__(&self) -> isize {
        // The Capture's globally-unique u32 id is the stable hash key.
        // (Earlier this used `format!("{:?}", self.inner).len()` which
        // collapsed every same-decimal-digit-count id to one bucket.)
        // Round through `i64` so 32-bit-isize platforms don't sign-wrap
        // for ids above 2^31; on 64-bit isize the cast is a no-op.
        self.inner.id() as i64 as isize
    }
}

// ── String → Capture interning ───────────────────────────────────────────
//
// The plan calls for per-pattern interning tables, but since each
// finalized PyPat is built up step-by-step from sub-PyPats (each of
// which already interns its strings), we use a *global* interning
// table keyed on string identity per process.  That means
// `add("x", "x")` aliases (same string in the same Python process →
// same Capture) and `add("x", "y")` doesn't.
//
// The reserved names "_" and "any_" raise PatternError when used as
// regular capture strings.

fn intern_table() -> &'static Mutex<HashMap<String, strider_analyze::pattern::Capture>> {
    static TABLE: std::sync::OnceLock<Mutex<HashMap<String, strider_analyze::pattern::Capture>>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn intern_str(name: &str) -> PyResult<strider_analyze::pattern::Capture> {
    if name == "_" || name == "any_" {
        return Err(into_pattern_err(anyhow::anyhow!(
            "{name:?} is reserved (use any_() / var() / _ explicitly)"
        )));
    }
    let mut table = intern_table()
        .lock()
        .map_err(|_| into_pattern_err(anyhow::anyhow!("intern table lock poisoned")))?;
    Ok(*table
        .entry(name.to_string())
        .or_insert_with(strider_analyze::pattern::Capture::new))
}

// ── PyPat ────────────────────────────────────────────────────────────────

/// Opaque wrapper around a `strider_analyze::pattern::Pat`.
///
/// Held inside an `Arc` so PyPat can be cheaply cloned and passed as
/// sub-patterns to multiple builder field methods.
// See PyCapture above for the `#[gen_stub_pyclass]` rationale.
#[pyo3_stub_gen::derive::gen_stub_pyclass]
#[pyclass(name = "Pat", module = "strider.pattern")]
#[derive(Clone)]
pub struct PyPat {
    pub(crate) inner: Arc<strider_analyze::pattern::Pat>,
}

impl PyPat {
    pub(crate) fn from_pat(p: strider_analyze::pattern::Pat) -> Self {
        Self { inner: Arc::new(p) }
    }

    pub(crate) fn as_inner(&self) -> &strider_analyze::pattern::Pat {
        &self.inner
    }
}

/// `CastMask` — bitset selecting which value-passthrough cast
/// `NodeKind`s the matcher walks through transparently.  Mirrors
/// `strider_analyze::pattern::CastMask`.  Construct via the classmethods (`zero_extend`,
/// `sign_extend`, `extend`, `truncate`, `cast_to_int`, `cast_to_bool`,
/// `cast_to_float`, `int_bits_to_float`, `float_bits_to_int`,
/// `all`, `none`/`empty`); combine with `|` (Python `__or__`).
///
/// Pass to `Graph.find_all(pat, ignore_casts_mask=...)` — granular
/// alternative to the all-or-nothing `ignore_casts=True`.
#[pyclass(name = "CastMask", module = "strider.pattern", frozen)]
#[derive(Clone, Copy)]
pub struct PyCastMask {
    pub(crate) inner: strider_analyze::pattern::CastMask,
}

#[pymethods]
impl PyCastMask {
    #[classmethod] fn zero_extend(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: strider_analyze::pattern::CastMask::ZERO_EXTEND }
    }
    #[classmethod] fn sign_extend(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: strider_analyze::pattern::CastMask::SIGN_EXTEND }
    }
    #[classmethod] fn extend(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: strider_analyze::pattern::CastMask::EXTEND }
    }
    #[classmethod] fn truncate(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: strider_analyze::pattern::CastMask::TRUNCATE }
    }
    #[classmethod] fn cast_to_int(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: strider_analyze::pattern::CastMask::CAST_TO_INT }
    }
    #[classmethod] fn cast_to_bool(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: strider_analyze::pattern::CastMask::CAST_TO_BOOL }
    }
    #[classmethod] fn cast_to_float(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: strider_analyze::pattern::CastMask::CAST_TO_FLOAT }
    }
    #[classmethod] fn int_bits_to_float(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: strider_analyze::pattern::CastMask::INT_BITS_TO_FLOAT }
    }
    #[classmethod] fn float_bits_to_int(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: strider_analyze::pattern::CastMask::FLOAT_BITS_TO_INT }
    }
    #[classmethod] fn all(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: strider_analyze::pattern::CastMask::all() }
    }
    #[classmethod] fn none(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: strider_analyze::pattern::CastMask::empty() }
    }
    /// Alias for `none()` — mirrors Rust's `strider_analyze::pattern::CastMask::empty()`.
    #[classmethod] fn empty(cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self::none(cls)
    }

    fn __or__(&self, other: &Self) -> Self {
        Self { inner: self.inner | other.inner }
    }
    fn __and__(&self, other: &Self) -> Self {
        Self { inner: self.inner & other.inner }
    }
    fn __eq__(&self, other: &Self) -> bool { self.inner == other.inner }
    fn __hash__(&self) -> u64 { self.inner.bits() as u64 }
    fn bits(&self) -> u32 { self.inner.bits() }

    fn __repr__(&self) -> String {
        format!("CastMask(0b{:08b})", self.inner.bits())
    }
}

/// Polymorphic input for builder field methods and `Graph.find_all`.
/// Accepts a `Pat`, a `Capture`, a string (which interns to a
/// Capture), or any of the typed builders that finalise to a `Pat`.
/// Adding a typed builder variant here lets users pass the
/// un-finalised builder directly into field setters and query
/// methods without a manual `.into_pat()` call.
#[derive(FromPyObject)]
pub enum PatLike<'py> {
    Pat(Bound<'py, PyPat>),
    Capture(Bound<'py, PyCapture>),
    Str(Bound<'py, PyString>),
    CallPat(Bound<'py, PyCallPat>),
    CallOtherPat(Bound<'py, PyCallOtherPat>),
    RetPat(Bound<'py, PyRetPat>),
    IfPat(Bound<'py, PyIfPat>),
    LoadPat(Bound<'py, PyLoadPat>),
    StorePat(Bound<'py, PyStorePat>),
    StackStorePat(Bound<'py, PyStackStorePat>),
    StackStorePhiPat(Bound<'py, PyStackStorePhiPat>),
    PhiPat(Bound<'py, PyPhiPat>),
    MemPhiPat(Bound<'py, PyMemPhiPat>),
    ValuePhiPat(Bound<'py, PyValuePhiPat>),
    FunctionArgPat(Bound<'py, PyFunctionArgPat>),
    IntBinaryPat(Bound<'py, PyIntBinaryPat>),
    BoolBinaryPat(Bound<'py, PyBoolBinaryPat>),
    FloatBinaryPat(Bound<'py, PyFloatBinaryPat>),
}

// Manual `PyStubType` impl so `pyo3-stub-gen`'s proc-macros translate
// `PatLike` parameters to the canonical `PatLike` Python type alias
// defined by hand in `strider/pattern.pyi` (line 34: `PatLike =
// Union[str, Capture, Pat, ...]`).  Without this impl, the type would
// be elided as `Any` in the generated stub and fail `mypy --strict`
// on callers that pass a typed builder directly.
//
// EMISSION_SPEC implication: when the proc-macro encounters a
// `PatLike<'_>` argument, it must NOT attempt to auto-derive the
// PyStubType; the macro-generated emission relies on this hand-written
// impl.
impl pyo3_stub_gen::PyStubType for PatLike<'_> {
    fn type_output() -> pyo3_stub_gen::TypeInfo {
        // Resolve to `strider.pattern.PatLike`, the typed Union alias
        // already declared in the hand-written `pattern.pyi`.  Using
        // `with_module` rather than `unqualified` so mypy can find
        // the alias when the consumer module imports `strider.pattern`.
        pyo3_stub_gen::TypeInfo::with_module("strider.pattern.PatLike", "strider.pattern".into())
    }
}

impl PatLike<'_> {
    pub(crate) fn into_pat(self) -> PyResult<strider_analyze::pattern::Pat> {
        match self {
            PatLike::Pat(p) => Ok((*p.borrow().inner).clone()),
            PatLike::Capture(c) => Ok(strider_analyze::pattern::var(c.borrow().inner)),
            PatLike::Str(s) => {
                let name_owned = s.to_string();
                let name = name_owned.as_str();
                if name == "_" || name == "any_" {
                    Ok(strider_analyze::pattern::any())
                } else {
                    let c = intern_str(name)?;
                    Ok(strider_analyze::pattern::var(c))
                }
            }
            PatLike::CallPat(b) => Ok(b.borrow().finalise()),
            PatLike::CallOtherPat(b) => Ok(b.borrow().finalise()),
            PatLike::RetPat(b) => Ok(b.borrow().finalise()),
            PatLike::IfPat(b) => Ok(b.borrow().finalise()),
            PatLike::LoadPat(b) => Ok(b.borrow().finalise()),
            PatLike::StorePat(b) => Ok(b.borrow().finalise()),
            PatLike::StackStorePat(b) => Ok(b.borrow().finalise()),
            PatLike::StackStorePhiPat(b) => Ok(b.borrow().finalise()),
            PatLike::PhiPat(b) => Ok(b.borrow().finalise()),
            PatLike::MemPhiPat(b) => Ok(b.borrow().finalise()),
            PatLike::ValuePhiPat(b) => Ok(b.borrow().finalise()),
            PatLike::FunctionArgPat(b) => Ok(b.borrow().finalise()),
            PatLike::IntBinaryPat(b) => Ok(b.borrow().finalise()),
            PatLike::BoolBinaryPat(b) => Ok(b.borrow().finalise()),
            PatLike::FloatBinaryPat(b) => Ok(b.borrow().finalise()),
        }
    }
}

// ── PyPartialMatch — proxy passed to .when predicates ────────────────────
//
// Holds a clone of the matcher's current Bindings + a raw pointer to the
// graph.  The pointer is set just before the Python predicate is called
// and cleared (via `clear_graph_ptr`) right after, so any attempt to use
// the proxy outside the predicate's call returns None / False instead of
// dereferencing a dangling pointer.
//
// Bindings is `Clone + Default`, and clones are cheap (small Vec).

#[pyclass(name = "PartialMatch", module = "strider.pattern", unsendable)]
pub struct PyPartialMatch {
    bindings: strider_analyze::pattern::Bindings,
    /// Raw pointer to the graph the matcher is operating on.  Wrapped
    /// in `Mutex<Option<...>>`: `Mutex` so PyO3's `&self`-only access
    /// from Python can still mutate (clear) the slot, and `Option` so
    /// the wrapper can replace it with `None` on predicate exit and any
    /// subsequent Python access from a leaked proxy returns `None`
    /// instead of dereferencing a dangling pointer.  PyPartialMatch is
    /// `unsendable`, so no `Arc` is needed — the proxy never crosses
    /// threads.
    graph_ptr: Mutex<Option<*const strider_ir::Graph>>,
}

// SAFETY: We never share PyPartialMatch across threads (`unsendable`).
// The `*const BuiltFunctionGraph` it holds is only valid for the
// duration of one synchronous predicate call, after which it's cleared.
// The Mutex guards against re-entrant access from a Python callback
// that re-enters Rust.

impl PyPartialMatch {
    fn new(bindings: strider_analyze::pattern::Bindings, graph: &strider_ir::Graph) -> Self {
        Self {
            bindings,
            graph_ptr: Mutex::new(Some(graph as *const _)),
        }
    }

    fn clear_graph_ptr(&self) {
        // On a poisoned mutex we still need to clear the pointer —
        // leaving a stale `Some(*const _)` in the proxy would let a
        // delayed Python reference dereference freed memory.  Recover
        // by taking the inner value via PoisonError; the *correctness*
        // is identical to the unpoisoned path.
        let mut g = self
            .graph_ptr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *g = None;
    }

    /// Borrow the graph pointer for a closure call.  Returns `None` if
    /// the proxy has been invalidated.
    ///
    /// Recovers from `Mutex` poisoning via `into_inner()` — the inner is
    /// `Option<*const strider_ir::Graph>` (a single Copy slot), and only ever
    /// written by `clear_graph_ptr` (atomic `*g = None`) or the matcher
    /// pointer-set (atomic `*g = Some(p)`).  Neither can panic after
    /// partial mutation, so the slot is consistent on entry.  Matches
    /// the existing recovery in [`Self::clear_graph_ptr`].
    ///
    /// Caller contract: `f` MUST NOT call back into Python code that
    /// re-invokes `with_graph` on the same proxy.  Doing so would
    /// re-lock the same `Mutex` and deadlock (`std::sync::Mutex` is
    /// non-reentrant).  Current callers (`bindings.get_uint`, etc.) are
    /// pure-Rust accessors so the constraint is honoured trivially.
    fn with_graph<R>(&self, f: impl FnOnce(&strider_ir::Graph) -> R) -> Option<R> {
        let guard = self.graph_ptr.lock().unwrap_or_else(|p| p.into_inner());
        let ptr = (*guard)?;
        // SAFETY: `ptr` was set to a valid `&BuiltFunctionGraph` by the
        // matcher and only cleared after the predicate returns.  The
        // outer Mutex guard prevents the cleanup from racing this call.
        let graph_ref = unsafe { &*ptr };
        Some(f(graph_ref))
    }

    fn capture_from_key(&self, key: &CaptureKeyOwned) -> PyResult<strider_analyze::pattern::Capture> {
        match key {
            CaptureKeyOwned::Capture(c) => Ok(*c),
            CaptureKeyOwned::Str(s) => intern_str(s.as_str()),
        }
    }
}

/// Owned variant of CaptureKey (no `Bound` lifetime), used by
/// PyPartialMatch's accessors which can't borrow from the Python args.
enum CaptureKeyOwned {
    Capture(strider_analyze::pattern::Capture),
    Str(String),
}

impl<'py> FromPyObject<'py> for CaptureKeyOwned {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(c) = ob.extract::<PyRef<'_, PyCapture>>() {
            return Ok(CaptureKeyOwned::Capture(c.inner));
        }
        if let Ok(s) = ob.extract::<String>() {
            return Ok(CaptureKeyOwned::Str(s));
        }
        Err(pyo3::exceptions::PyTypeError::new_err(
            "expected Capture or str",
        ))
    }
}

#[pymethods]
impl PyPartialMatch {
    fn uint(&self, key: CaptureKeyOwned) -> PyResult<Option<u128>> {
        let cap = self.capture_from_key(&key)?;
        Ok(self.with_graph(|g| self.bindings.get_uint(cap, g)).flatten())
    }

    #[pyo3(name = "int")]
    fn int_(&self, key: CaptureKeyOwned) -> PyResult<Option<i128>> {
        let cap = self.capture_from_key(&key)?;
        Ok(self.with_graph(|g| self.bindings.get_int(cap, g)).flatten())
    }

    #[pyo3(name = "bool")]
    fn bool_(&self, key: CaptureKeyOwned) -> PyResult<Option<bool>> {
        let cap = self.capture_from_key(&key)?;
        Ok(self.with_graph(|g| self.bindings.get_bool(cap, g)).flatten())
    }

    fn float_bits(&self, key: CaptureKeyOwned) -> PyResult<Option<u64>> {
        let cap = self.capture_from_key(&key)?;
        Ok(self
            .with_graph(|g| self.bindings.get_float_bits(cap, g))
            .flatten())
    }

    fn has(&self, key: CaptureKeyOwned) -> PyResult<bool> {
        let cap = self.capture_from_key(&key)?;
        Ok(self.bindings.get_node(cap).is_some())
    }

    fn __getitem__(&self, py: Python<'_>, key: CaptureKeyOwned) -> PyResult<PyObject> {
        let cap = self.capture_from_key(&key)?;
        if let Some(Some(v)) = self.with_graph(|g| self.bindings.get_uint(cap, g)) {
            // Pass `u128` directly — see `crates/strider-py/src/matcher.rs`
            // PyMatch::__getitem__ for why a `as i128` cast was wrong.
            return Ok(v.into_py(py));
        }
        if let Some(Some(b)) = self.with_graph(|g| self.bindings.get_bool(cap, g)) {
            return Ok(b.into_py(py));
        }
        if let Some(Some(f)) = self.with_graph(|g| self.bindings.get_float_bits(cap, g)) {
            return Ok(f.into_py(py));
        }
        Ok(py.None())
    }

    fn __contains__(&self, key: CaptureKeyOwned) -> PyResult<bool> {
        self.has(key)
    }
}

/// Build a `Pat::when_match` closure that calls a Python predicate with
/// a transient `PyPartialMatch` proxy.  Most failure modes (proxy alloc
/// failure, non-bool return, ordinary predicate exceptions) are
/// surfaced to stderr and treated as `false` (no match) — aborting
/// `find_all` mid-walk on a buggy predicate would be worse than
/// continuing.
///
/// `KeyboardInterrupt` and `SystemExit` are
/// **re-raised** rather than swallowed, so Ctrl-C in an interactive
/// Python session can interrupt a slow `find_all` walk that's stuck
/// inside a predicate.  Re-raising via `PyErr::restore` defers the
/// exception to the next GIL re-entry point, which the matcher's
/// shallow loop re-checks naturally.
pub(crate) fn wrap_when(inner: strider_analyze::pattern::Pat, py_func: PyObject) -> strider_analyze::pattern::Pat {
    inner.when_match(move |graph, _ty, bindings| {
        Python::with_gil(|py| {
            let proxy = PyPartialMatch::new(bindings.clone(), graph);
            let py_proxy = match Py::new(py, proxy) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("strider: .when() predicate proxy alloc failed: {e}");
                    return false;
                }
            };
            let args = PyTuple::new_bound(py, [py_proxy.clone_ref(py)]);
            let result = py_func.call_bound(py, args, None);
            // Always invalidate the proxy's graph pointer so any
            // subsequent use from Python doesn't deref a stale ptr.
            //
            // use `borrow` (panicking
            // on conflict) instead of `try_borrow` + silent skip.
            // `try_borrow` only fails when an active `&mut self`
            // borrow is held; `PyPartialMatch` exposes only `&self`
            // methods via #[pymethods] AND is `unsendable`, so that
            // failure mode is unreachable from any synchronous path.
            // Using `borrow` makes the unreachability explicit — a
            // future change that adds a `&mut self` method on the
            // proxy would surface as a clean panic instead of
            // silently leaking the pointer past the predicate's
            // return.
            py_proxy.borrow(py).clear_graph_ptr();
            match result {
                Ok(obj) => match obj.extract::<bool>(py) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!(
                            "strider: .when() predicate returned non-bool ({e}); treating as no-match"
                        );
                        false
                    }
                },
                Err(e) => {
                    // control-flow exceptions
                    // (KeyboardInterrupt, SystemExit) must propagate
                    // — Ctrl-C in an interactive session must be able
                    // to interrupt a slow find_all walk.  PyErr::restore
                    // sets the active exception state; the next time
                    // Python regains control (typically the next pyo3
                    // boundary in the matcher), it's re-raised.
                    if e.is_instance_of::<pyo3::exceptions::PyKeyboardInterrupt>(py)
                        || e.is_instance_of::<pyo3::exceptions::PySystemExit>(py)
                    {
                        e.restore(py);
                    } else {
                        // Ordinary predicate bug: surface to stderr,
                        // treat as no-match so `find_all` continues.
                        e.print(py);
                    }
                    false
                }
            }
        })
    })
}

#[pymethods]
impl PyPat {
    /// Capture this pattern's matched node.
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use strider_analyze::pattern::IntoPat;
        let inner = (*self.inner).clone();
        PyPat::from_pat(inner.capture(c.inner))
    }

    /// Capture this pattern under a string name (auto-interned).
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use strider_analyze::pattern::IntoPat;
        let c = intern_str(name)?;
        let inner = (*self.inner).clone();
        Ok(PyPat::from_pat(inner.capture(c)))
    }

    /// Attach a Python predicate that runs after this pattern matches.
    /// The predicate receives a `PartialMatch` proxy with `uint`/`int`/
    /// `bool`/`float_bits`/`has`/`__getitem__`/`__contains__` accessors
    /// over the captures bound so far.  Returning `False` (or raising)
    /// fails the match.
    fn when(&self, f: PyObject) -> PyPat {
        let inner = (*self.inner).clone();
        PyPat::from_pat(wrap_when(inner, f))
    }

    /// Force commutative binary ops not to try the swapped operand order.
    ///
    /// **`.ordered()` is only valid on a typed builder** — `int_binary(op,
    /// l, r).ordered()`, `bool_binary(op, l, r).ordered()`, or
    /// `float_binary(op, l, r).ordered()`.  Once a free constructor like
    /// `add(l, r)` returns a finalized `Pat`, the `InputsSpec` (and
    /// therefore commutativity) is baked in.  Calling `.ordered()` on a
    /// finalized `Pat` previously silently returned `self` — a trap that
    /// fooled users into thinking they had disabled commutativity.  This
    /// method now raises [`PatternError`] so the misuse is visible.
    fn ordered(&self) -> PyResult<PyPat> {
        Err(into_pattern_err(anyhow::anyhow!(
            "Pat.ordered() has no effect on a finalized Pat — \
             use int_binary(op, l, r).ordered() / bool_binary(op, l, r).ordered() / \
             float_binary(op, l, r).ordered() to force left-to-right matching"
        )))
    }

    fn __repr__(&self) -> String {
        "Pat(...)".to_string()
    }
}

// ── Free constructors ────────────────────────────────────────────────────

#[pyfunction]
pub fn any_() -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::any())
}

#[pyfunction]
pub fn var(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::var(c.inner))
}

/// Match an `IntConst` whose stored value, masked to the node's
/// output width, equals `value` (interpreted as a signed i128 and
/// reinterpreted as u128 for bit-pattern equality).
///
/// Negative `value` works for the *common* case where the lifter
/// stores the sign-extended form at the output width — e.g. a
/// 32-bit `IntConst(-50)` stored as `0xFFFFFFCE`.  But on x86-64
/// (and similar) gcc -O2 emits 32-bit ops with zero-extended
/// 64-bit results, so the same source `-50` lands as
/// `IntConst(0x00000000FFFFFFCE)` at U64 — which `int_const(-50)`
/// reads as the unsigned value `+4294967246` and **does not match**.
///
/// Use `signed_int_const(value)` when you want to recognise the
/// source-level signed value across both sign- and zero-extended
/// forms.
#[pyfunction]
pub fn int_const(value: i128) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::int_const(value))
}

/// Match an `IntConst` whose stored value, interpreted as a signed
/// integer at *some* natural width ≤ the output width, equals
/// `value`.  Strictly more permissive than [`int_const`]: also
/// matches the zero-extended form of a narrower signed value, which
/// is what compilers emit when a 32-bit signed result feeds a
/// 64-bit register.
///
/// **Use case** — `add(x, signed_int_const(-1))` for `x--`,
/// `mul(x, signed_int_const(-50))` for source-level negative
/// constants.  Where bit-pattern equality at the exact output
/// width matters (low-level rewrites that depend on storage
/// shape), prefer the strict [`int_const`].
#[pyfunction]
pub fn signed_int_const(value: i128) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::signed_int_const(value))
}

/// Match an `IntConst` whose stored value (masked to its declared
/// width) equals any value in `values` (also masked to the same
/// width).  Set-membership variant of `int_const` — useful when the
/// same query should fire on multiple known constants, e.g. several
/// candidate call targets when querying with
/// `call().target(int_const_any_of([...]))`.
///
/// An empty `values` list vacuously fails (matches nothing).
///
/// Match-only — no build-side semantics, so this pattern cannot
/// appear on the RHS of a rewrite rule.
#[pyfunction]
pub fn int_const_any_of(values: Vec<i128>) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::int_const_any_of(values))
}

#[pyfunction]
pub fn bool_const(value: bool) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::bool_const(value))
}

#[pyfunction]
pub fn float_const(bits: u64) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::float_const(bits))
}

#[pyfunction]
pub fn any_int_const(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::any_int_const(c.inner))
}

#[pyfunction]
pub fn any_bool_const(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::any_bool_const(c.inner))
}

#[pyfunction]
pub fn any_float_const(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::any_float_const(c.inner))
}

#[pyfunction]
pub fn initial_var() -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::initial_var())
}

/// Match `InitialVar(vn)` for a specific varnode.  Use the
/// `Sleigh.reg("RAX")` / `Vn(...)` helpers in the `strider` module
/// to construct the `Vn`.
#[pyfunction]
pub fn initial_var_for(vn: crate::sleigh::PyVn) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::initial_var_for(vn.inner))
}

// ── PhiPat ───────────────────────────────────────────────────────────

/// Typed builder for `VarPhi` patterns.  Chain `.for_vn(vn)` to
/// constrain the matched VarPhi to a specific varnode, and
/// `.input(idx, p)` to constrain the value arriving from the given
/// predecessor slot.
#[strider_pattern(
    rust_name = "PyPhiPat",
    py_name = "PhiPat",
    py_module = "strider.pattern",
    base_builder = "phi",
    node_phrase = "phi node",
)]
pub struct PhiPatDef {
    /// Restrict the match to phi nodes for varnode `vn`.
    //
    // The underlying `strider_analyze::pattern::PhiPat` exposes this as `for_vn(Vn)`,
    // not as a `phi_for(vn)` constructor — the macro's
    // `accepts = "Vn"` path emits `b.for_vn(v)` exactly.
    #[field(accepts = "Vn", arg = "vn")]
    for_vn: Option<rsleigh::Vn>,

    /// Constrain the value arriving from predecessor slot `idx`
    /// (0-based; the builder shifts onto raw input slot `idx + 1` to
    /// skip the phi-token edge from the owning `ControlState`).
    #[field(multi, accepts = "Pat", arg = "idx")]
    input: Option<Vec<(usize, strider_analyze::pattern::Pat)>>,
}

#[pyfunction]
pub fn phi() -> PyPhiPat { PyPhiPat::new() }

/// Match `VarPhi` for a specific varnode.  Equivalent to
/// `phi().for_vn(vn)` but reads more naturally at the call site.
#[pyfunction]
pub fn phi_for(vn: crate::sleigh::PyVn) -> PyPhiPat {
    let b = PyPhiPat::new();
    {
        let mut guard = b.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.for_vn = Some(vn.inner);
    }
    b
}

/// Builder for `MemPhi` patterns.  No varnode payload (memory-token
/// phis don't carry one); chain `.input(idx, p)` to constrain the
/// memory-input from predecessor `idx`.
#[strider_pattern(
    rust_name = "PyMemPhiPat",
    py_name = "MemPhiPat",
    py_module = "strider.pattern",
    base_builder = "mem_phi",
    node_phrase = "mem-phi node",
)]
pub struct MemPhiPatDef {
    /// Constrain the memory token arriving from predecessor slot
    /// `idx` (the builder shifts onto raw input `idx + 1`).
    #[field(multi, accepts = "Pat", arg = "idx")]
    input: Option<Vec<(usize, strider_analyze::pattern::Pat)>>,
}

#[pyfunction]
pub fn mem_phi() -> PyMemPhiPat { PyMemPhiPat::new() }

/// Builder for `ValuePhi` patterns.  ValuePhi is synthesised by
/// `StackLoadForward` to phi together stack-store values across a
/// control-flow join.
#[strider_pattern(
    rust_name = "PyValuePhiPat",
    py_name = "ValuePhiPat",
    py_module = "strider.pattern",
    base_builder = "value_phi",
    node_phrase = "value-phi node",
)]
pub struct ValuePhiPatDef {
    /// Constrain the value arriving from predecessor slot `idx`.
    #[field(multi, accepts = "Pat", arg = "idx")]
    input: Option<Vec<(usize, strider_analyze::pattern::Pat)>>,
}

#[pyfunction]
pub fn value_phi() -> PyValuePhiPat { PyValuePhiPat::new() }

// ── FunctionArgPat ───────────────────────────────────────────────────

/// Typed builder for `FunctionArg` node patterns.  Chain
/// `.index(i)` to constrain the argument position and
/// `.source_register(vn)` / `.source_stack(space, offset)` to
/// constrain where the argument was sourced from (matches the
/// `FunctionArgSource::Register` / `FunctionArgSource::Stack`
/// variants of the IR enum).
//
// Intentionally hand-written, not migrated to
// `#[strider_pattern]`.  Reason: `.source_register(vn)` and
// `.source_stack(space, offset)` are two Python methods that write
// the SAME underlying `Option<FunctionArgSource>` field via
// different enum variants.  The macro's current shape is one
// `Option<T>` per field with one setter per field; an enum-dispatch
// extension (call it `#[field(enum_dispatch = "FunctionArgSource")]`)
// would need to track multiple Python method names that all write
// to the same underlying state, which is out of scope for the
// current `Option<T>`-per-field design.  Adding it would gain ~30
// LOC at the call site versus a chunky proc-macro change.
#[pyclass(name = "FunctionArgPat", module = "strider.pattern")]
pub struct PyFunctionArgPat {
    source: std::cell::RefCell<Option<strider_ir::node::FunctionArgSource>>,
    index: std::cell::RefCell<Option<u32>>,
}

impl PyFunctionArgPat {
    fn new() -> Self {
        Self {
            source: std::cell::RefCell::new(None),
            index: std::cell::RefCell::new(None),
        }
    }
    pub(crate) fn finalise(&self) -> strider_analyze::pattern::Pat {
        let mut b = strider_analyze::pattern::function_arg_any();
        if let Some(s) = *self.source.borrow() { b = b.source(s); }
        if let Some(i) = *self.index.borrow() { b = b.index(i); }
        b.into()
    }
}

#[pymethods]
impl PyFunctionArgPat {
    fn index(slf: Py<Self>, py: Python<'_>, i: u32) -> Py<Self> {
        slf.borrow(py).index.replace(Some(i)); slf
    }
    fn source_register(slf: Py<Self>, py: Python<'_>, vn: crate::sleigh::PyVn) -> Py<Self> {
        slf.borrow(py).source.replace(Some(strider_ir::node::FunctionArgSource::Register(vn.inner))); slf
    }
    fn source_stack(slf: Py<Self>, py: Python<'_>, space: crate::sleigh::PyVnSpace, offset: i64) -> Py<Self> {
        slf.borrow(py).source.replace(Some(strider_ir::node::FunctionArgSource::Stack {
            space: space.inner,
            offset,
        }));
        slf
    }
}

#[pyfunction]
pub fn function_arg(i: u32) -> PyFunctionArgPat {
    let b = PyFunctionArgPat::new();
    b.index.replace(Some(i));
    b
}

#[pyfunction]
pub fn function_arg_any() -> PyFunctionArgPat {
    PyFunctionArgPat::new()
}

/// Match a `FunctionArg` whose source is a specific register.
#[pyfunction]
pub fn function_arg_reg(vn: crate::sleigh::PyVn) -> PyFunctionArgPat {
    let b = PyFunctionArgPat::new();
    b.source.replace(Some(strider_ir::node::FunctionArgSource::Register(vn.inner)));
    b
}

/// Match a `FunctionArg` whose source is a specific stack slot.
#[pyfunction]
pub fn function_arg_stack(space: crate::sleigh::PyVnSpace, offset: i64) -> PyFunctionArgPat {
    let b = PyFunctionArgPat::new();
    b.source.replace(Some(strider_ir::node::FunctionArgSource::Stack { space: space.inner, offset }));
    b
}

#[pyfunction]
pub fn predicate(f: PyObject) -> PyPat {
    PyPat::from_pat(wrap_when(strider_analyze::pattern::any(), f))
}

// ── Binary integer ops ───────────────────────────────────────────────────

macro_rules! int_binop {
    ($name:ident) => {
        #[pyfunction]
        pub fn $name(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
            let lp = l.into_pat()?;
            let rp = r.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$name(lp, rp).into()))
        }
    };
}

int_binop!(add);
int_binop!(sub);
int_binop!(mul);
int_binop!(div);
int_binop!(sdiv);
int_binop!(rem);
int_binop!(srem);
int_binop!(shl);
int_binop!(shr);
int_binop!(sshr);
// `and` / `or` are Python keywords; expose as `and_` / `or_`.
#[pyfunction(name = "and_")]
pub fn and_(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::and(lp, rp).into()))
}
#[pyfunction(name = "or_")]
pub fn or_(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::or(lp, rp).into()))
}
int_binop!(xor);
int_binop!(int_eq);
int_binop!(int_lt);
int_binop!(int_le);
int_binop!(int_slt);
int_binop!(int_sle);
int_binop!(int_carry);
int_binop!(int_scarry);
int_binop!(int_sborrow);

/// Match a specific `IntCmpOp` variant.  Op names: "Equal",
/// "Less" / "lt", "LessEqual" / "le", "Sless" / "slt",
/// "SlessEqual" / "sle", "Carry", "Scarry", "Sborrow".  Pair with
/// `var(c)` / `int_const(K)` operands when you need a specific
/// shape.  Note: there is no `IntNotEqual` variant — the lifter
/// lowers `p-code INT_NOTEQUAL` to `BoolNeg(IntEqual)`, so to match
/// `a != b` use `bool_not(int_cmp("Equal", a, b))`.
#[pyfunction]
pub fn int_cmp(op: &str, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let cmp_op = parse_int_cmp_op(op)?;
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::int_cmp(cmp_op, lp, rp)))
}

fn parse_int_cmp_op(name: &str) -> PyResult<strider_ir::IntCmpOp> {
    use strider_ir::IntCmpOp::*;
    // `LessEqual` / `SlessEqual` are deliberately absent: the IR has no
    // such primitives.  Python callers wanting `a <= b` must use
    // `pattern.int_le(a, b)` (or `pattern.int_sle` for signed), which
    // construct the lowered `BoolNeg(IntLess(b, a))` shape.
    Ok(match name {
        "Equal" | "eq" | "equal" => Equal,
        "Less" | "lt" | "less" => Less,
        "Sless" | "slt" | "sless" => Sless,
        "Carry" | "carry" => Carry,
        "Scarry" | "scarry" => Scarry,
        "Sborrow" | "sborrow" => Sborrow,
        other => {
            return Err(into_pattern_err(anyhow::anyhow!(
                "unknown IntCmpOp variant {other:?}"
            )))
        }
    })
}

// ── Integer unary ops ────────────────────────────────────────────────────

macro_rules! int_unop {
    ($name:ident) => {
        #[pyfunction]
        pub fn $name(operand: PatLike<'_>) -> PyResult<PyPat> {
            let op = operand.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$name(op)))
        }
    };
}

// `pattern.neg(x)` matches two's-complement negation (`-x`).
int_unop!(neg);
// `pattern.bit_not(x)` matches bitwise complement (`~x`).
int_unop!(bit_not);
// `pattern.not_(x)` is the keyword-collision-renamed alias for
// `bit_not` — the Rust pattern crate keeps `not` since it's not a Rust
// keyword, but `not` is a Python keyword so the Python surface uses
// `not_` (matching the `and_` / `or_` convention above).
#[pyfunction(name = "not_")]
pub fn not_(operand: PatLike<'_>) -> PyResult<PyPat> {
    let op = operand.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::bit_not(op)))
}

// ── Bool binary ops ──────────────────────────────────────────────────────

macro_rules! bool_binop {
    ($name:ident) => {
        #[pyfunction]
        pub fn $name(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
            let lp = l.into_pat()?;
            let rp = r.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$name(lp, rp).into()))
        }
    };
}

bool_binop!(bool_and);
bool_binop!(bool_or);
bool_binop!(bool_xor);

// ── Bool unary ops ───────────────────────────────────────────────────────

#[pyfunction]
pub fn bool_not(operand: PatLike<'_>) -> PyResult<PyPat> {
    let op = operand.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::bool_not(op)))
}

// ── Float binary ops ─────────────────────────────────────────────────────

macro_rules! float_binop {
    ($name:ident) => {
        #[pyfunction]
        pub fn $name(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
            let lp = l.into_pat()?;
            let rp = r.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$name(lp, rp).into()))
        }
    };
}

float_binop!(float_add);
float_binop!(float_sub);
float_binop!(float_mul);
float_binop!(float_div);

// ── Float unary ops ──────────────────────────────────────────────────────

macro_rules! float_unop {
    ($name:ident) => {
        #[pyfunction]
        pub fn $name(operand: PatLike<'_>) -> PyResult<PyPat> {
            let op = operand.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$name(op)))
        }
    };
}

float_unop!(float_neg);
float_unop!(float_abs);
float_unop!(float_sqrt);
float_unop!(float_ceil);
float_unop!(float_floor);
float_unop!(float_round);

// `float_is_nan(x)` is implemented as the IEEE 754 self-inequality
// `x != x` — the only value that is not equal to itself is NaN.  The
// pcode lifter lowers `FloatNan` to exactly this shape at lift time
// (see `pcode-lift/src/value/float.rs:78-90`), so this constructor
// matches the IR shape produced by Sleigh's FLOAT_NAN op as well as
// any explicit `x != x` written by the source.
//
// `Pat` is `Arc`-backed; cloning `op` is O(1).
#[pyfunction]
pub fn float_is_nan(operand: PatLike<'_>) -> PyResult<PyPat> {
    let op = operand.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::float_ne(op.clone(), op)))
}

// ── Float comparisons ────────────────────────────────────────────────────

macro_rules! float_cmp_op {
    ($name:ident) => {
        #[pyfunction]
        pub fn $name(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
            let lp = l.into_pat()?;
            let rp = r.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$name(lp, rp)))
        }
    };
}

float_cmp_op!(float_eq);
float_cmp_op!(float_ne);
float_cmp_op!(float_lt);
float_cmp_op!(float_le);

// ── Float / int conversions ──────────────────────────────────────────────

macro_rules! conv_op {
    ($name:ident) => {
        #[pyfunction]
        pub fn $name(operand: PatLike<'_>) -> PyResult<PyPat> {
            let op = operand.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$name(op)))
        }
    };
}

conv_op!(int_to_float);
conv_op!(float_to_int);
conv_op!(float_to_float);
conv_op!(int_bits_to_float);
conv_op!(float_bits_to_int);

// ── Cast / coercion / width ops ──────────────────────────────────────────

conv_op!(cast_to_int);
conv_op!(cast_to_bool);
conv_op!(cast_to_float);
conv_op!(truncate);
conv_op!(popcount);
conv_op!(lzcount);
conv_op!(zero_extend);
conv_op!(sign_extend);

/// `extend(op, operand)` where `op` is "zero" / "zero_extend" / "sign" /
/// "sign_extend".
#[pyfunction]
pub fn extend(op: &str, operand: PatLike<'_>) -> PyResult<PyPat> {
    let extend_op = match op {
        "zero" | "zero_extend" | "ZeroExtend" => strider_ir::ExtendOp::ZeroExtend,
        "sign" | "sign_extend" | "SignExtend" => strider_ir::ExtendOp::SignExtend,
        other => {
            return Err(into_pattern_err(anyhow::anyhow!(
                "unknown extend op {other:?} (expected 'zero' or 'sign')"
            )))
        }
    };
    let p = operand.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::extend(extend_op, p)))
}

// ── Memory ───────────────────────────────────────────────────────────────

/// Typed builder for `Load` node patterns.  Chain `.addr(p)` to
/// constrain the address operand and `.space(s)` to restrict the
/// match to a specific memory space (e.g. `VnSpace.ram()`).
#[strider_pattern(
    rust_name = "PyLoadPat",
    py_name = "LoadPat",
    py_module = "strider.pattern",
    base_builder = "load",
    node_phrase = "load node",
)]
pub struct LoadPatDef {
    /// Constrain the load's address operand.
    #[field(accepts = "Pat", arg = "p")]
    addr: Option<strider_analyze::pattern::Pat>,

    /// Restrict the match to a specific memory space (e.g.
    /// `VnSpace.ram()`).
    #[field(accepts = "VnSpace", arg = "s")]
    space: Option<rsleigh::VnSpace>,

    /// Constrain the load's memory predecessor (inputs[0]).
    #[field(accepts = "Pat", arg = "p")]
    mem_in: Option<strider_analyze::pattern::Pat>,

    /// Filter loads by value width in bits (matches U32 and F32 on
    /// bit_width(32), etc.).
    #[field(arg = "n")]
    bit_width: Option<u32>,
}

#[pyfunction]
#[pyo3(signature = (addr=None))]
pub fn load(addr: Option<PatLike<'_>>) -> PyResult<PyLoadPat> {
    let b = PyLoadPat::new();
    if let Some(a) = addr {
        let pat = a.into_pat()?;
        let mut guard = b.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.addr = Some(pat);
    }
    Ok(b)
}

/// Typed builder for `Store` node patterns.  Chain `.addr(p)`,
/// `.data(p)`, `.space(s)` to constrain the address, value, and
/// memory space respectively.
#[strider_pattern(
    rust_name = "PyStorePat",
    py_name = "StorePat",
    py_module = "strider.pattern",
    base_builder = "store",
    node_phrase = "store node",
)]
pub struct StorePatDef {
    /// Constrain the store's address operand.
    #[field(accepts = "Pat", arg = "p")]
    addr: Option<strider_analyze::pattern::Pat>,

    /// Constrain the store's stored-value operand.
    #[field(accepts = "Pat", arg = "p")]
    data: Option<strider_analyze::pattern::Pat>,

    /// Restrict the match to a specific memory space.
    #[field(accepts = "VnSpace", arg = "s")]
    space: Option<rsleigh::VnSpace>,

    /// Constrain the store's memory predecessor (inputs[0]).
    #[field(accepts = "Pat", arg = "p")]
    mem_in: Option<strider_analyze::pattern::Pat>,

    /// Match against the unique consumer of the store's memory output
    /// (outputs[0]).  No match if zero or multiple consumers.
    #[field(accepts = "Pat", arg = "p")]
    next_mem: Option<strider_analyze::pattern::Pat>,

    /// Filter stores by data width in bits (matches U32 and F32 on
    /// bit_width(32), etc.).
    #[field(arg = "n")]
    bit_width: Option<u32>,
}

#[pyfunction]
#[pyo3(signature = (addr=None, data=None))]
pub fn store(addr: Option<PatLike<'_>>, data: Option<PatLike<'_>>) -> PyResult<PyStorePat> {
    let b = PyStorePat::new();
    {
        let mut guard = b.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(a) = addr {
            guard.addr = Some(a.into_pat()?);
        }
        if let Some(v) = data {
            guard.data = Some(v.into_pat()?);
        }
    }
    Ok(b)
}

/// Typed builder for `StackStore` node patterns.  Chain
/// `.offset(o)`, `.offset_any([…])`, `.data(p)`, `.space(s)`.
#[strider_pattern(
    rust_name = "PyStackStorePat",
    py_name = "StackStorePat",
    py_module = "strider.pattern",
    base_builder = "stack_store",
    node_phrase = "stack-store node",
)]
pub struct StackStorePatDef {
    /// Match only stack-stores whose SP-relative offset equals `o`.
    #[field(arg = "o")]
    offset: Option<i64>,

    /// Match only stack-stores whose offset is in `offsets`.  Empty
    /// list vacuously fails (matches nothing) — mirrors the contract
    /// of `int_const_any_of`.  Accepts a Python list (or any iterable);
    /// the underlying builder dedupes internally so duplicates in the
    /// list are harmless.
    #[field(arg = "offsets")]
    offset_any: Option<Vec<i64>>,

    /// Constrain the stored-value operand.
    #[field(accepts = "Pat", arg = "p")]
    data: Option<strider_analyze::pattern::Pat>,

    /// Match only stack-stores in the given address space.
    #[field(accepts = "VnSpace", arg = "s")]
    space: Option<rsleigh::VnSpace>,
}

#[pyfunction]
#[pyo3(signature = (offset=None, data=None))]
pub fn stack_store(offset: Option<i64>, data: Option<PatLike<'_>>) -> PyResult<PyStackStorePat> {
    let b = PyStackStorePat::new();
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

/// Typed builder for `StackStorePhi` node patterns.  Chain
/// `.data(p)`, `.space(s)`, `.offsets(list)` (per-predecessor stack
/// offsets).
#[strider_pattern(
    rust_name = "PyStackStorePhiPat",
    py_name = "StackStorePhiPat",
    py_module = "strider.pattern",
    base_builder = "stack_store_phi",
    node_phrase = "stack-store-phi node",
)]
pub struct StackStorePhiPatDef {
    /// Constrain the per-predecessor stored value.
    #[field(accepts = "Pat", arg = "p")]
    data: Option<strider_analyze::pattern::Pat>,

    /// Match only in the given address space.
    #[field(accepts = "VnSpace", arg = "s")]
    space: Option<rsleigh::VnSpace>,

    /// Per-predecessor stack offsets, in CFG-predecessor order.  Must
    /// match the node's `Graph::stack_phi_offsets` entry exactly
    /// (length and values).
    #[field(arg = "os")]
    offsets: Option<Vec<i64>>,
}

#[pyfunction]
#[pyo3(signature = (data=None))]
pub fn stack_store_phi(data: Option<PatLike<'_>>) -> PyResult<PyStackStorePhiPat> {
    let b = PyStackStorePhiPat::new();
    if let Some(v) = data {
        let pat = v.into_pat()?;
        let mut guard = b.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.data = Some(pat);
    }
    Ok(b)
}

// ── Calls ────────────────────────────────────────────────────────────────

/// Typed builder for `Call` node patterns.  Wraps `strider_analyze::pattern::CallPat`
/// so callers can chain `.at(addr)`, `.target(p)`, `.arg(idx, p)`,
/// `.ret_output(idx, p)`, plus the universal capture / predicate /
/// finaliser methods (`.capture(c)` / `.cap(name)` / `.when(f)` /
/// `.into_pat()`).
///
/// Returned by [`call`] (the free function).  Because [`PyCallPat`]
/// is a variant of [`PatLike`], an un-finalised builder can be
/// passed directly to any field setter or query that takes a
/// pattern (e.g. `g.find_all(call().arg(0, int_const(8)))`); the
/// `into_pat()` call is implicit at use-site.
/// Typed builder for `Call` node patterns.  Wraps `strider_analyze::pattern::CallPat`
/// so callers can chain `.at(addr)`, `.target(p)`, `.arg(idx, p)`,
/// `.ret_output(idx, p)`, plus the universal capture / predicate /
/// finaliser methods (`.capture(c)` / `.cap(name)` / `.when(f)` /
/// `.into_pat()`).
///
/// Returned by [`call`] (the free function).  Because [`PyCallPat`]
/// is a variant of [`PatLike`], an un-finalised builder can be
/// passed directly to any field setter or query that takes a
/// pattern (e.g. `g.find_all(call().arg(0, int_const(8)))`); the
/// `into_pat()` call is implicit at use-site.
#[strider_pattern(
    rust_name = "PyCallPat",
    py_name = "CallPat",
    py_module = "strider.pattern",
    base_builder = "call",
    node_phrase = "Call node",
)]
pub struct CallPatDef {
    /// Constrain the call target with an arbitrary pattern (e.g.
    /// `function_arg(0)` or a captured value reference).
    #[field(accepts = "Pat", arg = "p")]
    target: Option<strider_analyze::pattern::Pat>,

    /// Constrain the argument at position `idx` (0-based, after the
    /// implicit `[ctrl, mem]` inputs).  The `Call` node's input layout
    /// is `[ctrl, mem, target, arg0, arg1, …]`; this method maps `idx`
    /// onto the arg slot.
    #[field(multi, accepts = "Pat", arg = "idx")]
    arg: Option<Vec<(usize, strider_analyze::pattern::Pat)>>,

    /// Capture the Call's return-value output at ABI position `idx`
    /// — e.g. `.ret_output(0, var(c))` binds `c` to the
    /// `NodeOutputId` of the calling convention's first return
    /// register.  See `strider_analyze::pattern::CallPat::ret_output` for details.
    #[field(multi, accepts = "Pat", arg = "idx")]
    ret_output: Option<Vec<(usize, strider_analyze::pattern::Pat)>>,
}

// `at` / `at_any` are special transformations on the same `target`
// field (constructing an `int_const` / `int_const_any_of` Pat from a
// literal address).  They don't fit the
// macro's `Option<T>`-per-field shape, so we expose them via a
// secondary `#[pymethods]` block (allowed by `multiple-pymethods`).
// `#[gen_stub_pymethods]` is required on the secondary block too so
// pyo3-stub-gen picks up these methods into the generated `.pyi`.
#[gen_stub_pymethods]
#[pymethods]
impl PyCallPat {
    /// Constrain the call target to the literal address `addr`.
    /// Equivalent to `target(int_const(addr))`.
    fn at(slf: PyRef<'_, Self>, addr: u64) -> PyRef<'_, Self> {
        {
            let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
            guard.target = Some(strider_analyze::pattern::int_const(addr));
        }
        slf
    }
    /// Constrain the call target to any address in `addrs`.
    /// Set-membership variant of `at` — fires when the call's target
    /// matches any address in the list.  Equivalent to
    /// `target(int_const_any_of(addrs))`.  An empty list vacuously
    /// fails (matches nothing).
    fn at_any(slf: PyRef<'_, Self>, addrs: Vec<u64>) -> PyRef<'_, Self> {
        {
            let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
            guard.target = Some(strider_analyze::pattern::int_const_any_of(addrs));
        }
        slf
    }
}

#[pyfunction]
#[pyo3(signature = (at=None))]
pub fn call(at: Option<u64>) -> PyCallPat {
    let b = PyCallPat::new();
    if let Some(addr) = at {
        let mut guard = b.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.target = Some(strider_analyze::pattern::int_const(addr));
    }
    b
}

// ── CallOtherPat ─────────────────────────────────────────────────────────

/// Typed builder for `CallOther` node patterns.  Mirrors
/// `strider_analyze::pattern::CallOtherPat` — chain `.user_op_id(v)` to constrain the
/// user-op id (e.g. ARM `setISAMode`'s id), `.name(s)` to constrain
/// the user-op name (read from `Graph::call_other_name`), and
/// `.arg(idx, p)` to constrain a specific argument.
#[strider_pattern(
    rust_name = "PyCallOtherPat",
    py_name = "CallOtherPat",
    py_module = "strider.pattern",
    base_builder = "call_other",
    node_phrase = "CallOther node",
)]
pub struct CallOtherPatDef {
    /// Constrain the matched node to a specific user-op id.
    #[field(arg = "v")]
    user_op_id: Option<u64>,

    /// Constrain the matched node's user-op name (read from
    /// `Graph::call_other_name`).  Combinable with `user_op_id` and
    /// `arg`.
    #[field(arg = "n")]
    name: Option<String>,

    /// Constrain raw `inputs[idx]` of the matched CallOther.
    /// `idx=0` is ctrl, `idx=1` is mem, `idx>=2` are pcode-explicit
    /// args followed by ABI implicit reads.
    #[field(multi, accepts = "Pat", arg = "idx")]
    arg: Option<Vec<(usize, strider_analyze::pattern::Pat)>>,

    /// Constrain raw `outputs[idx]` of the matched CallOther.
    /// `idx=0` is ctrl, `idx=1` is mem, `idx=2` is the pcode-explicit
    /// value (when present), `idx>=2+has_value` are ABI clobbers.
    #[field(multi, accepts = "Pat", arg = "idx")]
    ret: Option<Vec<(usize, strider_analyze::pattern::Pat)>>,

    /// Match against the unique consumer of the CallOther's control
    /// output (outputs[0]).  No match if zero or multiple consumers.
    #[field(accepts = "Pat", arg = "p")]
    next_ctrl: Option<strider_analyze::pattern::Pat>,

    /// Match against the unique consumer of the CallOther's memory
    /// output (outputs[1]).  No match if zero or multiple consumers,
    /// or when the ABI's `memory_edge` is `false`.
    #[field(accepts = "Pat", arg = "p")]
    next_mem: Option<strider_analyze::pattern::Pat>,
}

// `ctrl` / `mem` / `ctrl_out` / `mem_out` are convenience aliases that
// delegate to `arg(0/1, p)` / `ret(0/1, p)`.
// They drive the inner Mutex directly here rather than reusing the
// emitted `arg` / `ret` methods, because `multiple-pymethods` can't
// borrow `PyRef<Self>` recursively in a single chain.
// `#[gen_stub_pymethods]` on the secondary block so the four aliases
// appear in the generated `.pyi`.
#[gen_stub_pymethods]
#[pymethods]
impl PyCallOtherPat {
    /// Convenience: match `inputs[0]` (control predecessor).
    fn ctrl<'py>(
        slf: PyRef<'py, Self>,
        p: PatLike<'py>,
    ) -> PyResult<PyRef<'py, Self>> {
        let pat = p.into_pat()?;
        {
            let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
            guard.arg.get_or_insert_with(Vec::new).push((0, pat));
        }
        Ok(slf)
    }
    /// Convenience: match `inputs[1]` (memory predecessor).
    fn mem<'py>(
        slf: PyRef<'py, Self>,
        p: PatLike<'py>,
    ) -> PyResult<PyRef<'py, Self>> {
        let pat = p.into_pat()?;
        {
            let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
            guard.arg.get_or_insert_with(Vec::new).push((1, pat));
        }
        Ok(slf)
    }
    /// Convenience: match `outputs[0]` (control output).
    fn ctrl_out<'py>(
        slf: PyRef<'py, Self>,
        p: PatLike<'py>,
    ) -> PyResult<PyRef<'py, Self>> {
        let pat = p.into_pat()?;
        {
            let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
            guard.ret.get_or_insert_with(Vec::new).push((0, pat));
        }
        Ok(slf)
    }
    /// Convenience: match `outputs[1]` (memory output; dangles when
    /// the ABI's `memory_edge` is `false`).
    fn mem_out<'py>(
        slf: PyRef<'py, Self>,
        p: PatLike<'py>,
    ) -> PyResult<PyRef<'py, Self>> {
        let pat = p.into_pat()?;
        {
            let mut guard = slf.inner.lock().unwrap_or_else(|p| p.into_inner());
            guard.ret.get_or_insert_with(Vec::new).push((1, pat));
        }
        Ok(slf)
    }
}

#[pyfunction]
pub fn call_other() -> PyCallOtherPat {
    PyCallOtherPat::new()
}

// ── RetPat ───────────────────────────────────────────────────────────────

/// Typed builder for `Return` node patterns.  Chain `.preceded_by(p)`
/// to match Returns whose direct ctrl predecessor is `p` (typically a
/// `ControlState` after a Call), and `.ret_val(idx, p)` to constrain
/// the value returned at ABI position `idx`.
#[strider_pattern(
    rust_name = "PyRetPat",
    py_name = "RetPat",
    py_module = "strider.pattern",
    base_builder = "ret",
    node_phrase = "Return node",
)]
pub struct RetPatDef {
    /// Match `p` against the Return's direct ctrl predecessor (the
    /// node producing input slot 0 — typically a `ControlState` at a
    /// region header).  Single-step match, not a backward walk.
    #[field(accepts = "Pat", arg = "p")]
    preceded_by: Option<strider_analyze::pattern::Pat>,

    /// Constrain return value at ABI position `idx` (0-based after
    /// the ctrl and mem inputs — i.e. mapped to the Return's input
    /// slot `2 + idx`).
    #[field(multi, accepts = "Pat", arg = "idx")]
    ret_val: Option<Vec<(usize, strider_analyze::pattern::Pat)>>,
}

#[pyfunction]
pub fn ret() -> PyRetPat {
    PyRetPat::new()
}

// ── IfPat ────────────────────────────────────────────────────────────────

/// Typed builder for `If` node patterns.  Chain `.cond(p)`,
/// `.true_branch(p)`, `.false_branch(p)` to constrain the condition
/// and the consumers of the true/false outputs.  When `cond` is set
/// the matcher also tries the compiler-inverted layout (input
/// `Not(cond)` with branches swapped) — see `strider_analyze::pattern::IfPat` docs.
#[strider_pattern(
    rust_name = "PyIfPat",
    py_name = "IfPat",
    py_module = "strider.pattern",
    base_builder = "if_node",
    node_phrase = "If node",
)]
pub struct IfPatDef {
    /// Constrain the If's condition operand.
    #[field(accepts = "Pat", arg = "p")]
    cond: Option<strider_analyze::pattern::Pat>,

    /// Match the unique consumer of the If's true output.
    #[field(accepts = "Pat", arg = "p")]
    true_branch: Option<strider_analyze::pattern::Pat>,

    /// Match the unique consumer of the If's false output.
    #[field(accepts = "Pat", arg = "p")]
    false_branch: Option<strider_analyze::pattern::Pat>,
}

#[pyfunction]
#[pyo3(signature = (cond=None))]
pub fn if_(cond: Option<PatLike<'_>>) -> PyResult<PyIfPat> {
    let b = PyIfPat::new();
    if let Some(c) = cond {
        let pat = c.into_pat()?;
        let mut guard = b.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.cond = Some(pat);
    }
    Ok(b)
}

// ── Typed family dispatchers (with .ordered() chain via PyOrderedBinary) ──
//
// `int_binary("Add", x, y)`, `bool_binary("And", x, y)`, `float_binary("Sub", x, y)`.
// The op is a string that maps to the IR enum variant name.

fn parse_int_binary_op(name: &str) -> PyResult<strider_ir::IntBinaryOp> {
    // `Sub` is deliberately absent: `IntBinaryOp::Sub` is not a primitive
    // in this IR.  Python callers wanting `a - b` should use
    // `pattern.sub(a, b)` (which constructs the lowered
    // `Add(a, IntUnaryOp::Neg(b))` shape directly).
    use strider_ir::IntBinaryOp::*;
    Ok(match name {
        "Add" | "add" => Add,
        "Mul" | "mul" => Mul,
        "Div" | "div" => Div,
        "Sdiv" | "sdiv" => Sdiv,
        "Rem" | "rem" => Rem,
        "Srem" | "srem" => Srem,
        "And" | "and" => And,
        "Or" | "or" => Or,
        "Xor" | "xor" => Xor,
        "ShiftLeft" | "shl" => ShiftLeft,
        "ShiftRight" | "shr" => ShiftRight,
        "SShiftRight" | "sshr" => SShiftRight,
        other => {
            return Err(into_pattern_err(anyhow::anyhow!(
                "unknown IntBinaryOp variant {other:?}"
            )))
        }
    })
}

fn parse_bool_binary_op(name: &str) -> PyResult<strider_ir::BoolBinaryOp> {
    use strider_ir::BoolBinaryOp::*;
    Ok(match name {
        "And" | "and" => And,
        "Or" | "or" => Or,
        "Xor" | "xor" => Xor,
        other => {
            return Err(into_pattern_err(anyhow::anyhow!(
                "unknown BoolBinaryOp variant {other:?}"
            )))
        }
    })
}

fn parse_float_binary_op(name: &str) -> PyResult<strider_ir::FloatBinaryOp> {
    // `Sub` is deliberately absent: `FloatBinaryOp::Sub` is not a primitive.
    // Python callers wanting `a - b` should use `pattern.float_sub(a, b)`,
    // which constructs the lowered `FloatAdd(a, FloatUnaryOp::Neg(b))` shape.
    use strider_ir::FloatBinaryOp::*;
    Ok(match name {
        "Add" | "add" => Add,
        "Mul" | "mul" => Mul,
        "Div" | "div" => Div,
        other => {
            return Err(into_pattern_err(anyhow::anyhow!(
                "unknown FloatBinaryOp variant {other:?}"
            )))
        }
    })
}

/// Typed builder for an integer binary-op pattern.  Wraps
/// `strider_analyze::pattern::IntBinaryOpPat` so callers can chain `.ordered()` /
/// `.capture(c)` / `.cap(name)` / `.when(f)` before finalising as a
/// `Pat`.
//
// Emitted by `#[strider_pattern]` using the macro's
// `constructor_args` (required-construction) and
// `#[field(terminal)]` (no-arg setter that finalises to `PyPat`)
// extensions.  See `crates/strider-pattern-macros/EMISSION_SPEC.md`.
#[strider_pattern(
    rust_name = "PyIntBinaryPat",
    py_name = "IntBinaryPat",
    py_module = "strider.pattern",
    base_builder = "int_binary",
    node_phrase = "int-binary node",
    constructor_args = "op: strider_ir::IntBinaryOp, lhs: strider_analyze::pattern::Pat, rhs: strider_analyze::pattern::Pat",
)]
pub struct IntBinaryPatDef {
    /// Force the pattern to match operands in the stated order only.
    /// By default, commutative variants of the op family also try the
    /// reversed operand order.  Terminal — finalises to a [`Pat`] and
    /// does NOT chain (return type is `Pat`, not `IntBinaryPat`).
    #[field(terminal)]
    ordered: Option<bool>,
}

/// Typed builder for a boolean binary-op pattern.
#[strider_pattern(
    rust_name = "PyBoolBinaryPat",
    py_name = "BoolBinaryPat",
    py_module = "strider.pattern",
    base_builder = "bool_binary",
    node_phrase = "bool-binary node",
    constructor_args = "op: strider_ir::BoolBinaryOp, lhs: strider_analyze::pattern::Pat, rhs: strider_analyze::pattern::Pat",
)]
pub struct BoolBinaryPatDef {
    /// Force the pattern to match operands in the stated order only.
    /// By default, commutative variants of the op family also try the
    /// reversed operand order.  Terminal — finalises to a [`Pat`] and
    /// does NOT chain (return type is `Pat`, not `BoolBinaryPat`).
    #[field(terminal)]
    ordered: Option<bool>,
}

/// Typed builder for a float binary-op pattern.
#[strider_pattern(
    rust_name = "PyFloatBinaryPat",
    py_name = "FloatBinaryPat",
    py_module = "strider.pattern",
    base_builder = "float_binary",
    node_phrase = "float-binary node",
    constructor_args = "op: strider_ir::FloatBinaryOp, lhs: strider_analyze::pattern::Pat, rhs: strider_analyze::pattern::Pat",
)]
pub struct FloatBinaryPatDef {
    /// Force the pattern to match operands in the stated order only.
    /// By default, commutative variants of the op family also try the
    /// reversed operand order.  Terminal — finalises to a [`Pat`] and
    /// does NOT chain (return type is `Pat`, not `FloatBinaryPat`).
    #[field(terminal)]
    ordered: Option<bool>,
}

#[pyfunction]
pub fn int_binary(op: &str, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyIntBinaryPat> {
    Ok(PyIntBinaryPat::new(
        parse_int_binary_op(op)?,
        l.into_pat()?,
        r.into_pat()?,
    ))
}

#[pyfunction]
pub fn bool_binary(op: &str, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyBoolBinaryPat> {
    Ok(PyBoolBinaryPat::new(
        parse_bool_binary_op(op)?,
        l.into_pat()?,
        r.into_pat()?,
    ))
}

#[pyfunction]
pub fn float_binary(op: &str, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyFloatBinaryPat> {
    Ok(PyFloatBinaryPat::new(
        parse_float_binary_op(op)?,
        l.into_pat()?,
        r.into_pat()?,
    ))
}

// Allow PyO3 to convert these typed builders into PyPat via Into<PyPat>
// chains by exposing an explicit `into_pat()` method (above).  Python
// users can call `int_binary("Add", "x", "y").into_pat()` if a Pat is
// required by the surrounding API, or just chain `.capture(c)` /
// `.when(f)` / `.ordered()` to materialise a Pat directly.

// ── Variant-agnostic constructors ────────────────────────────────────────
//
// Mapped to Python names: `int_bin_any`, `int_un_any`, `int_cmp_any`,
// `bool_bin_any`, `bool_un_any`, `float_bin_any`, `float_un_any`,
// `float_cmp_any`.  Each takes a `Capture` for the matched op variant
// — recover the op via `Match.*_op(capture)` once those accessors land.
#[pyfunction]
pub fn int_bin_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::int_binary_any(c.inner, lp, rp)))
}

#[pyfunction]
pub fn int_un_any(c: PyRef<'_, PyCapture>, operand: PatLike<'_>) -> PyResult<PyPat> {
    let p = operand.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::int_unary_any(c.inner, p)))
}

#[pyfunction]
pub fn int_cmp_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::int_cmp_any(c.inner, lp, rp)))
}

#[pyfunction]
pub fn bool_bin_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::bool_binary_any(c.inner, lp, rp)))
}

#[pyfunction]
pub fn bool_un_any(c: PyRef<'_, PyCapture>, operand: PatLike<'_>) -> PyResult<PyPat> {
    let p = operand.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::bool_unary_any(c.inner, p)))
}

#[pyfunction]
pub fn float_bin_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::float_binary_any(c.inner, lp, rp)))
}

#[pyfunction]
pub fn float_un_any(c: PyRef<'_, PyCapture>, operand: PatLike<'_>) -> PyResult<PyPat> {
    let p = operand.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::float_unary_any(c.inner, p)))
}

#[pyfunction]
pub fn float_cmp_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::float_cmp_any(c.inner, lp, rp)))
}

// ── Module registration ──────────────────────────────────────────────────

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new_bound(py, "pattern")?;
    m.add_class::<PyCapture>()?;
    m.add_class::<PyPat>()?;
    m.add_class::<PyPartialMatch>()?;
    m.add_class::<PyIntBinaryPat>()?;
    m.add_class::<PyBoolBinaryPat>()?;
    m.add_class::<PyFloatBinaryPat>()?;
    m.add_class::<PyCallPat>()?;
    m.add_class::<PyCallOtherPat>()?;
    m.add_class::<PyRetPat>()?;
    m.add_class::<PyIfPat>()?;
    m.add_class::<PyLoadPat>()?;
    m.add_class::<PyStorePat>()?;
    m.add_class::<PyStackStorePat>()?;
    m.add_class::<PyStackStorePhiPat>()?;
    m.add_class::<PyPhiPat>()?;
    m.add_class::<PyMemPhiPat>()?;
    m.add_class::<PyValuePhiPat>()?;
    m.add_class::<PyFunctionArgPat>()?;
    m.add_class::<PyCastMask>()?;

    macro_rules! add_fn {
        ($name:ident) => {
            m.add_function(wrap_pyfunction!($name, &m)?)?;
        };
    }
    // wildcards / consts / phi / initial
    add_fn!(any_);
    add_fn!(var);
    add_fn!(int_const);
    add_fn!(signed_int_const);
    add_fn!(int_const_any_of);
    add_fn!(bool_const);
    add_fn!(float_const);
    add_fn!(any_int_const);
    add_fn!(any_bool_const);
    add_fn!(any_float_const);
    add_fn!(initial_var);
    add_fn!(initial_var_for);
    add_fn!(function_arg);
    add_fn!(function_arg_any);
    add_fn!(function_arg_reg);
    add_fn!(function_arg_stack);
    add_fn!(phi);
    add_fn!(phi_for);
    add_fn!(mem_phi);
    add_fn!(value_phi);
    add_fn!(predicate);
    add_fn!(int_cmp);
    // int binary
    add_fn!(add);
    add_fn!(sub);
    add_fn!(mul);
    add_fn!(div);
    add_fn!(sdiv);
    add_fn!(rem);
    add_fn!(srem);
    add_fn!(shl);
    add_fn!(shr);
    add_fn!(sshr);
    add_fn!(and_);
    add_fn!(or_);
    add_fn!(xor);
    add_fn!(int_eq);
    add_fn!(int_lt);
    add_fn!(int_le);
    add_fn!(int_slt);
    add_fn!(int_sle);
    add_fn!(int_carry);
    add_fn!(int_scarry);
    add_fn!(int_sborrow);
    // int unary
    add_fn!(neg);
    add_fn!(bit_not);
    add_fn!(not_);
    // bool
    add_fn!(bool_and);
    add_fn!(bool_or);
    add_fn!(bool_xor);
    add_fn!(bool_not);
    // float binary / unary / cmp
    add_fn!(float_add);
    add_fn!(float_sub);
    add_fn!(float_mul);
    add_fn!(float_div);
    add_fn!(float_neg);
    add_fn!(float_abs);
    add_fn!(float_sqrt);
    add_fn!(float_ceil);
    add_fn!(float_floor);
    add_fn!(float_round);
    add_fn!(float_is_nan);
    add_fn!(float_eq);
    add_fn!(float_ne);
    add_fn!(float_lt);
    add_fn!(float_le);
    // conversions / bitcasts / casts
    add_fn!(int_to_float);
    add_fn!(float_to_int);
    add_fn!(float_to_float);
    add_fn!(int_bits_to_float);
    add_fn!(float_bits_to_int);
    add_fn!(cast_to_int);
    add_fn!(cast_to_bool);
    add_fn!(cast_to_float);
    add_fn!(truncate);
    add_fn!(popcount);
    add_fn!(lzcount);
    add_fn!(zero_extend);
    add_fn!(sign_extend);
    add_fn!(extend);
    // memory / control
    add_fn!(load);
    add_fn!(store);
    add_fn!(stack_store);
    add_fn!(stack_store_phi);
    add_fn!(call);
    add_fn!(call_other);
    add_fn!(ret);
    add_fn!(if_);
    // typed family dispatchers
    add_fn!(int_binary);
    add_fn!(bool_binary);
    add_fn!(float_binary);
    // variant-agnostic binders
    add_fn!(int_bin_any);
    add_fn!(int_un_any);
    add_fn!(int_cmp_any);
    add_fn!(bool_bin_any);
    add_fn!(bool_un_any);
    add_fn!(float_bin_any);
    add_fn!(float_un_any);
    add_fn!(float_cmp_any);

    parent.add_submodule(&m)?;
    let sys = py.import_bound("sys")?;
    sys.getattr("modules")?.set_item("strider.pattern", &m)?;
    Ok(())
}

// ── Pat-builder finalise impls ───────────────────────────────────────────
//
// One macro invocation per typed builder.  Each emits a separate
// `#[pymethods]` block (allowed by the `multiple-pymethods` PyO3 feature)
// carrying the four shared `capture` / `cap` / `when` / `into_pat`
// methods.  See `pat_builder_finalise!` (declared near the top of the
// file) for the body.

// PyPhiPat, PyMemPhiPat, PyValuePhiPat: capture/cap/when/into_pat
// emitted by `#[strider_pattern]`.
pat_builder_finalise!(PyFunctionArgPat);
// PyLoadPat: capture/cap/when/into_pat emitted by `#[strider_pattern]`.
// PyStorePat, PyStackStorePat, PyStackStorePhiPat: capture/cap/when/into_pat
// emitted by `#[strider_pattern]`.
// PyCallPat, PyCallOtherPat, PyRetPat: capture/cap/when/into_pat
// emitted by `#[strider_pattern]`.
// PyIfPat: capture/cap/when/into_pat emitted by `#[strider_pattern]`.
// PyIntBinaryPat, PyBoolBinaryPat, PyFloatBinaryPat:
// capture/cap/when/into_pat emitted by `#[strider_pattern]` via the
// `constructor_args` (required-construction) + `#[field(terminal)]`
// extensions.
