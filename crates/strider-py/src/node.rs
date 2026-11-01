use pyo3::prelude::*;

use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

use crate::errors::into_strider_err;
use crate::function::FunctionAt;

/// A handle on a single node in the IR graph.
///
/// Every accessor raises `StriderError` once the function has been compacted
/// or otherwise reshuffled, rather than dereferencing a stale node id.
///
/// Equal, and hashed alike, when both reference the same function object, the
/// same id AND the same graph generation: a stale handle raises on every
/// accessor, so it is not the node a re-fetched handle names.
#[pyclass(eq, hash, name = "Node", module = "strider.ir", frozen)]
#[derive(PartialEq, Hash)]
pub struct PyNode {
    pub(crate) at: FunctionAt,
    pub(crate) id: u32,
}

/// `node_id` as a live node of `function`, else `StriderError`.
pub(crate) fn node_id(
    function: &strider_ir::Function,
    node_id: u32,
) -> PyResult<strider_ir::node::NodeId> {
    function
        .graph()
        .node_id_from_u32(node_id)
        .ok_or_else(|| into_strider_err(anyhow::anyhow!("no node with id {node_id} in function")))
}

impl PyNode {
    /// Run `f` on the function and this node's id, once [`FunctionAt::read`]
    /// admits it.
    fn with_node<F, R>(&self, py: Python<'_>, f: F) -> PyResult<R>
    where
        F: FnOnce(&strider_ir::Function, strider_ir::node::NodeId) -> R,
    {
        self.at.read(py, "Node", |function| {
            node_id(function, self.id).map(|nid| f(function, nid))
        })?
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

/// The unsigned integer constant a node's value output holds, if any.
pub(crate) fn uint_of(
    function: &strider_ir::Function,
    nid: strider_ir::node::NodeId,
) -> Option<strider_ir::UBig> {
    function
        .int_const(PyNode::value_output(function, nid)?)
        .cloned()
}

/// [`uint_of`] read as two's-complement at the declared width.
pub(crate) fn sint_of(
    function: &strider_ir::Function,
    nid: strider_ir::node::NodeId,
) -> Option<strider_ir::IBig> {
    function.int_const_ibig(PyNode::value_output(function, nid)?)
}

#[pymethods]
impl PyNode {
    /// Exposes the strong `function` back-reference so the cyclic GC can see a
    /// cycle routed through a `Node`. The cycle is broken at the reader's
    /// `__dict__` / `PyLifter::__clear__`, and `function` is load-bearing for
    /// as long as the `Node` lives.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.at.function)
    }

    /// The node's raw integer id in the graph. Raises once the handle is
    /// stale.
    #[getter]
    fn id(&self, py: Python<'_>) -> PyResult<u32> {
        self.with_node(py, |_, _| self.id)
    }

    /// The node's kind as a string. An op or a memory space renders with its
    /// payload (`"IntBinaryOp(Add)"`, `"Load(RAM)"`), and a float constant
    /// with its raw bits (`"FloatConst(<bits>)"`); an interned or opaque
    /// handle does not (`"IntConst"`, `"InitialVar"`, `"CallOther"`), nor does
    /// a kind with none (`"Region"`, `"Phi"`).
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
            NodeKind::IntConst(_) => "IntConst".to_string(),
            NodeKind::InitialVar(_) => "InitialVar".to_string(),
            NodeKind::CallOther { .. } => "CallOther".to_string(),
            NodeKind::Switch(_) => "Switch".to_string(),
            NodeKind::Call { .. } => "Call".to_string(),
            other => format!("{other:?}"),
        })
    }

    /// The operation variant of an op-carrying node (`"Add"`, `"Less"`,
    /// `"Sqrt"`, `"SignExtend"`), or `None` for kinds carrying no operation.
    pub(crate) fn op(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| match function.node_kind(nid) {
            NodeKind::IntBinaryOp(op) => Some(format!("{op:?}")),
            NodeKind::IntUnaryOp(op) => Some(format!("{op:?}")),
            NodeKind::IntCmpOp(op) => Some(format!("{op:?}")),
            NodeKind::FloatBinaryOp(op) => Some(format!("{op:?}")),
            NodeKind::FloatUnaryOp(op) => Some(format!("{op:?}")),
            NodeKind::FloatCmpOp(op) => Some(format!("{op:?}")),
            NodeKind::Extend(op) => Some(format!("{op:?}")),
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
        self.with_node(py, |function, nid| {
            function
                .node_inputs(nid)
                .into_iter()
                .map(|value| self.at.node(py, function.producer(value)))
                .collect()
        })
    }

    /// The nodes consuming this node's outputs. A consumer appears once per
    /// edge it draws from this node, in output-slot then use order.
    fn outputs(&self, py: Python<'_>) -> PyResult<Vec<PyNode>> {
        self.with_node(py, |function, nid| {
            let mut out = Vec::new();
            for &value in function.node_outputs(nid) {
                for (consumer, _slot) in function.value_uses(value) {
                    out.push(self.at.node(py, consumer));
                }
            }
            out
        })
    }

    /// Integer constant value, sign-extended at the declared width, at any
    /// width: a Python `int` is arbitrary-precision, so `I256` / `I512` read
    /// back here like any other. `None` when the value output isn't an
    /// integer `IntConst`.
    /// Booleans are 1-bit integers, so a bool constant surfaces as `0` / `-1`.
    pub(crate) fn sint(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        self.with_node(py, sint_of)?
            .map(|v| crate::bigint::ibig_to_py_int(py, &v))
            .transpose()
    }

    /// Integer constant value, masked to the declared width, at any width: a
    /// Python `int` is arbitrary-precision, so `I256` / `I512` read back here
    /// like any other. `None` when the value output isn't an integer
    /// `IntConst`.
    pub(crate) fn uint(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        self.with_node(py, uint_of)?
            .map(|v| crate::bigint::ubig_to_py_int(py, &v))
            .transpose()
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
                NodeKind::Call { .. } | NodeKind::CallOther { .. }
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
    /// (Entry, InitialMemory, InitialVar, Region, Phi, MemPhi).
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

    /// The user-op name attached to a `CallOther` node, else `None`.
    fn call_other_name(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| {
            function.call_other_name(nid).map(str::to_owned)
        })
    }

    /// A repr must not raise; a stale handle renders as such.
    fn __repr__(&self, py: Python<'_>) -> String {
        match self.kind(py) {
            Ok(kind) => format!("Node(#{} {})", self.id, kind),
            Err(_) => format!("Node(#{} stale)", self.id),
        }
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyNode>()
}
