//! Builder structs for [`crate::pat::Pat`].
//!
//! Every builder in this file emits a trait-backed pattern directly.  Data
//! builders (`IntBinaryOpPat`, `BoolBinaryOpPat`, `FloatBinaryOpPat`, the
//! memory family, `PhiPat`, `FunctionArgPat`) emit
//! [`Pat::from_dyn`](crate::pat::Pat::from_dyn) wrapping a [`NodePat`].
//! Control builders (`CallPat`, `CallOtherPat`, `RetPat`, `IfPat`) emit
//! [`Pat::from_ctrl`](crate::pat::Pat::from_ctrl) wrapping a
//! [`ControlNodePat`](crate::pat::control_pat::ControlNodePat).

use std::sync::Arc;

use ir::node::NodeKind;
use ir::{BoolBinaryOp, FloatBinaryOp, IntBinaryOp};

use crate::matcher::commutativity::{
    is_commutative_bool_op, is_commutative_float_op, is_commutative_int_op,
};
use crate::pat::control_pat::{ControlNodePat, CtrlKind};
use crate::pat::node_pat::{InputsSpec, NodePat};
use crate::pat::{Pat, int_const};
use crate::var::{NodeVar, Var};

// ── Builder: IntBinaryOpPat ───────────────────────────────────────────────────

/// Builder for integer binary operation patterns.
///
/// Returned by [`crate::pat::int_binary`] and the shorthand constructors
/// ([`crate::pat::add`], [`crate::pat::sub`], [`crate::pat::mul`], …).  Call
/// `.into()` (or pass directly to any `impl Into<Pat>` parameter) to obtain a
/// [`Pat`].
pub struct IntBinaryOpPat {
    pub(super) op: IntBinaryOp,
    pub(super) lhs: Pat,
    pub(super) rhs: Pat,
    pub(super) ordered: bool,
}

impl IntBinaryOpPat {
    pub(crate) fn new(op: IntBinaryOp, lhs: Pat, rhs: Pat) -> Self {
        Self {
            op,
            lhs,
            rhs,
            ordered: false,
        }
    }

    /// Force the pattern to match operands in the stated order only.
    /// By default, commutative operators (`Add`, `Mul`, `And`, `Or`, `Xor`)
    /// will also try the reversed operand order.
    pub fn ordered(mut self) -> Self {
        self.ordered = true;
        self
    }
}

impl From<IntBinaryOpPat> for Pat {
    fn from(b: IntBinaryOpPat) -> Pat {
        let op = b.op;
        let commutative_at_construction = !b.ordered && is_commutative_int_op(op);
        let inputs = if commutative_at_construction {
            InputsSpec::fixed_commutative(b.lhs, b.rhs)
        } else {
            InputsSpec::fixed_ordered(vec![b.lhs, b.rhs])
        };
        Pat::from_dyn(Arc::new(NodePat {
            kind_match: Arc::new(move |ctx, node, _b| {
                matches!(ctx.graph.graph.node_kind(node), NodeKind::IntBinaryOp(x) if *x == op)
            }),
            inputs,
            post_match: None,
            output_var: None,
            node_var: None,
        }))
    }
}

// ── Builder: BoolBinaryOpPat ──────────────────────────────────────────────────

/// Builder for boolean binary operation patterns.
///
/// Returned by [`crate::pat::bool_binary`] and the shorthand constructors
/// ([`crate::pat::bool_and`], [`crate::pat::bool_or`],
/// [`crate::pat::bool_xor`]).  Call `.into()` or pass directly to any
/// `impl Into<Pat>` parameter.
pub struct BoolBinaryOpPat {
    pub(super) op: BoolBinaryOp,
    pub(super) lhs: Pat,
    pub(super) rhs: Pat,
    pub(super) ordered: bool,
}

impl BoolBinaryOpPat {
    pub(crate) fn new(op: BoolBinaryOp, lhs: Pat, rhs: Pat) -> Self {
        Self {
            op,
            lhs,
            rhs,
            ordered: false,
        }
    }

    /// Force the pattern to match operands in the stated order only.
    pub fn ordered(mut self) -> Self {
        self.ordered = true;
        self
    }
}

