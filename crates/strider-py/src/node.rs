//! `PyNode` — a discoverable handle on a single IR graph node.
//!
//! Mirrors how [`crate::matcher::PyMatch`] references the function: a
//! `PyNode` carries a `Py<PyFunction>` so accessors can re-borrow the
//! shared `Arc<RwLock<Function>>`, plus a raw `u32` node id and a
//! generation snapshot taken at construction time.  Any arena-reshuffle
//! op (`Function.compact`, `optimize`, …) bumps `Function::generation()`,
//! and every accessor compares against the snapshot so a stale id
//! surfaces as a typed `StriderError` instead of dereferencing the wrong
//! node.
//!
//! Construction goes through [`PyNode::new`] (validated id) so the rest
//! of the bindings can hand out `Node`s from `Function.node(id)` and
//! `Match.node(key)` without duplicating the validation / generation
//! plumbing.

use std::hash::{Hash, Hasher};

use pyo3::basic::CompareOp;
use pyo3::prelude::*;

use strider_ir::node::NodeKind;
use strider_ir::IRViewer;

use crate::errors::into_strider_err;
use crate::function::PyFunction;

/// A handle on a single node in the IR graph.
///
/// Returned by `Function.node(id)` and `Match.node(capture)`.  Lets you
/// explore the sea-of-nodes IR beyond pattern matching: walk the
/// data/control edges feeding a node (`inputs()`), read its kind
/// (`kind()`), pull out constant values (`const_int()` / `const_bool()`),
/// and recover provenance (`fingerprint()`).
///
/// Snapshots the function's generation counter at construction.  A
/// subsequent arena-reshuffling op (`Function.compact`, `optimize`, …)
/// bumps the counter; every accessor then raises a `StriderError` rather
/// than dereferencing a stale node id.
#[pyclass(name = "Node", module = "strider")]
pub struct PyNode {
    pub(crate) function: Py<PyFunction>,
    /// Raw arena index of the node this handle points at.
    pub(crate) id: u32,
    /// Generation counter sampled at construction.  Compared against
    /// `Function::generation()` on every accessor.
    pub(crate) generation: u64,
}

impl PyNode {
    /// Construct a validated `PyNode` for `node_id`, snapshotting the
    /// function's current generation counter.  Returns `StriderError`
    /// when `node_id` is not a live node in the function.
    pub(crate) fn new(py: Python<'_>, function: Py<PyFunction>, node_id: u32) -> PyResult<Self> {
        let generation = {
            let borrow = function.borrow(py);
            let guard = borrow.read_inner().map_err(into_strider_err)?;
            // Validate the id eagerly so a bad id fails at the point of
            // construction rather than at the first accessor call.
            if guard.graph().node_id_from_u32(node_id).is_none() {
                return Err(into_strider_err(anyhow::anyhow!(
                    "no node with id {node_id} in function"
                )));
            }
            guard.graph().generation()
        };
        Ok(Self {
            function,
            id: node_id,
            generation,
        })
    }

    /// Borrow the function for read, confirm the generation hasn't
    /// drifted and the node id is still live, then run `f` against the
    /// resolved `(function, NodeId)`.  Centralises the borrow / poison-map
    /// / generation-check / id-revalidation ritual every accessor shares.
    fn with_node<F, R>(&self, py: Python<'_>, f: F) -> PyResult<R>
    where
        F: FnOnce(&strider_ir::Function, strider_ir::node::NodeId) -> R,
    {
        let borrow = self.function.borrow(py);
        let guard = borrow.read_inner().map_err(into_strider_err)?;
        if guard.graph().generation() != self.generation {
            return Err(into_strider_err(anyhow::anyhow!(
                "Node is stale: function was compacted / reshuffled after this Node was \
                 created (node generation = {}, function generation = {}).  Re-fetch the \
                 node from the post-compaction function.",
                self.generation,
                guard.graph().generation(),
            )));
        }
        let nid = guard.graph().node_id_from_u32(self.id).ok_or_else(|| {
            into_strider_err(anyhow::anyhow!("no node with id {} in function", self.id))
        })?;
        Ok(f(&guard, nid))
    }

    /// Returns the single value-producing `ValueId` of `nid`, if
    /// any.  Multi-output nodes (e.g. `Load = [Memory, Value]`) carry one
    /// value slot; control / memory / phi-token-only nodes have none.
    fn value_output(
        function: &strider_ir::Function,
        nid: strider_ir::node::NodeId,
    ) -> Option<strider_ir::node::ValueId> {
        function
            .node_outputs(nid)
            .iter()
            .copied()
            .find(|&value| function.value_kind(value).is_value())
    }
}

#[pymethods]
impl PyNode {
    /// The raw `u32` arena index of this node.  Stable for the lifetime
    /// of the function unless an arena-reshuffling op (`compact`,
    /// `optimize`) runs, which invalidates outstanding ids.
    #[getter]
    fn id(&self) -> u32 {
        self.id
    }

