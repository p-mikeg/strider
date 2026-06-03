//! `PyMatch` — result wrapper for a successful pattern match.
//!
//! The Rust `strider_pattern::Matcher` borrows the Function
//! immutably for its lifetime; we cannot store one across Python
//! method calls without an unsafe lifetime extension.  Instead each
//! call constructs a fresh `Matcher`, runs the query, and converts
//! every `strider_pattern::Match` to a `PyMatch` that carries:
//! - The `strider_pattern::Match` itself (for capture lookup).
//! - A `Py<PyFunction>` reference so accessors like `get_uint` can
//!   re-borrow the function and call `Match::get_uint(c, &function)`.
//!
//! The `Function.find_all` / `Function.match_at` / `Function.matcher` entry
//! points live on PyFunction in `function.rs`.

use pyo3::prelude::*;

use crate::errors::into_strider_err;
use crate::function::PyFunction;
use crate::pattern::{intern_str, PyCapture};

/// Result of a successful pattern match.
///
/// Snapshots the function's generation counter at construction so that
/// any subsequent arena-reshuffling op (`Function.compact`,
/// `retain_reachable`, etc.) bumps `Function::generation()` and every
/// subsequent capture accessor returns a typed `StriderError` rather
/// than silently dereferencing a stale `ValueId` on the
/// post-bump arena.
#[pyclass(name = "Match", module = "strider")]
pub struct PyMatch {
    pub(crate) inner: strider_pattern::Match,
    pub(crate) function: Py<PyFunction>,
    /// Generation counter sampled at `PyMatch` construction time.
    /// Compared against `Function::generation()` on every accessor; a
    /// mismatch means the underlying arena was reshuffled since the
    /// match was created and the stored `ValueId`s are stale.
    pub(crate) generation: u64,
}

/// Polymorphic capture key: a `Capture` instance or a string name
/// (looked up in the global intern table).
#[derive(FromPyObject)]
pub enum CaptureKey<'py> {
    Capture(Bound<'py, PyCapture>),
    Str(String),
}

impl CaptureKey<'_> {
    fn resolve(self) -> PyResult<strider_pattern::Capture> {
        match self {
            CaptureKey::Capture(c) => Ok(c.borrow().inner),
            CaptureKey::Str(s) => intern_str(s.as_str()),
        }
    }
}

impl PyMatch {
    /// Resolve `key` to a `Capture`, borrow the function for read, and run
    /// `f` against `(capture, &function)`.  Centralises the boilerplate that
    /// every typed accessor on `PyMatch` would otherwise repeat (resolve
    /// the key → borrow the PyFunction → read the inner RwLock → poison-map
    /// → check the generation hasn't drifted).
    fn with_function<F, R>(&self, py: Python<'_>, key: CaptureKey<'_>, f: F) -> PyResult<R>
    where
        F: FnOnce(strider_pattern::Capture, &strider_ir::Function) -> R,
    {
        let cap = key.resolve()?;
        let function = self.function.borrow(py);
        let function = function.read_inner().map_err(into_strider_err)?;
        self.assert_generation(&function)?;
        Ok(f(cap, &function))
    }

    /// Confirm the function's generation counter is still what it was
    /// when this `PyMatch` was constructed.  A mismatch indicates an
    /// arena-reshuffling op (`Function.compact`, `retain_reachable`,
    /// `optimize`) ran between match construction and this accessor —
    /// the stored `ValueId`s are stale.  Returns a
    /// `StriderError` rather than silently dereferencing the wrong
    /// node.
    fn assert_generation(&self, function: &strider_ir::Function) -> PyResult<()> {
        if function.graph().generation() != self.generation {
            return Err(into_strider_err(anyhow::anyhow!(
                "Match is stale: function was compacted / reshuffled after this Match was \
                 created (match generation = {}, function generation = {}).  Re-run the \
                 pattern against the post-compaction function.",
                self.generation,
                function.graph().generation(),
            )));
        }
        Ok(())
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
        self.with_function(py, key, |cap, g| {
            if let Some(v) = self.inner.bindings().get_uint(cap, g.graph()) {
                // Pass `u128` directly — PyO3 handles the conversion to a
                // Python int.  Casting to `i128` first would silently sign-
                // truncate any I128 value with bit 127 set (e.g. `u128::MAX`
                // would surface as `-1` to Python).
                return v.into_py(py);
            }
            if let Some(b) = self.inner.bindings().get_bool(cap, g.graph()) {
                return b.into_py(py);
            }
            if let Some(f) = self.inner.bindings().get_float_bits(cap, g.graph()) {
                return f.into_py(py);
            }
            // Fall back to None for control-flow captures.
            py.None()
        })
    }

    /// `capture in m` — True if `key` (a `Capture` or string name) has
    /// a binding in this match.
    fn __contains__(&self, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        Ok(self.inner.is_bound(cap))
    }

