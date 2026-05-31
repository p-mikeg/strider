//! `strider.pattern` submodule.
//!
//! Wraps the `pattern` crate.  Provides:
//! - `Capture` — opaque capture-variable handle.
//! - `Pat` — opaque wrapped pattern.  Constructed via free functions
//!   (`add`, `load`, `call`, `float_add`, `int_bits_to_float`, …) and
//!   chained via builder methods (`.addr()`, `.arg()`, `.capture()`,
//!   `.cap()`, `.when()`, `.ordered()`, etc.).
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

use crate::errors::into_strider_err;

// ── Capture ──────────────────────────────────────────────────────────────

// `#[gen_stub_pyclass]` derives `PyStubType` for `PyCapture` so the
// macro-emitted reference type's `.capture(c: PyRef<'_, PyCapture>)`
// signature compiles under `#[gen_stub_pymethods]`.  This only adds
// the type-info impl; the existing `#[pymethods]` block below is
// unchanged (no `#[gen_stub_pymethods]` here — the hand-written
// `pattern.pyi` already covers PyCapture's surface).
/// An opaque capture variable that binds a matched node so its value /
/// op-variant / fingerprint can be read back from the `Match`.  Each
/// `Capture()` call produces a globally unique id; pass it to `var(c)`,
/// `any_int_const(c)`, `.capture(c)`, etc.
#[pyo3_stub_gen::derive::gen_stub_pyclass]
#[pyclass(name = "Capture", module = "strider.pattern", frozen)]
#[derive(Clone)]
pub struct PyCapture {
    pub(crate) inner: strider_analyze::pattern::Capture,
}

#[pymethods]
impl PyCapture {
    /// Create a fresh, globally-unique capture variable for binding a
    /// matched node.  Retrieve the binding after a match via
    /// `Match[c]` / `Match.uint(c)` / etc.
    #[new]
    fn new() -> Self {
        Self {
            inner: strider_analyze::pattern::Capture::new(),
        }
    }

    /// `Capture(<id>)`.
    fn __repr__(&self) -> String {
        format!("Capture({:?})", self.inner)
    }

    /// Hash on the capture's globally-unique id (stable per instance).
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
        return Err(into_strider_err(anyhow::anyhow!(
            "{name:?} is reserved (use any_() / var() / _ explicitly)"
        )));
    }
    let mut table = intern_table()
        .lock()
        .map_err(|_| into_strider_err(anyhow::anyhow!("intern table lock poisoned")))?;
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
/// `sign_extend`, `extend`, `truncate`, `int_bits_to_float`,
/// `float_bits_to_int`, `all`, `none`/`empty`); combine with `|`
/// (Python `__or__`).
///
/// Pass to `Graph.find_all(pat, ignore_casts_mask=...)` — granular
/// alternative to the all-or-nothing `ignore_casts=True`.
#[pyclass(name = "CastMask", module = "strider.pattern", frozen)]
#[derive(Clone, Copy)]
pub struct PyCastMask {
    pub(crate) inner: strider_analyze::pattern::CastMask,
}

// Stamp out the 11 `CastMask` factory classmethods inside a dedicated
// `#[pymethods]` block (PyO3's `multiple-pymethods` feature lets one
// `#[pyclass]` carry several `#[pymethods]` impls).  Forms:
//
//   forall_castmask!(zero_extend => ZERO_EXTEND);  // const bitflags
//   forall_castmask!(all => fn all);                // assoc fn
//
// The macro emits an entire `#[pymethods] impl PyCastMask { … }` block
// per row so the `#[pymethods]` proc-macro sees the bare `#[classmethod]`
// attribute it expects (the previous in-block `macro_rules!` form failed
// because `#[classmethod]` is only recognised when it appears literally
// to PyO3's `#[pymethods]` pass, not after a `macro_rules!` expansion).
macro_rules! forall_castmask {
    ($name:ident => $value:ident) => {
        #[pymethods]
        impl PyCastMask {
            #[doc = concat!(
                "Mask selecting the `", stringify!($value),
                "` value-passthrough cast for the matcher to walk through."
            )]
            #[classmethod]
            fn $name(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
                Self { inner: strider_analyze::pattern::CastMask::$value }
            }
        }
    };
    ($name:ident => fn $value:ident) => {
        #[pymethods]
        impl PyCastMask {
            #[doc = concat!(
                "`CastMask::", stringify!($value), "()` — ",
                "the all-casts (`all`) / no-casts (`empty`) mask."
            )]
            #[classmethod]
            fn $name(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
                Self { inner: strider_analyze::pattern::CastMask::$value() }
            }
        }
    };
}

forall_castmask!(zero_extend => ZERO_EXTEND);
forall_castmask!(sign_extend => SIGN_EXTEND);
forall_castmask!(extend => EXTEND);
forall_castmask!(truncate => TRUNCATE);
forall_castmask!(int_bits_to_float => INT_BITS_TO_FLOAT);
forall_castmask!(float_bits_to_int => FLOAT_BITS_TO_INT);
forall_castmask!(all => fn all);
forall_castmask!(none => fn empty);

