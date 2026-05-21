//! `PyMatch` — result wrapper for a successful pattern match.
//!
//! The Rust `strider_analyze::pattern::Matcher` borrows the BuiltFunctionGraph
//! immutably for its lifetime; we cannot store one across Python
//! method calls without an unsafe lifetime extension.  Instead each
//! call constructs a fresh `Matcher`, runs the query, and converts
//! every `strider_analyze::pattern::Match` to a `PyMatch` that carries:
//! - The `strider_analyze::pattern::Match` itself (for capture lookup).
//! - A `Py<PyGraph>` reference so accessors like `get_uint` can
//!   re-borrow the graph and call `Match::get_uint(c, &graph)`.
//!
//! The `Graph.find_all` / `Graph.match_at` / `Graph.matcher` entry
//! points live on PyGraph in `graph.rs`.

use pyo3::prelude::*;

use crate::errors::into_strider_err;
use crate::graph::PyGraph;
use crate::pattern::{intern_str, PyCapture};

/// Result of a successful pattern match.
#[pyclass(name = "Match", module = "strider")]
pub struct PyMatch {
    pub(crate) inner: strider_analyze::pattern::Match,
    pub(crate) graph: Py<PyGraph>,
}

/// Polymorphic capture key: a `Capture` instance or a string name
/// (looked up in the global intern table).
#[derive(FromPyObject)]
pub enum CaptureKey<'py> {
    Capture(Bound<'py, PyCapture>),
    Str(String),
}

impl CaptureKey<'_> {
    fn resolve(self) -> PyResult<strider_analyze::pattern::Capture> {
        match self {
            CaptureKey::Capture(c) => Ok(c.borrow().inner),
            CaptureKey::Str(s) => intern_str(s.as_str()),
        }
    }
}

impl PyMatch {
    /// Resolve `key` to a `Capture`, borrow the graph for read, and run
    /// `f` against `(capture, &graph)`.  Centralises the boilerplate that
    /// every typed accessor on `PyMatch` would otherwise repeat (resolve
    /// the key → borrow the PyGraph → read the inner RwLock → poison-map).
    fn with_graph<F, R>(&self, py: Python<'_>, key: CaptureKey<'_>, f: F) -> PyResult<R>
    where
        F: FnOnce(strider_analyze::pattern::Capture, &strider_ir::BuiltFunctionGraph) -> R,
    {
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(f(cap, &g))
    }
}

#[pymethods]
impl PyMatch {
    /// The root node where the top-level pattern matched, as a `u32`
    /// node id.  Pair with `Graph.asm_fingerprint(node_id)` /
    /// `Analysis.fingerprint(node)` for proof-of-correctness queries
    /// that don't carry an explicit `Capture` (the root has no
    /// user-visible capture binding).
    #[getter]
    fn root(&self) -> u32 {
        self.inner.root().as_u32()
    }

    /// `m["name"]` / `m[capture]` — best-effort: integer if value
    /// output is an int, bool if it's a bool, raw bits otherwise.
    fn __getitem__(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<PyObject> {
        self.with_graph(py, key, |cap, g| {
            if let Some(v) = self.inner.get_uint(cap, g) {
                // Pass `u128` directly — PyO3 handles the conversion to a
                // Python int.  Casting to `i128` first would silently sign-
                // truncate any U128 value with bit 127 set (e.g. `u128::MAX`
                // would surface as `-1` to Python).
                return v.into_py(py);
            }
            if let Some(b) = self.inner.get_bool(cap, g) {
                return b.into_py(py);
            }
            if let Some(f) = self.inner.get_float_bits(cap, g) {
                return f.into_py(py);
            }
            // Fall back to None for control-flow captures.
            py.None()
        })
    }

    fn __contains__(&self, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        Ok(self.inner.node(cap).is_some())
    }

    fn uint(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<u128>> {
        self.with_graph(py, key, |c, g| self.inner.get_uint(c, g))
    }

    #[pyo3(name = "int")]
    fn int_(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<i128>> {
        self.with_graph(py, key, |c, g| self.inner.get_int(c, g))
    }

    #[pyo3(name = "bool")]
    fn bool_(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<bool>> {
        self.with_graph(py, key, |c, g| self.inner.get_bool(c, g))
    }

    fn float_bits(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<u64>> {
        self.with_graph(py, key, |c, g| self.inner.get_float_bits(c, g))
    }

    /// Returns True if the capture has a binding.
    fn has(&self, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        Ok(self.inner.node(cap).is_some())
    }

    // ── Op-variant accessors (for *_any captures) ───────────────────
    //
    // When you match via `int_bin_any(c, l, r)`, the bound capture
    // `c` carries the matched op variant.  The accessors below
    // recover that variant as the op's canonical Sleigh-style
    // string ("Add", "Sub", "Less", "Equal", ...).  Returns `None`
    // when the capture isn't bound or the bound node isn't of the
    // matching kind family.

    /// Recover the matched `IntBinaryOp` variant name from `c`,
    /// e.g. `"Add"`, `"Sub"`, `"And"`.
    fn int_binary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_graph(py, key, |c, g| self.inner.get_int_binary_op(c, g).map(op_name))
    }

    /// Recover the matched `IntUnaryOp` variant name from `c`.
    fn int_unary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_graph(py, key, |c, g| self.inner.get_int_unary_op(c, g).map(op_name))
    }

    /// Recover the matched `IntCmpOp` variant name from `c`,
    /// e.g. `"Less"`, `"Equal"`, `"Sless"`.
    fn int_cmp_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_graph(py, key, |c, g| self.inner.get_int_cmp_op(c, g).map(op_name))
    }

    /// Recover the matched `BoolBinaryOp` variant name from `c`.
    fn bool_binary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_graph(py, key, |c, g| self.inner.get_bool_binary_op(c, g).map(op_name))
    }

    /// Recover the matched `BoolUnaryOp` variant name from `c`.
    fn bool_unary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_graph(py, key, |c, g| self.inner.get_bool_unary_op(c, g).map(op_name))
    }

