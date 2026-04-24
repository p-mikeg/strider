//! Builder structs for [`crate::pat::Pat`].
//!
//! Every builder here produces a [`NodePat`] via [`NodePat::matcher`] + the
//! `.with_*` fluent setters.  Data builders (`IntBinaryOpPat`,
//! `BoolBinaryOpPat`, `FloatBinaryOpPat`, the memory family, `PhiPat`,
//! `FunctionArgPat`) use `InputsSpec::Fixed` or `InputsSpec::Indexed`;
//! control builders (`CallPat`, `CallOtherPat`, `RetPat`, `IfPat`) use
//! `InputsSpec::Indexed` plus, for `If`, the `ConsumersSpec::Indexed`
//! direct-step forward walk for branch successors.
//!
//! Builders that expose `capture_output(v)` / `capture_node(nv)` implement
//! [`CaptureBuilder`] to share a single pair of setter methods.

use std::sync::Arc;

use ir::node::{NodeKind, NodeOutputType};
use ir::{BoolBinaryOp, FloatBinaryOp, IntBinaryOp};

use crate::matcher::commutativity::{
    is_commutative_bool_op, is_commutative_float_op, is_commutative_int_op,
};
use crate::pat::node_pat::{
    BuildTy, ConsumersSpec, InputsSpec, NodeKindBuilder, NodeKindCheck, NodePat, OutputsSpec,
};
use crate::pat::{Pat, int_const};
use crate::var::{NodeVar, Var};

// ── Shared trait: capture_output / capture_node ───────────────────────────────

/// Shared plumbing for builder types that bind the matched
/// `NodeOutputId` and/or `NodeId` to a capture variable.
///
/// Implementing types expose `&mut Option<Var>` and `&mut Option<NodeVar>`
/// slots; the trait provides the fluent `capture_output` / `capture_node`
/// setters so each builder does not re-write them.
pub trait CaptureBuilder: Sized {
    fn output_slot(&mut self) -> &mut Option<Var>;
    fn node_slot(&mut self) -> &mut Option<NodeVar>;

    /// Bind the matched node's primary value output (`NodeOutputId`) to `v`.
    fn capture_output(mut self, v: Var) -> Self {
        *self.output_slot() = Some(v);
        self
    }

    /// Bind the matched node's id (`NodeId`) to `nv`.
    fn capture_node(mut self, nv: NodeVar) -> Self {
        *self.node_slot() = Some(nv);
        self
    }
}

// ── Helper: build a binary-op `Pat` shared by Int/Bool/Float variants ─────────

fn binary_op_pat(
    kind_match: NodeKindCheck,
    kind_build: NodeKindBuilder,
    build_ty: BuildTy,
    inputs: InputsSpec,
) -> Pat {
    NodePat::matcher(kind_match, inputs)
        .with_build(kind_build)
        .with_build_ty(build_ty)
        .into_pat()
}

// ── Builder: IntBinaryOpPat ───────────────────────────────────────────────────

/// Builder for integer binary operation patterns.
pub struct IntBinaryOpPat {
    pub(super) op: IntBinaryOp,
    pub(super) lhs: Pat,
    pub(super) rhs: Pat,
    pub(super) ordered: bool,
}

impl IntBinaryOpPat {
    pub(crate) fn new(op: IntBinaryOp, lhs: Pat, rhs: Pat) -> Self {
        Self { op, lhs, rhs, ordered: false }
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
        let inputs = if !b.ordered && is_commutative_int_op(op) {
            InputsSpec::fixed_commutative(b.lhs, b.rhs)
        } else {
            InputsSpec::fixed_ordered(vec![b.lhs, b.rhs])
        };
        binary_op_pat(
            Arc::new(move |ctx, node, _b| {
                matches!(ctx.graph.graph.node_kind(node), NodeKind::IntBinaryOp(x) if *x == op)
            }),
            Arc::new(move |_b| Ok(NodeKind::IntBinaryOp(op))),
            BuildTy::InheritRoot,
            inputs,
        )
    }
}

// ── Builder: BoolBinaryOpPat ──────────────────────────────────────────────────

/// Builder for boolean binary operation patterns.
pub struct BoolBinaryOpPat {
    pub(super) op: BoolBinaryOp,
    pub(super) lhs: Pat,
    pub(super) rhs: Pat,
    pub(super) ordered: bool,
}