    /// The node's `NodeKind` formatted as a string (e.g. `"IntConst"`,
    /// `"Call"`, `"Phi"`, `"Add"`).  The node's `NodeKind` formatted as a
    /// string.
    fn kind(&self, py: Python<'_>) -> PyResult<String> {
        self.with_node(py, |function, nid| format!("{:?}", function.node_kind(nid)))
    }

    /// The data / control nodes feeding this one, as a list of `Node`s.
    ///
    /// Each input edge is a `ValueId`; this maps every one through
    /// to its producer `NodeId` and returns those producers.  Order
    /// follows the node's input-slot order.  An input may appear more
    /// than once if the same producer feeds multiple slots.
    fn inputs(&self, py: Python<'_>) -> PyResult<Vec<PyNode>> {
        // Collect the producer ids under the read borrow, then build the
        // child `PyNode`s after dropping the guard (each `PyNode::new`
        // re-borrows the function).
        let producer_ids: Vec<u32> = self.with_node(py, |function, nid| {
            function
                .node_inputs(nid)
                .into_iter()
                .map(|value| function.producer(value).as_u32())
                .collect()
        })?;
        let mut out = Vec::with_capacity(producer_ids.len());
        for pid in producer_ids {
            out.push(PyNode::new(py, self.function.clone_ref(py), pid)?);
        }
        Ok(out)
    }

    /// The node's integer constant value as a signed `int` (sign-extended
    /// at the value's declared width), or `None` when its value output
    /// isn't an integer `IntConst` or the stored magnitude exceeds 128
    /// bits (I256/I512 — use `wide_const_bytes()` for those).  Booleans
    /// are 1-bit integers, so a bool constant surfaces here as `0` / `-1`.
    pub(crate) fn const_int(&self, py: Python<'_>) -> PyResult<Option<i128>> {
        self.with_node(py, |function, nid| {
            Self::value_output(function, nid).and_then(|value| function.int_const_i128(value))
        })
    }

    /// The node's integer constant value as an unsigned `int` (masked to
    /// the value's declared width), or `None` when its value output isn't
    /// an integer `IntConst` or the stored magnitude exceeds 128 bits
    /// (I256/I512 — use `wide_const_bytes()` for those).
    pub(crate) fn const_uint(&self, py: Python<'_>) -> PyResult<Option<u128>> {
        self.with_node(py, |function, nid| {
            Self::value_output(function, nid).and_then(|value| function.int_const_u128(value))
        })
    }

    /// The node's boolean constant value, or `None` when its value
    /// output isn't an `I1`-typed `IntConst`.
    pub(crate) fn const_bool(&self, py: Python<'_>) -> PyResult<Option<bool>> {
        self.with_node(py, |function, nid| {
            Self::value_output(function, nid).and_then(|value| function.bool_const_val(value))
        })
    }

    /// The node's raw IEEE 754 bit pattern as `u64`, or `None` when it
    /// isn't a `FloatConst`.
    pub(crate) fn float_bits(&self, py: Python<'_>) -> PyResult<Option<u64>> {
        self.with_node(py, |function, nid| match function.node_kind(nid) {
            NodeKind::FloatConst(bits) => Some(*bits),
            _ => None,
        })
    }

    /// The `Vn` associated with this node, if one can be determined.
    /// Well-defined only for a handful of producer kinds:
    ///
    /// * `InitialVar(vn)` — the varnode whose function-entry value is
    ///   read.
    /// * `Call` / `CallOther` clobber output — the register the call
    ///   clobbers, recovered via `Function::get_vn_for_value` on the
    ///   node's value output.
    ///
    /// Returns `None` for any other node kind.
    pub(crate) fn vn(&self, py: Python<'_>) -> PyResult<Option<crate::sleigh::PyVn>> {
        let vn = self.with_node(py, |function, nid| {
            if matches!(function.node_kind(nid), NodeKind::Call | NodeKind::CallOther { .. })
                && let Some(value) = Self::value_output(function, nid)
                && let Some(vn) = function.get_vn_for_value(value)
            {
                return Some(vn);
            }
            match function.node_kind(nid) {
                NodeKind::InitialVar(id) => Some(function.initial_vn(*id)),
                _ => None,
            }
        })?;
        Ok(vn.map(crate::sleigh::PyVn::from_inner))
    }

    /// If this node is an `IntBinaryOp`, its variant name (e.g. `"Add"`),
    /// else `None`.
    pub(crate) fn int_binary_op(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| match function.node_kind(nid) {
            NodeKind::IntBinaryOp(op) => Some(format!("{op:?}")),
            _ => None,
        })
    }