impl From<BoolBinaryOpPat> for Pat {
    fn from(b: BoolBinaryOpPat) -> Pat {
        let op = b.op;
        let commutative_at_construction = !b.ordered && is_commutative_bool_op(op);
        let inputs = if commutative_at_construction {
            InputsSpec::fixed_commutative(b.lhs, b.rhs)
        } else {
            InputsSpec::fixed_ordered(vec![b.lhs, b.rhs])
        };
        Pat::from_dyn(Arc::new(NodePat {
            kind_match: Arc::new(move |ctx, node, _b| {
                matches!(ctx.graph.graph.node_kind(node), NodeKind::BoolBinaryOp(x) if *x == op)
            }),
            inputs,
            post_match: None,
            output_var: None,
            node_var: None,
        }))
    }
}

// ── Builder: FloatBinaryOpPat ─────────────────────────────────────────────────

/// Builder for float binary operation patterns.
///
/// Returned by [`crate::pat::float_binary`] and the shorthand constructors
/// ([`crate::pat::float_add`], [`crate::pat::float_sub`],
/// [`crate::pat::float_mul`], [`crate::pat::float_div`]).  Call `.into()` or
/// pass directly to any `impl Into<Pat>` parameter to obtain a [`Pat`].
pub struct FloatBinaryOpPat {
    pub(super) op: FloatBinaryOp,
    pub(super) lhs: Pat,
    pub(super) rhs: Pat,
    pub(super) ordered: bool,
}

impl FloatBinaryOpPat {
    pub(crate) fn new(op: FloatBinaryOp, lhs: Pat, rhs: Pat) -> Self {
        Self {
            op,
            lhs,
            rhs,
            ordered: false,
        }
    }

    /// Force the pattern to match operands in the stated order only.
    /// By default, commutative operators (`Add`, `Mul`) will also try the
    /// reversed operand order.
    pub fn ordered(mut self) -> Self {
        self.ordered = true;
        self
    }
}

impl From<FloatBinaryOpPat> for Pat {
    fn from(b: FloatBinaryOpPat) -> Pat {
        let op = b.op;
        let commutative_at_construction = !b.ordered && is_commutative_float_op(op);
        let inputs = if commutative_at_construction {
            InputsSpec::fixed_commutative(b.lhs, b.rhs)
        } else {
            InputsSpec::fixed_ordered(vec![b.lhs, b.rhs])
        };
        Pat::from_dyn(Arc::new(NodePat {
            kind_match: Arc::new(move |ctx, node, _b| {
                matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatBinaryOp(x) if *x == op)
            }),
            inputs,
            post_match: None,
            output_var: None,
            node_var: None,
        }))
    }
}

// ── Builder: LoadPat ──────────────────────────────────────────────────────────

/// Builder for `Load` node patterns.  Created by [`crate::pat::load`].
pub struct LoadPat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat>,
    output_var: Option<Var>,
    node_var: Option<NodeVar>,
}

impl LoadPat {
    pub(crate) fn new() -> Self {
        Self {
            space: None,
            addr: None,
            output_var: None,
            node_var: None,
        }
    }
    /// Restrict the match to loads in address space `s`.
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    /// Constrain the load's address operand.
    pub fn addr(mut self, p: impl Into<Pat>) -> Self {
        self.addr = Some(p.into());
        self
    }
    /// Bind the load's value output (`NodeOutputId`) to `v`.
    pub fn capture_output(mut self, v: Var) -> Self {
        self.output_var = Some(v);
        self
    }
    /// Bind the load's node (`NodeId`) to `nv`.
    pub fn capture_node(mut self, nv: NodeVar) -> Self {
        self.node_var = Some(nv);
        self
    }
}