impl BoolBinaryOpPat {
    pub(crate) fn new(op: BoolBinaryOp, lhs: Pat, rhs: Pat) -> Self {
        Self { op, lhs, rhs, ordered: false }
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
        let inputs = if !b.ordered && is_commutative_bool_op(op) {
            InputsSpec::fixed_commutative(b.lhs, b.rhs)
        } else {
            InputsSpec::fixed_ordered(vec![b.lhs, b.rhs])
        };
        binary_op_pat(
            Arc::new(move |ctx, node, _b| {
                matches!(ctx.graph.graph.node_kind(node), NodeKind::BoolBinaryOp(x) if *x == op)
            }),
            Arc::new(move |_b| Ok(NodeKind::BoolBinaryOp(op))),
            BuildTy::Fixed(NodeOutputType::Bool),
            inputs,
        )
    }
}

// ── Builder: FloatBinaryOpPat ─────────────────────────────────────────────────

/// Builder for float binary operation patterns.
pub struct FloatBinaryOpPat {
    pub(super) op: FloatBinaryOp,
    pub(super) lhs: Pat,
    pub(super) rhs: Pat,
    pub(super) ordered: bool,
}

impl FloatBinaryOpPat {
    pub(crate) fn new(op: FloatBinaryOp, lhs: Pat, rhs: Pat) -> Self {
        Self { op, lhs, rhs, ordered: false }
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
        let inputs = if !b.ordered && is_commutative_float_op(op) {
            InputsSpec::fixed_commutative(b.lhs, b.rhs)
        } else {
            InputsSpec::fixed_ordered(vec![b.lhs, b.rhs])
        };
        binary_op_pat(
            Arc::new(move |ctx, node, _b| {
                matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatBinaryOp(x) if *x == op)
            }),
            Arc::new(move |_b| Ok(NodeKind::FloatBinaryOp(op))),
            BuildTy::InheritRoot,
            inputs,
        )
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
        Self { space: None, addr: None, output_var: None, node_var: None }
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
}

impl CaptureBuilder for LoadPat {
    fn output_slot(&mut self) -> &mut Option<Var> { &mut self.output_var }
    fn node_slot(&mut self) -> &mut Option<NodeVar> { &mut self.node_var }
}

impl From<LoadPat> for Pat {
    fn from(b: LoadPat) -> Pat {
        let LoadPat { space, addr, output_var, node_var } = b;
        // Load inputs = [mem(0), addr(1)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(addr_pat) = addr {
            indexed.push((1, addr_pat));
        }
        NodePat::matcher(
            Arc::new(move |ctx, node, _b| {
                matches!(
                    ctx.graph.graph.node_kind(node),
                    NodeKind::Load(actual) if space.is_none_or(|s| *actual == s)
                )
            }),
            InputsSpec::Indexed(indexed),
        )
        .with_output_var(output_var)
        .with_node_var(node_var)
        .into_pat()
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
        Self { space: None, addr: None, data: None, output_var: None, node_var: None }
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
}

impl CaptureBuilder for StorePat {
    fn output_slot(&mut self) -> &mut Option<Var> { &mut self.output_var }
    fn node_slot(&mut self) -> &mut Option<NodeVar> { &mut self.node_var }
}

impl From<StorePat> for Pat {
    fn from(b: StorePat) -> Pat {
        let StorePat { space, addr, data, output_var, node_var } = b;
        // Store inputs = [mem(0), addr(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(addr_pat) = addr {
            indexed.push((1, addr_pat));
        }
        if let Some(data_pat) = data {
            indexed.push((2, data_pat));
        }
        NodePat::matcher(
            Arc::new(move |ctx, node, _b| {
                matches!(
                    ctx.graph.graph.node_kind(node),
                    NodeKind::Store(actual) if space.is_none_or(|s| *actual == s)
                )
            }),
            InputsSpec::Indexed(indexed),
        )
        .with_output_var(output_var)
        .with_node_var(node_var)
        .into_pat()
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
        Self { space: None, offset: None, data: None, output_var: None, node_var: None }
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
}

impl CaptureBuilder for StackStorePat {
    fn output_slot(&mut self) -> &mut Option<Var> { &mut self.output_var }
    fn node_slot(&mut self) -> &mut Option<NodeVar> { &mut self.node_var }
}

impl From<StackStorePat> for Pat {
    fn from(b: StackStorePat) -> Pat {
        let StackStorePat { space, offset, data, output_var, node_var } = b;
        // StackStore inputs = [memory(0), base(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(data_pat) = data {
            indexed.push((2, data_pat));
        }
        NodePat::matcher(
            Arc::new(move |ctx, node, _b| {
                matches!(
                    ctx.graph.graph.node_kind(node),
                    NodeKind::StackStore { space: actual_space, offset: actual_offset }
                        if space.is_none_or(|s| *actual_space == s)
                            && offset.is_none_or(|o| *actual_offset == o)
                )
            }),
            InputsSpec::Indexed(indexed),
        )
        .with_output_var(output_var)
        .with_node_var(node_var)
        .into_pat()
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
        Self { space: None, offsets: None, data: None, output_var: None, node_var: None }
    }
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    pub fn data(mut self, p: impl Into<Pat>) -> Self {
        self.data = Some(p.into());
        self
    }
    /// Match the per-branch offsets exactly (multiset comparison).  The
    /// supplied list is sorted ascending before comparison, so caller order
    /// is irrelevant.
    pub fn offsets<I: IntoIterator<Item = i64>>(mut self, os: I) -> Self {
        let mut v: Vec<i64> = os.into_iter().collect();
        v.sort();
        self.offsets = Some(v);
        self
    }
}

