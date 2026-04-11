use std::sync::Arc;

use ir::{BoolBinaryOp, BoolUnaryOp, ExtendOp, IntBinaryOp, IntCmpOp, IntUnaryOp};
use ir::BuiltFunctionGraph;
use ir::node::NodeOutputId;

use crate::var::{NodeVar, Var};

/// Predicate function type used in [`PatKind::WithPredicate`].
pub type PredicateFn = Arc<dyn Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync>;

// ── Core pattern type ─────────────────────────────────────────────────────────

/// A graph pattern.  Cheap to clone — the inner data is reference-counted.
#[derive(Clone)]
pub struct Pat(Arc<PatKind>);

impl Pat {
    fn new(kind: PatKind) -> Self {
        Self(Arc::new(kind))
    }

    /// Returns a reference to the underlying [`PatKind`].
    pub fn inner(&self) -> &PatKind {
        &self.0
    }

    /// After this pattern matches successfully, additionally bind the matched
    /// output to `v`.  If `v` is already bound the output must equal the
    /// stored binding, otherwise the match fails.
    pub fn capture(self, v: Var) -> Pat {
        Pat::new(PatKind::WithCapture { inner: self, var: v })
    }

    /// After this pattern matches successfully, additionally run `f` against
    /// the matched output.  The match fails if `f` returns `false`.
    pub fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        Pat::new(PatKind::WithPredicate { inner: self, func: Arc::new(f) })
    }
}

// ── PatKind ───────────────────────────────────────────────────────────────────

pub enum PatKind {
    // ── Wildcards ─────────────────────────────────────────────────────────────
    /// Matches any single `NodeOutputId` unconditionally.
    Any,
    /// Matches any output and binds it to `v`.  If `v` is already bound the
    /// output must equal the stored binding.
    Capture(Var),

    // ── Constants ─────────────────────────────────────────────────────────────
    /// Matches an `IntConst` node whose value equals the given literal.
    IntConst(u64),
    /// Matches a `BoolConst` node whose value equals the given literal.
    BoolConst(bool),
    /// Matches any `IntConst` node and binds its output to `v`.  Fails if the
    /// producing node is not an `IntConst`.  Use `m.get_int_const(v, graph)` to
    /// read the concrete value after matching.
    AnyIntConst(Var),

    // ── Integer ops ───────────────────────────────────────────────────────────
    /// Matches an integer binary operation node.
    /// When `ordered` is `false` and the op is commutative, both operand
    /// orderings are tried automatically.
    IntBinaryOp { op: IntBinaryOp, lhs: Pat, rhs: Pat, ordered: bool },
    /// Matches an integer unary operation node.
    IntUnaryOp  { op: IntUnaryOp,  operand: Pat },
    /// Matches an integer comparison node (produces a `Bool` output).
    IntCmpOp    { op: IntCmpOp,    lhs: Pat, rhs: Pat },

    // ── Bool ops ──────────────────────────────────────────────────────────────
    /// Matches a boolean binary operation node.
    /// When `ordered` is `false` and the op is commutative, both orderings are
    /// tried automatically.
    BoolBinaryOp { op: BoolBinaryOp, lhs: Pat, rhs: Pat, ordered: bool },
    /// Matches a boolean unary operation node.
    BoolUnaryOp  { op: BoolUnaryOp,  operand: Pat },

    // ── Casts / coercions (single value input) ────────────────────────────────
    /// Matches a `CastToBool` node (non-zero integer → `true`).
    CastToBool { operand: Pat },
    /// Matches a `CastToInt` node (`bool` → `0` or `1`).
    CastToInt  { operand: Pat },
    /// Matches a `Truncate` node (narrows an integer to fewer bits).
    Truncate   { operand: Pat },
    /// Matches an `Extend` node (widens an integer).
    Extend     { op: ExtendOp, operand: Pat },
    /// Matches a `Popcount` node (counts set bits).
    Popcount   { operand: Pat },