impl From<LoadPat> for Pat {
    fn from(b: LoadPat) -> Pat {
        let LoadPat {
            space,
            addr,
            output_var,
            node_var,
        } = b;
        // Load inputs = [mem(0), addr(1)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(addr_pat) = addr {
            indexed.push((1, addr_pat));
        }
        Pat::from_dyn(Arc::new(NodePat {
            kind_match: Arc::new(move |ctx, node, _b| {
                matches!(
                    ctx.graph.graph.node_kind(node),
                    NodeKind::Load(actual) if space.is_none_or(|s| *actual == s)
                )
            }),
            inputs: InputsSpec::Indexed(indexed),
            post_match: None,
            output_var,
            node_var,
        }))
    }
}

// ── Builder: StorePat ─────────────────────────────────────────────────────────

/// Builder for `Store` node patterns.  Created by [`crate::pat::store`].
pub struct StorePat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat>,
    data: Option<Pat>,
    output_var: Option<Var>,
    node_var: Option<NodeVar>,
}

impl StorePat {
    pub(crate) fn new() -> Self {
        Self {
            space: None,
            addr: None,
            data: None,
            output_var: None,
            node_var: None,
        }
    }
    /// Restrict the match to stores in address space `s`.
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    /// Constrain the store's address operand.
    pub fn addr(mut self, p: impl Into<Pat>) -> Self {
        self.addr = Some(p.into());
        self
    }
    /// Constrain the value being stored.
    pub fn data(mut self, p: impl Into<Pat>) -> Self {
        self.data = Some(p.into());
        self
    }
    /// Bind the store's memory output (`NodeOutputId`) to `v`.
    pub fn capture_output(mut self, v: Var) -> Self {
        self.output_var = Some(v);
        self
    }
    /// Bind the store's node (`NodeId`) to `nv`.
    pub fn capture_node(mut self, nv: NodeVar) -> Self {
        self.node_var = Some(nv);
        self
    }
}

impl From<StorePat> for Pat {
    fn from(b: StorePat) -> Pat {
        let StorePat {
            space,
            addr,
            data,
            output_var,
            node_var,
        } = b;
        // Store inputs = [mem(0), addr(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(addr_pat) = addr {
            indexed.push((1, addr_pat));
        }
        if let Some(data_pat) = data {
            indexed.push((2, data_pat));
        }
        Pat::from_dyn(Arc::new(NodePat {
            kind_match: Arc::new(move |ctx, node, _b| {
                matches!(
                    ctx.graph.graph.node_kind(node),
                    NodeKind::Store(actual) if space.is_none_or(|s| *actual == s)
                )
            }),
            inputs: InputsSpec::Indexed(indexed),
            post_match: None,
            output_var,
            node_var,
        }))
    }
}

// ── Builder: StackStorePat ────────────────────────────────────────────────────

/// Builder for `StackStore` node patterns.  Created by [`crate::pat::stack_store`].
pub struct StackStorePat {
    space: Option<rsleigh::VnSpace>,
    offset: Option<i64>,
    data: Option<Pat>,
    output_var: Option<Var>,
    node_var: Option<NodeVar>,
}

impl StackStorePat {
    pub(crate) fn new() -> Self {
        Self {
            space: None,
            offset: None,
            data: None,
            output_var: None,
            node_var: None,
        }
    }
    /// Restrict the match to stack-stores in address space `s`.
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    /// Match only the stack-store at the given SP-relative offset.
    pub fn offset(mut self, o: i64) -> Self {
        self.offset = Some(o);
        self
    }
    /// Constrain the stored value.
    pub fn data(mut self, p: impl Into<Pat>) -> Self {
        self.data = Some(p.into());
        self
    }
    /// Bind the stack-store's memory output (`NodeOutputId`) to `v`.
    pub fn capture_output(mut self, v: Var) -> Self {
        self.output_var = Some(v);
        self
    }
    /// Bind the stack-store's node (`NodeId`) to `nv`.
    pub fn capture_node(mut self, nv: NodeVar) -> Self {
        self.node_var = Some(nv);
        self
    }
}