impl CaptureBuilder for StackStorePhiPat {
    fn output_slot(&mut self) -> &mut Option<Var> { &mut self.output_var }
    fn node_slot(&mut self) -> &mut Option<NodeVar> { &mut self.node_var }
}

impl From<StackStorePhiPat> for Pat {
    fn from(b: StackStorePhiPat) -> Pat {
        let StackStorePhiPat { space, offsets, data, output_var, node_var } = b;
        // StackStorePhi inputs = [phi_token(0), memory(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(data_pat) = data {
            indexed.push((2, data_pat));
        }
        NodePat::matcher(
            Arc::new(move |ctx, node, _b| {
                let NodeKind::StackStorePhi { space: actual_space } =
                    ctx.graph.graph.node_kind(node)
                else {
                    return false;
                };
                if let Some(expected_space) = space
                    && *actual_space != expected_space
                {
                    return false;
                }
                if let Some(expected_offsets) = &offsets {
                    // `expected_offsets` is already sorted (see
                    // `StackStorePhiPat::offsets`).  Compare as multisets
                    // without allocating: skip on length mismatch, then sort
                    // a fixed-size stack buffer for small arities and fall
                    // back to a heap Vec only in the unlikely arity > 8 case.
                    let actual_slice = ctx.graph.graph.stack_phi_offsets(node);
                    if actual_slice.len() != expected_offsets.len() {
                        return false;
                    }
                    const INLINE: usize = 8;
                    if actual_slice.len() <= INLINE {
                        let mut buf = [0i64; INLINE];
                        buf[..actual_slice.len()].copy_from_slice(actual_slice);
                        buf[..actual_slice.len()].sort();
                        if &buf[..actual_slice.len()] != expected_offsets.as_slice() {
                            return false;
                        }
                    } else {
                        let mut actual: Vec<i64> = actual_slice.to_vec();
                        actual.sort();
                        if &actual != expected_offsets {
                            return false;
                        }
                    }
                }
                true
            }),
            InputsSpec::Indexed(indexed),
        )
        .with_output_var(output_var)
        .with_node_var(node_var)
        .into_pat()
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
        Self { vn: None, inputs: Vec::new(), output_var: None, node_var: None }
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
}

impl CaptureBuilder for PhiPat {
    fn output_slot(&mut self) -> &mut Option<Var> { &mut self.output_var }
    fn node_slot(&mut self) -> &mut Option<NodeVar> { &mut self.node_var }
}