    // ── Memory ops ────────────────────────────────────────────────────────────
    /// `Load(space)`: inputs = [mem(0), addr(1)] → value output.
    Load {
        space:      Option<rsleigh::VnSpace>,
        addr:       Option<Pat>,
        /// Bind the load's output `NodeOutputId` to this variable.
        output_var: Option<Var>,
        /// Bind the load's `NodeId` to this variable.
        node_var:   Option<NodeVar>,
    },
    /// `Store(space)`: inputs = [mem(0), addr(1), data(2)] → mem output.
    Store {
        space:      Option<rsleigh::VnSpace>,
        addr:       Option<Pat>,
        data:       Option<Pat>,
        /// Bind the store's memory output `NodeOutputId` to this variable.
        output_var: Option<Var>,
        /// Bind the store's `NodeId` to this variable.
        node_var:   Option<NodeVar>,
    },

    // ── Phi nodes ─────────────────────────────────────────────────────────────
    /// `ControlPhi(vn)`: inputs = [phi_token(0), pred_val(1), pred_val(2)…].
    /// `vn = None` matches any phi; `inputs` constrains specific predecessor slots.
    Phi {
        vn:         Option<rsleigh::Vn>,
        inputs:     Vec<(usize, Pat)>,
        /// Bind the phi's output `NodeOutputId` to this variable.
        output_var: Option<Var>,
        /// Bind the phi's `NodeId` to this variable.
        node_var:   Option<NodeVar>,
    },

    // ── Function-entry values ─────────────────────────────────────────────────
    /// Matches `NodeKind::InitialVar(vn)`.  `vn = None` matches any.
    InitialVar { vn: Option<rsleigh::Vn> },

    // ── Control-level nodes ───────────────────────────────────────────────────
    /// `Call`: inputs = [ctrl(0), mem(1), target(2), arg0(3), arg1(4)…]
    Call {
        target:   Option<Pat>,
        args:     Vec<(usize, Pat)>,
        node_var: Option<NodeVar>,
    },
    /// `Return`: inputs = [ctrl(0), mem(1), retval0(2)…]
    Return {
        preceded_by: Option<Pat>,
        ret_vals:    Vec<(usize, Pat)>,
        node_var:    Option<NodeVar>,
    },
    /// `If`: inputs = [ctrl(0), cond(1)]; outputs = [true_ctrl(0), false_ctrl(1)]
    If {
        cond:         Option<Pat>,
        true_branch:  Option<Pat>,
        false_branch: Option<Pat>,
        node_var:     Option<NodeVar>,
    },

    // ── Region search ─────────────────────────────────────────────────────────
    /// Forward walk along a ctrl chain, searching for a node matching `inner`.
    Contains(Pat),

    // ── Post-match guards ─────────────────────────────────────────────────────
    /// Matches `inner`, then additionally binds the matched output to `var`.
    WithCapture { inner: Pat, var: Var },
    /// Matches `inner`, then additionally runs `func` — fails if it returns false.
    WithPredicate {
        inner: Pat,
        func:  PredicateFn,
    },
}

// ── Builder: IntBinaryOpPat ───────────────────────────────────────────────────

/// Builder for integer binary operation patterns.
///
/// Returned by [`int_binary`] and the shorthand constructors ([`add`], [`sub`],
/// [`mul`], …).  Call `.into()` (or pass directly to any `impl Into<Pat>`
/// parameter) to obtain a [`Pat`].
pub struct IntBinaryOpPat {
    op:      IntBinaryOp,
    lhs:     Pat,
    rhs:     Pat,
    ordered: bool,
}

impl IntBinaryOpPat {
    /// Force the pattern to match operands in the stated order only.
    /// By default, commutative operators (`Add`, `Mul`, `And`, `Or`, `Xor`)
    /// will also try the reversed operand order.
    pub fn ordered(mut self) -> Self { self.ordered = true; self }

    /// After matching, bind the matched output to `v`.
    pub fn capture(self, v: Var) -> Pat { Pat::from(self).capture(v) }

    /// After matching, additionally run `f` — fails if it returns `false`.
    pub fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        Pat::from(self).when(f)
    }
}

impl From<IntBinaryOpPat> for Pat {
    fn from(b: IntBinaryOpPat) -> Pat {
        Pat::new(PatKind::IntBinaryOp {
            op:      b.op,
            lhs:     b.lhs,
            rhs:     b.rhs,
            ordered: b.ordered,
        })
    }
}

// ── Builder: BoolBinaryOpPat ──────────────────────────────────────────────────