#[pymethods]
impl PyCastMask {
    /// Alias for `none()` — mirrors Rust's `strider_analyze::pattern::CastMask::empty()`.
    #[classmethod] fn empty(cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self::none(cls)
    }

    /// Union of two masks (`a | b`).
    fn __or__(&self, other: &Self) -> Self {
        Self { inner: self.inner | other.inner }
    }
    /// Intersection of two masks (`a & b`).
    fn __and__(&self, other: &Self) -> Self {
        Self { inner: self.inner & other.inner }
    }
    /// Equality on the underlying bitset.
    fn __eq__(&self, other: &Self) -> bool { self.inner == other.inner }
    /// Hash on the underlying bits (consistent with `__eq__`).
    fn __hash__(&self) -> u64 { self.inner.bits() as u64 }
    /// The raw bitset value as a `u32`.
    fn bits(&self) -> u32 { self.inner.bits() }

    /// `CastMask(0b........)` showing the raw bits.
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
    PhiPat(Bound<'py, PyPhiPat>),
    MemPhiPat(Bound<'py, PyMemPhiPat>),
    ValuePhiPat(Bound<'py, PyValuePhiPat>),
    FunctionArgPat(Bound<'py, PyFunctionArgPat>),
    IntBinaryPat(Bound<'py, PyIntBinaryPat>),
    FloatBinaryPat(Bound<'py, PyFloatBinaryPat>),
    BoolBinaryPat(Bound<'py, PyBoolBinaryPat>),
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
            PatLike::PhiPat(b) => Ok(b.borrow().finalise()),
            PatLike::MemPhiPat(b) => Ok(b.borrow().finalise()),
            PatLike::ValuePhiPat(b) => Ok(b.borrow().finalise()),
            PatLike::FunctionArgPat(b) => Ok(b.borrow().finalise()),
            PatLike::IntBinaryPat(b) => Ok(b.borrow().finalise()),
            PatLike::FloatBinaryPat(b) => Ok(b.borrow().finalise()),
            PatLike::BoolBinaryPat(b) => Ok(b.borrow().finalise()),
        }
    }
}

// ── Pending control-flow exception (KeyboardInterrupt / SystemExit) ─────
//
// When a `.when()` predicate raises a control-flow exception, we can't
// just `PyErr::restore` it on the thread: the matcher will keep
// iterating across candidates and invoke the predicate again, and on
// the next `call_bound` invocation CPython sees the still-set error
// indicator and replaces the original `KeyboardInterrupt`/`SystemExit`
// with `SystemError: "returned a result with an exception set"`.  By
// the time `find_all` finishes the original control-flow signal is
// lost.
//
// Instead: stash the first control-flow PyErr in a thread-local cell.
// `wrap_when` short-circuits subsequent predicate invocations once the
// cell is non-empty (no more `call_bound`), and the outer `find_all`
// boundary in `function.rs` drains the cell via [`take_pending_control_flow`]
// and surfaces the stored `PyErr` as `Err(...)`.
//
// Thread-local because the matcher's predicate callback chain is
// single-threaded under the GIL.

thread_local! {
    static PENDING_CONTROL_FLOW: std::cell::Cell<Option<PyErr>> =
        const { std::cell::Cell::new(None) };
}

/// Drain the thread-local pending-control-flow slot, if any.  Called
/// from the outer `find_all` / `find_joined` / `run`
/// boundaries to surface a saved `KeyboardInterrupt` / `SystemExit`
/// after the matcher walk completes.
pub(crate) fn take_pending_control_flow() -> Option<PyErr> {
    PENDING_CONTROL_FLOW.with(|cell| cell.take())
}

/// Peek at the pending-control-flow cell without draining it.
/// Returns true iff a PyErr is stashed.  Used by the
/// `PyReadOnlyMemoryAdapter::read` short-circuit so subsequent
/// `read` calls bail out cleanly without invoking Python.
pub(crate) fn peek_pending_control_flow() -> bool {
    PENDING_CONTROL_FLOW.with(|cell| {
        let t = cell.take();
        let pending = t.is_some();
        cell.set(t);
        pending
    })
}

/// Stash a control-flow PyErr in the pending cell, unconditionally
/// overwriting any existing stash.  In practice "the first error wins"
/// because the sole caller (`PyReadOnlyMemoryAdapter::read`) checks
/// `peek_pending_control_flow` and bails before invoking the
/// callback again, so this is only reached once per walk.
pub(crate) fn stash_pending_control_flow(e: PyErr) {
    PENDING_CONTROL_FLOW.with(|cell| cell.set(Some(e)));
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

/// Transient read-only view of the captures bound so far, passed to a
/// `.when(...)` / `predicate(...)` Python callback.  Offers
/// `uint`/`int`/`bool`/`float_bits`/`has`/`__getitem__`/`__contains__`
/// over those bindings.  Valid only for the duration of the predicate
/// call; accessors return `None`/`False` if used afterwards.
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
// The `*const Graph` it holds is only valid for the
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
        // SAFETY: `ptr` was set to a valid `&Graph` by the
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
    /// The capture's value as an unsigned `int`, or `None` when not
    /// bound to an integer node (or the proxy has expired).
    fn uint(&self, key: CaptureKeyOwned) -> PyResult<Option<u128>> {
        let cap = self.capture_from_key(&key)?;
        Ok(self.with_graph(|g| self.bindings.get_uint(cap, g)).flatten())
    }

    /// The capture's value as a signed `int`, or `None` when not bound
    /// to an integer node.
    #[pyo3(name = "int")]
    fn int_(&self, key: CaptureKeyOwned) -> PyResult<Option<i128>> {
        let cap = self.capture_from_key(&key)?;
        Ok(self.with_graph(|g| self.bindings.get_int(cap, g)).flatten())
    }

    /// The capture's value as a `bool`, or `None` when not bound to a
    /// boolean node.
    #[pyo3(name = "bool")]
    fn bool_(&self, key: CaptureKeyOwned) -> PyResult<Option<bool>> {
        let cap = self.capture_from_key(&key)?;
        Ok(self.with_graph(|g| self.bindings.get_bool(cap, g)).flatten())
    }

    /// The capture's value as raw float bits (`u64`), or `None` when not
    /// bound to a float node.
    fn float_bits(&self, key: CaptureKeyOwned) -> PyResult<Option<u64>> {
        let cap = self.capture_from_key(&key)?;
        Ok(self
            .with_graph(|g| self.bindings.get_float_bits(cap, g))
            .flatten())
    }

    /// True if the capture has a binding so far in this partial match.
    fn has(&self, key: CaptureKeyOwned) -> PyResult<bool> {
        let cap = self.capture_from_key(&key)?;
        Ok(self.bindings.get_node(cap).is_some())
    }

    /// Look up a capture by key (Python `m[c]`).  Returns an unsigned int,
    /// bool, or raw float bits depending on the bound node's type; returns
    /// `None` when the capture is unbound or the proxy has expired.
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

    /// Whether `c` is bound in this partial match (Python `c in m`).
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
            // Short-circuit: a prior predicate already raised a
            // control-flow exception that we stashed in the
            // thread-local PENDING_CONTROL_FLOW cell.  Stop calling
            // user code so we don't trip CPython's "returned a result
            // with an exception set" guard on the next invocation
            // (Python's error indicator must be clear when re-entering
            // a C function).  The outer find_all boundary will drain
            // the cell + surface the saved PyErr.
            if PENDING_CONTROL_FLOW.with(|cell| {
                let t = cell.take();
                let pending = t.is_some();
                cell.set(t);
                pending
            }) {
                return false;
            }
            let proxy = PyPartialMatch::new(bindings.clone(), graph);
            let py_proxy = match Py::new(py, proxy) {
                Ok(e_err) => e_err,
                Err(e) => {
                    // Proxy alloc failure — stash for the outer
                    // boundary to pick up.  Treat as control-flow-
                    // equivalent: a failing predicate setup aborts the
                    // walk because we have no useful no-match
                    // semantics for it.
                    PENDING_CONTROL_FLOW.with(|c| c.set(Some(e)));
                    return false;
                }
            };
            let args = PyTuple::new_bound(py, [py_proxy.clone_ref(py)]);
            let result = py_func.call_bound(py, args, None);
            // Always invalidate the proxy's graph pointer so any
            // subsequent use from Python doesn't deref a stale ptr.
            //
            // `try_borrow` can only fail when an active `&mut self`
            // borrow is held; `PyPartialMatch` exposes only `&self`
            // methods via #[pymethods] AND is `unsendable`, so that
            // failure mode is unreachable from any synchronous path.
            // We still avoid the panicking `borrow` so that a future
            // `&mut self` method (or a re-entrant call) degrades to a
            // skipped invalidation rather than panicking across the
            // FFI boundary.
            if let Ok(proxy_ref) = py_proxy.try_borrow(py) {
                proxy_ref.clear_graph_ptr();
            }
            match result {
                Ok(obj) => match obj.extract::<bool>(py) {
                    Ok(b) => b,
                    Err(e) => {
                        // Bad return type (e.g. non-bool) — stash for
                        // the outer boundary to surface, then short-
                        // circuit subsequent calls via the pending
                        // cell.  This was previously logged to stderr
                        // and silently treated as no-match, which hid
                        // predicate-type-mismatch bugs.
                        PENDING_CONTROL_FLOW.with(|c| c.set(Some(e)));
                        false
                    }
                },
                Err(e) => {
                    // Contract: only control-flow exceptions
                    // (`KeyboardInterrupt`, `SystemExit`) propagate
                    // out of `find_all`.  Ordinary predicate bugs
                    // (`ValueError`, `AttributeError`, `TypeError`,
                    // …) are SWALLOWED + treated as no-match —
                    // aborting the whole walk on one buggy predicate
                    // hit would be worse than continuing.
                    let is_control_flow = {
                        let t = e.get_type_bound(py);
                        t.is_subclass_of::<pyo3::exceptions::PyKeyboardInterrupt>()
                            .unwrap_or(false)
                            || t.is_subclass_of::<pyo3::exceptions::PySystemExit>()
                                .unwrap_or(false)
                    };
                    if is_control_flow {
                        // Stash without restoring on the thread error
                        // indicator: `PyErr::restore(py)` would leave
                        // the error set between predicate calls, and
                        // CPython would wrap the next `call_bound`'s
                        // outcome in `SystemError("returned a result
                        // with an exception set")`, destroying the
                        // original control-flow signal.  The pending
                        // cell + short-circuit above on subsequent
                        // invocations keeps the error indicator clean
                        // and preserves the original PyErr for the
                        // outer find_all boundary to drain.
                        PENDING_CONTROL_FLOW.with(|c| c.set(Some(e)));
                    } else {
                        // Surface ordinary predicate bugs to stderr
                        // so they're visible in CI logs without
                        // aborting the user's walk.
                        eprintln!(
                            "strider .when() predicate raised — treating as no-match: {e}"
                        );
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
    /// method now raises [`StriderError`] so the misuse is visible.
    fn ordered(&self) -> PyResult<PyPat> {
        Err(into_strider_err(anyhow::anyhow!(
            "Pat.ordered() has no effect on a finalized Pat — \
             use int_binary(op, l, r).ordered() / bool_binary(op, l, r).ordered() / \
             float_binary(op, l, r).ordered() to force left-to-right matching"
        )))
    }

    /// Opaque `Pat(...)` repr (the pattern's internal structure is not
    /// surfaced to Python).
    fn __repr__(&self) -> String {
        "Pat(...)".to_string()
    }
}

// ── Free constructors ────────────────────────────────────────────────────

/// Wildcard: matches any node without binding it.
#[pyfunction]
pub fn any_() -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::any())
}

/// Wildcard that binds the matched node to capture `c` (retrieve via
/// `Match[c]` etc.).
#[pyfunction]
pub fn var(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::var(c.inner))
}

/// Match any value output exactly `n` bits wide (the output-width filter).
/// `value_of_width(1)` (see `bool_value`) selects booleans; matches both
/// integer and float types of the width (e.g. 32 matches `I32` and `F32`).
#[pyfunction]
pub fn value_of_width(n: u32) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::value_of_width(n))
}

/// Match any boolean value — any 1-bit (`I1`) value output.  Note this
/// matches anything that *produces* a bool, including comparisons; to match
/// operations that *operate on* booleans use `bool_inputs`.
#[pyfunction]
pub fn bool_value() -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::bool_value())
}

