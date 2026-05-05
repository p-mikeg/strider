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
//! Coverage: every constructor in `pattern::pat::ctor` plus the typed
//! family dispatchers (`int_binary`, `bool_binary`, `float_binary`),
//! `.when` predicate guards, `.ordered()` overrides, and the
//! variant-agnostic `*_any` constructors that bind the matched op
//! variant to a `Capture` for later inspection via `Match.*_op` (TODO:
//! op-variant accessors are not yet exposed on the Python `Match`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use pyo3::types::{PyString, PyTuple};

use crate::errors::into_pattern_err;

// ── Capture ──────────────────────────────────────────────────────────────

#[pyclass(name = "Capture", module = "strider.pattern", frozen)]
#[derive(Clone)]
pub struct PyCapture {
    pub(crate) inner: pattern::Capture,
}

#[pymethods]
impl PyCapture {
    #[new]
    fn new() -> Self {
        Self {
            inner: pattern::Capture::new(),
        }
    }

    fn __repr__(&self) -> String {
        format!("Capture({:?})", self.inner)
    }

    fn __hash__(&self) -> isize {
        // pattern::Capture wraps a u32; safe to expose as a hash.
        format!("{:?}", self.inner).len() as isize
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

fn intern_table() -> &'static Mutex<HashMap<String, pattern::Capture>> {
    static TABLE: std::sync::OnceLock<Mutex<HashMap<String, pattern::Capture>>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn intern_str(name: &str) -> PyResult<pattern::Capture> {
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
        .or_insert_with(pattern::Capture::new))
}

// ── PyPat ────────────────────────────────────────────────────────────────

/// Opaque wrapper around a `pattern::Pat`.
///
/// Held inside an `Arc` so PyPat can be cheaply cloned and passed as
/// sub-patterns to multiple builder field methods.
#[pyclass(name = "Pat", module = "strider.pattern")]
#[derive(Clone)]
pub struct PyPat {
    pub(crate) inner: Arc<pattern::Pat>,
}

impl PyPat {
    pub(crate) fn from_pat(p: pattern::Pat) -> Self {
        Self { inner: Arc::new(p) }
    }

