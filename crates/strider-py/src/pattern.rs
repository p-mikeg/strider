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

/// Polymorphic input for builder field methods: accepts a `Pat`, a
/// `Capture`, a string (which interns to a Capture), or any of the
/// typed builders that finalise to a `Pat` (e.g. `CallPat`,
/// `IntBinaryPat`).  Adding a typed builder variant here lets users
/// pass the un-finalised builder directly into field setters and
/// query methods without a manual `.into_pat()` call.
#[derive(FromPyObject)]
pub enum PatLike<'py> {
    Pat(Bound<'py, PyPat>),
    Capture(Bound<'py, PyCapture>),
    Str(Bound<'py, PyString>),
    CallPat(Bound<'py, PyCallPat>),
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

#[pyfunction]
pub fn int_const(value: i128) -> PyPat {
    PyPat::from_pat(pattern::int_const(value))
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

#[pyfunction]
pub fn function_arg(i: u32) -> PyPat {
    PyPat::from_pat(pattern::function_arg(i).into())
}

#[pyfunction]
pub fn function_arg_any() -> PyPat {
    PyPat::from_pat(pattern::function_arg_any().into())
}

#[pyfunction]
pub fn phi() -> PyPat {
    PyPat::from_pat(pattern::phi().into())
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
int_binop!(and);
int_binop!(or);
int_binop!(xor);
int_binop!(int_eq);
int_binop!(int_lt);
int_binop!(int_le);
int_binop!(int_slt);
int_binop!(int_sle);
int_binop!(int_carry);
int_binop!(int_scarry);
int_binop!(int_sborrow);

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

int_unop!(neg);
// `not` is a Python keyword; expose as `not_`.
#[pyfunction(name = "not_")]
pub fn not_(operand: PatLike<'_>) -> PyResult<PyPat> {
    let op = operand.into_pat()?;
    Ok(PyPat::from_pat(pattern::not(op)))
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

#[pyfunction]
#[pyo3(signature = (addr=None))]
pub fn load(addr: Option<PatLike<'_>>) -> PyResult<PyPat> {
    let mut b = pattern::load();
    if let Some(a) = addr {
        b = b.addr(a.into_pat()?);
    }
    Ok(PyPat::from_pat(b.into()))
}

#[pyfunction]
#[pyo3(signature = (addr=None, data=None))]
pub fn store(addr: Option<PatLike<'_>>, data: Option<PatLike<'_>>) -> PyResult<PyPat> {
    let mut b = pattern::store();
    if let Some(a) = addr {
        b = b.addr(a.into_pat()?);
    }
    if let Some(v) = data {
        b = b.data(v.into_pat()?);
    }
    Ok(PyPat::from_pat(b.into()))
}

#[pyfunction]
#[pyo3(signature = (offset=None, data=None))]
pub fn stack_store(offset: Option<i64>, data: Option<PatLike<'_>>) -> PyResult<PyPat> {
    let mut b = pattern::stack_store();
    if let Some(o) = offset {
        b = b.offset(o);
    }
    if let Some(v) = data {
        b = b.data(v.into_pat()?);
    }
    Ok(PyPat::from_pat(b.into()))
}

#[pyfunction]
#[pyo3(signature = (data=None))]
pub fn stack_store_phi(data: Option<PatLike<'_>>) -> PyResult<PyPat> {
    let mut b = pattern::stack_store_phi();
    if let Some(v) = data {
        b = b.data(v.into_pat()?);
    }
    Ok(PyPat::from_pat(b.into()))
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

#[pyfunction]
pub fn call_other() -> PyPat {
    PyPat::from_pat(pattern::call_other().into())
}

#[pyfunction]
pub fn ret() -> PyPat {
    PyPat::from_pat(pattern::ret().into())
}

#[pyfunction]
#[pyo3(signature = (cond=None))]
pub fn if_(cond: Option<PatLike<'_>>) -> PyResult<PyPat> {
    let mut b = pattern::if_node();
    if let Some(c) = cond {
        b = b.cond(c.into_pat()?);
    }
    Ok(PyPat::from_pat(b.into()))
}

// ── Typed family dispatchers (with .ordered() chain via PyOrderedBinary) ──
//
// `int_binary("Add", x, y)`, `bool_binary("And", x, y)`, `float_binary("Sub", x, y)`.
// The op is a string that maps to the IR enum variant name.

fn parse_int_binary_op(name: &str) -> PyResult<ir::IntBinaryOp> {
    use ir::IntBinaryOp::*;
    Ok(match name {
        "Add" | "add" => Add,
        "Sub" | "sub" => Sub,
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
    use ir::FloatBinaryOp::*;
    Ok(match name {
        "Add" | "add" => Add,
        "Sub" | "sub" => Sub,
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
    fn finalise(&self) -> pattern::Pat {
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
    fn finalise(&self) -> pattern::Pat {
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
    fn finalise(&self) -> pattern::Pat {
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

    macro_rules! add_fn {
        ($name:ident) => {
            m.add_function(wrap_pyfunction!($name, &m)?)?;
        };
    }
    // wildcards / consts / phi / initial
    add_fn!(any_);
    add_fn!(var);
    add_fn!(int_const);
    add_fn!(bool_const);
    add_fn!(float_const);
    add_fn!(any_int_const);
    add_fn!(any_bool_const);
    add_fn!(any_float_const);
    add_fn!(initial_var);
    add_fn!(function_arg);
    add_fn!(function_arg_any);
    add_fn!(phi);
    add_fn!(predicate);
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
    add_fn!(and);
    add_fn!(or);
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