/// Match `inner` and require all of the matched node's value inputs to be
/// `n` bits wide (the input-width filter).  `inputs_of_width(1, ...)`
/// (see `bool_inputs`) selects operations that operate on booleans.
#[pyfunction]
pub fn inputs_of_width(n: u32, inner: PatLike<'_>) -> PyResult<PyPat> {
    Ok(PyPat::from_pat(strider_analyze::pattern::inputs_of_width(
        n,
        inner.into_pat()?,
    )))
}

/// Match `inner` whose value inputs are all booleans (1-bit `I1`) — i.e. an
/// operation that operates on booleans (`And`/`Or`/`Xor`/`Not` of bools),
/// excluding comparisons (whose operands are wider).
#[pyfunction]
pub fn bool_inputs(inner: PatLike<'_>) -> PyResult<PyPat> {
    Ok(PyPat::from_pat(strider_analyze::pattern::bool_inputs(
        inner.into_pat()?,
    )))
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
/// `IntConst(0x00000000FFFFFFCE)` at I64 — which `int_const(-50)`
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

/// Match an `I1` boolean constant (an `IntConst` typed `I1`) equal to `value`.
#[pyfunction]
pub fn bool_const(value: bool) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::bool_const(value))
}

/// Match a `FloatConst` whose raw bits equal `bits`.
#[pyfunction]
pub fn float_const(bits: u64) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::float_const(bits))
}

/// Match any `IntConst` and bind its value to `c`
/// (read back via `Match.uint(c)` / `Match.int(c)`).
#[pyfunction]
pub fn any_int_const(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::any_int_const(c.inner))
}

/// Match any `I1` boolean constant (an `IntConst` typed `I1`) and bind it to
/// `c` (read back via `Match.bool(c)`).
#[pyfunction]
pub fn any_bool_const(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::any_bool_const(c.inner))
}

/// Match any `FloatConst` and bind it to `c`
/// (read back via `Match.float_bits(c)`).
#[pyfunction]
pub fn any_float_const(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_pat(strider_analyze::pattern::any_float_const(c.inner))
}

