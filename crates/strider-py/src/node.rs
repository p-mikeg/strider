use std::hash::{Hash, Hasher};

use pyo3::basic::CompareOp;
use pyo3::prelude::*;

use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

use crate::errors::into_strider_err;
use crate::function::PyFunction;

/// A handle on a single node in the IR graph.
///
/// Every accessor raises `StriderError` once the function has been compacted
/// or otherwise reshuffled, rather than dereferencing a stale node id.
#[pyclass(name = "Node", module = "strider.ir")]
pub struct PyNode {
    pub(crate) function: Py<PyFunction>,
    pub(crate) id: u32,
    /// Sampled at construction, compared on every accessor.
    pub(crate) generation: u64,
}

impl PyNode {
    /// For a caller that has already resolved `node_id` against `generation`
    /// under its own read borrow.
    pub(crate) fn validated(function: Py<PyFunction>, node_id: u32, generation: u64) -> Self {
        Self {
            function,
            id: node_id,
            generation,
        }
    }

    /// `StriderError` when `node_id` is not a live node in the function.
    pub(crate) fn new(py: Python<'_>, function: Py<PyFunction>, node_id: u32) -> PyResult<Self> {
        let generation = {
            let borrow = function.borrow(py);
            let guard = borrow.read_inner().map_err(into_strider_err)?;
            // Validate eagerly so a bad id fails here, not at the first
            // accessor call.
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

    /// Borrow for read, check the generation hasn't drifted and the id is
    /// still live, then run `f` on the resolved `(function, NodeId)`.
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

    /// The FIRST value output, skipping control / memory slots. A `Call`
    /// carries several (return value, then the clobbers), so this is not a
    /// unique answer for one; control / memory / phi-token-only nodes have
    /// none at all.
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

/// The unsigned integer constant a node's value output holds, if any. For
/// reading a bound capture without constructing a `PyNode`.
pub(crate) fn uint_of(function: &strider_ir::Function, node_id: u32) -> Option<u128> {
    let nid = function.graph().node_id_from_u32(node_id)?;
    let value = PyNode::value_output(function, nid)?;
    function.int_const_u128(value)
}

/// [`uint_of`] read as two's-complement at the declared width.
pub(crate) fn sint_of(function: &strider_ir::Function, node_id: u32) -> Option<i128> {
    let nid = function.graph().node_id_from_u32(node_id)?;
    let value = PyNode::value_output(function, nid)?;
    function.int_const_i128(value)
}

#[pymethods]
impl PyNode {
    /// Exposes the strong `function` back-reference so the cyclic GC can see a
    /// cycle routed through a `Node`. The cycle is broken at the reader's
    /// `__dict__` / `PyLifter::__clear__`, and `function` is load-bearing for
    /// as long as the `Node` lives.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.function)
    }

    /// The node's raw integer id in the graph. Raises once `compact` /
    /// `optimize` has invalidated every outstanding id.
    #[getter]
    fn id(&self, py: Python<'_>) -> PyResult<u32> {
        self.with_node(py, |_, _| self.id)
    }

    /// The node's kind as a string. Payload-carrying kinds render with their
    /// payload (`"IntBinaryOp(Add)"`, `"Load(RAM)"`), others render bare
    /// (`"Region"`, `"Phi"`).
    fn kind(&self, py: Python<'_>) -> PyResult<String> {
        // `Load` / `Store` carry an `rsleigh::VnSpace`, whose `Debug` is its
        // internal shortcut byte. Render the space by the name `VnSpace`
        // exposes, so the string a caller matches on is one they can write.
        self.with_node(py, |function, nid| match function.node_kind(nid) {
            NodeKind::Load(space) => {
                format!(
                    "Load({})",
                    crate::sleigh::PyVnSpace { inner: *space }.name()
                )
            }
            NodeKind::Store(space) => {
                format!(
                    "Store({})",
                    crate::sleigh::PyVnSpace { inner: *space }.name()
                )
            }
            other => format!("{other:?}"),
        })
    }

    /// The operation variant of an op-carrying node (`"Add"`, `"Less"`,
    /// `"Sqrt"`), or `None` for kinds carrying no operation.
    pub(crate) fn op(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| match function.node_kind(nid) {
            NodeKind::IntBinaryOp(op) => Some(format!("{op:?}")),
            NodeKind::IntUnaryOp(op) => Some(format!("{op:?}")),
            NodeKind::IntCmpOp(op) => Some(format!("{op:?}")),
            NodeKind::FloatBinaryOp(op) => Some(format!("{op:?}")),
            NodeKind::FloatUnaryOp(op) => Some(format!("{op:?}")),
            NodeKind::FloatCmpOp(op) => Some(format!("{op:?}")),
            _ => None,
        })
    }

    /// The node's value-output type as a string (`"I1"`, `"I32"`, `"F64"`), or
    /// `None` for a node with no value output. Booleans are the 1-bit integer
    /// `I1`.
    pub(crate) fn value_type(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| {
            let value = Self::value_output(function, nid)?;
            let ty = function.value_kind(value).as_value()?;
            Some(format!("{ty:?}"))
        })
    }