    /// The capture's value as an unsigned `int`, or `None` when the
    /// capture isn't bound to an integer-valued node.
    fn uint(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<u128>> {
        self.with_function(py, key, |c, g| self.inner.bindings().get_uint(c, g.graph()))
    }

    /// The capture's value as a signed `int` (sign-interpreted at the
    /// node's width), or `None` when not bound to an integer node.
    #[pyo3(name = "int")]
    fn int_(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<i128>> {
        self.with_function(py, key, |c, g| self.inner.bindings().get_int(c, g.graph()))
    }

    /// The capture's value as a `bool`, or `None` when not bound to a
    /// boolean-valued node.
    #[pyo3(name = "bool")]
    fn bool_(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<bool>> {
        self.with_function(py, key, |c, g| self.inner.bindings().get_bool(c, g.graph()))
    }

    /// The capture's value as raw float bits (`u64`), or `None` when not
    /// bound to a float-valued node.
    fn float_bits(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<u64>> {
        self.with_function(py, key, |c, g| self.inner.bindings().get_float_bits(c, g.graph()))
    }

    /// Returns True if the capture has a binding.
    fn has(&self, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        Ok(self.inner.is_bound(cap))
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
        self.with_function(py, key, |c, g| self.inner.bindings().get_int_binary_op(c, g.graph()).map(op_name))
    }

    /// Recover the matched `IntUnaryOp` variant name from `c`.
    fn int_unary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_function(py, key, |c, g| self.inner.bindings().get_int_unary_op(c, g.graph()).map(op_name))
    }

    /// Recover the matched `IntCmpOp` variant name from `c`,
    /// e.g. `"Less"`, `"Equal"`, `"Sless"`.
    fn int_cmp_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_function(py, key, |c, g| self.inner.bindings().get_int_cmp_op(c, g.graph()).map(op_name))
    }

    /// Recover the matched boolean binary op's variant name (an `IntBinaryOp`
    /// — `And` / `Or` / `Xor` — at `I1`) from `c`.
    fn bool_binary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_function(py, key, |c, g| self.inner.bindings().get_bool_binary_op(c, g.graph()).map(op_name))
    }

    // Note: there is no `bool_unary_op` accessor.  A boolean logical NOT
    // is `Xor(x, IntConst(1)):I1` since the former BitNot unary-op was removed
    // in favour of `Xor(_, all_ones)`, so the matching op variant is
    // recovered via `bool_binary_op` (which returns `"Xor"`).

    /// Recover the matched `FloatBinaryOp` variant name from `c`.
    fn float_binary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_function(py, key, |c, g| {
            self.inner.bindings().get_float_binary_op(c, g.graph()).map(op_name)
        })
    }

    /// Recover the matched `FloatUnaryOp` variant name from `c`.
    fn float_unary_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_function(py, key, |c, g| self.inner.bindings().get_float_unary_op(c, g.graph()).map(op_name))
    }

    /// Recover the matched `FloatCmpOp` variant name from `c`.
    fn float_cmp_op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        self.with_function(py, key, |c, g| self.inner.bindings().get_float_cmp_op(c, g.graph()).map(op_name))
    }

    /// Recover the matched varnode from `c`.  Returns the `Vn`
    /// associated with the captured `InitialVar` / tagged `Phi`
    /// (via `Graph::phi_var_tag`) / `FunctionArg` node, or `None`
    /// when `c` doesn't bind such a node.
    fn vn(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<crate::sleigh::PyVn>> {
        self.with_function(py, key, |c, g| {
            self.inner.get_vn(c, g).map(crate::sleigh::PyVn::from_inner)
        })
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
        let function = self.function.borrow(py);
        let function = function.read_inner().map_err(into_strider_err)?;
        self.assert_generation(&function)?;
        Ok(self.inner.asm_fingerprint(cap, &function).to_vec())
    }

    /// Returns a `Node` handle on the node bound to `key` (a `Capture`
    /// or string capture-name), or `None` when `key` is unbound in this
    /// match.
    ///
    /// The returned `Node` is a discoverable entry point into the IR
    /// graph: walk its `inputs()`, read its `kind()`, pull out constant
    /// values, etc.  Unlike `Match.root` (which always returns the raw
    /// top-level `u32` id), this resolves an explicit capture binding.
    fn node(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<crate::node::PyNode>> {
        let cap = key.resolve()?;
        // Re-borrow the function to validate the generation hasn't drifted
        // before handing out a node id that may point at a stale arena,
        // and to resolve an `Output` binding back to its owning node.
        let nid = {
            let function = self.function.borrow(py);
            let function = function.read_inner().map_err(into_strider_err)?;
            self.assert_generation(&function)?;
            self.inner.node(cap, function.graph())
        };
        match nid {
            Some(nid) => {
                let pynode = crate::node::PyNode::new(
                    py,
                    self.function.clone_ref(py),
                    nid.as_u32(),
                )?;
                Ok(Some(pynode))
            }
            None => Ok(None),
        }
    }
}

// ── Op-variant name helper ──────────────────────────────────────────────
//
// Mirror the Sleigh-style spelling used by the `int_binary("Add",
// ...)` / `parse_int_cmp_op` constructors: `op_name` returns the
// `Debug`-formatted variant name, which matches the string the
// constructor accepts, so a `find_all → recover op → reconstruct
// pattern` round-trip stays consistent.  Every op enum (IntBinaryOp,
// IntUnaryOp, IntCmpOp, FloatBinaryOp, FloatUnaryOp, FloatCmpOp)
// derives `Debug` whose output is exactly the variant identifier.

fn op_name<T: std::fmt::Debug>(op: T) -> String {
    format!("{op:?}")
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMatch>()
}
