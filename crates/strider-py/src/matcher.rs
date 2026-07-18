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
//! The `Function.find_all` / `Function.find_unique`
//! entry points live on PyFunction in `function.rs`.

use pyo3::prelude::*;

use crate::errors::into_strider_err;
use crate::function::PyFunction;
use crate::pattern::{PyCapture, intern_str};

/// Result of a successful pattern match.
///
/// Snapshots the function's generation counter at construction so that
/// any subsequent arena-reshuffling op (`Function.compact`,
/// `retain_reachable`, etc.) bumps `Function::generation()` and every
/// subsequent capture accessor returns a typed `StriderError` rather
/// than silently dereferencing a stale `ValueId` on the
/// post-bump arena.
#[pyclass(name = "Match", module = "strider", unsendable)]
pub struct PyMatch {
    /// The per-input-pattern sub-matches (non-empty).  A single-pattern
    /// query yields a one-element vec; a list (join) query yields one entry
    /// per pattern, whose shared captures the matcher already unified.  A
    /// capture accessor reads the first sub-match that binds it, so the
    /// `Match` presents the *union* of every pattern's captures.
    pub(crate) inner: Vec<strider_pattern::Match>,
    pub(crate) function: Py<PyFunction>,
    /// Generation counter sampled at `PyMatch` construction time.
    /// Compared against `Function::generation()` on every accessor; a
    /// mismatch means the underlying arena was reshuffled since the
    /// match was created and the stored `ValueId`s are stale.
    pub(crate) generation: u64,
}

/// Deduplication key for a `Match`: the per-pattern roots (empty when
/// `ignore_root`) paired with each sub-match's `(capture-id, node-id)`
/// signature.  `Hash + Eq` via its component `Vec`s.
type DedupKey = (Vec<u32>, Vec<Vec<(u32, u32)>>);

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

/// Convert a capture's already-resolved value Options to a Python object
/// per the `m[c]` precedence used by `PyMatch::__getitem__` (shared by
/// both a finished match and the in-progress `Match` passed to a
/// `.when()` predicate — both go through this same `PyMatch`).
///
/// Check bool (an `I1`-typed IntConst) BEFORE the general uint path:
/// `get_uint` also matches an `I1` value (returning 0/1), so probing it
/// first would make a boolean capture surface as a plain int, contradicting
/// the "bool if it's a bool" contract.  `get_bool` is `I1`-only, so wider
/// ints still fall through to uint.  Then uint (pass `u128` directly — PyO3
/// handles the conversion; casting to `i128` first would silently
/// sign-truncate any I128 value with bit 127 set), then raw float bits, then
/// `None` for control-flow captures.
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

    /// The first sub-match that binds `cap`, if any.  Shared captures agree
    /// across sub-matches (the join unified them), so "first" is well-defined.
    fn binding_for(&self, cap: strider_pattern::Capture) -> Option<&strider_pattern::Match> {
        self.inner.iter().find(|m| m.is_bound(cap))
    }

    /// Whether any sub-match binds `cap`.
    fn is_bound(&self, cap: strider_pattern::Capture) -> bool {
        self.inner.iter().any(|m| m.is_bound(cap))
    }

    /// Dedup key for `find_all`.  With `ignore_root == false` the per-pattern
    /// roots are part of the key (distinct sites stay apart); with `true` only
    /// the captured bindings matter (collapses diamonds and capture-less hits).
    /// Reads the function once to resolve capture signatures.
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

/// Emits an op-variant forwarder as its own `#[pymethods] impl PyMatch`
/// block (one fn per name).  Each resolves `key` to a `Node` (via
/// `Self::node`) and forwards to the identically-named `Node` reader,
/// or returns `None` when the capture is unbound.  (pyo3 forbids a bare
/// `macro_rules!` invocation *inside* a `#[pymethods]` block, so the macro
/// emits the whole block instead; pyo3 permits multiple such blocks.)
macro_rules! op_forwarders {
    ($($name:ident, $doc:literal;)+) => {
        #[pymethods]
        impl PyMatch {
            $(
                #[doc = $doc]
                fn $name(
                    &self,
                    py: Python<'_>,
                    key: CaptureKey<'_>,
                ) -> PyResult<Option<String>> {
                    match self.node(py, key)? {
                        Some(node) => node.$name(py),
                        None => Ok(None),
                    }
                }
            )+
        }
    };
}

op_forwarders! {
    int_binary_op,
        "Recover the matched `IntBinaryOp` variant name from `key`, \
         e.g. `\"Add\"`, `\"Sub\"`, `\"And\"`.  Thin forwarder to \
         `Node.int_binary_op()`.";
    int_unary_op,
        "Recover the matched `IntUnaryOp` variant name from `key`. \
         Thin forwarder to `Node.int_unary_op()`.";
    int_cmp_op,
        "Recover the matched `IntCmpOp` variant name from `key`, \
         e.g. `\"Less\"`, `\"Equal\"`, `\"Sless\"`.  Thin forwarder to \
         `Node.int_cmp_op()`.";
    bool_binary_op,
        "Recover the matched boolean binary op's variant name (an \
         `IntBinaryOp` — `And` / `Or` / `Xor` — at `I1`) from `key`. \
         Thin forwarder to `Node.bool_binary_op()`.";
    // Note: there is no `bool_unary_op` accessor.  A boolean logical NOT
    // is `Xor(x, IntConst(1)):I1` since the former BitNot unary-op was
    // removed in favour of `Xor(_, all_ones)`, so the matching op variant
    // is recovered via `bool_binary_op` (which returns `"Xor"`).
    float_binary_op,
        "Recover the matched `FloatBinaryOp` variant name from `key`. \
         Thin forwarder to `Node.float_binary_op()`.";
    float_unary_op,
        "Recover the matched `FloatUnaryOp` variant name from `key`. \
         Thin forwarder to `Node.float_unary_op()`.";
    float_cmp_op,
        "Recover the matched `FloatCmpOp` variant name from `key`. \
         Thin forwarder to `Node.float_cmp_op()`.";
}