/// Match any `InitialVar` node (an initial-state register read).  Use
/// `initial_var_for(vn)` to pin a specific varnode.
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

/// Typed builder for tagged-`Phi` patterns (the lifter-emitted SSA
/// φ for a register-aliased read, whose `Graph::phi_var_tag` entry
/// is `Some`).  Chain `.for_vn(vn)` to constrain the matched phi to
/// a specific varnode, and `.input(idx, p)` to constrain the value
/// arriving from the given predecessor slot.
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
    /// skip the phi-token edge from the owning `Region`).
    #[field(multi, accepts = "Pat", arg = "idx")]
    input: Option<Vec<(usize, strider_analyze::pattern::Pat)>>,
}

/// Start a tagged-`Phi` pattern builder (see `PhiPat`).
#[pyfunction]
pub fn phi() -> PyPhiPat { PyPhiPat::new() }

/// Match a tagged `Phi` (see [`phi`]) for a specific varnode in
/// `Graph::phi_var_tag`.  Equivalent to `phi().for_vn(vn)` but
/// reads more naturally at the call site.
#[pyfunction]
pub fn phi_for(vn: crate::sleigh::PyVn) -> PyPhiPat {
    let b = PyPhiPat::new();
    b.with_inner(|inner| inner.for_vn = Some(vn.inner));
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

/// Start a `MemPhi` pattern builder (memory-token phi; see `MemPhiPat`).
#[pyfunction]
pub fn mem_phi() -> PyMemPhiPat { PyMemPhiPat::new() }

/// Builder for `ValuePhi` patterns.  ValuePhi is synthesised by
/// `LoadForward` to phi together stack-store values across a
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

/// Start a `ValuePhi` pattern builder (anonymous value phi synthesised
/// by `LoadForward`; see `ValuePhiPat`).
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
    /// Constrain the match to argument at ABI position `i` (0-based).
    fn index(slf: Py<Self>, py: Python<'_>, i: u32) -> Py<Self> {
        slf.borrow(py).index.replace(Some(i)); slf
    }
    /// Constrain the match to an argument sourced from register varnode `vn`.
    fn source_register(slf: Py<Self>, py: Python<'_>, vn: crate::sleigh::PyVn) -> Py<Self> {
        slf.borrow(py).source.replace(Some(strider_ir::node::FunctionArgSource::Register(vn.inner))); slf
    }
    /// Constrain the match to an argument sourced from the stack at `(space, offset)`.
    fn source_stack(slf: Py<Self>, py: Python<'_>, space: crate::sleigh::PyVnSpace, offset: i64) -> Py<Self> {
        slf.borrow(py).source.replace(Some(strider_ir::node::FunctionArgSource::Stack {
            space: space.inner,
            offset,
        }));
        slf
    }
}

/// Start a `FunctionArg` pattern builder constrained to argument index
/// `i` (see `FunctionArgPat`).
#[pyfunction]
pub fn function_arg(i: u32) -> PyFunctionArgPat {
    let b = PyFunctionArgPat::new();
    b.index.replace(Some(i));
    b
}

/// Start a `FunctionArg` pattern builder matching any argument index.
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

/// Match any node, subject to the Python predicate `f` (called with a
/// `PartialMatch` proxy; returning `False` fails the match).  Shorthand
/// for `any_().when(f)`.
#[pyfunction]
pub fn predicate(f: PyObject) -> PyPat {
    PyPat::from_pat(wrap_when(strider_analyze::pattern::any(), f))
}

// Two unified macros for the binop / unop / conv builder family.  Each
// emits a `#[pyfunction]` that wraps the same-named constructor in
// `strider_analyze::pattern`.  Pass `, into` when the underlying
// constructor returns a typed `*BinaryOpPat` / `*UnaryOpPat` wrapper
// that needs `.into()` to widen to `Pat`; omit it when the constructor
// already returns `Pat`.

macro_rules! binary {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
            let lp = l.into_pat()?;
            let rp = r.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$name(lp, rp)))
        }
    };
    ($name:ident, into, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
            let lp = l.into_pat()?;
            let rp = r.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$name(lp, rp).into()))
        }
    };
    // Python-name override: exported Rust fn is `$py_name` (matching the
    // Python attribute literal), but the underlying `strider_analyze::pattern`
    // constructor is `$rust_name` without the keyword-collision suffix.
    ($py_name:ident as $py_name_lit:literal => $rust_name:ident, into, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction(name = $py_name_lit)]
        pub fn $py_name(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
            let lp = l.into_pat()?;
            let rp = r.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$rust_name(lp, rp).into()))
        }
    };
}

macro_rules! unary {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(operand: PatLike<'_>) -> PyResult<PyPat> {
            let op = operand.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$name(op)))
        }
    };
    ($name:ident, into, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction]
        pub fn $name(operand: PatLike<'_>) -> PyResult<PyPat> {
            let op = operand.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$name(op).into()))
        }
    };
    // Python-name override: exported Rust fn is `$py_name` (matching the
    // Python attribute literal), but the underlying `strider_analyze::pattern`
    // constructor is `$rust_name`.
    ($py_name:ident as $py_name_lit:literal => $rust_name:ident, $doc:literal) => {
        #[doc = $doc]
        #[pyfunction(name = $py_name_lit)]
        pub fn $py_name(operand: PatLike<'_>) -> PyResult<PyPat> {
            let op = operand.into_pat()?;
            Ok(PyPat::from_pat(strider_analyze::pattern::$rust_name(op)))
        }
    };
}

// ── Binary integer ops ───────────────────────────────────────────────────

binary!(add, into, "Pattern: `IntBinaryOp::Add` (`a + b`).  Commutative — \
    tries both operand orders.");
binary!(sub, into, "Pattern: integer subtraction `a - b`.  The IR has no \
    Sub op; this matches the lifter-canonical `Add(a, Neg(b))` shape.");
binary!(mul, into, "Pattern: `IntBinaryOp::Mul` (`a * b`).  Commutative.");
binary!(div, into, "Pattern: `IntBinaryOp::Div` (unsigned `a / b`).");
binary!(sdiv, into, "Pattern: `IntBinaryOp::Sdiv` (signed `a / b`).");
binary!(rem, into, "Pattern: `IntBinaryOp::Rem` (unsigned `a % b`).");
binary!(srem, into, "Pattern: `IntBinaryOp::Srem` (signed `a % b`).");
binary!(shl, into, "Pattern: `IntBinaryOp::ShiftLeft` (`a << b`).");
binary!(shr, into, "Pattern: `IntBinaryOp::ShiftRight` (logical `a >> b`).");
binary!(sshr, into, "Pattern: `IntBinaryOp::SShiftRight` (arithmetic `a >> b`).");
// `and` / `or` are Python keywords; expose as `and_` / `or_`.
binary!(and_ as "and_" => and, into,
    "Pattern: `IntBinaryOp::And` (`a & b`).  Commutative.");
