use std::sync::Arc;

use ir::{BoolBinaryOp, BoolUnaryOp, ExtendOp, IntBinaryOp, IntCmpOp, IntUnaryOp};

use crate::var::{NodeVar, Var};

// ── Core pattern type ─────────────────────────────────────────────────────────

/// A graph pattern.  Cheap to clone — the inner data is reference-counted.
#[derive(Clone)]
pub struct Pat(Arc<PatKind>);

impl Pat {
    fn new(kind: PatKind) -> Self {
        Self(Arc::new(kind))
    }

    pub fn inner(&self) -> &PatKind {
        &self.0
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
    IntConst(u64),
    BoolConst(bool),

    // ── Integer ops ───────────────────────────────────────────────────────────
    IntBinaryOp { op: IntBinaryOp, lhs: Pat, rhs: Pat },
    IntUnaryOp  { op: IntUnaryOp,  operand: Pat },
    /// Comparison ops produce a `Bool` output.
    IntCmpOp    { op: IntCmpOp,    lhs: Pat, rhs: Pat },

    // ── Bool ops ──────────────────────────────────────────────────────────────
    BoolBinaryOp { op: BoolBinaryOp, lhs: Pat, rhs: Pat },
    BoolUnaryOp  { op: BoolUnaryOp,  operand: Pat },

    // ── Casts / coercions (single value input) ────────────────────────────────
    CastToBool { operand: Pat },
    CastToInt  { operand: Pat },
    Truncate   { operand: Pat },
    Extend     { op: ExtendOp, operand: Pat },
    Popcount   { operand: Pat },

    // ── Memory ops ────────────────────────────────────────────────────────────
    /// `Load(space)`: inputs = [mem(0), addr(1)] → value output.
    Load {
        space: Option<rsleigh::VnSpace>,
        addr:  Option<Pat>,
    },
    /// `Store(space)`: inputs = [mem(0), addr(1), data(2)] → mem output.
    Store {
        space: Option<rsleigh::VnSpace>,
        addr:  Option<Pat>,
        data:  Option<Pat>,
    },

    // ── Phi / selector ────────────────────────────────────────────────────────
    /// `ControlSelector(vn)`: inputs = [selector(0), pred_val(1), pred_val(2)…].
    /// `vn = None` matches any selector; `inputs` constrains specific predecessor slots.
    Selector {
        vn:     Option<rsleigh::Vn>,
        inputs: Vec<(usize, Pat)>,
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
}

// ── Builder: LoadPat ──────────────────────────────────────────────────────────

pub struct LoadPat {
    space: Option<rsleigh::VnSpace>,
    addr:  Option<Pat>,
}

impl LoadPat {
    pub(crate) fn new() -> Self { Self { space: None, addr: None } }

    pub fn space(mut self, s: rsleigh::VnSpace) -> Self { self.space = Some(s); self }
    pub fn addr(mut self, p: Pat) -> Self { self.addr = Some(p); self }
}

impl From<LoadPat> for Pat {
    fn from(b: LoadPat) -> Pat {
        Pat::new(PatKind::Load { space: b.space, addr: b.addr })
    }
}

// ── Builder: StorePat ─────────────────────────────────────────────────────────

pub struct StorePat {
    space: Option<rsleigh::VnSpace>,
    addr:  Option<Pat>,
    data:  Option<Pat>,
}

impl StorePat {
    pub(crate) fn new() -> Self { Self { space: None, addr: None, data: None } }

    pub fn space(mut self, s: rsleigh::VnSpace) -> Self { self.space = Some(s); self }
    pub fn addr(mut self, p: Pat) -> Self { self.addr = Some(p); self }
    pub fn data(mut self, p: Pat) -> Self { self.data = Some(p); self }
}

impl From<StorePat> for Pat {
    fn from(b: StorePat) -> Pat {
        Pat::new(PatKind::Store { space: b.space, addr: b.addr, data: b.data })
    }
}

// ── Builder: SelectorPat ─────────────────────────────────────────────────────

pub struct SelectorPat {
    vn:     Option<rsleigh::Vn>,
    inputs: Vec<(usize, Pat)>,
}

impl SelectorPat {
    pub(crate) fn new() -> Self { Self { vn: None, inputs: Vec::new() } }

    pub fn for_vn(mut self, vn: rsleigh::Vn) -> Self { self.vn = Some(vn); self }
    pub fn input(mut self, idx: usize, p: Pat) -> Self { self.inputs.push((idx, p)); self }
}

impl From<SelectorPat> for Pat {
    fn from(b: SelectorPat) -> Pat {
        Pat::new(PatKind::Selector { vn: b.vn, inputs: b.inputs })
    }
}

// ── Builder: CallPat ──────────────────────────────────────────────────────────

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
    pub fn target(mut self, p: Pat) -> Self { self.target = Some(p); self }
    pub fn arg(mut self, idx: usize, p: Pat) -> Self { self.args.push((idx, p)); self }
    pub fn capture(mut self, nv: NodeVar) -> Self { self.node_var = Some(nv); self }
}

impl From<CallPat> for Pat {
    fn from(b: CallPat) -> Pat {
        Pat::new(PatKind::Call { target: b.target, args: b.args, node_var: b.node_var })
    }
}

// ── Builder: RetPat ───────────────────────────────────────────────────────────

pub struct RetPat {
    preceded_by: Option<Pat>,
    ret_vals:    Vec<(usize, Pat)>,
    node_var:    Option<NodeVar>,
}

impl RetPat {
    pub(crate) fn new() -> Self { Self { preceded_by: None, ret_vals: Vec::new(), node_var: None } }