    /// The producer node behind each input edge, in input-slot order. A
    /// producer appears once per slot it feeds.
    fn inputs(&self, py: Python<'_>) -> PyResult<Vec<PyNode>> {
        // Collect ids under the read borrow, build the child `PyNode`s after
        // dropping it: `PyNode::new` re-borrows the function.
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

    /// The nodes consuming this node's outputs. A consumer appears once per
    /// edge it draws from this node, in output-slot then use order.
    fn outputs(&self, py: Python<'_>) -> PyResult<Vec<PyNode>> {
        // Collect ids under the read borrow, build the child `PyNode`s after
        // dropping it: `PyNode::new` re-borrows the function.
        let consumer_ids: Vec<u32> = self.with_node(py, |function, nid| {
            let mut ids = Vec::new();
            for &out in function.node_outputs(nid) {
                for (consumer, _slot) in function.value_uses(out) {
                    ids.push(consumer.as_u32());
                }
            }
            ids
        })?;
        let mut out = Vec::with_capacity(consumer_ids.len());
        for cid in consumer_ids {
            out.push(PyNode::new(py, self.function.clone_ref(py), cid)?);
        }
        Ok(out)
    }

    /// Integer constant value, sign-extended at the declared width. `None`
    /// when the value output isn't an integer `IntConst`, or is declared wider
    /// than 128 bits: an `I256` / `I512` holding a small value still answers
    /// `None` here, unlike `uint` (use `wide_const_bytes()` for those).
    /// Booleans are 1-bit integers, so a bool constant surfaces as `0` / `-1`.
    pub(crate) fn sint(&self, py: Python<'_>) -> PyResult<Option<i128>> {
        self.with_node(py, |function, nid| {
            Self::value_output(function, nid).and_then(|value| function.int_const_i128(value))
        })
    }

    /// Integer constant value, masked to the declared width. `None` when the
    /// value output isn't an integer `IntConst`, or holds a value that does
    /// not fit in 128 bits. An `I256` / `I512` constant small enough to fit
    /// still reads back here, unlike `sint`.
    pub(crate) fn uint(&self, py: Python<'_>) -> PyResult<Option<u128>> {
        self.with_node(py, |function, nid| {
            Self::value_output(function, nid).and_then(|value| function.int_const_u128(value))
        })
    }

    /// Boolean constant value, or `None` when the value output isn't an
    /// `I1`-typed `IntConst`.
    pub(crate) fn boolean(&self, py: Python<'_>) -> PyResult<Option<bool>> {
        self.with_node(py, |function, nid| {
            Self::value_output(function, nid).and_then(|value| function.bool_const_val(value))
        })
    }

    /// Raw IEEE 754 bit pattern, or `None` when the node isn't a `FloatConst`.
    pub(crate) fn float_bits(&self, py: Python<'_>) -> PyResult<Option<u64>> {
        self.with_node(py, |function, nid| match function.node_kind(nid) {
            NodeKind::FloatConst(bits) => Some(*bits),
            _ => None,
        })
    }

    /// The varnode this node names, else `None`: for `InitialVar` the varnode
    /// read at entry, for a `Call` the register it returns in, and for a
    /// `CallOther` whatever varnode the sla assigns its result to: a
    /// `unique` temporary (x86 `cpuid`), a tracked register (AArch64
    /// `popcount32` writes `q0`), or nothing at all (MIPS `udiv`).
    ///
    /// A `Node` names a node, not one of its outputs, so a multi-output `Call`
    /// answers for its FIRST value output, never for one clobber in particular.
    pub(crate) fn vn(&self, py: Python<'_>) -> PyResult<Option<crate::sleigh::PyVn>> {
        let vn = self.with_node(py, |function, nid| {
            if matches!(
                function.node_kind(nid),
                NodeKind::Call | NodeKind::CallOther { .. }
            ) && let Some(value) = Self::value_output(function, nid)
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

    /// Sorted, deduped machine-instruction addresses whose lift or subsequent
    /// rewrite contributed to this node's value. Empty for structural kinds
    /// (Entry, InitialMemory, phis, Region).
    pub(crate) fn asm_fingerprint(&self, py: Python<'_>) -> PyResult<Vec<u64>> {
        self.with_node(py, |function, nid| {
            // The side table yields an unordered set; sort for the documented
            // Python-facing order.
            let mut addrs: Vec<u64> = function
                .side_tables()
                .asm_fingerprint(nid)
                .into_iter()
                .collect();
            addrs.sort_unstable();
            addrs
        })
    }

    /// Raw little-endian bytes of a wide integer constant, one per byte of the
    /// declared width (9 for I72, 10 for I80, 12 for I96, 14 for I112, 16 for
    /// I128, 32 for I256, 64 for I512), or `None` for a narrow constant and
    /// any non-const kind.
    fn wide_const_bytes<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<Option<Bound<'py, pyo3::types::PyBytes>>> {
        let raw = self.with_node(py, |function, nid| function.int_const_wide_le_bytes(nid))?;
        Ok(raw.map(|b| pyo3::types::PyBytes::new_bound(py, &b)))
    }

    /// Sleigh user-op name on a `CallOther` node, else `None`.
    fn call_other_name(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| {
            function
                .side_tables()
                .call_other_name(nid)
                .map(str::to_owned)
        })
    }

    /// A repr must not raise; a stale handle renders as such.
    fn __repr__(&self, py: Python<'_>) -> String {
        match self.kind(py) {
            Ok(kind) => format!("Node(#{} {})", self.id, kind),
            Err(_) => format!("Node(#{} stale)", self.id),
        }
    }

    /// Equal when both reference the same function object, the same id AND the
    /// same graph generation: a stale handle raises on every accessor, so it is
    /// not the node a re-fetched handle names.
    fn __richcmp__(&self, py: Python<'_>, other: &PyNode, op: CompareOp) -> PyResult<PyObject> {
        let same = self.id == other.id
            && self.generation == other.generation
            && self.function.as_ptr() == other.function.as_ptr();
        match op {
            CompareOp::Eq => Ok(same.into_py(py)),
            CompareOp::Ne => Ok((!same).into_py(py)),
            // No meaningful ordering on nodes.
            _ => Ok(py.NotImplemented()),
        }
    }

    /// Hashes `(function identity, id, generation)` to stay consistent with
    /// `__eq__`.
    fn __hash__(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (self.function.as_ptr() as usize).hash(&mut hasher);
        self.id.hash(&mut hasher);
        self.generation.hash(&mut hasher);
        hasher.finish()
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyNode>()
}