binary!(or_ as "or_" => or, into,
    "Pattern: `IntBinaryOp::Or` (`a | b`).  Commutative.");
binary!(xor, into, "Pattern: `IntBinaryOp::Xor` (`a ^ b`).  Commutative.");
binary!(int_eq, into, "Pattern: `IntCmpOp::Equal` (`a == b`).  Commutative.");
binary!(int_lt, into, "Pattern: `IntCmpOp::Less` (unsigned `a < b`).");
binary!(int_le, into, "Pattern: unsigned `a <= b`.  The IR has no LessEqual \
    op; this matches the lifter-canonical `BitNot(IntLess(b, a))` shape (at `I1`).");
binary!(int_slt, into, "Pattern: `IntCmpOp::Sless` (signed `a < b`).");
binary!(int_sle, into, "Pattern: signed `a <= b`.  Matches the \
    lifter-canonical `BitNot(Sless(b, a))` shape (at `I1`).");
binary!(int_carry, into,
    "Pattern: `IntCmpOp::Carry` (unsigned add carry-out).  Commutative.");
binary!(int_scarry, into,
    "Pattern: `IntCmpOp::Scarry` (signed add overflow).  Commutative.");
binary!(int_sborrow, into,
    "Pattern: `IntCmpOp::Sborrow` (signed subtract overflow).");

/// Match a specific `IntCmpOp` variant.  Op names: "Equal",
/// "Less" / "lt", "LessEqual" / "le", "Sless" / "slt",
/// "SlessEqual" / "sle", "Carry", "Scarry", "Sborrow".  Pair with
/// `var(c)` / `int_const(K)` operands when you need a specific
/// shape.  Note: there is no `IntNotEqual` variant — the lifter
/// lowers `p-code INT_NOTEQUAL` to `BitNot(IntEqual)` at `I1`, so to match
/// `a != b` use `bool_not(int_cmp("Equal", a, b))`.
#[pyfunction]
pub fn int_cmp(op: &str, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let cmp_op = parse_int_cmp_op(op)?;
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::int_cmp(cmp_op, lp, rp)))
}

// Shared lookup helper for the four `parse_*_op` family.  Each `parse_*_op`
// is a thin wrapper that supplies its op-enum's name/variant table and the
// `op_kind` label used in the error message.  Lookups are case-insensitive
// after an initial exact-match pass, so both the canonical form (`"Add"`)
// and any lowercase aliases (`"add"`) succeed without the table needing
// duplicate rows.
//
// The table is `&'static [(&'static str, Op)]`; for the tiny enums we have
// (≤ 12 entries) the linear scan is well below the previous `match`'s
// codegen footprint and avoids the parallel-table maintenance burden.
fn lookup_op<Op: Copy>(
    table: &[(&str, Op)],
    name: &str,
    op_kind: &str,
) -> PyResult<Op> {
    if let Some(&(_, op)) = table.iter().find(|(n, _)| *n == name) {
        return Ok(op);
    }
    let lowered = name.to_ascii_lowercase();
    if let Some(&(_, op)) = table.iter().find(|(n, _)| n.eq_ignore_ascii_case(&lowered)) {
        return Ok(op);
    }
    Err(into_strider_err(anyhow::anyhow!(
        "unknown {op_kind} variant {name:?}"
    )))
}

fn parse_int_cmp_op(name: &str) -> PyResult<strider_ir::IntCmpOp> {
    use strider_ir::IntCmpOp::*;
    // `LessEqual` / `SlessEqual` are deliberately absent: the IR has no
    // such primitives.  Python callers wanting `a <= b` must use
    // `pattern.int_le(a, b)` (or `pattern.int_sle` for signed), which
    // construct the lowered `BitNot(IntLess(b, a))` shape (at `I1`).
    static TABLE: &[(&str, strider_ir::IntCmpOp)] = &[
        ("Equal", Equal),
        ("Less", Less),
        ("Sless", Sless),
        ("Carry", Carry),
        ("Scarry", Scarry),
        ("Sborrow", Sborrow),
        ("eq", Equal),
        ("lt", Less),
        ("slt", Sless),
    ];
    lookup_op(TABLE, name, "IntCmpOp")
}

// ── Integer unary ops ────────────────────────────────────────────────────

// `pattern.neg(x)` matches two's-complement negation (`-x`).
unary!(neg, "Pattern: `IntUnaryOp::Neg` — two's-complement negation (`-x`).");
// `pattern.bit_not(x)` matches bitwise complement (`~x`).  the former BitNot unary-op
// was removed in favour of `Xor(x, all_ones)`; the constructor produces that
// shape directly so `~x` at any width is captured by a single pattern.
unary!(bit_not, "Pattern: bitwise complement (`~x`) — matches the canonical \
    `Xor(x, IntConst(all_ones)):ty` shape.");
// `pattern.not_(x)` is the keyword-collision-renamed alias for
// `bit_not` — the Rust pattern crate keeps `not` since it's not a Rust
// keyword, but `not` is a Python keyword so the Python surface uses
// `not_` (matching the `and_` / `or_` convention above).
unary!(not_ as "not_" => bit_not,
    "Pattern: bitwise complement (`~x`).  Alias for `bit_not` (`not` is a \
     Python keyword).");

// ── Bool binary ops ──────────────────────────────────────────────────────

binary!(bool_and, into,
    "Pattern: `IntBinaryOp::And` at `I1` (boolean `a && b`).  Commutative.");
binary!(bool_or, into,
    "Pattern: `IntBinaryOp::Or` at `I1` (boolean `a || b`).  Commutative.");
binary!(bool_xor, into,
    "Pattern: `IntBinaryOp::Xor` at `I1` (boolean `a ^ b`).  Commutative.");

// ── Bool unary ops ───────────────────────────────────────────────────────

/// Pattern: boolean negation (`!x`) — matches the canonical
/// `Xor(x, IntConst(1)):I1` shape since the former BitNot unary-op was removed
/// in favour of `Xor(_, all_ones)`.  The canonical shape for `a != b` is
/// `bool_not(int_cmp("Equal", a, b))`.
#[pyfunction]
pub fn bool_not(operand: PatLike<'_>) -> PyResult<PyPat> {
    let op = operand.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::bool_not(op)))
}