/// Builder for boolean binary operation patterns.
///
/// Returned by [`bool_binary`] and the shorthand constructors ([`bool_and`],
/// [`bool_or`], [`bool_xor`]).  Call `.into()` or pass directly to any
/// `impl Into<Pat>` parameter.
pub struct BoolBinaryOpPat {
    op:      BoolBinaryOp,
    lhs:     Pat,
    rhs:     Pat,
    ordered: bool,
}

impl BoolBinaryOpPat {
    /// Force the pattern to match operands in the stated order only.
    pub fn ordered(mut self) -> Self { self.ordered = true; self }

    /// After matching, bind the matched output to `v`.
    pub fn capture(self, v: Var) -> Pat { Pat::from(self).capture(v) }

    /// After matching, additionally run `f` — fails if it returns `false`.
    pub fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        Pat::from(self).when(f)
    }
}

impl From<BoolBinaryOpPat> for Pat {
    fn from(b: BoolBinaryOpPat) -> Pat {
        Pat::new(PatKind::BoolBinaryOp {
            op:      b.op,
            lhs:     b.lhs,
            rhs:     b.rhs,
            ordered: b.ordered,
        })
    }
}

// ── Builder: LoadPat ──────────────────────────────────────────────────────────

/// Builder for `Load` node patterns.  Created by [`load`].
pub struct LoadPat {
    space:      Option<rsleigh::VnSpace>,
    addr:       Option<Pat>,
    output_var: Option<Var>,
    node_var:   Option<NodeVar>,
}

impl LoadPat {
    pub(crate) fn new() -> Self { Self { space: None, addr: None, output_var: None, node_var: None } }

    /// Restrict the match to loads in address space `s`.
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self { self.space = Some(s); self }
    /// Constrain the load's address operand.
    pub fn addr(mut self, p: impl Into<Pat>) -> Self { self.addr = Some(p.into()); self }
    /// Bind the load's value output (`NodeOutputId`) to `v`.
    pub fn capture_output(mut self, v: Var) -> Self { self.output_var = Some(v); self }
    /// Bind the load's node (`NodeId`) to `nv`.
    pub fn capture_node(mut self, nv: NodeVar) -> Self { self.node_var = Some(nv); self }
    /// After matching, bind the matched output to `v`.
    pub fn capture(self, v: Var) -> Pat { Pat::from(self).capture(v) }
    /// After matching, additionally run `f` — fails if it returns `false`.
    pub fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        Pat::from(self).when(f)
    }
}

impl From<LoadPat> for Pat {
    fn from(b: LoadPat) -> Pat {
        Pat::new(PatKind::Load {
            space:      b.space,
            addr:       b.addr,
            output_var: b.output_var,
            node_var:   b.node_var,
        })
    }
}

// ── Builder: StorePat ─────────────────────────────────────────────────────────

/// Builder for `Store` node patterns.  Created by [`store`].
pub struct StorePat {
    space:      Option<rsleigh::VnSpace>,
    addr:       Option<Pat>,
    data:       Option<Pat>,
    output_var: Option<Var>,
    node_var:   Option<NodeVar>,
}

impl StorePat {
    pub(crate) fn new() -> Self { Self { space: None, addr: None, data: None, output_var: None, node_var: None } }

    /// Restrict the match to stores in address space `s`.
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self { self.space = Some(s); self }
    /// Constrain the store's address operand.
    pub fn addr(mut self, p: impl Into<Pat>) -> Self { self.addr = Some(p.into()); self }
    /// Constrain the value being stored.
    pub fn data(mut self, p: impl Into<Pat>) -> Self { self.data = Some(p.into()); self }
    /// Bind the store's memory output (`NodeOutputId`) to `v`.
    pub fn capture_output(mut self, v: Var) -> Self { self.output_var = Some(v); self }
    /// Bind the store's node (`NodeId`) to `nv`.
    pub fn capture_node(mut self, nv: NodeVar) -> Self { self.node_var = Some(nv); self }
    /// After matching, bind the matched output to `v`.
    pub fn capture(self, v: Var) -> Pat { Pat::from(self).capture(v) }
    /// After matching, additionally run `f` — fails if it returns `false`.
    pub fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        Pat::from(self).when(f)
    }
}

