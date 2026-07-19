//! Result wrapper for a successful pattern match.
//!
//! A `strider_pattern::Matcher` borrows the Function for its lifetime and
//! cannot be stored across Python method calls without an unsafe lifetime
//! extension, so each query builds a fresh one and converts its `Match`es into
//! `PyMatch`es that hold a `Py<PyFunction>` to re-borrow from. The query entry
//! points themselves live on `PyFunction`.

use pyo3::prelude::*;

use crate::errors::into_strider_err;
use crate::function::PyFunction;
use crate::pattern::{PyCapture, intern_str};

/// Result of a successful pattern match.
///
/// Every capture accessor raises `StriderError` once the function has been
/// compacted or otherwise reshuffled, rather than dereferencing the stored
/// `ValueId`s against the new arena.
#[pyclass(name = "Match", module = "strider.pattern", unsendable)]
pub struct PyMatch {
    /// Per-input-pattern sub-matches, non-empty. A join query yields one entry
    /// per pattern, with shared captures already unified by the matcher. A
    /// capture accessor reads the first sub-match binding it, so the `Match`
    /// presents the union of every pattern's captures.
    pub(crate) inner: Vec<strider_pattern::Match>,
    pub(crate) function: Py<PyFunction>,
    /// Sampled at construction, compared on every accessor.
    pub(crate) generation: u64,
}

/// Per-pattern roots (empty under `ignore_root`) paired with each sub-match's
/// `(capture-id, node-id)` signature.
type DedupKey = (Vec<u32>, Vec<Vec<(u32, u32)>>);

/// A `Capture` instance or a string name, looked up in the global intern
/// table.
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

/// The `m[c]` value precedence, shared by a finished match and the in-progress
/// one handed to a `.when()` predicate.
///
/// Bool must be probed BEFORE uint: the uint read also matches an `I1` value
/// (as 0/1), so checking it first would surface a boolean capture as a plain
/// int. The bool read is `I1`-only, so wider ints still fall through. `u128`
/// goes to PyO3 directly; casting via `i128` would sign-truncate any I128
/// value with bit 127 set.
pub(crate) fn capture_value_to_py(
    py: Python<'_>,
    bool_val: Option<bool>,
    uint_val: Option<u128>,
    float_bits: Option<u64>,
) -> PyObject {
    if let Some(b) = bool_val {
        return b.into_py(py);
    }
    if let Some(v) = uint_val {
        return v.into_py(py);
    }
    if let Some(f) = float_bits {
        return f.into_py(py);
    }
    py.None()
}

impl PyMatch {
    /// A mismatch means the arena was reshuffled since this match was built,
    /// so its `ValueId`s are stale.
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

    /// "First" is well-defined because the join already unified shared
    /// captures, so every sub-match binding `cap` agrees on it.
    fn binding_for(&self, cap: strider_pattern::Capture) -> Option<&strider_pattern::Match> {
        self.inner.iter().find(|m| m.is_bound(cap))
    }

    fn is_bound(&self, cap: strider_pattern::Capture) -> bool {
        self.inner.iter().any(|m| m.is_bound(cap))
    }

    /// Without `ignore_root` the per-pattern roots join the key, keeping
    /// distinct sites apart; with it only bindings matter, which collapses
    /// diamonds and capture-less hits.
    pub(crate) fn dedup_key(&self, py: Python<'_>, ignore_root: bool) -> PyResult<DedupKey> {
        let function = self.function.borrow(py);
        let function = function.read_inner().map_err(into_strider_err)?;
        self.assert_generation(&function)?;
        let roots = if ignore_root {
            Vec::new()
        } else {
            self.inner.iter().map(|m| m.root().as_u32()).collect()
        };
        let sigs = self
            .inner
            .iter()
            .map(|m| m.capture_signature(function.graph()))
            .collect();
        Ok((roots, sigs))
    }
}

#[pymethods]
impl PyMatch {
    /// The operation variant of the node bound to `key` (`"Add"`, `"Less"`),
    /// or `None` when `key` is unbound or names a node carrying no operation.
    /// Covers every op family; pair with `value_type` to tell a boolean op
    /// (`Xor` at `I1`) from a wide bitwise one.
    fn op(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        match self.node(py, key)? {
            Some(node) => node.op(py),
            None => Ok(None),
        }
    }