// ── Float binary ops ─────────────────────────────────────────────────────

binary!(float_add, into,
    "Pattern: `FloatBinaryOp::Add` (`a + b`).  Commutative.");
binary!(float_sub, into,
    "Pattern: float subtraction `a - b`.  No Sub op in the IR; matches the \
     lifter-canonical `FloatAdd(a, Neg(b))` shape.");
binary!(float_mul, into,
    "Pattern: `FloatBinaryOp::Mul` (`a * b`).  Commutative.");
binary!(float_div, into, "Pattern: `FloatBinaryOp::Div` (`a / b`).");

// ── Float unary ops ──────────────────────────────────────────────────────

unary!(float_neg, "Pattern: `FloatUnaryOp::Neg` (`-x`).");
unary!(float_abs, "Pattern: `FloatUnaryOp::Abs` (`fabs(x)`).");
unary!(float_sqrt, "Pattern: `FloatUnaryOp::Sqrt` (`sqrt(x)`).");
unary!(float_ceil, "Pattern: `FloatUnaryOp::Ceil` (`ceil(x)`).");
unary!(float_floor, "Pattern: `FloatUnaryOp::Floor` (`floor(x)`).");
unary!(float_round, "Pattern: `FloatUnaryOp::Round` (`round(x)`).");

/// Pattern: `x` is NaN.  Matches the lifter-canonical IEEE 754
/// self-inequality `BitNot(FloatEqual(x, x))` at `I1` (the shape Sleigh's
/// `FLOAT_NAN` lowers to, and what `x != x` produces).
//
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

binary!(float_eq, "Pattern: `FloatCmpOp::Equal` (`a == b`).  Commutative.");
binary!(float_ne, "Pattern: float `a != b`.  No NotEqual op; matches the \
    lifter-canonical `BitNot(FloatEqual(a, b))` shape (at `I1`).");
binary!(float_lt, "Pattern: `FloatCmpOp::Less` (`a < b`).");
binary!(float_le, "Pattern: float `a <= b`.  No LessEqual op; matches the \
    lifter-canonical `Or(FloatLess(a, b), FloatEqual(a, b))` (NaN-aware) shape.");

// ── Float / int conversions ──────────────────────────────────────────────

unary!(int_to_float, "Pattern: `IntToFloat` — int→float value conversion.");
unary!(float_to_int, "Pattern: `FloatToInt` — float→int value conversion.");
unary!(float_to_float, "Pattern: `FloatToFloat` — float→float (re-width) conversion.");
unary!(int_bits_to_float, "Pattern: `IntBitsToFloat` — reinterpret int bits as a float.");
unary!(float_bits_to_int, "Pattern: `FloatBitsToInt` — reinterpret float bits as an int.");

// ── Cast / coercion / width ops ──────────────────────────────────────────

unary!(truncate, "Pattern: `Truncate` — narrow an integer to a smaller width.");
unary!(popcount, "Pattern: `Popcount` — count of set bits.");
unary!(lzcount, "Pattern: `Lzcount` — count of leading zero bits.");
unary!(zero_extend, "Pattern: `Extend(ZeroExtend)` — zero-extend to a wider width.");
unary!(sign_extend, "Pattern: `Extend(SignExtend)` — sign-extend to a wider width.");

/// `extend(op, operand)` where `op` is "zero" / "zero_extend" / "sign" /
/// "sign_extend".
#[pyfunction]
pub fn extend(op: &str, operand: PatLike<'_>) -> PyResult<PyPat> {
    let extend_op = match op {
        "zero" | "zero_extend" | "ZeroExtend" => strider_ir::ExtendOp::ZeroExtend,
        "sign" | "sign_extend" | "SignExtend" => strider_ir::ExtendOp::SignExtend,
        other => {
            return Err(into_strider_err(anyhow::anyhow!(
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

    /// Filter loads by value width in bits (matches I32 and F32 on
    /// bit_width(32), etc.).
    #[field(arg = "n")]
    bit_width: Option<u32>,

    /// Reject matches where `Function::stack_offset(node)` is `None`.
    /// Capture the matched node via `.capture(c)` and read its SP-relative
    /// offset directly from the `Function::stack_offset` side-table.
    #[field(no_arg_toggle)]
    stack_only: Option<bool>,
}

/// Start a `Load` pattern builder, optionally pre-setting the address
/// operand (see `LoadPat`).
#[pyfunction]
#[pyo3(signature = (addr=None))]
pub fn load(addr: Option<PatLike<'_>>) -> PyResult<PyLoadPat> {
    let b = PyLoadPat::new();
    if let Some(a) = addr {
        let pat = a.into_pat()?;
        b.with_inner(|inner| inner.addr = Some(pat));
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

    /// Filter stores by data width in bits (matches I32 and F32 on
    /// bit_width(32), etc.).
    #[field(arg = "n")]
    bit_width: Option<u32>,

    /// Reject matches where `Function::stack_offset(node)` is `None`.
    /// Capture the matched node via `.capture(c)` and read its SP-relative
    /// offset directly from the `Function::stack_offset` side-table.
    #[field(no_arg_toggle)]
    stack_only: Option<bool>,
}

/// Start a `Store` pattern builder, optionally pre-setting the address
/// and stored-value operands (see `StorePat`).
#[pyfunction]
#[pyo3(signature = (addr=None, data=None))]
pub fn store(addr: Option<PatLike<'_>>, data: Option<PatLike<'_>>) -> PyResult<PyStorePat> {
    let b = PyStorePat::new();
    let addr_pat = addr.map(|a| a.into_pat()).transpose()?;
    let data_pat = data.map(|v| v.into_pat()).transpose()?;
    b.with_inner(|inner| {
        inner.addr = addr_pat;
        inner.data = data_pat;
    });
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
        slf.with_inner(|inner| {
            inner.target = Some(strider_analyze::pattern::int_const(addr));
        });
        slf
    }
    /// Constrain the call target to any address in `addrs`.
    /// Set-membership variant of `at` — fires when the call's target
    /// matches any address in the list.  Equivalent to
    /// `target(int_const_any_of(addrs))`.  An empty list vacuously
    /// fails (matches nothing).
    fn at_any(slf: PyRef<'_, Self>, addrs: Vec<u64>) -> PyRef<'_, Self> {
        slf.with_inner(|inner| {
            inner.target = Some(strider_analyze::pattern::int_const_any_of(addrs));
        });
        slf
    }
}

/// Start a `Call` pattern builder, optionally pinning the call target
/// to literal address `at` (see `CallPat`).
#[pyfunction]
#[pyo3(signature = (at=None))]
pub fn call(at: Option<u64>) -> PyCallPat {
    let b = PyCallPat::new();
    if let Some(addr) = at {
        b.with_inner(|inner| {
            inner.target = Some(strider_analyze::pattern::int_const(addr));
        });
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
    /// or when the ABI's `mem_clobbers` set is empty.
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
        slf.with_inner(|inner| {
            inner.arg.get_or_insert_with(Vec::new).push((0, pat));
        });
        Ok(slf)
    }
    /// Convenience: match `inputs[1]` (memory predecessor).
    fn mem<'py>(
        slf: PyRef<'py, Self>,
        p: PatLike<'py>,
    ) -> PyResult<PyRef<'py, Self>> {
        let pat = p.into_pat()?;
        slf.with_inner(|inner| {
            inner.arg.get_or_insert_with(Vec::new).push((1, pat));
        });
        Ok(slf)
    }
    /// Convenience: match `outputs[0]` (control output).
    fn ctrl_out<'py>(
        slf: PyRef<'py, Self>,
        p: PatLike<'py>,
    ) -> PyResult<PyRef<'py, Self>> {
        let pat = p.into_pat()?;
        slf.with_inner(|inner| {
            inner.ret.get_or_insert_with(Vec::new).push((0, pat));
        });
        Ok(slf)
    }
    /// Convenience: match `outputs[1]` (memory output; dangles when
    /// the ABI's `mem_clobbers` set is empty).
    fn mem_out<'py>(
        slf: PyRef<'py, Self>,
        p: PatLike<'py>,
    ) -> PyResult<PyRef<'py, Self>> {
        let pat = p.into_pat()?;
        slf.with_inner(|inner| {
            inner.ret.get_or_insert_with(Vec::new).push((1, pat));
        });
        Ok(slf)
    }
}