impl From<StorePat> for Pat {
    fn from(b: StorePat) -> Pat {
        Pat::new(PatKind::Store {
            space:      b.space,
            addr:       b.addr,
            data:       b.data,
            output_var: b.output_var,
            node_var:   b.node_var,
        })
    }
}

// ── Builder: PhiPat ──────────────────────────────────────────────────────────

/// Builder for `ControlPhi` node patterns.  Created by [`phi`] or [`phi_for`].
pub struct PhiPat {
    vn:         Option<rsleigh::Vn>,
    inputs:     Vec<(usize, Pat)>,
    output_var: Option<Var>,
    node_var:   Option<NodeVar>,
}

impl PhiPat {
    pub(crate) fn new() -> Self { Self { vn: None, inputs: Vec::new(), output_var: None, node_var: None } }

    /// Restrict the match to phi nodes for varnode `vn`.
    pub fn for_vn(mut self, vn: rsleigh::Vn) -> Self { self.vn = Some(vn); self }
    /// Constrain the value arriving from predecessor slot `idx`.
    pub fn input(mut self, idx: usize, p: impl Into<Pat>) -> Self { self.inputs.push((idx, p.into())); self }
    /// Bind the phi's output (`NodeOutputId`) to `v`.
    pub fn capture_output(mut self, v: Var) -> Self { self.output_var = Some(v); self }
    /// Bind the phi's `NodeId` to `nv`.
    pub fn capture_node(mut self, nv: NodeVar) -> Self { self.node_var = Some(nv); self }
    /// After matching, bind the matched output to `v`.
    pub fn capture(self, v: Var) -> Pat { Pat::from(self).capture(v) }
    /// After matching, additionally run `f` — fails if it returns `false`.
    pub fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        Pat::from(self).when(f)
    }
}

impl From<PhiPat> for Pat {
    fn from(b: PhiPat) -> Pat {
        Pat::new(PatKind::Phi {
            vn:         b.vn,
            inputs:     b.inputs,
            output_var: b.output_var,
            node_var:   b.node_var,
        })
    }
}

// ── Builder: CallPat ──────────────────────────────────────────────────────────

/// Builder for `Call` node patterns.  Created by [`call`].
pub struct CallPat {
    target:   Option<Pat>,
    args:     Vec<(usize, Pat)>,
    node_var: Option<NodeVar>,
}

impl CallPat {
    pub(crate) fn new() -> Self { Self { target: None, args: Vec::new(), node_var: None } }

    /// Constrain the call target to the literal address `addr`.
    pub fn at(self, addr: u64) -> Self {
        self.target(Pat::new(PatKind::IntConst(addr)))
    }
    /// Constrain the call target with an arbitrary pattern.
    pub fn target(mut self, p: impl Into<Pat>) -> Self { self.target = Some(p.into()); self }
    /// Constrain argument at position `idx` (0-based, after ctrl and mem inputs).
    pub fn arg(mut self, idx: usize, p: impl Into<Pat>) -> Self { self.args.push((idx, p.into())); self }
    /// Bind the matched `Call` node to `nv`.
    pub fn capture(mut self, nv: NodeVar) -> Self { self.node_var = Some(nv); self }
    /// After matching, additionally run `f` — fails if it returns `false`.
    pub fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        Pat::from(self).when(f)
    }
}

impl From<CallPat> for Pat {
    fn from(b: CallPat) -> Pat {
        Pat::new(PatKind::Call { target: b.target, args: b.args, node_var: b.node_var })
    }
}

// ── Builder: RetPat ───────────────────────────────────────────────────────────

/// Builder for `Return` node patterns.  Created by [`ret`].
pub struct RetPat {
    preceded_by: Option<Pat>,
    ret_vals:    Vec<(usize, Pat)>,
    node_var:    Option<NodeVar>,
}

impl RetPat {
    pub(crate) fn new() -> Self { Self { preceded_by: None, ret_vals: Vec::new(), node_var: None } }