    /// If this node is an `IntUnaryOp`, its variant name, else `None`.
    pub(crate) fn int_unary_op(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| match function.node_kind(nid) {
            NodeKind::IntUnaryOp(op) => Some(format!("{op:?}")),
            _ => None,
        })
    }

    /// If this node is an `IntCmpOp`, its variant name (e.g. `"Less"`,
    /// `"Equal"`), else `None`.
    pub(crate) fn int_cmp_op(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| match function.node_kind(nid) {
            NodeKind::IntCmpOp(op) => Some(format!("{op:?}")),
            _ => None,
        })
    }

    /// If this node is a boolean binary op (an `IntBinaryOp` whose output
    /// is `I1`), its variant name, else `None`.
    ///
    /// Note: there is no `bool_unary_op`.  A boolean logical NOT is
    /// `Xor(x, IntConst(1)):I1`, so it is recovered here (returning
    /// `"Xor"`).
    pub(crate) fn bool_binary_op(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| {
            let NodeKind::IntBinaryOp(op) = function.node_kind(nid) else {
                return None;
            };
            let value = Self::value_output(function, nid)?;
            if !function.value_kind(value).as_value()?.is_bool() {
                return None;
            }
            Some(format!("{op:?}"))
        })
    }

    /// If this node is a `FloatBinaryOp`, its variant name, else `None`.
    pub(crate) fn float_binary_op(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| match function.node_kind(nid) {
            NodeKind::FloatBinaryOp(op) => Some(format!("{op:?}")),
            _ => None,
        })
    }

    /// If this node is a `FloatUnaryOp`, its variant name, else `None`.
    pub(crate) fn float_unary_op(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| match function.node_kind(nid) {
            NodeKind::FloatUnaryOp(op) => Some(format!("{op:?}")),
            _ => None,
        })
    }

    /// If this node is a `FloatCmpOp`, its variant name, else `None`.
    pub(crate) fn float_cmp_op(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| match function.node_kind(nid) {
            NodeKind::FloatCmpOp(op) => Some(format!("{op:?}")),
            _ => None,
        })
    }

    /// The asm-fingerprint addresses recorded on this node — a sorted,
    /// deduped list of machine-instruction addresses whose lift (or
    /// subsequent rewrite) contributed to the node's value.
    ///
    /// Empty for structural node kinds (Entry, InitialMemory, phis,
    /// Region) whose existence is synthesised by the IR builder rather
    /// than tied to a specific asm instruction.
    ///
    /// `pub(crate)` so `PyCfg::fingerprint_pcode` (`cfg.rs`)
    /// can reuse the same addr-only lookup instead of duplicating the
    /// side-table read.
    pub(crate) fn fingerprint(&self, py: Python<'_>) -> PyResult<Vec<u64>> {
        self.with_node(py, |function, nid| function.side_tables().asm_fingerprint(nid).to_vec())
    }

    /// Raw little-endian bytes of a wide-typed integer constant (10 bytes
    /// for I80, 16 for I128, 32 for I256, 64 for I512), or `None` for a
    /// narrow (≤ I64) constant and any non-const node kind.  Works whether
    /// the value is stored inline or interned — the width comes from the
    /// constant's declared type.
    fn wide_const_bytes(&self, py: Python<'_>) -> PyResult<Option<Vec<u8>>> {
        self.with_node(py, |function, nid| function.int_const_wide_le_bytes(nid))
    }

    /// The Sleigh user-op name attached to a `CallOther` node, or `None`
    /// for any other node kind.
    fn call_other_name(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| {
            function.side_tables().call_other_name(nid).map(str::to_owned)
        })
    }

    /// `Node(#<id> <kind>)` — e.g. `Node(#7 IntConst)`.
    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let kind = self.kind(py)?;
        Ok(format!("Node(#{} {})", self.id, kind))
    }

    /// Two `Node`s are equal when they reference the same function
    /// (object identity) and the same node id.
    fn __richcmp__(&self, py: Python<'_>, other: &PyNode, op: CompareOp) -> PyResult<PyObject> {
        let same = self.id == other.id && self.function.as_ptr() == other.function.as_ptr();
        match op {
            CompareOp::Eq => Ok(same.into_py(py)),
            CompareOp::Ne => Ok((!same).into_py(py)),
            // Nodes have no meaningful ordering.
            _ => Ok(py.NotImplemented()),
        }
    }

    /// Hash on `(function identity, id)` — consistent with `__eq__`.
    fn __hash__(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (self.function.as_ptr() as usize).hash(&mut hasher);
        self.id.hash(&mut hasher);
        hasher.finish()
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyNode>()
}
