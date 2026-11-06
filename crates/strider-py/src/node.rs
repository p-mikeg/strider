use pyo3::prelude::*;

use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

use crate::errors::into_strider_err;
use crate::function::FunctionAt;

/// A handle on one IR node. Every accessor raises `StriderError` once the
/// function has been compacted, optimized or rewritten. Equal handles share
/// the function, id and graph generation.
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
    /// Run `f` on the function and this node's id, unless the handle is stale.
    fn with_node<F, R>(&self, py: Python<'_>, f: F) -> PyResult<R>
    where
        F: FnOnce(&strider_ir::Function, strider_ir::node::NodeId) -> R,
    {
        self.at.read(py, "Node", |function| {
            node_id(function, self.id).map(|nid| f(function, nid))
        })?
    }

    /// The first value output; a `Call`'s return value, ahead of its clobbers.
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
#[inline]
pub(crate) fn uint_of(
    function: &strider_ir::Function,
    nid: strider_ir::node::NodeId,
) -> Option<strider_ir::UBig> {
    function
        .int_const(PyNode::value_output(function, nid)?)
        .cloned()
}

/// [`uint_of`] read as two's-complement at the declared width.
#[inline]
pub(crate) fn sint_of(
    function: &strider_ir::Function,
    nid: strider_ir::node::NodeId,
) -> Option<strider_ir::IBig> {
    function.int_const_ibig(PyNode::value_output(function, nid)?)
}

#[pymethods]
impl PyNode {
    // The cycle is broken at the reader's `__dict__` / `PyLifter::__clear__`.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.at.function)
    }

    /// The node's integer id.
    #[getter]
    fn id(&self, py: Python<'_>) -> PyResult<u32> {
        self.with_node(py, |_, _| self.id)
    }

    /// The node's kind: an op or memory space carries its payload
    /// (`"IntBinaryOp(Add)"`, `"Load(RAM)"`), a float constant its raw bits
    /// (`"FloatConst(<bits>)"`), other kinds are bare (`"IntConst"`).
    fn kind(&self, py: Python<'_>) -> PyResult<String> {
        // A `VnSpace`'s `Debug` is its shortcut byte; render its name instead.
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

    /// The operation of an op node (`"Add"`, `"Less"`, `"SignExtend"`), else
    /// `None`.
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

    /// The value-output type (`"I1"`, `"I32"`, `"F64"`), or `None` without
    /// one.
    pub(crate) fn value_type(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_node(py, |function, nid| {
            let value = Self::value_output(function, nid)?;
            let ty = function.value_kind(value).as_value()?;
            Some(format!("{ty:?}"))
        })
    }

    /// The producer of each input, in slot order (once per slot).
    fn inputs(&self, py: Python<'_>) -> PyResult<Vec<PyNode>> {
        self.with_node(py, |function, nid| {
            function
                .node_inputs(nid)
                .into_iter()
                .map(|value| self.at.node(py, function.producer(value)))
                .collect()
        })
    }

    /// The consumers of this node's outputs, in output-slot then use order
    /// (once per edge).
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

    /// The integer constant, sign-extended at its width, else `None`. A
    /// boolean reads as `0` / `-1`.
    pub(crate) fn sint(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        self.with_node(py, sint_of)?
            .map(|v| crate::bigint::ibig_to_py_int(py, &v))
            .transpose()
    }

    /// The integer constant, masked to its width, else `None`.
    pub(crate) fn uint(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        self.with_node(py, uint_of)?
            .map(|v| crate::bigint::ubig_to_py_int(py, &v))
            .transpose()
    }

    /// The boolean constant, else `None`.
    pub(crate) fn boolean(&self, py: Python<'_>) -> PyResult<Option<bool>> {
        self.with_node(py, |function, nid| {
            Self::value_output(function, nid).and_then(|value| function.bool_const_val(value))
        })
    }

    /// A float constant's IEEE 754 bits, else `None`.
    pub(crate) fn float_bits(&self, py: Python<'_>) -> PyResult<Option<u64>> {
        self.with_node(py, |function, nid| match function.node_kind(nid) {
            NodeKind::FloatConst(bits) => Some(*bits),
            _ => None,
        })
    }

    /// The varnode an `InitialVar` reads, a `Call` returns in, or a
    /// `CallOther` result is assigned to; else `None`.
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

    /// Sorted, deduplicated machine addresses recorded on this node: its
    /// lift's, each reusing lift's, and those of nodes folded into it. A
    /// superset of the instructions behind the value: a shared constant
    /// carries every address that used it, and so does a node folded with
    /// one, so the set can be far wider than the node's own instructions.
    pub(crate) fn asm_fingerprint(&self, py: Python<'_>) -> PyResult<Vec<u64>> {
        self.with_node(py, |function, nid| {
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

    /// `Node(#<id> <kind>)`, or `Node(#<id> stale)`.
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