impl From<StackStorePat> for Pat {
    fn from(b: StackStorePat) -> Pat {
        let StackStorePat {
            space,
            offset,
            data,
            output_var,
            node_var,
        } = b;
        // StackStore inputs = [memory(0), base(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(data_pat) = data {
            indexed.push((2, data_pat));
        }
        Pat::from_dyn(Arc::new(NodePat {
            kind_match: Arc::new(move |ctx, node, _b| {
                matches!(
                    ctx.graph.graph.node_kind(node),
                    NodeKind::StackStore { space: actual_space, offset: actual_offset }
                        if space.is_none_or(|s| *actual_space == s)
                            && offset.is_none_or(|o| *actual_offset == o)
                )
            }),
            inputs: InputsSpec::Indexed(indexed),
            post_match: None,
            output_var,
            node_var,
        }))
    }
}

// ── Builder: StackStorePhiPat ─────────────────────────────────────────────────

/// Builder for `StackStorePhi` node patterns.  Created by [`crate::pat::stack_store_phi`].
pub struct StackStorePhiPat {
    space: Option<rsleigh::VnSpace>,
    offsets: Option<Vec<i64>>,
    data: Option<Pat>,
    output_var: Option<Var>,
    node_var: Option<NodeVar>,
}

impl StackStorePhiPat {
    pub(crate) fn new() -> Self {
        Self {
            space: None,
            offsets: None,
            data: None,
            output_var: None,
            node_var: None,
        }
    }
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    pub fn data(mut self, p: impl Into<Pat>) -> Self {
        self.data = Some(p.into());
        self
    }
    pub fn capture_output(mut self, v: Var) -> Self {
        self.output_var = Some(v);
        self
    }
    pub fn capture_node(mut self, nv: NodeVar) -> Self {
        self.node_var = Some(nv);
        self
    }
    /// Match the per-branch offsets exactly.  The supplied list is sorted
    /// ascending before comparison, so caller order is irrelevant.
    pub fn offsets<I: IntoIterator<Item = i64>>(mut self, os: I) -> Self {
        let mut v: Vec<i64> = os.into_iter().collect();
        v.sort();
        self.offsets = Some(v);
        self
    }
}

impl From<StackStorePhiPat> for Pat {
    fn from(b: StackStorePhiPat) -> Pat {
        let StackStorePhiPat {
            space,
            offsets,
            data,
            output_var,
            node_var,
        } = b;
        // StackStorePhi inputs = [phi_token(0), memory(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(data_pat) = data {
            indexed.push((2, data_pat));
        }
        Pat::from_dyn(Arc::new(NodePat {
            kind_match: Arc::new(move |ctx, node, _b| {
                let NodeKind::StackStorePhi {
                    space: actual_space,
                } = ctx.graph.graph.node_kind(node)
                else {
                    return false;
                };
                if let Some(expected_space) = space
                    && *actual_space != expected_space
                {
                    return false;
                }
                if let Some(expected_offsets) = &offsets {
                    let mut actual: Vec<i64> =
                        ctx.graph.graph.stack_phi_offsets(node).to_vec();
                    actual.sort();
                    if &actual != expected_offsets {
                        return false;
                    }
                }
                true
            }),
            inputs: InputsSpec::Indexed(indexed),
            post_match: None,
            output_var,
            node_var,
        }))
    }
}

// ── Builder: PhiPat ──────────────────────────────────────────────────────────

/// Builder for `ControlPhi` node patterns.  Created by [`crate::pat::phi`] or
/// [`crate::pat::phi_for`].
pub struct PhiPat {
    vn: Option<rsleigh::Vn>,
    inputs: Vec<(usize, Pat)>,
    output_var: Option<Var>,
    node_var: Option<NodeVar>,
}

impl PhiPat {
    pub(crate) fn new() -> Self {
        Self {
            vn: None,
            inputs: Vec::new(),
            output_var: None,
            node_var: None,
        }
    }
    /// Restrict the match to phi nodes for varnode `vn`.
    pub fn for_vn(mut self, v: rsleigh::Vn) -> Self {
        self.vn = Some(v);
        self
    }
    /// Constrain the value arriving from predecessor slot `idx`.
    pub fn input(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.inputs.push((idx, p.into()));
        self
    }
    /// Bind the phi's output (`NodeOutputId`) to `v`.
    pub fn capture_output(mut self, v: Var) -> Self {
        self.output_var = Some(v);
        self
    }
    /// Bind the phi's `NodeId` to `nv`.
    pub fn capture_node(mut self, nv: NodeVar) -> Self {
        self.node_var = Some(nv);
        self
    }
}