impl From<PhiPat> for Pat {
    fn from(b: PhiPat) -> Pat {
        let PhiPat { vn, inputs, output_var, node_var } = b;
        NodePat::matcher(
            Arc::new(move |ctx, node, _b| {
                let NodeKind::ControlPhi(actual_vn) = ctx.graph.graph.node_kind(node) else {
                    return false;
                };
                vn.is_none_or(|expected| *actual_vn == expected)
            }),
            InputsSpec::Indexed(inputs),
        )
        .with_output_var(output_var)
        .with_node_var(node_var)
        .into_pat()
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
        Self { target: None, args: Vec::new(), ret_outputs: Vec::new(), node_var: None }
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
        let CallPat { target, args, ret_outputs, node_var } = b;
        // Call inputs: [ctrl(0), mem(1), target(2), arg0(3), arg1(4), ...].
        let mut indexed_inputs: Vec<(usize, Pat)> = Vec::new();
        if let Some(tgt) = target {
            indexed_inputs.push((2, tgt));
        }
        for (i, p) in args {
            indexed_inputs.push((3 + i, p));
        }
        // Call outputs: [ctrl(0), mem(1), retval0(2), retval1(3), ...].
        let outputs_spec = if ret_outputs.is_empty() {
            OutputsSpec::None
        } else {
            OutputsSpec::Indexed(ret_outputs.into_iter().map(|(i, p)| (2 + i, p)).collect())
        };
        NodePat::matcher(
            Arc::new(|ctx, node, _b| matches!(ctx.graph.graph.node_kind(node), NodeKind::Call)),
            InputsSpec::Indexed(indexed_inputs),
        )
        .with_outputs(outputs_spec)
        .with_node_var(node_var)
        .into_pat()
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
        Self { user_op_id: None, args: Vec::new(), node_var: None }
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
        let CallOtherPat { user_op_id, args, node_var } = b;
        // CallOther inputs: [ctrl(0), mem(1), arg0(2), arg1(3), ...].
        let indexed_inputs: Vec<(usize, Pat)> =
            args.into_iter().map(|(i, p)| (2 + i, p)).collect();
        NodePat::matcher(
            Arc::new(move |ctx, node, _b| {
                let NodeKind::CallOther { user_op_id: actual } = ctx.graph.graph.node_kind(node)
                else {
                    return false;
                };
                user_op_id.is_none_or(|id| *actual == id)
            }),
            InputsSpec::Indexed(indexed_inputs),
        )
        .with_node_var(node_var)
        .into_pat()
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
        Self { preceded_by: None, ret_vals: Vec::new(), node_var: None }
    }
    /// Match `p` against the Return's **direct** ctrl predecessor (the node
    /// producing input slot 0 — typically a `ControlState` at a region
    /// header).  This is a single-step match, not a backward walk through the
    /// CFG; to reach a non-adjacent ancestor the caller must structure `p`
    /// accordingly (e.g. `.preceded_by(cs().preceded_by(call()))`).
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
        let RetPat { preceded_by, ret_vals, node_var } = b;
        // Return inputs: [ctrl(0), mem(1), retval0(2), retval1(3), ...].
        // `preceded_by` matches against the ctrl input (index 0); the default
        // `Pattern::try_match` on the sub-pattern then does
        // `get_node_from_output`, giving a direct-step backward match.
        let mut indexed_inputs: Vec<(usize, Pat)> = Vec::new();
        if let Some(prev) = preceded_by {
            indexed_inputs.push((0, prev));
        }
        for (i, p) in ret_vals {
            indexed_inputs.push((2 + i, p));
        }
        NodePat::matcher(
            Arc::new(|ctx, node, _b| matches!(ctx.graph.graph.node_kind(node), NodeKind::Return)),
            InputsSpec::Indexed(indexed_inputs),
        )
        .with_node_var(node_var)
        .into_pat()
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
        Self { source: None, index: None, output_var: None, node_var: None }
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
}

impl CaptureBuilder for FunctionArgPat {
    fn output_slot(&mut self) -> &mut Option<Var> { &mut self.output_var }
    fn node_slot(&mut self) -> &mut Option<NodeVar> { &mut self.node_var }
}

impl From<FunctionArgPat> for Pat {
    fn from(b: FunctionArgPat) -> Pat {
        let FunctionArgPat { source, index, output_var, node_var } = b;
        NodePat::matcher(
            Arc::new(move |ctx, node, _b| {
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
                index.is_none_or(|expected| *actual_index == expected)
            }),
            InputsSpec::None,
        )
        .with_output_var(output_var)
        .with_node_var(node_var)
        .into_pat()
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
        Self { cond: None, true_branch: None, false_branch: None, node_var: None }
    }
    /// Constrain the branch condition.
    pub fn cond(mut self, p: impl Into<Pat>) -> Self {
        self.cond = Some(p.into());
        self
    }
    /// Match `p` against the single consumer of the If's true-branch output.
    pub fn true_branch(mut self, p: impl Into<Pat>) -> Self {
        self.true_branch = Some(p.into());
        self
    }
    /// Match `p` against the single consumer of the If's false-branch output.
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
        let IfPat { cond, true_branch, false_branch, node_var } = b;
        // If inputs: [ctrl(0), cond(1)]. Outputs: [true-ctrl(0), false-ctrl(1)].
        let mut indexed_inputs: Vec<(usize, Pat)> = Vec::new();
        if let Some(c) = cond {
            indexed_inputs.push((1, c));
        }
        let mut indexed_consumers: Vec<(usize, Pat)> = Vec::new();
        if let Some(tb) = true_branch {
            indexed_consumers.push((0, tb));
        }
        if let Some(fb) = false_branch {
            indexed_consumers.push((1, fb));
        }
        let consumers_spec = if indexed_consumers.is_empty() {
            ConsumersSpec::None
        } else {
            ConsumersSpec::Indexed(indexed_consumers)
        };
        NodePat::matcher(
            Arc::new(|ctx, node, _b| matches!(ctx.graph.graph.node_kind(node), NodeKind::If)),
            InputsSpec::Indexed(indexed_inputs),
        )
        .with_consumers(consumers_spec)
        .with_node_var(node_var)
        .into_pat()
    }
}