    pub fn preceded_by(mut self, call: impl Into<Pat>) -> Self {
        self.preceded_by = Some(call.into()); self
    }
    pub fn ret_val(mut self, idx: usize, p: Pat) -> Self { self.ret_vals.push((idx, p)); self }
    pub fn capture(mut self, nv: NodeVar) -> Self { self.node_var = Some(nv); self }
}

impl From<RetPat> for Pat {
    fn from(b: RetPat) -> Pat {
        Pat::new(PatKind::Return { preceded_by: b.preceded_by, ret_vals: b.ret_vals, node_var: b.node_var })
    }
}

// ── Builder: IfPat ────────────────────────────────────────────────────────────

pub struct IfPat {
    cond:         Option<Pat>,
    true_branch:  Option<Pat>,
    false_branch: Option<Pat>,
    node_var:     Option<NodeVar>,
}

impl IfPat {
    pub(crate) fn new() -> Self { Self { cond: None, true_branch: None, false_branch: None, node_var: None } }

    pub fn cond(mut self, p: Pat) -> Self { self.cond = Some(p); self }
    pub fn true_branch(mut self, p: impl Into<Pat>) -> Self { self.true_branch = Some(p.into()); self }
    pub fn false_branch(mut self, p: impl Into<Pat>) -> Self { self.false_branch = Some(p.into()); self }
    pub fn capture(mut self, nv: NodeVar) -> Self { self.node_var = Some(nv); self }
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

pub fn any() -> Pat { Pat::new(PatKind::Any) }
pub fn var(v: Var) -> Pat { Pat::new(PatKind::Capture(v)) }
pub fn int_const(v: u64) -> Pat { Pat::new(PatKind::IntConst(v)) }
pub fn bool_const(v: bool) -> Pat { Pat::new(PatKind::BoolConst(v)) }

// Integer binary ops
pub fn int_binary(op: IntBinaryOp, lhs: Pat, rhs: Pat) -> Pat {
    Pat::new(PatKind::IntBinaryOp { op, lhs, rhs })
}
pub fn add(lhs: Pat, rhs: Pat) -> Pat  { int_binary(IntBinaryOp::Add,        lhs, rhs) }
pub fn sub(lhs: Pat, rhs: Pat) -> Pat  { int_binary(IntBinaryOp::Sub,        lhs, rhs) }
pub fn mul(lhs: Pat, rhs: Pat) -> Pat  { int_binary(IntBinaryOp::Mul,        lhs, rhs) }
pub fn div(lhs: Pat, rhs: Pat) -> Pat  { int_binary(IntBinaryOp::Div,        lhs, rhs) }
pub fn sdiv(lhs: Pat, rhs: Pat) -> Pat { int_binary(IntBinaryOp::Sdiv,       lhs, rhs) }
pub fn rem(lhs: Pat, rhs: Pat) -> Pat  { int_binary(IntBinaryOp::Rem,        lhs, rhs) }
pub fn srem(lhs: Pat, rhs: Pat) -> Pat { int_binary(IntBinaryOp::Srem,       lhs, rhs) }
pub fn and(lhs: Pat, rhs: Pat) -> Pat  { int_binary(IntBinaryOp::And,        lhs, rhs) }
pub fn or(lhs: Pat, rhs: Pat) -> Pat   { int_binary(IntBinaryOp::Or,         lhs, rhs) }
pub fn xor(lhs: Pat, rhs: Pat) -> Pat  { int_binary(IntBinaryOp::Xor,        lhs, rhs) }
pub fn shl(lhs: Pat, rhs: Pat) -> Pat  { int_binary(IntBinaryOp::ShiftLeft,  lhs, rhs) }
pub fn shr(lhs: Pat, rhs: Pat) -> Pat  { int_binary(IntBinaryOp::ShiftRight, lhs, rhs) }
pub fn sshr(lhs: Pat, rhs: Pat) -> Pat { int_binary(IntBinaryOp::SShiftRight,lhs, rhs) }

// Integer unary ops
pub fn int_unary(op: IntUnaryOp, operand: Pat) -> Pat {
    Pat::new(PatKind::IntUnaryOp { op, operand })
}
pub fn neg(operand: Pat) -> Pat { int_unary(IntUnaryOp::Neg, operand) }
pub fn not(operand: Pat) -> Pat { int_unary(IntUnaryOp::Not, operand) }

// Integer comparisons (→ Bool)
pub fn int_cmp(op: IntCmpOp, lhs: Pat, rhs: Pat) -> Pat {
    Pat::new(PatKind::IntCmpOp { op, lhs, rhs })
}
pub fn int_eq(lhs: Pat, rhs: Pat) -> Pat  { int_cmp(IntCmpOp::Equal,      lhs, rhs) }
pub fn int_lt(lhs: Pat, rhs: Pat) -> Pat  { int_cmp(IntCmpOp::Less,       lhs, rhs) }
pub fn int_le(lhs: Pat, rhs: Pat) -> Pat  { int_cmp(IntCmpOp::LessEqual,  lhs, rhs) }
pub fn int_slt(lhs: Pat, rhs: Pat) -> Pat { int_cmp(IntCmpOp::Sless,      lhs, rhs) }
pub fn int_sle(lhs: Pat, rhs: Pat) -> Pat { int_cmp(IntCmpOp::SlessEqual, lhs, rhs) }
pub fn int_carry(lhs: Pat, rhs: Pat) -> Pat   { int_cmp(IntCmpOp::Carry,   lhs, rhs) }
pub fn int_scarry(lhs: Pat, rhs: Pat) -> Pat  { int_cmp(IntCmpOp::Scarry,  lhs, rhs) }
pub fn int_sborrow(lhs: Pat, rhs: Pat) -> Pat { int_cmp(IntCmpOp::Sborrow, lhs, rhs) }

// Bool ops
pub fn bool_binary(op: BoolBinaryOp, lhs: Pat, rhs: Pat) -> Pat {
    Pat::new(PatKind::BoolBinaryOp { op, lhs, rhs })
}
pub fn bool_and(lhs: Pat, rhs: Pat) -> Pat { bool_binary(BoolBinaryOp::And, lhs, rhs) }
pub fn bool_or(lhs: Pat, rhs: Pat) -> Pat  { bool_binary(BoolBinaryOp::Or,  lhs, rhs) }
pub fn bool_xor(lhs: Pat, rhs: Pat) -> Pat { bool_binary(BoolBinaryOp::Xor, lhs, rhs) }
pub fn bool_unary(op: BoolUnaryOp, operand: Pat) -> Pat {
    Pat::new(PatKind::BoolUnaryOp { op, operand })
}
pub fn bool_not(operand: Pat) -> Pat { bool_unary(BoolUnaryOp::Neg, operand) }

// Casts / coercions
pub fn cast_to_bool(operand: Pat) -> Pat { Pat::new(PatKind::CastToBool { operand }) }
pub fn cast_to_int(operand: Pat) -> Pat  { Pat::new(PatKind::CastToInt  { operand }) }
pub fn truncate(operand: Pat) -> Pat     { Pat::new(PatKind::Truncate   { operand }) }
pub fn extend(op: ExtendOp, operand: Pat) -> Pat { Pat::new(PatKind::Extend { op, operand }) }
pub fn zero_extend(operand: Pat) -> Pat  { extend(ExtendOp::ZeroExtend, operand) }
pub fn sign_extend(operand: Pat) -> Pat  { extend(ExtendOp::SignExtend, operand) }
pub fn popcount(operand: Pat) -> Pat     { Pat::new(PatKind::Popcount   { operand }) }

// Memory
pub fn load() -> LoadPat  { LoadPat::new() }
pub fn store() -> StorePat { StorePat::new() }

// Selector / phi
pub fn selector() -> SelectorPat { SelectorPat::new() }
pub fn selector_for(vn: rsleigh::Vn) -> SelectorPat { SelectorPat::new().for_vn(vn) }

// Entry values
pub fn initial_var() -> Pat { Pat::new(PatKind::InitialVar { vn: None }) }
pub fn initial_var_for(vn: rsleigh::Vn) -> Pat { Pat::new(PatKind::InitialVar { vn: Some(vn) }) }

// Control nodes
pub fn call() -> CallPat    { CallPat::new() }
pub fn ret() -> RetPat      { RetPat::new() }
pub fn if_node() -> IfPat   { IfPat::new() }

// Region search
pub fn contains(p: impl Into<Pat>) -> Pat { Pat::new(PatKind::Contains(p.into())) }