impl From<PhiPat> for Pat {
    fn from(b: PhiPat) -> Pat {
        let PhiPat {
            vn,
            inputs,
            output_var,
            node_var,
        } = b;
        Pat::from_dyn(Arc::new(NodePat {
            kind_match: Arc::new(move |ctx, node, _b| {
                let NodeKind::ControlPhi(actual_vn) = ctx.graph.graph.node_kind(node) else {
                    return false;
                };
                if let Some(expected) = vn
                    && *actual_vn != expected
                {
                    return false;
                }
                true
            }),
            inputs: InputsSpec::Indexed(inputs),
            post_match: None,
            output_var,
            node_var,
        }))
    }
}

// ── Builder: CallPat ──────────────────────────────────────────────────────────

/// Builder for `Call` node patterns.  Created by [`crate::pat::call`].
pub struct CallPat {
    target: Option<Pat>,
    args: Vec<(usize, Pat)>,
    ret_outputs: Vec<(usize, Pat)>,
    node_var: Option<NodeVar>,
}

impl CallPat {
    pub(crate) fn new() -> Self {
        Self {
            target: None,
            args: Vec::new(),
            ret_outputs: Vec::new(),
            node_var: None,
        }
    }
    /// Constrain the call target with an arbitrary pattern.
    pub fn target(mut self, p: impl Into<Pat>) -> Self {
        self.target = Some(p.into());
        self
    }
    /// Constrain argument at position `idx` (0-based, after ctrl and mem inputs).
    pub fn arg(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.args.push((idx, p.into()));
        self
    }
    /// Capture or constrain the Call's return-value output at ABI position
    /// `idx` — e.g. `.ret_output(0, var(v))` binds `v` to the `NodeOutputId`
    /// of the calling convention's first return register (`rax` on x86_64,
    /// `x0` on AArch64).  The inner pattern should be `var(v)` or `any()`;
    /// richer patterns are matched against the value output but will
    /// typically fail because the Call itself produces the value.  If the
    /// ret reg at `idx` is callee-saved, it does not appear as a Call
    /// output and the match fails.
    pub fn ret_output(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.ret_outputs.push((idx, p.into()));
        self
    }
    /// Bind the matched `Call` node to `nv`.
    pub fn capture(mut self, nv: NodeVar) -> Self {
        self.node_var = Some(nv);
        self
    }
    /// Constrain the call target to the literal address `addr`.
    pub fn at(self, addr: u64) -> Self {
        self.target(int_const(addr))
    }
}

impl From<CallPat> for Pat {
    fn from(b: CallPat) -> Pat {
        let CallPat {
            target,
            args,
            ret_outputs,
            node_var,
        } = b;
        Pat::from_ctrl(Arc::new(ControlNodePat {
            kind: CtrlKind::Call {
                target: target.map(pat_to_data),
                args: indexed_pats_to_data(args),
                ret_outputs: indexed_pats_to_data(ret_outputs),
            },
            node_var,
        }))
    }
}

// ── Builder: CallOtherPat ─────────────────────────────────────────────────────

/// Builder for `CallOther` node patterns.  Created by [`crate::pat::call_other`].
pub struct CallOtherPat {
    user_op_id: Option<u64>,
    args: Vec<(usize, Pat)>,
    node_var: Option<NodeVar>,
}

impl CallOtherPat {
    pub(crate) fn new() -> Self {
        Self {
            user_op_id: None,
            args: Vec::new(),
            node_var: None,
        }
    }
    /// Constrain the matched node to a specific user-op id.
    pub fn user_op_id(mut self, v: u64) -> Self {
        self.user_op_id = Some(v);
        self
    }
    /// Constrain argument at position `idx` (0-based, after ctrl and mem inputs).
    pub fn arg(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.args.push((idx, p.into()));
        self
    }
    /// Bind the matched `CallOther` node to `nv`.
    pub fn capture(mut self, nv: NodeVar) -> Self {
        self.node_var = Some(nv);
        self
    }
}