    /// Require that the return is preceded by a call matching `call` somewhere
    /// earlier on the same control path (backward walk).
    pub fn preceded_by(mut self, call: impl Into<Pat>) -> Self {
        self.preceded_by = Some(call.into()); self
    }
    /// Constrain return value at position `idx` (0-based after the ctrl input).
    pub fn ret_val(mut self, idx: usize, p: impl Into<Pat>) -> Self { self.ret_vals.push((idx, p.into())); self }
    /// Bind the matched `Return` node to `nv`.
    pub fn capture(mut self, nv: NodeVar) -> Self { self.node_var = Some(nv); self }
    /// After matching, additionally run `f` — fails if it returns `false`.
    pub fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        Pat::from(self).when(f)
    }
}

impl From<RetPat> for Pat {
    fn from(b: RetPat) -> Pat {
        Pat::new(PatKind::Return { preceded_by: b.preceded_by, ret_vals: b.ret_vals, node_var: b.node_var })
    }
}

// ── Builder: IfPat ────────────────────────────────────────────────────────────

/// Builder for `If` node patterns.  Created by [`if_node`].
pub struct IfPat {
    cond:         Option<Pat>,
    true_branch:  Option<Pat>,
    false_branch: Option<Pat>,
    node_var:     Option<NodeVar>,
}

impl IfPat {
    pub(crate) fn new() -> Self { Self { cond: None, true_branch: None, false_branch: None, node_var: None } }

    /// Constrain the branch condition.
    pub fn cond(mut self, p: impl Into<Pat>) -> Self { self.cond = Some(p.into()); self }
    /// Require a node matching `p` to be reachable on the true branch (forward search).
    pub fn true_branch(mut self, p: impl Into<Pat>) -> Self { self.true_branch = Some(p.into()); self }
    /// Require a node matching `p` to be reachable on the false branch (forward search).
    pub fn false_branch(mut self, p: impl Into<Pat>) -> Self { self.false_branch = Some(p.into()); self }
    /// Bind the matched `If` node to `nv`.
    pub fn capture(mut self, nv: NodeVar) -> Self { self.node_var = Some(nv); self }
    /// After matching, additionally run `f` — fails if it returns `false`.
    pub fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        Pat::from(self).when(f)
    }
}

impl From<IfPat> for Pat {
    fn from(b: IfPat) -> Pat {
        Pat::new(PatKind::If {
            cond: b.cond,
            true_branch: b.true_branch,
            false_branch: b.false_branch,
            node_var: b.node_var,
        })
    }
}

// ── Free-function constructors ────────────────────────────────────────────────

/// Matches any single output unconditionally.
pub fn any() -> Pat { Pat::new(PatKind::Any) }

/// Matches any output and binds it to `v`.
///
/// If `v` is already bound the output must equal the stored binding.
/// Shorthand for `any().capture(v)`.
pub fn var(v: Var) -> Pat { Pat::new(PatKind::Capture(v)) }

/// Matches an `IntConst` node with value exactly `v`.
pub fn int_const(v: u64) -> Pat { Pat::new(PatKind::IntConst(v)) }

/// Matches a `BoolConst` node with value exactly `v`.
pub fn bool_const(v: bool) -> Pat { Pat::new(PatKind::BoolConst(v)) }
/// Matches any `IntConst` node and binds its output to `v`.
/// Fails if the node is not an `IntConst` — use this instead of `var(v)` when
/// you want the pattern itself to enforce the node is a compile-time constant.
pub fn any_const(v: Var) -> Pat { Pat::new(PatKind::AnyIntConst(v)) }

/// Matches any output for which `f` returns `true`.  Equivalent to `any().when(f)`.
pub fn predicate<F>(f: F) -> Pat
where
    F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
{
    any().when(f)
}

// Integer binary ops