    /// The value-output type of the node bound to `key` (`"I1"`, `"I64"`,
    /// `"F64"`), or `None` when unbound or the node has no value output.
    fn value_type(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<String>> {
        match self.node(py, key)? {
            Some(node) => node.value_type(py),
            None => Ok(None),
        }
    }
}

#[pymethods]
impl PyMatch {
    /// Node id where the top-level pattern matched. The root carries no
    /// user-visible capture binding, so pair this with
    /// `Function.node(id).asm_fingerprint()` or `Cfg.fingerprint_pcode(node)`
    /// for proof queries that name no `Capture`.
    #[getter]
    fn root(&self) -> u32 {
        self.inner[0].root().as_u32()
    }

    /// One root node id per pattern passed to the query. `root` is the
    /// convenience accessor for the single-pattern case.
    #[getter]
    fn roots(&self) -> Vec<u32> {
        self.inner.iter().map(|m| m.root().as_u32()).collect()
    }

    /// Best-effort value of a capture: bool if it is one, else int, else raw
    /// float bits, else `None`.
    fn __getitem__(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<PyObject> {
        let node = match self.node(py, key)? {
            Some(node) => node,
            None => return Ok(py.None()),
        };
        let b = node.const_bool(py)?;
        let v = node.const_uint(py)?;
        let f = node.float_bits(py)?;
        Ok(capture_value_to_py(py, b, v, f))
    }

    /// True when `key` (a `Capture` or string name) is bound in this match.
    fn __contains__(&self, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        Ok(self.is_bound(cap))
    }

    /// The capture's value as an unsigned `int`, or `None` when it isn't
    /// bound to an integer-valued node.
    fn const_uint(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<u128>> {
        match self.node(py, key)? {
            Some(node) => node.const_uint(py),
            None => Ok(None),
        }
    }

    /// The capture's value as a signed `int`, sign-interpreted at the node's
    /// width, or `None` when it isn't bound to an integer node.
    #[pyo3(name = "const_int")]
    fn int_(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<i128>> {
        match self.node(py, key)? {
            Some(node) => node.const_int(py),
            None => Ok(None),
        }
    }

    /// The capture's value as a `bool`, or `None` when it isn't bound to a
    /// boolean-valued node.
    #[pyo3(name = "const_bool")]
    fn bool_(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<bool>> {
        match self.node(py, key)? {
            Some(node) => node.const_bool(py),
            None => Ok(None),
        }
    }

    /// The capture's value as raw float bits, or `None` when it isn't bound to
    /// a float-valued node.
    fn float_bits(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<u64>> {
        match self.node(py, key)? {
            Some(node) => node.float_bits(py),
            None => Ok(None),
        }
    }

    /// True when the capture has a binding.
    fn has(&self, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        Ok(self.is_bound(cap))
    }

    /// The varnode behind a captured `InitialVar` or `Call` / `CallOther`
    /// clobber output, or `None` when `key` binds neither.
    fn vn(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<crate::sleigh::PyVn>> {
        match self.node(py, key)? {
            Some(node) => node.vn(py),
            None => Ok(None),
        }
    }

    /// Sorted, deduped machine-instruction addresses whose lift or subsequent
    /// rewrite contributed to the value of the node bound to `key`: the
    /// proof-of-correctness aid for a query. Empty when `key` is unbound or
    /// binds one of the exempt structural kinds.
    fn asm_fingerprint(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Vec<u64>> {
        match self.node(py, key)? {
            Some(node) => node.asm_fingerprint(py),
            None => Ok(Vec::new()),
        }
    }

    /// A `Node` handle on the node bound to `key`, or `None` when `key` is
    /// unbound. Unlike `root`, this resolves an explicit capture binding;
    /// every other value/op reader on `Match` forwards to the `Node` it
    /// returns.
    fn node(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<crate::node::PyNode>> {
        let cap = key.resolve()?;
        // Re-borrow to check the generation before handing out a node id that
        // could point into a stale arena, and to resolve an `Output` binding
        // back to its owning node.
        let nid = {
            let function = self.function.borrow(py);
            let function = function.read_inner().map_err(into_strider_err)?;
            self.assert_generation(&function)?;
            self.binding_for(cap)
                .and_then(|m| m.node(cap, function.graph()))
        };
        match nid {
            Some(nid) => {
                let pynode =
                    crate::node::PyNode::new(py, self.function.clone_ref(py), nid.as_u32())?;
                Ok(Some(pynode))
            }
            None => Ok(None),
        }
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMatch>()
}