/// Start a `CallOther` pattern builder (see `CallOtherPat`).
#[pyfunction]
pub fn call_other() -> PyCallOtherPat {
    PyCallOtherPat::new()
}

// ── RetPat ───────────────────────────────────────────────────────────────

/// Typed builder for `Return` node patterns.  Chain `.preceded_by(p)`
/// to match Returns whose direct ctrl predecessor is `p` (typically a
/// `Region` after a Call), and `.ret_val(idx, p)` to constrain
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
    /// node producing input slot 0 — typically a `Region` at a
    /// region header).  Single-step match, not a backward walk.
    #[field(accepts = "Pat", arg = "p")]
    preceded_by: Option<strider_analyze::pattern::Pat>,

    /// Constrain return value at ABI position `idx` (0-based after
    /// the ctrl and mem inputs — i.e. mapped to the Return's input
    /// slot `2 + idx`).
    #[field(multi, accepts = "Pat", arg = "idx")]
    ret_val: Option<Vec<(usize, strider_analyze::pattern::Pat)>>,
}

/// Start a `Return` pattern builder (see `RetPat`).
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

/// Start an `If` pattern builder, optionally pre-setting the condition
/// operand (see `IfPat`).
#[pyfunction]
#[pyo3(signature = (cond=None))]
pub fn if_(cond: Option<PatLike<'_>>) -> PyResult<PyIfPat> {
    let b = PyIfPat::new();
    if let Some(c) = cond {
        let pat = c.into_pat()?;
        b.with_inner(|inner| inner.cond = Some(pat));
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
    static TABLE: &[(&str, strider_ir::IntBinaryOp)] = &[
        ("Add", Add),
        ("Mul", Mul),
        ("Div", Div),
        ("Sdiv", Sdiv),
        ("Rem", Rem),
        ("Srem", Srem),
        ("And", And),
        ("Or", Or),
        ("Xor", Xor),
        ("ShiftLeft", ShiftLeft),
        ("ShiftRight", ShiftRight),
        ("SShiftRight", SShiftRight),
        ("shl", ShiftLeft),
        ("shr", ShiftRight),
        ("sshr", SShiftRight),
    ];
    lookup_op(TABLE, name, "IntBinaryOp")
}

fn parse_bool_binary_op(name: &str) -> PyResult<strider_ir::IntBinaryOp> {
    // Booleans are the 1-bit integer `I1`; logical and/or/xor are the
    // corresponding `IntBinaryOp` variants at `I1`.
    use strider_ir::IntBinaryOp::*;
    static TABLE: &[(&str, strider_ir::IntBinaryOp)] = &[
        ("And", And),
        ("Or", Or),
        ("Xor", Xor),
    ];
    lookup_op(TABLE, name, "boolean binary op")
}

fn parse_float_binary_op(name: &str) -> PyResult<strider_ir::FloatBinaryOp> {
    // `Sub` is deliberately absent: `FloatBinaryOp::Sub` is not a primitive.
    // Python callers wanting `a - b` should use `pattern.float_sub(a, b)`,
    // which constructs the lowered `FloatAdd(a, FloatUnaryOp::Neg(b))` shape.
    use strider_ir::FloatBinaryOp::*;
    static TABLE: &[(&str, strider_ir::FloatBinaryOp)] = &[
        ("Add", Add),
        ("Mul", Mul),
        ("Div", Div),
    ];
    lookup_op(TABLE, name, "FloatBinaryOp")
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

/// Typed builder for a boolean binary-op pattern.  Booleans are 1-bit
/// integers (`I1`) in this IR, so this builds an `IntBinaryOp`
/// (`And` / `Or` / `Xor`) whose output is `I1` and carries an
/// `I1`-output post-match guard (so it never matches a same-shaped wide
/// integer op).  Symmetric with `int_binary` / `float_binary`: chain
/// `.ordered()` to disable commutative matching.
#[strider_pattern(
    rust_name = "PyBoolBinaryPat",
    py_name = "BoolBinaryPat",
    py_module = "strider.pattern",
    base_builder = "bool_binary",
    node_phrase = "bool-binary node",
    constructor_args = "op: strider_ir::IntBinaryOp, lhs: strider_analyze::pattern::Pat, rhs: strider_analyze::pattern::Pat",
)]
pub struct BoolBinaryPatDef {
    /// Force the pattern to match operands in the stated order only.
    /// By default, commutative variants of the op family also try the
    /// reversed operand order.  Terminal — finalises to a [`Pat`] and
    /// does NOT chain (return type is `Pat`, not `BoolBinaryPat`).
    #[field(terminal)]
    ordered: Option<bool>,
}