    /// Recover the matched `FloatBinaryOp` variant name from `c`.
    fn float_binary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_graph(py, key, |c, g| {
            self.inner.get_float_binary_op(c, g).map(op_name)
        })
    }

    /// Recover the matched `FloatUnaryOp` variant name from `c`.
    fn float_unary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_graph(py, key, |c, g| self.inner.get_float_unary_op(c, g).map(op_name))
    }

    /// Recover the matched `FloatCmpOp` variant name from `c`.
    fn float_cmp_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_graph(py, key, |c, g| self.inner.get_float_cmp_op(c, g).map(op_name))
    }

    /// Recover the matched varnode from `c`.  Returns the `Vn`
    /// associated with the captured `InitialVar` / `VarPhi` /
    /// `FunctionArg` node, or `None` when `c` doesn't bind such a
    /// node.
    fn vn(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<crate::sleigh::PyVn>> {
        self.with_graph(py, key, |c, g| {
            self.inner.get_vn(c, g).map(crate::sleigh::PyVn::from_inner)
        })
    }

    /// If `c` binds a `StackStore` node, returns its compile-time
    /// SP-relative offset.  `None` for an unbound capture or a
    /// non-`StackStore` producer.
    ///
    /// Pairs with `stack_store().capture(c)` for cross-pattern
    /// field-offset recoveries — capture the stack store, capture an
    /// `any_int_const(other)` on the call-arg side, compute the
    /// difference.
    fn stack_offset(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<i64>> {
        self.with_graph(py, key, |c, g| self.inner.stack_offset(c, g))
    }

    /// If `c` binds a `StackStorePhi`, returns its per-predecessor
    /// offset list (read from `Graph::stack_phi_offsets`).  `None`
    /// for an unbound capture or a non-`StackStorePhi` producer.
    fn stack_phi_offsets(
        &self,
        py: Python<'_>,
        key: CaptureKey<'_>,
    ) -> PyResult<Option<Vec<i64>>> {
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.stack_phi_offsets(cap, &g).map(<[i64]>::to_vec))
    }

    /// Returns the asm-instruction-address fingerprint of the node
    /// bound to `c` as a sorted-deduplicated `list[int]`.  Returns an
    /// empty list when the capture is unbound or when the captured
    /// node is one of the documented exempt kinds (see
    /// `strider_ir::Graph::asm_fingerprint`).
    ///
    /// The fingerprint is the proof-of-correctness aid: when a pattern
    /// query captures a value node, this list names the machine
    /// instructions whose lifting (or subsequent rewrite) contributed
    /// to that node's value.
    fn asm_fingerprint(
        &self,
        py: Python<'_>,
        key: CaptureKey<'_>,
    ) -> PyResult<Vec<u64>> {
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.asm_fingerprint(cap, &g).to_vec())
    }
}

// ── Op-variant name helper ──────────────────────────────────────────────
//
// Mirror the Sleigh-style spelling used by the `int_binary("Add",
// ...)` / `parse_int_cmp_op` constructors: `op_name` returns the
// `Debug`-formatted variant name, which matches the string the
// constructor accepts, so a `find_all → recover op → reconstruct
// pattern` round-trip stays consistent.  Every op enum (IntBinaryOp,
// IntUnaryOp, IntCmpOp, BoolBinaryOp, BoolUnaryOp, FloatBinaryOp,
// FloatUnaryOp, FloatCmpOp) derives `Debug` whose output is exactly
// the variant identifier.

fn op_name<T: std::fmt::Debug>(op: T) -> String {
    format!("{op:?}")
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMatch>()
}