impl From<CallOtherPat> for Pat {
    fn from(b: CallOtherPat) -> Pat {
        let CallOtherPat {
            user_op_id,
            args,
            node_var,
        } = b;
        Pat::from_ctrl(Arc::new(ControlNodePat {
            kind: CtrlKind::CallOther {
                user_op_id,
                args: indexed_pats_to_data(args),
            },
            node_var,
        }))
    }
}

// ── Builder: RetPat ───────────────────────────────────────────────────────────

/// Builder for `Return` node patterns.  Created by [`crate::pat::ret`].
pub struct RetPat {
    preceded_by: Option<Pat>,
    ret_vals: Vec<(usize, Pat)>,
    node_var: Option<NodeVar>,
}

impl RetPat {
    pub(crate) fn new() -> Self {
        Self {
            preceded_by: None,
            ret_vals: Vec::new(),
            node_var: None,
        }
    }
    /// Require that the return is preceded by a call matching `call` somewhere
    /// earlier on the same control path (backward walk).
    pub fn preceded_by(mut self, p: impl Into<Pat>) -> Self {
        self.preceded_by = Some(p.into());
        self
    }
    /// Constrain return value at position `idx` (0-based after the ctrl input).
    pub fn ret_val(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.ret_vals.push((idx, p.into()));
        self
    }
    /// Bind the matched `Return` node to `nv`.
    pub fn capture(mut self, nv: NodeVar) -> Self {
        self.node_var = Some(nv);
        self
    }
}

impl From<RetPat> for Pat {
    fn from(b: RetPat) -> Pat {
        let RetPat {
            preceded_by,
            ret_vals,
            node_var,
        } = b;
        Pat::from_ctrl(Arc::new(ControlNodePat {
            kind: CtrlKind::Return {
                preceded_by: preceded_by.map(pat_to_ctrl),
                ret_vals: indexed_pats_to_data(ret_vals),
            },
            node_var,
        }))
    }
}

// ── Builder: FunctionArgPat ───────────────────────────────────────────────────

/// Builder for `FunctionArg` node patterns.  Created by
/// [`crate::pat::function_arg`], [`crate::pat::function_arg_any`],
/// [`crate::pat::function_arg_reg`], [`crate::pat::function_arg_stack`].
pub struct FunctionArgPat {
    source: Option<ir::node::FunctionArgSource>,
    index: Option<u32>,
    output_var: Option<Var>,
    node_var: Option<NodeVar>,
}

impl FunctionArgPat {
    pub(crate) fn new() -> Self {
        Self {
            source: None,
            index: None,
            output_var: None,
            node_var: None,
        }
    }
    /// Restrict the match to a specific ABI source (register or stack slot).
    pub fn source(mut self, s: ir::node::FunctionArgSource) -> Self {
        self.source = Some(s);
        self
    }
    /// Restrict the match to a specific argument index.
    pub fn index(mut self, i: u32) -> Self {
        self.index = Some(i);
        self
    }
    /// Bind the arg's value output (`NodeOutputId`) to `v`.
    pub fn capture_output(mut self, v: Var) -> Self {
        self.output_var = Some(v);
        self
    }
    /// Bind the arg's `NodeId` to `nv`.
    pub fn capture_node(mut self, nv: NodeVar) -> Self {
        self.node_var = Some(nv);
        self
    }
}

impl From<FunctionArgPat> for Pat {
    fn from(b: FunctionArgPat) -> Pat {
        let FunctionArgPat {
            source,
            index,
            output_var,
            node_var,
        } = b;
        Pat::from_dyn(Arc::new(NodePat {
            kind_match: Arc::new(move |ctx, node, _b| {
                let NodeKind::FunctionArg {
                    source: actual_source,
                    index: actual_index,
                } = ctx.graph.graph.node_kind(node)
                else {
                    return false;
                };
                if let Some(ref expected_source) = source
                    && actual_source != expected_source
                {
                    return false;
                }
                if let Some(expected_index) = index
                    && *actual_index != expected_index
                {
                    return false;
                }
                true
            }),
            inputs: InputsSpec::None,
            post_match: None,
            output_var,
            node_var,
        }))
    }
}

