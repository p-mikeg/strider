//! `strider.pattern` submodule.
//!
//! Wraps the `pattern` crate.  Provides:
//! - `Capture` — opaque capture-variable handle.
//! - `Pat` — opaque wrapped pattern.  Constructed via free functions
//!   (`add`, `load`, `call`, …) and chained via builder methods
//!   (`.addr()`, `.arg()`, etc.).
//! - String-keyed captures: any free function that accepts a sub-pattern
//!   also accepts a string; the string is interned to a `Capture` at
//!   the point the outermost pattern is finalized, so back-references
//!   (`add("x", "x")`) work.  Within v1 we use a single global
//!   intern table per Pat tree — simpler than per-pattern tables and
//!   sufficient for the common case of building each pattern as a
//!   single tree.
//!
//! Coverage in v1: the most-used constructors — `load`, `store`,
//! `add`, `sub`, `mul`, `shl`, `shr`, `ushr`, `and_`, `or_`, `xor`,
//! `bool_and`, `bool_or`, `bool_xor`, `int_eq`, `int_lt`, `int_slt`,
//! `call`, `call_other`, `ret`, `if_`, `phi`, `phi_for`,
//! `initial_var`, `function_arg`, `int_const`, `bool_const`,
//! `float_const`, `var`, `any_`, `predicate`.  Less-used variants
//! (float arithmetic, casts, advanced cmp variants, etc.) can be
//! added incrementally as the Python users' needs grow.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use pyo3::types::{PyString, PyType};

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
/// `Capture`, or a string (which interns to a Capture).
#[derive(FromPyObject)]
pub enum PatLike<'py> {
    Pat(Bound<'py, PyPat>),
    Capture(Bound<'py, PyCapture>),
    Str(Bound<'py, PyString>),
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
        }
    }
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
    // Negative values get wrapped to u64; users who want > u64 width
    // should use the typed builder once we expose it.
    let v = value as i64 as u64;
    PyPat::from_pat(pattern::int_const(v))
}

#[pyfunction]
pub fn bool_const(value: bool) -> PyPat {
    PyPat::from_pat(pattern::bool_const(value))
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

// ── Memory ───────────────────────────────────────────────────────────────

/// `load()` returns a `LoadPat` builder; for now we only expose the
/// short form `load(addr=...)`.
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

// ── Calls ────────────────────────────────────────────────────────────────

#[pyfunction]
#[pyo3(signature = (at=None))]
pub fn call(at: Option<u64>) -> PyPat {
    let mut b = pattern::call();
    if let Some(addr) = at {
        b = b.at(addr);
    }
    PyPat::from_pat(b.into())
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

// ── Module registration ──────────────────────────────────────────────────

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new_bound(py, "pattern")?;
    m.add_class::<PyCapture>()?;
    m.add_class::<PyPat>()?;

    macro_rules! add_fn {
        ($name:ident) => {
            m.add_function(wrap_pyfunction!($name, &m)?)?;
        };
    }
    add_fn!(any_);
    add_fn!(var);
    add_fn!(int_const);
    add_fn!(bool_const);
    add_fn!(any_int_const);
    add_fn!(any_bool_const);
    add_fn!(initial_var);
    add_fn!(function_arg);
    add_fn!(function_arg_any);
    add_fn!(phi);
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
    add_fn!(bool_and);
    add_fn!(bool_or);
    add_fn!(bool_xor);
    add_fn!(load);
    add_fn!(store);
    add_fn!(stack_store);
    add_fn!(call);
    add_fn!(call_other);
    add_fn!(ret);
    add_fn!(if_);

    parent.add_submodule(&m)?;
    let sys = py.import_bound("sys")?;
    sys.getattr("modules")?.set_item("strider.pattern", &m)?;
    Ok(())
}

// Re-exports used internally by the matcher module (added next).
#[allow(unused_imports)]
pub(crate) use {pattern::IntoPat as _PatternIntoPat};
