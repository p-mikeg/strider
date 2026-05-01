//! `PyMatch` — result wrapper for a successful pattern match.
//!
//! The Rust `pattern::Matcher` borrows the BuiltFunctionGraph
//! immutably for its lifetime; we cannot store one across Python
//! method calls without an unsafe lifetime extension.  Instead each
//! call constructs a fresh `Matcher`, runs the query, and converts
//! every `pattern::Match` to a `PyMatch` that carries:
//! - The `pattern::Match` itself (for capture lookup).
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
    pub(crate) inner: pattern::Match,
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
    fn resolve(self) -> PyResult<pattern::Capture> {
        match self {
            CaptureKey::Capture(c) => Ok(c.borrow().inner),
            CaptureKey::Str(s) => intern_str(s.as_str()),
        }
    }
}

#[pymethods]
impl PyMatch {
    /// `m["name"]` / `m[capture]` — best-effort: integer if value
    /// output is an int, bool if it's a bool, raw bits otherwise.
    fn __getitem__(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<PyObject> {
        let cap = key.resolve()?;
        let graph_borrow = self.graph.borrow(py);
        let g = graph_borrow.read_inner().map_err(into_strider_err)?;
        if let Some(v) = self.inner.get_uint(cap, &g) {
            // Try fitting in a Python int via i128.
            return Ok((v as i128).into_py(py));
        }
        if let Some(b) = self.inner.get_bool(cap, &g) {
            return Ok(b.into_py(py));
        }
        if let Some(f) = self.inner.get_float_bits(cap, &g) {
            return Ok(f.into_py(py));
        }
        // Fall back to None for control-flow captures.
        Ok(py.None())
    }

    fn __contains__(&self, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        Ok(self.inner.node(cap).is_some())
    }

    fn uint(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<u128>> {
        let cap = key.resolve()?;
        let graph_borrow = self.graph.borrow(py);
        let g = graph_borrow.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_uint(cap, &g))
    }

    #[pyo3(name = "int")]
    fn int_(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<i128>> {
        let cap = key.resolve()?;
        let graph_borrow = self.graph.borrow(py);
        let g = graph_borrow.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_int(cap, &g))
    }

    #[pyo3(name = "bool")]
    fn bool_(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<bool>> {
        let cap = key.resolve()?;
        let graph_borrow = self.graph.borrow(py);
        let g = graph_borrow.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_bool(cap, &g))
    }

    fn float_bits(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<u64>> {
        let cap = key.resolve()?;
        let graph_borrow = self.graph.borrow(py);
        let g = graph_borrow.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_float_bits(cap, &g))
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
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_int_binary_op(cap, &g).map(int_binary_op_name))
    }

    /// Recover the matched `IntUnaryOp` variant name from `c`.
    fn int_unary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_int_unary_op(cap, &g).map(int_unary_op_name))
    }

    /// Recover the matched `IntCmpOp` variant name from `c`,
    /// e.g. `"Less"`, `"Equal"`, `"Sless"`.
    fn int_cmp_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_int_cmp_op(cap, &g).map(int_cmp_op_name))
    }

    /// Recover the matched `BoolBinaryOp` variant name from `c`.
    fn bool_binary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_bool_binary_op(cap, &g).map(bool_binary_op_name))
    }

    /// Recover the matched `BoolUnaryOp` variant name from `c`.
    fn bool_unary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_bool_unary_op(cap, &g).map(bool_unary_op_name))
    }

    /// Recover the matched `FloatBinaryOp` variant name from `c`.
    fn float_binary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_float_binary_op(cap, &g).map(float_binary_op_name))
    }

    /// Recover the matched `FloatUnaryOp` variant name from `c`.
    fn float_unary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_float_unary_op(cap, &g).map(float_unary_op_name))
    }

    /// Recover the matched `FloatCmpOp` variant name from `c`.
    fn float_cmp_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_float_cmp_op(cap, &g).map(float_cmp_op_name))
    }

    /// Recover the matched varnode from `c`.  Returns the `Vn`
    /// associated with the captured `InitialVar` / `VarPhi` /
    /// `FunctionArg` node, or `None` when `c` doesn't bind such a
    /// node.
    fn vn(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<crate::sleigh::PyVn>> {
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(self.inner.get_vn(cap, &g).map(crate::sleigh::PyVn::from_inner))
    }
}

// ── Op-variant name helpers ──────────────────────────────────────────────
//
// Mirror the Sleigh-style spelling used by the `int_binary("Add",
// ...)` / `parse_int_cmp_op` constructors: every op_name returns
// the same string the constructor accepts, so a `find_all → recover
// op → reconstruct pattern` round-trip stays consistent.

fn int_binary_op_name(op: ir::IntBinaryOp) -> String {
    use ir::IntBinaryOp::*;
    match op {
        Add => "Add",
        Sub => "Sub",
        Mul => "Mul",
        Div => "Div",
        Sdiv => "Sdiv",
        Rem => "Rem",
        Srem => "Srem",
        And => "And",
        Or => "Or",
        Xor => "Xor",
        ShiftLeft => "ShiftLeft",
        ShiftRight => "ShiftRight",
        SShiftRight => "SShiftRight",
    }
    .to_string()
}

fn int_unary_op_name(op: ir::IntUnaryOp) -> String {
    use ir::IntUnaryOp::*;
    match op {
        Neg => "Neg",
        Not => "Not",
    }
    .to_string()
}

fn int_cmp_op_name(op: ir::IntCmpOp) -> String {
    use ir::IntCmpOp::*;
    match op {
        Equal => "Equal",
        Less => "Less",
        LessEqual => "LessEqual",
        Sless => "Sless",
        SlessEqual => "SlessEqual",
        Carry => "Carry",
        Scarry => "Scarry",
        Sborrow => "Sborrow",
        Borrow => "Borrow",
    }
    .to_string()
}

fn bool_binary_op_name(op: ir::BoolBinaryOp) -> String {
    use ir::BoolBinaryOp::*;
    match op {
        And => "And",
        Or => "Or",
        Xor => "Xor",
    }
    .to_string()
}

fn bool_unary_op_name(op: ir::BoolUnaryOp) -> String {
    use ir::BoolUnaryOp::*;
    match op {
        Neg => "Neg",
    }
    .to_string()
}

fn float_binary_op_name(op: ir::FloatBinaryOp) -> String {
    use ir::FloatBinaryOp::*;
    match op {
        Add => "Add",
        Sub => "Sub",
        Mul => "Mul",
        Div => "Div",
    }
    .to_string()
}

fn float_unary_op_name(op: ir::FloatUnaryOp) -> String {
    use ir::FloatUnaryOp::*;
    match op {
        Neg => "Neg",
        Abs => "Abs",
        Sqrt => "Sqrt",
        Ceil => "Ceil",
        Floor => "Floor",
        Round => "Round",
    }
    .to_string()
}

fn float_cmp_op_name(op: ir::FloatCmpOp) -> String {
    use ir::FloatCmpOp::*;
    match op {
        Equal => "Equal",
        NotEqual => "NotEqual",
        Less => "Less",
        LessEqual => "LessEqual",
    }
    .to_string()
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMatch>()
}