/// Build an `IntBinaryOp` pattern for the named `op` (e.g. `"Add"`,
/// `"And"`, `"ShiftLeft"` / `"shl"`).  Returns an `IntBinaryPat` so you
/// can chain `.ordered()` to disable commutative matching.  `Sub` is
/// not a valid op — use `sub(a, b)`.  Raises `StriderError` on an
/// unknown op name.
#[pyfunction]
pub fn int_binary(op: &str, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyIntBinaryPat> {
    Ok(PyIntBinaryPat::new(
        parse_int_binary_op(op)?,
        l.into_pat()?,
        r.into_pat()?,
    ))
}

/// Build a boolean binary pattern for the named `op` (`"And"`, `"Or"`,
/// `"Xor"`).  Booleans are 1-bit integers, so this matches the
/// corresponding `IntBinaryOp` at `I1` (commutative — both orderings).
/// Returns a `BoolBinaryPat` so you can chain `.ordered()` to disable
/// commutative matching, symmetric with `int_binary` / `float_binary`.
/// The `I1`-output constraint is preserved regardless.  Raises
/// `StriderError` on an unknown op name.
#[pyfunction]
pub fn bool_binary(op: &str, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyBoolBinaryPat> {
    Ok(PyBoolBinaryPat::new(
        parse_bool_binary_op(op)?,
        l.into_pat()?,
        r.into_pat()?,
    ))
}

/// Build a `FloatBinaryOp` pattern for the named `op` (`"Add"`, `"Mul"`,
/// `"Div"`).  Returns a `FloatBinaryPat` (chain `.ordered()` to disable
/// commutative matching).  `Sub` is not a valid op — use
/// `float_sub(a, b)`.  Raises `StriderError` on an unknown op name.
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
// `bool_bin_any`, `float_bin_any`, `float_un_any`,
// `float_cmp_any`.  Each takes a `Capture` for the matched op variant
// — recover the op via `Match.*_op(capture)` once those accessors land.
/// Match any `IntBinaryOp` over `(l, r)` and bind the op variant to `c`.
/// Recover the variant after a match via `Match.int_binary_op(c)`.
#[pyfunction]
pub fn int_bin_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::int_binary_any(c.inner, lp, rp)))
}

/// Match any `IntUnaryOp` over `operand` and bind the op variant to `c`.
/// Recover via `Match.int_unary_op(c)`.
#[pyfunction]
pub fn int_un_any(c: PyRef<'_, PyCapture>, operand: PatLike<'_>) -> PyResult<PyPat> {
    let p = operand.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::int_unary_any(c.inner, p)))
}

/// Match any `IntCmpOp` over `(l, r)` and bind the op variant to `c`.
/// Recover via `Match.int_cmp_op(c)`.
#[pyfunction]
pub fn int_cmp_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::int_cmp_any(c.inner, lp, rp)))
}

/// Match any boolean binary op (an `IntBinaryOp` — `And`/`Or`/`Xor` — at `I1`)
/// over `(l, r)` and bind the op variant to `c`.
/// Recover via `Match.bool_binary_op(c)`.
#[pyfunction]
pub fn bool_bin_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::bool_binary_any(c.inner, lp, rp)))
}

// Note: there is no `bool_un_any` constructor.  A boolean logical NOT
// is `Xor(x, IntConst(1)):I1` since the former BitNot unary-op was removed in
// favour of `Xor(_, all_ones)`.  Use `bool_bin_any(c, operand, bool_const(true))`
// to match any boolean unary shape (a bool_binary_any whose RHS is the
// I1 all-ones constant).

/// Match any `FloatBinaryOp` over `(l, r)` and bind the op variant to `c`.
/// Recover via `Match.float_binary_op(c)`.
#[pyfunction]
pub fn float_bin_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::float_binary_any(c.inner, lp, rp)))
}

/// Match any `FloatUnaryOp` over `operand` and bind the op variant to `c`.
/// Recover via `Match.float_unary_op(c)`.
#[pyfunction]
pub fn float_un_any(c: PyRef<'_, PyCapture>, operand: PatLike<'_>) -> PyResult<PyPat> {
    let p = operand.into_pat()?;
    Ok(PyPat::from_pat(strider_analyze::pattern::float_unary_any(c.inner, p)))
}

/// Match any `FloatCmpOp` over `(l, r)` and bind the op variant to `c`.
/// Recover via `Match.float_cmp_op(c)`.
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
    m.add_class::<PyFloatBinaryPat>()?;
    m.add_class::<PyBoolBinaryPat>()?;
    m.add_class::<PyCallPat>()?;
    m.add_class::<PyCallOtherPat>()?;
    m.add_class::<PyRetPat>()?;
    m.add_class::<PyIfPat>()?;
    m.add_class::<PyLoadPat>()?;
    m.add_class::<PyStorePat>()?;
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
    add_fn!(value_of_width);
    add_fn!(bool_value);
    add_fn!(inputs_of_width);
    add_fn!(bool_inputs);
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
    add_fn!(truncate);
    add_fn!(popcount);
    add_fn!(lzcount);
    add_fn!(zero_extend);
    add_fn!(sign_extend);
    add_fn!(extend);
    // memory / control
    add_fn!(load);
    add_fn!(store);
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
    add_fn!(float_bin_any);
    add_fn!(float_un_any);
    add_fn!(float_cmp_any);

    parent.add_submodule(&m)?;
    let sys = py.import_bound("sys")?;
    sys.getattr("modules")?.set_item("strider.pattern", &m)?;
    Ok(())
}

// ── PyFunctionArgPat: capture/cap/when/into_pat finaliser ────────────────
//
// `PyFunctionArgPat` is the only hand-written builder whose finaliser
// methods aren't emitted by `#[strider_pattern]` (it stays hand-written
// because its `source_register` / `source_stack` setters share one
// underlying field via enum-dispatch, which the macro's per-field
// shape doesn't model).  Every other typed builder gets these four
// methods from the proc-macro.  This separate `#[pymethods]` block
// relies on PyO3's `multiple-pymethods` feature.
#[pymethods]
impl PyFunctionArgPat {
    /// Capture this pattern's matched node under the given [`Capture`].
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
    /// Finalise into a [`PyPat`].  Most call sites accept a builder
    /// directly via `PatLike`, so explicit `.into_pat()` is rarely
    /// needed.
    fn into_pat(&self) -> PyPat {
        PyPat::from_pat(self.finalise())
    }
}