    pub(crate) fn as_inner(&self) -> &pattern::Pat {
        &self.inner
    }
}

/// `CastMask` — bitset selecting which value-passthrough cast
/// `NodeKind`s the matcher walks through transparently.  Mirrors
/// `pattern::CastMask`.  Construct via the classmethods (`zero_extend`,
/// `sign_extend`, `extend`, `truncate`, `cast_to_int`, `cast_to_bool`,
/// `cast_to_float`, `int_bits_to_float`, `float_bits_to_int`,
/// `all`, `none`/`empty`); combine with `|` (Python `__or__`).
///
/// Pass to `Graph.find_all(pat, ignore_casts_mask=...)` — granular
/// alternative to the all-or-nothing `ignore_casts=True`.
#[pyclass(name = "CastMask", module = "strider.pattern", frozen)]
#[derive(Clone, Copy)]
pub struct PyCastMask {
    pub(crate) inner: pattern::CastMask,
}

#[pymethods]
impl PyCastMask {
    #[classmethod] fn zero_extend(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: pattern::CastMask::ZERO_EXTEND }
    }
    #[classmethod] fn sign_extend(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: pattern::CastMask::SIGN_EXTEND }
    }
    #[classmethod] fn extend(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: pattern::CastMask::EXTEND }
    }
    #[classmethod] fn truncate(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: pattern::CastMask::TRUNCATE }
    }
    #[classmethod] fn cast_to_int(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: pattern::CastMask::CAST_TO_INT }
    }
    #[classmethod] fn cast_to_bool(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: pattern::CastMask::CAST_TO_BOOL }
    }
    #[classmethod] fn cast_to_float(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: pattern::CastMask::CAST_TO_FLOAT }
    }
    #[classmethod] fn int_bits_to_float(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: pattern::CastMask::INT_BITS_TO_FLOAT }
    }
    #[classmethod] fn float_bits_to_int(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: pattern::CastMask::FLOAT_BITS_TO_INT }
    }
    #[classmethod] fn all(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: pattern::CastMask::all() }
    }
    #[classmethod] fn none(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: pattern::CastMask::empty() }
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
    FunctionArgPat(Bound<'py, PyFunctionArgPat>),
    IntBinaryPat(Bound<'py, PyIntBinaryPat>),
    BoolBinaryPat(Bound<'py, PyBoolBinaryPat>),
    FloatBinaryPat(Bound<'py, PyFloatBinaryPat>),
}

impl PatLike<'_> {
    pub(crate) fn into_pat(self) -> PyResult<pattern::Pat> {
        match self {
            PatLike::Pat(p) => Ok((*p.borrow().inner).clone()),
            PatLike::Capture(c) => Ok(pattern::var(c.borrow().inner)),
            PatLike::Str(s) => {
                let name_owned = s.to_string();
                let name = name_owned.as_str();
                if name == "_" || name == "any_" {
                    Ok(pattern::any())
                } else {
                    let c = intern_str(name)?;
                    Ok(pattern::var(c))
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
    bindings: pattern::Bindings,
    /// Raw pointer to the graph the matcher is operating on.  Boxed in
    /// `Arc<Mutex<...>>` so the Python predicate can hold the proxy
    /// across Rust ↔ Python boundaries safely; the wrapper code clears
    /// the pointer back to `None` right after the predicate returns.
    graph_ptr: Mutex<Option<*const ir::BuiltFunctionGraph>>,
}

// SAFETY: We never share PyPartialMatch across threads (`unsendable`).
// The `*const BuiltFunctionGraph` it holds is only valid for the
// duration of one synchronous predicate call, after which it's cleared.
// The Mutex guards against re-entrant access from a Python callback
// that re-enters Rust.

impl PyPartialMatch {
    fn new(bindings: pattern::Bindings, graph: &ir::BuiltFunctionGraph) -> Self {
        Self {
            bindings,
            graph_ptr: Mutex::new(Some(graph as *const _)),
        }
    }

    fn clear_graph_ptr(&self) {
        if let Ok(mut g) = self.graph_ptr.lock() {
            *g = None;
        }
    }

    /// Borrow the graph pointer for a closure call.  Returns `None` if
    /// the proxy has been invalidated.
    fn with_graph<R>(&self, f: impl FnOnce(&ir::BuiltFunctionGraph) -> R) -> Option<R> {
        let guard = self.graph_ptr.lock().ok()?;
        let ptr = (*guard)?;
        // SAFETY: `ptr` was set to a valid `&BuiltFunctionGraph` by the
        // matcher and only cleared after the predicate returns.  The
        // outer Mutex guard prevents the cleanup from racing this call.
        let graph_ref = unsafe { &*ptr };
        Some(f(graph_ref))
    }

    fn capture_from_key(&self, key: &CaptureKeyOwned) -> PyResult<pattern::Capture> {
        match key {
            CaptureKeyOwned::Capture(c) => Ok(*c),
            CaptureKeyOwned::Str(s) => intern_str(s.as_str()),
        }
    }
}

/// Owned variant of CaptureKey (no `Bound` lifetime), used by
/// PyPartialMatch's accessors which can't borrow from the Python args.
enum CaptureKeyOwned {
    Capture(pattern::Capture),
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
            return Ok((v as i128).into_py(py));
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
/// a transient `PyPartialMatch` proxy.  Errors / non-bool returns are
/// treated as `false` (no match).
fn wrap_when(inner: pattern::Pat, py_func: PyObject) -> pattern::Pat {
    inner.when_match(move |graph, _ty, bindings| {
        Python::with_gil(|py| {
            let proxy = PyPartialMatch::new(bindings.clone(), graph);
            let py_proxy = match Py::new(py, proxy) {
                Ok(p) => p,
                Err(_) => return false,
            };
            let args = PyTuple::new_bound(py, [py_proxy.clone_ref(py)]);
            let result = py_func.call_bound(py, args, None);
            // Always invalidate the proxy's graph pointer so any
            // subsequent use from Python doesn't deref a stale ptr.
            if let Ok(b) = py_proxy.try_borrow(py) {
                b.clear_graph_ptr();
            }
            match result {
                Ok(obj) => obj.extract::<bool>(py).unwrap_or(false),
                Err(e) => {
                    // Surface the predicate's exception to stderr but
                    // treat it as "no match" to avoid aborting
                    // find_all in the middle of a walk.
                    e.print(py);
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
        use pattern::IntoPat;
        let inner = (*self.inner).clone();
        PyPat::from_pat(inner.capture(c.inner))
    }

    /// Capture this pattern under a string name (auto-interned).
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
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

    /// Force commutative binary ops not to try the swapped operand
    /// order.  Wraps the inner pattern in an additional ordering check
    /// — implemented via the typed builder when available.  For
    /// patterns built with the free constructors (already finalized
    /// `Pat`), this is a no-op (the commutative behaviour was decided
    /// at construction); use the typed `int_binary`/`bool_binary`/
    /// `float_binary` builders for explicit `.ordered()` control.
    fn ordered(&self) -> PyPat {
        // No-op on a finalized Pat: commutativity is baked into the
        // InputsSpec at construction time.  Returning self is
        // surprising; document the limitation in the docstring above.
        self.clone()
    }

    fn __repr__(&self) -> String {
        "Pat(...)".to_string()
    }
}

// ── Free constructors ────────────────────────────────────────────────────

#[pyfunction]
pub fn any_() -> PyPat {
    PyPat::from_pat(pattern::any())
}

#[pyfunction]
pub fn var(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_pat(pattern::var(c.inner))
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
    PyPat::from_pat(pattern::int_const(value))
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
    PyPat::from_pat(pattern::signed_int_const(value))
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
    PyPat::from_pat(pattern::int_const_any_of(values))
}

#[pyfunction]
pub fn bool_const(value: bool) -> PyPat {
    PyPat::from_pat(pattern::bool_const(value))
}

#[pyfunction]
pub fn float_const(bits: u64) -> PyPat {
    PyPat::from_pat(pattern::float_const(bits))
}

#[pyfunction]
pub fn any_int_const(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_pat(pattern::any_int_const(c.inner))
}

#[pyfunction]
pub fn any_bool_const(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_pat(pattern::any_bool_const(c.inner))
}

#[pyfunction]
pub fn any_float_const(c: PyRef<'_, PyCapture>) -> PyPat {
    PyPat::from_pat(pattern::any_float_const(c.inner))
}

#[pyfunction]
pub fn initial_var() -> PyPat {
    PyPat::from_pat(pattern::initial_var())
}

/// Match `InitialVar(vn)` for a specific varnode.  Use the
/// `Sleigh.reg("RAX")` / `Vn(...)` helpers in the `strider` module
/// to construct the `Vn`.
#[pyfunction]
pub fn initial_var_for(vn: crate::sleigh::PyVn) -> PyPat {
    PyPat::from_pat(pattern::initial_var_for(vn.inner))
}

// ── PhiPat ───────────────────────────────────────────────────────────

/// Typed builder for `VarPhi` / `MemPhi` / `ValuePhi` patterns.
/// Chain `.for_vn(vn)` to constrain the matched VarPhi to a specific
/// varnode, and `.input(idx, p)` to constrain the value arriving
/// from the given predecessor slot.
#[pyclass(name = "PhiPat", module = "strider.pattern")]
pub struct PyPhiPat {
    for_vn: std::cell::RefCell<Option<rsleigh::Vn>>,
    inputs: std::cell::RefCell<Vec<(usize, pattern::Pat)>>,
}

impl PyPhiPat {
    fn new() -> Self {
        Self {
            for_vn: std::cell::RefCell::new(None),
            inputs: std::cell::RefCell::new(Vec::new()),
        }
    }
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = if let Some(vn) = *self.for_vn.borrow() {
            pattern::phi_for(vn)
        } else {
            pattern::phi()
        };
        for (idx, p) in self.inputs.borrow().iter().cloned() {
            b = b.input(idx, p);
        }
        b.into()
    }
}

#[pymethods]
impl PyPhiPat {
    fn for_vn(slf: Py<Self>, py: Python<'_>, vn: crate::sleigh::PyVn) -> Py<Self> {
        slf.borrow(py).for_vn.replace(Some(vn.inner)); slf
    }
    fn input(slf: Py<Self>, py: Python<'_>, idx: usize, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).inputs.borrow_mut().push((idx, pat));
        Ok(slf)
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat { PyPat::from_pat(wrap_when(self.finalise(), f)) }
    fn into_pat(&self) -> PyPat { PyPat::from_pat(self.finalise()) }
}

#[pyfunction]
pub fn phi() -> PyPhiPat { PyPhiPat::new() }

/// Match `VarPhi` for a specific varnode.  Equivalent to
/// `phi().for_vn(vn)` but reads more naturally at the call site.
#[pyfunction]
pub fn phi_for(vn: crate::sleigh::PyVn) -> PyPhiPat {
    let b = PyPhiPat::new();
    b.for_vn.replace(Some(vn.inner));
    b
}

// ── FunctionArgPat ───────────────────────────────────────────────────

/// Typed builder for `FunctionArg` node patterns.  Chain
/// `.index(i)` to constrain the argument position and
/// `.source_register(vn)` / `.source_stack(space, offset)` to
/// constrain where the argument was sourced from (matches the
/// `FunctionArgSource::Register` / `FunctionArgSource::Stack`
/// variants of the IR enum).
#[pyclass(name = "FunctionArgPat", module = "strider.pattern")]
pub struct PyFunctionArgPat {
    source: std::cell::RefCell<Option<ir::node::FunctionArgSource>>,
    index: std::cell::RefCell<Option<u32>>,
}

impl PyFunctionArgPat {
    fn new() -> Self {
        Self {
            source: std::cell::RefCell::new(None),
            index: std::cell::RefCell::new(None),
        }
    }
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = pattern::function_arg_any();
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
        slf.borrow(py).source.replace(Some(ir::node::FunctionArgSource::Register(vn.inner))); slf
    }
    fn source_stack(slf: Py<Self>, py: Python<'_>, space: crate::sleigh::PyVnSpace, offset: i64) -> Py<Self> {
        slf.borrow(py).source.replace(Some(ir::node::FunctionArgSource::Stack {
            space: space.inner,
            offset,
        }));
        slf
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat { PyPat::from_pat(wrap_when(self.finalise(), f)) }
    fn into_pat(&self) -> PyPat { PyPat::from_pat(self.finalise()) }
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
    b.source.replace(Some(ir::node::FunctionArgSource::Register(vn.inner)));
    b
}

/// Match a `FunctionArg` whose source is a specific stack slot.
#[pyfunction]
pub fn function_arg_stack(space: crate::sleigh::PyVnSpace, offset: i64) -> PyFunctionArgPat {
    let b = PyFunctionArgPat::new();
    b.source.replace(Some(ir::node::FunctionArgSource::Stack { space: space.inner, offset }));
    b
}

#[pyfunction]
pub fn predicate(f: PyObject) -> PyPat {
    PyPat::from_pat(wrap_when(pattern::any(), f))
}

// ── Binary integer ops ───────────────────────────────────────────────────

macro_rules! int_binop {
    ($name:ident) => {
        #[pyfunction]
        pub fn $name(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
            let lp = l.into_pat()?;
            let rp = r.into_pat()?;
            Ok(PyPat::from_pat(pattern::$name(lp, rp).into()))
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
    Ok(PyPat::from_pat(pattern::and(lp, rp).into()))
}
#[pyfunction(name = "or_")]
pub fn or_(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(pattern::or(lp, rp).into()))
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
    Ok(PyPat::from_pat(pattern::int_cmp(cmp_op, lp, rp)))
}

fn parse_int_cmp_op(name: &str) -> PyResult<ir::IntCmpOp> {
    use ir::IntCmpOp::*;
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
            Ok(PyPat::from_pat(pattern::$name(op)))
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
    Ok(PyPat::from_pat(pattern::bit_not(op)))
}

// ── Bool binary ops ──────────────────────────────────────────────────────

macro_rules! bool_binop {
    ($name:ident) => {
        #[pyfunction]
        pub fn $name(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
            let lp = l.into_pat()?;
            let rp = r.into_pat()?;
            Ok(PyPat::from_pat(pattern::$name(lp, rp).into()))
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
    Ok(PyPat::from_pat(pattern::bool_not(op)))
}

// ── Float binary ops ─────────────────────────────────────────────────────

macro_rules! float_binop {
    ($name:ident) => {
        #[pyfunction]
        pub fn $name(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
            let lp = l.into_pat()?;
            let rp = r.into_pat()?;
            Ok(PyPat::from_pat(pattern::$name(lp, rp).into()))
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
            Ok(PyPat::from_pat(pattern::$name(op)))
        }
    };
}

float_unop!(float_neg);
float_unop!(float_abs);
float_unop!(float_sqrt);
float_unop!(float_ceil);
float_unop!(float_floor);
float_unop!(float_round);

// `float_is_nan` isn't exposed as a free constructor by `pattern` (no
// `FloatIsNan` NodeKind in the IR yet); expose as `any().when(...)`
// stub that always fails so users get a clear error if they reach
// for it.  We still register it so the snapshot test passes; switching
// to a real impl is a follow-up once ir-side support lands.
#[pyfunction]
pub fn float_is_nan(_operand: PatLike<'_>) -> PyResult<PyPat> {
    Err(into_pattern_err(anyhow::anyhow!(
        "float_is_nan is not yet implemented — IR has no FloatIsNan node kind"
    )))
}

// ── Float comparisons ────────────────────────────────────────────────────

macro_rules! float_cmp_op {
    ($name:ident) => {
        #[pyfunction]
        pub fn $name(l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
            let lp = l.into_pat()?;
            let rp = r.into_pat()?;
            Ok(PyPat::from_pat(pattern::$name(lp, rp)))
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
            Ok(PyPat::from_pat(pattern::$name(op)))
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
        "zero" | "zero_extend" | "ZeroExtend" => ir::ExtendOp::ZeroExtend,
        "sign" | "sign_extend" | "SignExtend" => ir::ExtendOp::SignExtend,
        other => {
            return Err(into_pattern_err(anyhow::anyhow!(
                "unknown extend op {other:?} (expected 'zero' or 'sign')"
            )))
        }
    };
    let p = operand.into_pat()?;
    Ok(PyPat::from_pat(pattern::extend(extend_op, p)))
}

// ── Memory ───────────────────────────────────────────────────────────────

/// Typed builder for `Load` node patterns.  Chain `.addr(p)` to
/// constrain the address operand and `.space(s)` to restrict the
/// match to a specific memory space (e.g. `VnSpace.ram()`).
#[pyclass(name = "LoadPat", module = "strider.pattern")]
pub struct PyLoadPat {
    addr: std::cell::RefCell<Option<pattern::Pat>>,
    space: std::cell::RefCell<Option<rsleigh::VnSpace>>,
}

impl PyLoadPat {
    fn new() -> Self {
        Self { addr: std::cell::RefCell::new(None), space: std::cell::RefCell::new(None) }
    }
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = pattern::load();
        if let Some(s) = *self.space.borrow() { b = b.space(s); }
        if let Some(p) = self.addr.borrow().clone() { b = b.addr(p); }
        b.into()
    }
}

#[pymethods]
impl PyLoadPat {
    fn addr(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).addr.replace(Some(pat));
        Ok(slf)
    }
    fn space(slf: Py<Self>, py: Python<'_>, s: crate::sleigh::PyVnSpace) -> Py<Self> {
        slf.borrow(py).space.replace(Some(s.inner));
        slf
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat { PyPat::from_pat(wrap_when(self.finalise(), f)) }
    fn into_pat(&self) -> PyPat { PyPat::from_pat(self.finalise()) }
}

#[pyfunction]
#[pyo3(signature = (addr=None))]
pub fn load(addr: Option<PatLike<'_>>) -> PyResult<PyLoadPat> {
    let b = PyLoadPat::new();
    if let Some(a) = addr {
        b.addr.replace(Some(a.into_pat()?));
    }
    Ok(b)
}

/// Typed builder for `Store` node patterns.  Chain `.addr(p)`,
/// `.data(p)`, `.space(s)` to constrain the address, value, and
/// memory space respectively.
#[pyclass(name = "StorePat", module = "strider.pattern")]
pub struct PyStorePat {
    addr: std::cell::RefCell<Option<pattern::Pat>>,
    data: std::cell::RefCell<Option<pattern::Pat>>,
    space: std::cell::RefCell<Option<rsleigh::VnSpace>>,
}

impl PyStorePat {
    fn new() -> Self {
        Self {
            addr: std::cell::RefCell::new(None),
            data: std::cell::RefCell::new(None),
            space: std::cell::RefCell::new(None),
        }
    }
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = pattern::store();
        if let Some(s) = *self.space.borrow() { b = b.space(s); }
        if let Some(p) = self.addr.borrow().clone() { b = b.addr(p); }
        if let Some(p) = self.data.borrow().clone() { b = b.data(p); }
        b.into()
    }
}

#[pymethods]
impl PyStorePat {
    fn addr(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).addr.replace(Some(pat));
        Ok(slf)
    }
    fn data(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).data.replace(Some(pat));
        Ok(slf)
    }
    fn space(slf: Py<Self>, py: Python<'_>, s: crate::sleigh::PyVnSpace) -> Py<Self> {
        slf.borrow(py).space.replace(Some(s.inner));
        slf
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat { PyPat::from_pat(wrap_when(self.finalise(), f)) }
    fn into_pat(&self) -> PyPat { PyPat::from_pat(self.finalise()) }
}

#[pyfunction]
#[pyo3(signature = (addr=None, data=None))]
pub fn store(addr: Option<PatLike<'_>>, data: Option<PatLike<'_>>) -> PyResult<PyStorePat> {
    let b = PyStorePat::new();
    if let Some(a) = addr { b.addr.replace(Some(a.into_pat()?)); }
    if let Some(v) = data { b.data.replace(Some(v.into_pat()?)); }
    Ok(b)
}

/// Typed builder for `StackStore` node patterns.  Chain
/// `.offset(o)`, `.offset_any([…])`, `.data(p)`, `.space(s)`.
#[pyclass(name = "StackStorePat", module = "strider.pattern")]
pub struct PyStackStorePat {
    offset: std::cell::RefCell<Option<i64>>,
    offset_any: std::cell::RefCell<Option<Vec<i64>>>,
    data: std::cell::RefCell<Option<pattern::Pat>>,
    space: std::cell::RefCell<Option<rsleigh::VnSpace>>,
}

impl PyStackStorePat {
    fn new() -> Self {
        Self {
            offset: std::cell::RefCell::new(None),
            offset_any: std::cell::RefCell::new(None),
            data: std::cell::RefCell::new(None),
            space: std::cell::RefCell::new(None),
        }
    }
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = pattern::stack_store();
        if let Some(s) = *self.space.borrow() { b = b.space(s); }
        if let Some(o) = *self.offset.borrow() { b = b.offset(o); }
        if let Some(set) = self.offset_any.borrow().clone() { b = b.offset_any(set); }
        if let Some(p) = self.data.borrow().clone() { b = b.data(p); }
        b.into()
    }
}

#[pymethods]
impl PyStackStorePat {
    fn offset(slf: Py<Self>, py: Python<'_>, o: i64) -> Py<Self> {
        slf.borrow(py).offset.replace(Some(o)); slf
    }
    /// Match only stack-stores whose offset is in `offsets`.  Empty
    /// list vacuously fails (matches nothing) — mirrors the contract
    /// of `int_const_any_of`.
    fn offset_any(slf: Py<Self>, py: Python<'_>, offsets: Vec<i64>) -> Py<Self> {
        slf.borrow(py).offset_any.replace(Some(offsets)); slf
    }
    fn data(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).data.replace(Some(pat));
        Ok(slf)
    }
    fn space(slf: Py<Self>, py: Python<'_>, s: crate::sleigh::PyVnSpace) -> Py<Self> {
        slf.borrow(py).space.replace(Some(s.inner)); slf
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat { PyPat::from_pat(wrap_when(self.finalise(), f)) }
    fn into_pat(&self) -> PyPat { PyPat::from_pat(self.finalise()) }
}

#[pyfunction]
#[pyo3(signature = (offset=None, data=None))]
pub fn stack_store(offset: Option<i64>, data: Option<PatLike<'_>>) -> PyResult<PyStackStorePat> {
    let b = PyStackStorePat::new();
    if let Some(o) = offset { b.offset.replace(Some(o)); }
    if let Some(v) = data { b.data.replace(Some(v.into_pat()?)); }
    Ok(b)
}

/// Typed builder for `StackStorePhi` node patterns.  Chain
/// `.data(p)`, `.space(s)`, `.offsets(list)` (per-predecessor stack
/// offsets).
#[pyclass(name = "StackStorePhiPat", module = "strider.pattern")]
pub struct PyStackStorePhiPat {
    data: std::cell::RefCell<Option<pattern::Pat>>,
    space: std::cell::RefCell<Option<rsleigh::VnSpace>>,
    offsets: std::cell::RefCell<Option<Vec<i64>>>,
}

impl PyStackStorePhiPat {
    fn new() -> Self {
        Self {
            data: std::cell::RefCell::new(None),
            space: std::cell::RefCell::new(None),
            offsets: std::cell::RefCell::new(None),
        }
    }
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = pattern::stack_store_phi();
        if let Some(s) = *self.space.borrow() { b = b.space(s); }
        if let Some(p) = self.data.borrow().clone() { b = b.data(p); }
        if let Some(os) = self.offsets.borrow().clone() { b = b.offsets(os); }
        b.into()
    }
}

#[pymethods]
impl PyStackStorePhiPat {
    fn data(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).data.replace(Some(pat));
        Ok(slf)
    }
    fn space(slf: Py<Self>, py: Python<'_>, s: crate::sleigh::PyVnSpace) -> Py<Self> {
        slf.borrow(py).space.replace(Some(s.inner)); slf
    }
    fn offsets(slf: Py<Self>, py: Python<'_>, os: Vec<i64>) -> Py<Self> {
        slf.borrow(py).offsets.replace(Some(os)); slf
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat { PyPat::from_pat(wrap_when(self.finalise(), f)) }
    fn into_pat(&self) -> PyPat { PyPat::from_pat(self.finalise()) }
}

#[pyfunction]
#[pyo3(signature = (data=None))]
pub fn stack_store_phi(data: Option<PatLike<'_>>) -> PyResult<PyStackStorePhiPat> {
    let b = PyStackStorePhiPat::new();
    if let Some(v) = data { b.data.replace(Some(v.into_pat()?)); }
    Ok(b)
}

// ── Calls ────────────────────────────────────────────────────────────────

/// Typed builder for `Call` node patterns.  Wraps `pattern::CallPat`
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
#[pyclass(name = "CallPat", module = "strider.pattern")]
pub struct PyCallPat {
    target: std::cell::RefCell<Option<pattern::Pat>>,
    args: std::cell::RefCell<Vec<(usize, pattern::Pat)>>,
    ret_outputs: std::cell::RefCell<Vec<(usize, pattern::Pat)>>,
}

impl PyCallPat {
    fn new() -> Self {
        Self {
            target: std::cell::RefCell::new(None),
            args: std::cell::RefCell::new(Vec::new()),
            ret_outputs: std::cell::RefCell::new(Vec::new()),
        }
    }
    /// Materialise the current builder state into a finalised `Pat`.
    /// Cheap: clones the inner Vecs once.
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = pattern::call();
        if let Some(t) = self.target.borrow().clone() {
            b = b.target(t);
        }
        for (idx, p) in self.args.borrow().iter().cloned() {
            b = b.arg(idx, p);
        }
        for (idx, p) in self.ret_outputs.borrow().iter().cloned() {
            b = b.ret_output(idx, p);
        }
        b.into()
    }
}

#[pymethods]
impl PyCallPat {
    /// Constrain the call target to the literal address `addr`.
    /// Equivalent to `target(int_const(addr))`.
    fn at(slf: Py<Self>, py: Python<'_>, addr: u64) -> Py<Self> {
        slf.borrow(py).target.replace(Some(pattern::int_const(addr)));
        slf
    }
    /// Constrain the call target to any address in `addrs`.
    /// Set-membership variant of `at` — fires when the call's target
    /// matches any address in the list.  Equivalent to
    /// `target(int_const_any_of(addrs))`.  An empty list vacuously
    /// fails (matches nothing).
    fn at_any(slf: Py<Self>, py: Python<'_>, addrs: Vec<u64>) -> Py<Self> {
        slf.borrow(py)
            .target
            .replace(Some(pattern::int_const_any_of(addrs)));
        slf
    }
    /// Constrain the call target with an arbitrary pattern (e.g.
    /// `function_arg(0)` or a captured value reference).
    fn target(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).target.replace(Some(pat));
        Ok(slf)
    }
    /// Constrain the argument at position `idx` (0-based, after the
    /// implicit `[ctrl, mem]` inputs).  The `Call` node's input layout
    /// is `[ctrl, mem, target, arg0, arg1, …]`; this method maps `idx`
    /// onto the arg slot.
    fn arg(slf: Py<Self>, py: Python<'_>, idx: usize, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).args.borrow_mut().push((idx, pat));
        Ok(slf)
    }
    /// Capture the Call's return-value output at ABI position `idx`
    /// — e.g. `.ret_output(0, var(c))` binds `c` to the
    /// `NodeOutputId` of the calling convention's first return
    /// register.  See `pattern::CallPat::ret_output` for details.
    fn ret_output(slf: Py<Self>, py: Python<'_>, idx: usize, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).ret_outputs.borrow_mut().push((idx, pat));
        Ok(slf)
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat {
        PyPat::from_pat(wrap_when(self.finalise(), f))
    }
    /// Materialise this builder as a `Pat`.  Use this when a function
    /// that doesn't accept `PatLike` (e.g. `Graph.rewrite_all`'s pair
    /// list) needs a finalised pattern.  `Graph.find_all` accepts the
    /// builder directly via `PatLike` — `into_pat()` isn't required there.
    fn into_pat(&self) -> PyPat {
        PyPat::from_pat(self.finalise())
    }
}

#[pyfunction]
#[pyo3(signature = (at=None))]
pub fn call(at: Option<u64>) -> PyCallPat {
    let b = PyCallPat::new();
    if let Some(addr) = at {
        b.target.replace(Some(pattern::int_const(addr)));
    }
    b
}

// ── CallOtherPat ─────────────────────────────────────────────────────────

/// Typed builder for `CallOther` node patterns.  Mirrors
/// `pattern::CallOtherPat` — chain `.user_op_id(v)` to constrain the
/// user-op id (e.g. ARM `setISAMode`'s id), `.name(s)` to constrain
/// the user-op name (read from `Graph::call_other_name`), and
/// `.arg(idx, p)` to constrain a specific argument.
#[pyclass(name = "CallOtherPat", module = "strider.pattern")]
pub struct PyCallOtherPat {
    user_op_id: std::cell::RefCell<Option<u64>>,
    name: std::cell::RefCell<Option<String>>,
    args: std::cell::RefCell<Vec<(usize, pattern::Pat)>>,
}

impl PyCallOtherPat {
    fn new() -> Self {
        Self {
            user_op_id: std::cell::RefCell::new(None),
            name: std::cell::RefCell::new(None),
            args: std::cell::RefCell::new(Vec::new()),
        }
    }
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = pattern::call_other();
        if let Some(id) = *self.user_op_id.borrow() {
            b = b.user_op_id(id);
        }
        if let Some(n) = self.name.borrow().clone() {
            b = b.name(n);
        }
        for (idx, p) in self.args.borrow().iter().cloned() {
            b = b.arg(idx, p);
        }
        b.into()
    }
}

#[pymethods]
impl PyCallOtherPat {
    fn user_op_id(slf: Py<Self>, py: Python<'_>, v: u64) -> Py<Self> {
        slf.borrow(py).user_op_id.replace(Some(v));
        slf
    }
    fn name(slf: Py<Self>, py: Python<'_>, n: String) -> Py<Self> {
        slf.borrow(py).name.replace(Some(n));
        slf
    }
    fn arg(slf: Py<Self>, py: Python<'_>, idx: usize, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).args.borrow_mut().push((idx, pat));
        Ok(slf)
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat { PyPat::from_pat(wrap_when(self.finalise(), f)) }
    fn into_pat(&self) -> PyPat { PyPat::from_pat(self.finalise()) }
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
#[pyclass(name = "RetPat", module = "strider.pattern")]
pub struct PyRetPat {
    preceded_by: std::cell::RefCell<Option<pattern::Pat>>,
    ret_vals: std::cell::RefCell<Vec<(usize, pattern::Pat)>>,
}

impl PyRetPat {
    fn new() -> Self {
        Self {
            preceded_by: std::cell::RefCell::new(None),
            ret_vals: std::cell::RefCell::new(Vec::new()),
        }
    }
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = pattern::ret();
        if let Some(p) = self.preceded_by.borrow().clone() {
            b = b.preceded_by(p);
        }
        for (idx, p) in self.ret_vals.borrow().iter().cloned() {
            b = b.ret_val(idx, p);
        }
        b.into()
    }
}

#[pymethods]
impl PyRetPat {
    fn preceded_by(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).preceded_by.replace(Some(pat));
        Ok(slf)
    }
    fn ret_val(slf: Py<Self>, py: Python<'_>, idx: usize, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).ret_vals.borrow_mut().push((idx, pat));
        Ok(slf)
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat { PyPat::from_pat(wrap_when(self.finalise(), f)) }
    fn into_pat(&self) -> PyPat { PyPat::from_pat(self.finalise()) }
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
/// `Not(cond)` with branches swapped) — see `pattern::IfPat` docs.
#[pyclass(name = "IfPat", module = "strider.pattern")]
pub struct PyIfPat {
    cond: std::cell::RefCell<Option<pattern::Pat>>,
    true_branch: std::cell::RefCell<Option<pattern::Pat>>,
    false_branch: std::cell::RefCell<Option<pattern::Pat>>,
}

impl PyIfPat {
    fn new() -> Self {
        Self {
            cond: std::cell::RefCell::new(None),
            true_branch: std::cell::RefCell::new(None),
            false_branch: std::cell::RefCell::new(None),
        }
    }
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = pattern::if_node();
        if let Some(c) = self.cond.borrow().clone() { b = b.cond(c); }
        if let Some(t) = self.true_branch.borrow().clone() { b = b.true_branch(t); }
        if let Some(f) = self.false_branch.borrow().clone() { b = b.false_branch(f); }
        b.into()
    }
}

#[pymethods]
impl PyIfPat {
    fn cond(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).cond.replace(Some(pat));
        Ok(slf)
    }
    fn true_branch(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).true_branch.replace(Some(pat));
        Ok(slf)
    }
    fn false_branch(slf: Py<Self>, py: Python<'_>, p: PatLike<'_>) -> PyResult<Py<Self>> {
        let pat = p.into_pat()?;
        slf.borrow(py).false_branch.replace(Some(pat));
        Ok(slf)
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat { PyPat::from_pat(wrap_when(self.finalise(), f)) }
    fn into_pat(&self) -> PyPat { PyPat::from_pat(self.finalise()) }
}

#[pyfunction]
#[pyo3(signature = (cond=None))]
pub fn if_(cond: Option<PatLike<'_>>) -> PyResult<PyIfPat> {
    let b = PyIfPat::new();
    if let Some(c) = cond {
        b.cond.replace(Some(c.into_pat()?));
    }
    Ok(b)
}

// ── Typed family dispatchers (with .ordered() chain via PyOrderedBinary) ──
//
// `int_binary("Add", x, y)`, `bool_binary("And", x, y)`, `float_binary("Sub", x, y)`.
// The op is a string that maps to the IR enum variant name.

fn parse_int_binary_op(name: &str) -> PyResult<ir::IntBinaryOp> {
    // `Sub` is deliberately absent: `IntBinaryOp::Sub` is not a primitive
    // in this IR.  Python callers wanting `a - b` should use
    // `pattern.sub(a, b)` (which constructs the lowered
    // `Add(a, IntUnaryOp::Neg(b))` shape directly).
    use ir::IntBinaryOp::*;
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

fn parse_bool_binary_op(name: &str) -> PyResult<ir::BoolBinaryOp> {
    use ir::BoolBinaryOp::*;
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

fn parse_float_binary_op(name: &str) -> PyResult<ir::FloatBinaryOp> {
    // `Sub` is deliberately absent: `FloatBinaryOp::Sub` is not a primitive.
    // Python callers wanting `a - b` should use `pattern.float_sub(a, b)`,
    // which constructs the lowered `FloatAdd(a, FloatUnaryOp::Neg(b))` shape.
    use ir::FloatBinaryOp::*;
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
/// `pattern::IntBinaryOpPat` so callers can chain `.ordered()` /
/// `.capture(c)` / `.cap(name)` / `.when(f)` before finalising as a
/// `Pat`.
#[pyclass(name = "IntBinaryPat", module = "strider.pattern")]
pub struct PyIntBinaryPat {
    op: ir::IntBinaryOp,
    lhs: pattern::Pat,
    rhs: pattern::Pat,
    ordered: bool,
}

impl PyIntBinaryPat {
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = pattern::int_binary(self.op, self.lhs.clone(), self.rhs.clone());
        if self.ordered {
            b = b.ordered();
        }
        b.into()
    }
}

#[pymethods]
impl PyIntBinaryPat {
    fn ordered(&mut self) -> PyPat {
        self.ordered = true;
        PyPat::from_pat(self.finalise())
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat {
        PyPat::from_pat(wrap_when(self.finalise(), f))
    }
    fn into_pat(&self) -> PyPat {
        PyPat::from_pat(self.finalise())
    }
}

/// Typed builder for a boolean binary-op pattern.
#[pyclass(name = "BoolBinaryPat", module = "strider.pattern")]
pub struct PyBoolBinaryPat {
    op: ir::BoolBinaryOp,
    lhs: pattern::Pat,
    rhs: pattern::Pat,
    ordered: bool,
}

impl PyBoolBinaryPat {
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = pattern::bool_binary(self.op, self.lhs.clone(), self.rhs.clone());
        if self.ordered {
            b = b.ordered();
        }
        b.into()
    }
}

#[pymethods]
impl PyBoolBinaryPat {
    fn ordered(&mut self) -> PyPat {
        self.ordered = true;
        PyPat::from_pat(self.finalise())
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat {
        PyPat::from_pat(wrap_when(self.finalise(), f))
    }
    fn into_pat(&self) -> PyPat {
        PyPat::from_pat(self.finalise())
    }
}

/// Typed builder for a float binary-op pattern.
#[pyclass(name = "FloatBinaryPat", module = "strider.pattern")]
pub struct PyFloatBinaryPat {
    op: ir::FloatBinaryOp,
    lhs: pattern::Pat,
    rhs: pattern::Pat,
    ordered: bool,
}

impl PyFloatBinaryPat {
    pub(crate) fn finalise(&self) -> pattern::Pat {
        let mut b = pattern::float_binary(self.op, self.lhs.clone(), self.rhs.clone());
        if self.ordered {
            b = b.ordered();
        }
        b.into()
    }
}

#[pymethods]
impl PyFloatBinaryPat {
    fn ordered(&mut self) -> PyPat {
        self.ordered = true;
        PyPat::from_pat(self.finalise())
    }
    fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
        use pattern::IntoPat;
        PyPat::from_pat(self.finalise().capture(c.inner))
    }
    fn cap(&self, name: &str) -> PyResult<PyPat> {
        use pattern::IntoPat;
        let c = intern_str(name)?;
        Ok(PyPat::from_pat(self.finalise().capture(c)))
    }
    fn when(&self, f: PyObject) -> PyPat {
        PyPat::from_pat(wrap_when(self.finalise(), f))
    }
    fn into_pat(&self) -> PyPat {
        PyPat::from_pat(self.finalise())
    }
}

#[pyfunction]
pub fn int_binary(op: &str, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyIntBinaryPat> {
    Ok(PyIntBinaryPat {
        op: parse_int_binary_op(op)?,
        lhs: l.into_pat()?,
        rhs: r.into_pat()?,
        ordered: false,
    })
}

#[pyfunction]
pub fn bool_binary(op: &str, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyBoolBinaryPat> {
    Ok(PyBoolBinaryPat {
        op: parse_bool_binary_op(op)?,
        lhs: l.into_pat()?,
        rhs: r.into_pat()?,
        ordered: false,
    })
}

#[pyfunction]
pub fn float_binary(op: &str, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyFloatBinaryPat> {
    Ok(PyFloatBinaryPat {
        op: parse_float_binary_op(op)?,
        lhs: l.into_pat()?,
        rhs: r.into_pat()?,
        ordered: false,
    })
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
    Ok(PyPat::from_pat(pattern::int_binary_any(c.inner, lp, rp)))
}

#[pyfunction]
pub fn int_un_any(c: PyRef<'_, PyCapture>, operand: PatLike<'_>) -> PyResult<PyPat> {
    let p = operand.into_pat()?;
    Ok(PyPat::from_pat(pattern::int_unary_any(c.inner, p)))
}

#[pyfunction]
pub fn int_cmp_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(pattern::int_cmp_any(c.inner, lp, rp)))
}

#[pyfunction]
pub fn bool_bin_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(pattern::bool_binary_any(c.inner, lp, rp)))
}

#[pyfunction]
pub fn bool_un_any(c: PyRef<'_, PyCapture>, operand: PatLike<'_>) -> PyResult<PyPat> {
    let p = operand.into_pat()?;
    Ok(PyPat::from_pat(pattern::bool_unary_any(c.inner, p)))
}

#[pyfunction]
pub fn float_bin_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(pattern::float_binary_any(c.inner, lp, rp)))
}

#[pyfunction]
pub fn float_un_any(c: PyRef<'_, PyCapture>, operand: PatLike<'_>) -> PyResult<PyPat> {
    let p = operand.into_pat()?;
    Ok(PyPat::from_pat(pattern::float_unary_any(c.inner, p)))
}

#[pyfunction]
pub fn float_cmp_any(c: PyRef<'_, PyCapture>, l: PatLike<'_>, r: PatLike<'_>) -> PyResult<PyPat> {
    let lp = l.into_pat()?;
    let rp = r.into_pat()?;
    Ok(PyPat::from_pat(pattern::float_cmp_any(c.inner, lp, rp)))
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