// ── Builder: IfPat ────────────────────────────────────────────────────────────

/// Builder for `If` node patterns.  Created by [`crate::pat::if_node`].
pub struct IfPat {
    cond: Option<Pat>,
    true_branch: Option<Pat>,
    false_branch: Option<Pat>,
    node_var: Option<NodeVar>,
}

impl IfPat {
    pub(crate) fn new() -> Self {
        Self {
            cond: None,
            true_branch: None,
            false_branch: None,
            node_var: None,
        }
    }
    /// Constrain the branch condition.
    pub fn cond(mut self, p: impl Into<Pat>) -> Self {
        self.cond = Some(p.into());
        self
    }
    /// Require a node matching `p` to be reachable on the true branch (forward search).
    pub fn true_branch(mut self, p: impl Into<Pat>) -> Self {
        self.true_branch = Some(p.into());
        self
    }
    /// Require a node matching `p` to be reachable on the false branch (forward search).
    pub fn false_branch(mut self, p: impl Into<Pat>) -> Self {
        self.false_branch = Some(p.into());
        self
    }
    /// Bind the matched `If` node to `nv`.
    pub fn capture(mut self, nv: NodeVar) -> Self {
        self.node_var = Some(nv);
        self
    }
}

impl From<IfPat> for Pat {
    fn from(b: IfPat) -> Pat {
        let IfPat {
            cond,
            true_branch,
            false_branch,
            node_var,
        } = b;
        Pat::from_ctrl(Arc::new(ControlNodePat {
            kind: CtrlKind::If {
                cond: cond.map(pat_to_data),
                true_branch: true_branch.map(pat_to_ctrl),
                false_branch: false_branch.map(pat_to_ctrl),
            },
            node_var,
        }))
    }
}

// ── Helpers for lifting `Pat` into the typed `DynDataPat` / `DynCtrlPat` ─────
// required by `CtrlKind`.
//
// `ControlNodePat` stores typed sub-patterns so the compiler enforces the
// data-vs-control distinction at its boundary, but the fluent builders
// accept `impl Into<Pat>` for source compatibility.  These lifts wrap a
// `Pat` into an adapter that re-enters the dispatch through
// `Matcher::match_output` or `Matcher::match_node_id`, preserving every
// variant (Legacy / Dyn / Ctrl) that the caller might supply.

fn pat_to_data(p: Pat) -> crate::pat::traits::DynDataPat {
    Arc::new(PatAsData(p))
}

fn pat_to_ctrl(p: Pat) -> crate::pat::traits::DynCtrlPat {
    Arc::new(PatAsCtrl(p))
}

fn indexed_pats_to_data(v: Vec<(usize, Pat)>) -> Vec<(usize, crate::pat::traits::DynDataPat)> {
    v.into_iter().map(|(i, p)| (i, pat_to_data(p))).collect()
}

struct PatAsData(Pat);

impl crate::pat::traits::DataPattern for PatAsData {
    fn try_match(
        &self,
        ctx: &crate::pat::traits::MatchCtx,
        target: ir::node::NodeOutputId,
        b: &mut crate::matcher::Bindings,
    ) -> bool {
        ctx.matcher.match_output(target, &self.0, b)
    }
}

struct PatAsCtrl(Pat);

impl crate::pat::traits::ControlPattern for PatAsCtrl {
    fn try_match(
        &self,
        ctx: &crate::pat::traits::MatchCtx,
        target: ir::node::NodeId,
        b: &mut crate::matcher::Bindings,
    ) -> bool {
        ctx.matcher.match_node_id(target, &self.0, b)
    }

    fn contains_inner(&self) -> Option<&Pat> {
        // Forward the peel so that a `Pat::Ctrl(ContainsPat { .. })` wrapped
        // in `PatAsCtrl` still lets the ControlNodePat boundary peel the
        // outer `Contains` shell.  Without this, the forward walker would
        // double-walk.
        self.0.as_ctrl().and_then(|c| c.contains_inner())
    }
}