/// Matches an integer binary operation with the given `op`.
///
/// Commutative ops (`Add`, `Mul`, `And`, `Or`, `Xor`) will try both operand
/// orderings automatically.  Call `.ordered()` on the result to disable this.
pub fn int_binary(op: IntBinaryOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    IntBinaryOpPat { op, lhs: lhs.into(), rhs: rhs.into(), ordered: false }
}
/// Matches an unsigned addition node (`lhs + rhs`).  Commutative.
pub fn add(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat  { int_binary(IntBinaryOp::Add,         lhs, rhs) }
/// Matches an unsigned subtraction node (`lhs - rhs`).  Not commutative.
pub fn sub(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat  { int_binary(IntBinaryOp::Sub,         lhs, rhs) }
/// Matches an unsigned multiplication node.  Commutative.
pub fn mul(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat  { int_binary(IntBinaryOp::Mul,         lhs, rhs) }
/// Matches an unsigned division node.  Not commutative.
pub fn div(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat  { int_binary(IntBinaryOp::Div,         lhs, rhs) }
/// Matches a signed division node.  Not commutative.
pub fn sdiv(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat { int_binary(IntBinaryOp::Sdiv,        lhs, rhs) }
/// Matches an unsigned remainder node.  Not commutative.
pub fn rem(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat  { int_binary(IntBinaryOp::Rem,         lhs, rhs) }
/// Matches a signed remainder node.  Not commutative.
pub fn srem(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat { int_binary(IntBinaryOp::Srem,        lhs, rhs) }
/// Matches a bitwise AND node.  Commutative.
pub fn and(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat  { int_binary(IntBinaryOp::And,         lhs, rhs) }
/// Matches a bitwise OR node.  Commutative.
pub fn or(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat   { int_binary(IntBinaryOp::Or,          lhs, rhs) }
/// Matches a bitwise XOR node.  Commutative.
pub fn xor(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat  { int_binary(IntBinaryOp::Xor,         lhs, rhs) }
/// Matches a logical left-shift node.  Not commutative.
pub fn shl(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat  { int_binary(IntBinaryOp::ShiftLeft,   lhs, rhs) }
/// Matches a logical right-shift node.  Not commutative.
pub fn shr(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat  { int_binary(IntBinaryOp::ShiftRight,  lhs, rhs) }
/// Matches an arithmetic (signed) right-shift node.  Not commutative.
pub fn sshr(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat { int_binary(IntBinaryOp::SShiftRight, lhs, rhs) }

// Integer unary ops

/// Matches an integer unary operation with the given `op`.
pub fn int_unary(op: IntUnaryOp, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntUnaryOp { op, operand: operand.into() })
}
/// Matches an integer negation node (`-operand`).
pub fn neg(operand: impl Into<Pat>) -> Pat { int_unary(IntUnaryOp::Neg, operand) }
/// Matches a bitwise complement node (`~operand`).
pub fn not(operand: impl Into<Pat>) -> Pat { int_unary(IntUnaryOp::Not, operand) }

// Integer comparisons (→ Bool)

/// Matches an integer comparison node with the given `op`.
pub fn int_cmp(op: IntCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntCmpOp { op, lhs: lhs.into(), rhs: rhs.into() })
}
/// Matches an unsigned equality comparison (`lhs == rhs`).
pub fn int_eq(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat  { int_cmp(IntCmpOp::Equal,      lhs, rhs) }
/// Matches an unsigned less-than comparison (`lhs < rhs`).
pub fn int_lt(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat  { int_cmp(IntCmpOp::Less,       lhs, rhs) }
/// Matches an unsigned less-or-equal comparison (`lhs <= rhs`).
pub fn int_le(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat  { int_cmp(IntCmpOp::LessEqual,  lhs, rhs) }
/// Matches a signed less-than comparison.
pub fn int_slt(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat { int_cmp(IntCmpOp::Sless,      lhs, rhs) }
/// Matches a signed less-or-equal comparison.
pub fn int_sle(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat { int_cmp(IntCmpOp::SlessEqual, lhs, rhs) }
/// Matches an unsigned addition carry-out check.
pub fn int_carry(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat   { int_cmp(IntCmpOp::Carry,   lhs, rhs) }
/// Matches a signed addition overflow check.
pub fn int_scarry(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat  { int_cmp(IntCmpOp::Scarry,  lhs, rhs) }
/// Matches a signed subtraction borrow check.
pub fn int_sborrow(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat { int_cmp(IntCmpOp::Sborrow, lhs, rhs) }

// Bool ops

/// Matches a boolean binary operation with the given `op`.
///
/// Commutative ops (`And`, `Or`, `Xor`) try both orderings automatically.
pub fn bool_binary(op: BoolBinaryOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    BoolBinaryOpPat { op, lhs: lhs.into(), rhs: rhs.into(), ordered: false }
}
/// Matches a boolean AND node.  Commutative.
pub fn bool_and(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat { bool_binary(BoolBinaryOp::And, lhs, rhs) }
/// Matches a boolean OR node.  Commutative.
pub fn bool_or(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat  { bool_binary(BoolBinaryOp::Or,  lhs, rhs) }
/// Matches a boolean XOR node.  Commutative.
pub fn bool_xor(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat { bool_binary(BoolBinaryOp::Xor, lhs, rhs) }
/// Matches a boolean unary operation with the given `op`.
pub fn bool_unary(op: BoolUnaryOp, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::BoolUnaryOp { op, operand: operand.into() })
}
/// Matches a boolean NOT node.
pub fn bool_not(operand: impl Into<Pat>) -> Pat { bool_unary(BoolUnaryOp::Neg, operand) }

// Casts / coercions

/// Matches a `CastToBool` node (non-zero integer → `true`).
pub fn cast_to_bool(operand: impl Into<Pat>) -> Pat { Pat::new(PatKind::CastToBool { operand: operand.into() }) }
/// Matches a `CastToInt` node (`bool` → `0` or `1`).
pub fn cast_to_int(operand: impl Into<Pat>) -> Pat  { Pat::new(PatKind::CastToInt  { operand: operand.into() }) }
/// Matches a `Truncate` node (narrows an integer to fewer bits).
pub fn truncate(operand: impl Into<Pat>) -> Pat     { Pat::new(PatKind::Truncate   { operand: operand.into() }) }
/// Matches an `Extend` node with the given extension kind.
pub fn extend(op: ExtendOp, operand: impl Into<Pat>) -> Pat { Pat::new(PatKind::Extend { op, operand: operand.into() }) }
/// Matches a zero-extension node.
pub fn zero_extend(operand: impl Into<Pat>) -> Pat  { extend(ExtendOp::ZeroExtend, operand) }
/// Matches a sign-extension node.
pub fn sign_extend(operand: impl Into<Pat>) -> Pat  { extend(ExtendOp::SignExtend, operand) }
/// Matches a popcount (count-set-bits) node.
pub fn popcount(operand: impl Into<Pat>) -> Pat     { Pat::new(PatKind::Popcount   { operand: operand.into() }) }

// Memory

/// Starts building a `Load` pattern.  Chain `.addr()` / `.space()` to add
/// constraints.
pub fn load() -> LoadPat  { LoadPat::new() }
/// Starts building a `Store` pattern.  Chain `.addr()` / `.data()` / `.space()`
/// to add constraints.
pub fn store() -> StorePat { StorePat::new() }

// Phi nodes

/// Starts building a `ControlPhi` pattern.  Matches any phi node.
pub fn phi() -> PhiPat { PhiPat::new() }
/// Starts building a `ControlPhi` pattern pinned to varnode `vn`.
pub fn phi_for(vn: rsleigh::Vn) -> PhiPat { PhiPat::new().for_vn(vn) }

// Entry values

/// Matches any `InitialVar` node (function-entry value of any varnode).
pub fn initial_var() -> Pat { Pat::new(PatKind::InitialVar { vn: None }) }
/// Matches the `InitialVar` node for the specific varnode `vn`.
pub fn initial_var_for(vn: rsleigh::Vn) -> Pat { Pat::new(PatKind::InitialVar { vn: Some(vn) }) }

// Control nodes

/// Starts building a `Call` pattern.  Chain `.at()`, `.arg()`, `.target()` to
/// add constraints.
pub fn call() -> CallPat    { CallPat::new() }
/// Starts building a `Return` pattern.  Chain `.preceded_by()` / `.ret_val()`
/// to add constraints.
pub fn ret() -> RetPat      { RetPat::new() }
/// Starts building an `If` pattern.  Chain `.cond()`, `.true_branch()`,
/// `.false_branch()` to add constraints.
pub fn if_node() -> IfPat   { IfPat::new() }

// Region search

/// Matches any node reachable via a forward control-chain walk from the current
/// node that satisfies `p`.
///
/// Transparent to `ControlState`, `IfCase`, and `Call` nodes; stops at `If`
/// and `Return` terminators.
pub fn contains(p: impl Into<Pat>) -> Pat { Pat::new(PatKind::Contains(p.into())) }