#[pymethods]
impl PyMatch {
    /// The root node where the top-level pattern matched, as a `u32`
    /// node id.  Pair with `Function.node(node_id).asm_fingerprint()` /
    /// `Cfg.fingerprint_pcode(node)` (both accept this `Match` or its
    /// raw `root` id directly) for proof-of-correctness queries that
    /// don't carry an explicit `Capture` (the root has no user-visible
    /// capture binding).
    #[getter]
    fn root(&self) -> u32 {
        self.inner[0].root().as_u32()
    }

    /// The per-input-pattern root node ids as a `list[int]` — one entry per
    /// pattern passed to the query (a single-pattern query yields `[root]`).
    /// `root` is the convenience accessor for the first (single-pattern) case.
    #[getter]
    fn roots(&self) -> Vec<u32> {
        self.inner.iter().map(|m| m.root().as_u32()).collect()
    }

    /// `m["name"]` / `m[capture]` — best-effort: integer if value
    /// output is an int, bool if it's a bool, raw bits otherwise.
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

    /// `capture in m` — True if `key` (a `Capture` or string name) has
    /// a binding in this match.
    fn __contains__(&self, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        Ok(self.is_bound(cap))
    }

    /// The capture's value as an unsigned `int`, or `None` when the
    /// capture isn't bound to an integer-valued node.  Thin forwarder to
    /// `Node.const_uint()`.
    fn const_uint(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<u128>> {
        match self.node(py, key)? {
            Some(node) => node.const_uint(py),
            None => Ok(None),
        }
    }

    /// The capture's value as a signed `int` (sign-interpreted at the
    /// node's width), or `None` when not bound to an integer node.  Thin
    /// forwarder to `Node.const_int()`.
    #[pyo3(name = "const_int")]
    fn int_(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<i128>> {
        match self.node(py, key)? {
            Some(node) => node.const_int(py),
            None => Ok(None),
        }
    }

    /// The capture's value as a `bool`, or `None` when not bound to a
    /// boolean-valued node.  Thin forwarder to `Node.const_bool()`.
    #[pyo3(name = "const_bool")]
    fn bool_(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<bool>> {
        match self.node(py, key)? {
            Some(node) => node.const_bool(py),
            None => Ok(None),
        }
    }

    /// The capture's value as raw float bits (`u64`), or `None` when not
    /// bound to a float-valued node.  Thin forwarder to
    /// `Node.float_bits()`.
    fn float_bits(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<u64>> {
        match self.node(py, key)? {
            Some(node) => node.float_bits(py),
            None => Ok(None),
        }
    }

    /// Returns True if the capture has a binding.
    fn has(&self, key: CaptureKey<'_>) -> PyResult<bool> {
        let cap = key.resolve()?;
        Ok(self.is_bound(cap))
    }

    // ── Op-variant accessors (for *_any captures) ───────────────────
    //
    // When you match via `int_bin_any(c, l, r)`, the bound capture
    // `c` carries the matched op variant.  The accessors below
    // recover that variant as the op's canonical Sleigh-style
    // string ("Add", "Sub", "Less", "Equal", ...).  Returns `None`
    // when the capture isn't bound or the bound node isn't of the
    // matching kind family.  See the `op_forwarders!` block above.

    /// Recover the matched varnode from `key`.  Returns the `Vn`
    /// associated with the captured `InitialVar` / `Call`/`CallOther`
    /// clobber output, or `None` when `key` doesn't bind such a node.
    /// Thin forwarder to `Node.vn()`.
    fn vn(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<crate::sleigh::PyVn>> {
        match self.node(py, key)? {
            Some(node) => node.vn(py),
            None => Ok(None),
        }
    }

    /// Returns the asm-instruction-address fingerprint of the node
    /// bound to `key` as a sorted-deduplicated `list[int]`.  Returns an
    /// empty list when the capture is unbound or when the captured
    /// node is one of the documented exempt kinds (see
    /// `strider_ir::Graph::asm_fingerprint`).  Thin forwarder to
    /// `Node.asm_fingerprint()`.
    ///
    /// The fingerprint is the proof-of-correctness aid: when a pattern
    /// query captures a value node, this list names the machine
    /// instructions whose lifting (or subsequent rewrite) contributed
    /// to that node's value.
    fn asm_fingerprint(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Vec<u64>> {
        match self.node(py, key)? {
            Some(node) => node.asm_fingerprint(py),
            None => Ok(Vec::new()),
        }
    }

    /// Returns a `Node` handle on the node bound to `key` (a `Capture`
    /// or string capture-name), or `None` when `key` is unbound in this
    /// match.
    ///
    /// The returned `Node` is a discoverable entry point into the IR
    /// graph: walk its `inputs()`, read its `kind()`, pull out constant
    /// values, etc.  Unlike `Match.root` (which always returns the raw
    /// top-level `u32` id), this resolves an explicit capture binding.
    /// Every other value/op reader on `Match` is a thin forwarder built
    /// on top of this resolution — `Node` is the single source of truth
    /// for per-node reads.
    fn node(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<crate::node::PyNode>> {
        let cap = key.resolve()?;
        // Re-borrow the function to validate the generation hasn't drifted
        // before handing out a node id that may point at a stale arena,
        // and to resolve an `Output` binding back to its owning node.
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
