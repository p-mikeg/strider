use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::NodeOutputId;
use ir::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};

use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, BoolVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    FloatVar, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar, NodeVar, Var,
};

/// Predicate function type used in [`PatKind::WithPredicate`].
pub type PredicateFn = Arc<dyn Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync>;

/// Predicate function type used in [`PatKind::WithMatchPredicate`].
///
/// Unlike [`PredicateFn`], this variant sees the full capture [`crate::matcher::Bindings`]
/// map, not just the single matched output — useful for guards that
/// reference multiple captures.
pub type MatchPredicateFn =
    Arc<dyn Fn(&BuiltFunctionGraph, &crate::matcher::Bindings) -> bool + Send + Sync>;

// ── Const-capture overloading traits ──────────────────────────────────────────

/// Sealed trait used by [`any_int_const`] to accept either a [`Var`] (binds
/// the matched `NodeOutputId`) or an [`IntVar`] (binds the concrete
/// constant value as `u64`).
pub trait IntoAnyIntConst: sealed::SealedAnyIntConst {
    #[doc(hidden)]
    fn into_any_int_const_pat(self) -> Pat;
}

impl IntoAnyIntConst for Var {
    fn into_any_int_const_pat(self) -> Pat {
        Pat::new(PatKind::AnyIntConst(self))
    }
}

impl IntoAnyIntConst for IntVar {
    fn into_any_int_const_pat(self) -> Pat {
        Pat::new(PatKind::AnyIntConstTyped(self))
    }
}

/// Sealed trait used by [`any_bool_const`] to accept either a [`Var`] or a
/// [`BoolVar`].
pub trait IntoAnyBoolConst: sealed::SealedAnyBoolConst {
    #[doc(hidden)]
    fn into_any_bool_const_pat(self) -> Pat;
}

impl IntoAnyBoolConst for Var {
    fn into_any_bool_const_pat(self) -> Pat {
        Pat::new(PatKind::AnyBoolConst(self))
    }
}

impl IntoAnyBoolConst for BoolVar {
    fn into_any_bool_const_pat(self) -> Pat {
        Pat::new(PatKind::AnyBoolConstTyped(self))
    }
}

/// Sealed trait used by [`any_float_const`] to accept either a [`Var`] or a
/// [`FloatVar`].
pub trait IntoAnyFloatConst: sealed::SealedAnyFloatConst {
    #[doc(hidden)]
    fn into_any_float_const_pat(self) -> Pat;
}

impl IntoAnyFloatConst for Var {
    fn into_any_float_const_pat(self) -> Pat {
        Pat::new(PatKind::AnyFloatConst(self))
    }
}

impl IntoAnyFloatConst for FloatVar {
    fn into_any_float_const_pat(self) -> Pat {
        Pat::new(PatKind::AnyFloatConstTyped(self))
    }
}

mod sealed {
    use crate::var::{BoolVar, FloatVar, IntVar, Var};

    pub trait SealedAnyIntConst {}
    impl SealedAnyIntConst for Var {}
    impl SealedAnyIntConst for IntVar {}

    pub trait SealedAnyBoolConst {}
    impl SealedAnyBoolConst for Var {}
    impl SealedAnyBoolConst for BoolVar {}

    pub trait SealedAnyFloatConst {}
    impl SealedAnyFloatConst for Var {}
    impl SealedAnyFloatConst for FloatVar {}
}

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
    fn capture_impl(self, v: Var) -> Pat {
        Pat::new(PatKind::WithCapture {
            inner: self,
            var: v,
        })
    }

    /// After this pattern matches successfully, additionally run `f` against
    /// the matched output.  The match fails if `f` returns `false`.
    fn when_impl<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        Pat::new(PatKind::WithPredicate {
            inner: self,
            func: Arc::new(f),
        })
    }

    /// After this pattern matches successfully, additionally run `f` with
    /// access to the full capture [`crate::matcher::Bindings`].  The match
    /// fails if `f` returns `false`.  For commutative binary ops this failure
    /// triggers the other-ordering retry automatically.
    pub fn when_match<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, &crate::matcher::Bindings) -> bool + Send + Sync + 'static,
    {
        Pat::new(PatKind::WithMatchPredicate {
            inner: self,
            func: Arc::new(f),
        })
    }
}

// ── IntoPat blanket trait ─────────────────────────────────────────────────────

/// Blanket trait that lets every builder struct (`IntBinaryOpPat`, `LoadPat`, …)
/// participate in the common `capture` / `when` suffix operations without
/// each builder re-implementing them.
///
/// Every type that implements `Into<Pat>` automatically gets `capture` and `when`
/// for free.  Import this trait to call `.capture(v)` / `.when(f)` on builder
/// types.
pub trait IntoPat: Into<Pat> + Sized {
    /// After matching, bind the matched output to `v`.
    fn capture(self, v: Var) -> Pat {
        self.into().capture_impl(v)
    }
    /// After matching, additionally run `f` — fails if it returns `false`.
    fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        self.into().when_impl(f)
    }
}

impl<T: Into<Pat>> IntoPat for T {}

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
    /// Matches any `BoolConst` node and binds its output to `v`.
    AnyBoolConst(Var),

    /// Matches any `IntConst` node and binds the **concrete constant value**
    /// (`u64`) to `v`.  Same structural match as [`PatKind::AnyIntConst`] but
    /// the capture is typed so rewrite-rule closures can read the value
    /// directly via `FromCtx` without a graph lookup.
    AnyIntConstTyped(IntVar),
    /// Matches any `BoolConst` node and binds its value to `v`.
    AnyBoolConstTyped(BoolVar),
    /// Matches any `FloatConst` node and binds its IEEE 754 bit pattern to
    /// `v`.
    AnyFloatConstTyped(FloatVar),

    // ── Integer ops ───────────────────────────────────────────────────────────
    /// Matches an integer binary operation node.
    /// When `ordered` is `false` and the op is commutative, both operand
    /// orderings are tried automatically.
    IntBinaryOp {
        op: IntBinaryOp,
        lhs: Pat,
        rhs: Pat,
        ordered: bool,
    },
    /// Matches an integer unary operation node.
    IntUnaryOp { op: IntUnaryOp, operand: Pat },
    /// Matches an integer comparison node (produces a `Bool` output).
    /// When `ordered` is `false` and the op is commutative (`Equal`, `Carry`,
    /// `Scarry`), both operand orderings are tried automatically.
    IntCmpOp { op: IntCmpOp, lhs: Pat, rhs: Pat, ordered: bool },

    // ── Bool ops ──────────────────────────────────────────────────────────────
    /// Matches a boolean binary operation node.
    /// When `ordered` is `false` and the op is commutative, both orderings are
    /// tried automatically.
    BoolBinaryOp {
        op: BoolBinaryOp,
        lhs: Pat,
        rhs: Pat,
        ordered: bool,
    },
    /// Matches a boolean unary operation node.
    BoolUnaryOp { op: BoolUnaryOp, operand: Pat },

    // ── Variant-agnostic integer op patterns ─────────────────────────────────
    /// Matches **any** integer binary operation, binding the actual operator
    /// variant to `op`.  When `ordered` is `false` and the matched op is
    /// commutative, both operand orderings are tried automatically.
    IntBinaryAny {
        op: IntBinaryOpVar,
        lhs: Pat,
        rhs: Pat,
        ordered: bool,
    },
    /// Matches **any** integer unary operation, binding the actual operator
    /// variant to `op`.
    IntUnaryAny { op: IntUnaryOpVar, operand: Pat },
    /// Matches **any** integer comparison, binding the actual operator variant
    /// to `op`.  When `ordered` is `false` and the matched op is commutative
    /// (`Equal`, `Carry`, `Scarry`), both operand orderings are tried.
    IntCmpAny {
        op: IntCmpOpVar,
        lhs: Pat,
        rhs: Pat,
        ordered: bool,
    },
    /// Matches **any** boolean binary operation, binding the actual operator
    /// variant to `op`.  When `ordered` is `false` and the matched op is
    /// commutative, both operand orderings are tried automatically.
    BoolBinaryAny {
        op: BoolBinaryOpVar,
        lhs: Pat,
        rhs: Pat,
        ordered: bool,
    },
    /// Matches **any** boolean unary operation, binding the actual operator
    /// variant to `op`.
    BoolUnaryAny { op: BoolUnaryOpVar, operand: Pat },
    /// Matches **any** float binary operation, binding the actual operator
    /// variant to `op`.  When `ordered` is `false` and the matched op is
    /// commutative (`Add`, `Mul`), both operand orderings are tried.
    FloatBinaryAny {
        op: FloatBinaryOpVar,
        lhs: Pat,
        rhs: Pat,
        ordered: bool,
    },
    /// Matches **any** float unary operation, binding the actual operator
    /// variant to `op`.
    FloatUnaryAny { op: FloatUnaryOpVar, operand: Pat },
    /// Matches **any** float comparison, binding the actual operator variant
    /// to `op`.  When `ordered` is `false`, no automatic commutative retry is
    /// applied (no float cmp op is commutative in the existing helper).
    FloatCmpAny {
        op: FloatCmpOpVar,
        lhs: Pat,
        rhs: Pat,
        ordered: bool,
    },

    // ── Casts / coercions (single value input) ────────────────────────────────
    /// Matches a `CastToBool` node (non-zero integer → `true`).
    CastToBool { operand: Pat },
    /// Matches a `CastToInt` node (`bool` → `0` or `1`).
    CastToInt { operand: Pat },
    /// Matches a `Truncate` node (narrows an integer to fewer bits).
    Truncate { operand: Pat },
    /// Matches an `Extend` node (widens an integer).
    Extend { op: ExtendOp, operand: Pat },
    /// Matches a `Popcount` node (counts set bits).
    Popcount { operand: Pat },
    /// Matches a `Lzcount` node (counts leading zero bits).
    Lzcount { operand: Pat },
    /// Matches a `Piece` node (concatenates hi and lo: `(hi << bits(lo)) | lo`).
    Piece { hi: Pat, lo: Pat },
    /// Matches an `Extract` node.  `None` for `lsb`/`len` matches any value.
    Extract {
        lsb: Option<u8>,
        len: Option<u8>,
        operand: Pat,
    },
    /// Matches an `Insert` node.  `None` for `lsb`/`len` matches any value.
    Insert {
        lsb: Option<u8>,
        len: Option<u8>,
        dest: Pat,
        src: Pat,
    },

    // ── Float ops ─────────────────────────────────────────────────────────────
    /// Matches a `FloatConst` node whose raw bit representation equals `v`.
    FloatConst(u64),
    /// Matches any `FloatConst` node and binds its output to `v`.
    AnyFloatConst(Var),
    /// Matches a float binary operation node.
    /// When `ordered` is `false` and the op is commutative (`Add`, `Mul`), both
    /// operand orderings are tried automatically.
    FloatBinaryOp {
        op: FloatBinaryOp,
        lhs: Pat,
        rhs: Pat,
        ordered: bool,
    },
    /// Matches a float unary operation node.
    FloatUnaryOp { op: FloatUnaryOp, operand: Pat },
    /// Matches a float comparison node (produces a `Bool` output).
    FloatCmpOp { op: FloatCmpOp, lhs: Pat, rhs: Pat },
    /// Matches a `FloatIsNan` node (unary, produces `Bool`).
    FloatIsNan { operand: Pat },
    /// Matches an `IntToFloat` value-conversion node.
    IntToFloat { operand: Pat },
    /// Matches a `FloatToInt` value-conversion node.
    FloatToInt { operand: Pat },
    /// Matches a `FloatToFloat` precision-conversion node.
    FloatToFloat { operand: Pat },
    /// Matches an `IntBitsToFloat` bitcast node.
    IntBitsToFloat { operand: Pat },
    /// Matches a `FloatBitsToInt` bitcast node.
    FloatBitsToInt { operand: Pat },
    /// Matches a `CastToFloat` generic-cast node.
    CastToFloat { operand: Pat },

    // ── Memory ops ────────────────────────────────────────────────────────────
    /// `Load(space)`: inputs = [mem(0), addr(1)] → value output.
    Load {
        space: Option<rsleigh::VnSpace>,
        addr: Option<Pat>,
        /// Bind the load's output `NodeOutputId` to this variable.
        output_var: Option<Var>,
        /// Bind the load's `NodeId` to this variable.
        node_var: Option<NodeVar>,
    },
    /// `Store(space)`: inputs = [mem(0), addr(1), data(2)] → mem output.
    Store {
        space: Option<rsleigh::VnSpace>,
        addr: Option<Pat>,
        data: Option<Pat>,
        /// Bind the store's memory output `NodeOutputId` to this variable.
        output_var: Option<Var>,
        /// Bind the store's `NodeId` to this variable.
        node_var: Option<NodeVar>,
    },
    /// `StackStore { space, offset }`: inputs = [mem(0), data(1)] → mem output.
    /// Produced by the `StackStoreDetect` optimization pass when a store's
    /// address resolves to `InitialVar(stack_ptr) + offset`.
    StackStore {
        space: Option<rsleigh::VnSpace>,
        offset: Option<i64>,
        data: Option<Pat>,
        /// Bind the stack-store's memory output `NodeOutputId` to this variable.
        output_var: Option<Var>,
        /// Bind the stack-store's `NodeId` to this variable.
        node_var: Option<NodeVar>,
    },
    /// `StackStorePhi { space }`: inputs = [phi_token(0), mem(1), data(2)] → mem output.
    /// Per-branch offsets are stored as a side map on the graph; see
    /// [`ir::Graph::stack_phi_offsets`].  `offsets` (if supplied) matches the
    /// sorted-ascending list of per-branch offsets exactly.
    StackStorePhi {
        space: Option<rsleigh::VnSpace>,
        offsets: Option<Vec<i64>>,
        data: Option<Pat>,
        /// Bind the stack-store-phi's memory output to this variable.
        output_var: Option<Var>,
        /// Bind the stack-store-phi's `NodeId` to this variable.
        node_var: Option<NodeVar>,
    },

    // ── Phi nodes ─────────────────────────────────────────────────────────────
    /// `ControlPhi(vn)`: inputs = [phi_token(0), pred_val(1), pred_val(2)…].
    /// `vn = None` matches any phi; `inputs` constrains specific predecessor slots.
    Phi {
        vn: Option<rsleigh::Vn>,
        inputs: Vec<(usize, Pat)>,
        /// Bind the phi's output `NodeOutputId` to this variable.
        output_var: Option<Var>,
        /// Bind the phi's `NodeId` to this variable.
        node_var: Option<NodeVar>,
    },

    // ── Function-entry values ─────────────────────────────────────────────────
    /// Matches `NodeKind::InitialVar(vn)`.  `vn = None` matches any.
    InitialVar { vn: Option<rsleigh::Vn> },

    // ── Control-level nodes ───────────────────────────────────────────────────
    /// `Call`: inputs = [ctrl(0), mem(1), target(2), arg0(3), arg1(4)…]
    Call {
        target: Option<Pat>,
        args: Vec<(usize, Pat)>,
        node_var: Option<NodeVar>,
    },
    /// `CallOther`: inputs = [ctrl(0), mem(1), arg0(2), arg1(3)…].
    /// `user_op_id = None` matches any user-op id.
    CallOther {
        user_op_id: Option<u64>,
        args: Vec<(usize, Pat)>,
        node_var: Option<NodeVar>,
    },
    /// `Return`: inputs = [ctrl(0), mem(1), retval0(2)…]
    Return {
        preceded_by: Option<Pat>,
        ret_vals: Vec<(usize, Pat)>,
        node_var: Option<NodeVar>,
    },
    /// `If`: inputs = [ctrl(0), cond(1)]; outputs = [true_ctrl(0), false_ctrl(1)]
    If {
        cond: Option<Pat>,
        true_branch: Option<Pat>,
        false_branch: Option<Pat>,
        node_var: Option<NodeVar>,
    },

    // ── Region search ─────────────────────────────────────────────────────────
    /// Forward walk along a ctrl chain, searching for a node matching `inner`.
    Contains(Pat),

    // ── Post-match guards ─────────────────────────────────────────────────────
    /// Matches `inner`, then additionally binds the matched output to `var`.
    WithCapture { inner: Pat, var: Var },
    /// Matches `inner`, then additionally runs `func` — fails if it returns false.
    WithPredicate { inner: Pat, func: PredicateFn },
    /// Matches `inner`, then additionally runs `func` with the full capture
    /// bindings.  Fails (returning `false`) rejects the current operand
    /// ordering and lets commutative backtracks retry.
    WithMatchPredicate { inner: Pat, func: MatchPredicateFn },
}

// ── Builder: IntBinaryOpPat ───────────────────────────────────────────────────

/// Builder for integer binary operation patterns.
///
/// Returned by [`int_binary`] and the shorthand constructors ([`add`], [`sub`],
/// [`mul`], …).  Call `.into()` (or pass directly to any `impl Into<Pat>`
/// parameter) to obtain a [`Pat`].
pub struct IntBinaryOpPat {
    op: IntBinaryOp,
    lhs: Pat,
    rhs: Pat,
    ordered: bool,
}

impl IntBinaryOpPat {
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
        Pat::new(PatKind::IntBinaryOp {
            op: b.op,
            lhs: b.lhs,
            rhs: b.rhs,
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
    op: BoolBinaryOp,
    lhs: Pat,
    rhs: Pat,
    ordered: bool,
}

impl BoolBinaryOpPat {
    /// Force the pattern to match operands in the stated order only.
    pub fn ordered(mut self) -> Self {
        self.ordered = true;
        self
    }
}

impl From<BoolBinaryOpPat> for Pat {
    fn from(b: BoolBinaryOpPat) -> Pat {
        Pat::new(PatKind::BoolBinaryOp {
            op: b.op,
            lhs: b.lhs,
            rhs: b.rhs,
            ordered: b.ordered,
        })
    }
}

// ── Builder: FloatBinaryOpPat ─────────────────────────────────────────────────

/// Builder for float binary operation patterns.
///
/// Returned by [`float_binary`] and the shorthand constructors ([`float_add`],
/// [`float_sub`], [`float_mul`], [`float_div`]).  Call `.into()` or pass
/// directly to any `impl Into<Pat>` parameter to obtain a [`Pat`].
pub struct FloatBinaryOpPat {
    op: FloatBinaryOp,
    lhs: Pat,
    rhs: Pat,
    ordered: bool,
}

impl FloatBinaryOpPat {
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
        Pat::new(PatKind::FloatBinaryOp {
            op: b.op,
            lhs: b.lhs,
            rhs: b.rhs,
            ordered: b.ordered,
        })
    }
}

// ── Builder: LoadPat ──────────────────────────────────────────────────────────

/// Builder for `Load` node patterns.  Created by [`load`].
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
        Pat::new(PatKind::Load {
            space: b.space,
            addr: b.addr,
            output_var: b.output_var,
            node_var: b.node_var,
        })
    }
}

// ── Builder: StorePat ─────────────────────────────────────────────────────────

/// Builder for `Store` node patterns.  Created by [`store`].
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
        Pat::new(PatKind::Store {
            space: b.space,
            addr: b.addr,
            data: b.data,
            output_var: b.output_var,
            node_var: b.node_var,
        })
    }
}

// ── Builder: StackStorePat ────────────────────────────────────────────────────

/// Builder for `StackStore` node patterns.  Created by [`stack_store`].
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
        Pat::new(PatKind::StackStore {
            space: b.space,
            offset: b.offset,
            data: b.data,
            output_var: b.output_var,
            node_var: b.node_var,
        })
    }
}

// ── Builder: StackStorePhiPat ─────────────────────────────────────────────────

/// Builder for `StackStorePhi` node patterns.  Created by [`stack_store_phi`].
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
    /// Match the per-branch offsets exactly.  The supplied list is sorted
    /// ascending before comparison, so caller order is irrelevant.
    pub fn offsets<I: IntoIterator<Item = i64>>(mut self, os: I) -> Self {
        let mut v: Vec<i64> = os.into_iter().collect();
        v.sort();
        self.offsets = Some(v);
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
}

impl From<StackStorePhiPat> for Pat {
    fn from(b: StackStorePhiPat) -> Pat {
        Pat::new(PatKind::StackStorePhi {
            space: b.space,
            offsets: b.offsets,
            data: b.data,
            output_var: b.output_var,
            node_var: b.node_var,
        })
    }
}

// ── Builder: PhiPat ──────────────────────────────────────────────────────────

/// Builder for `ControlPhi` node patterns.  Created by [`phi`] or [`phi_for`].
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
    pub fn for_vn(mut self, vn: rsleigh::Vn) -> Self {
        self.vn = Some(vn);
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
        Pat::new(PatKind::Phi {
            vn: b.vn,
            inputs: b.inputs,
            output_var: b.output_var,
            node_var: b.node_var,
        })
    }
}

// ── Builder: CallPat ──────────────────────────────────────────────────────────

/// Builder for `Call` node patterns.  Created by [`call`].
pub struct CallPat {
    target: Option<Pat>,
    args: Vec<(usize, Pat)>,
    node_var: Option<NodeVar>,
}

impl CallPat {
    pub(crate) fn new() -> Self {
        Self {
            target: None,
            args: Vec::new(),
            node_var: None,
        }
    }

    /// Constrain the call target to the literal address `addr`.
    pub fn at(self, addr: u64) -> Self {
        self.target(Pat::new(PatKind::IntConst(addr)))
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
    /// Bind the matched `Call` node to `nv`.
    pub fn capture(mut self, nv: NodeVar) -> Self {
        self.node_var = Some(nv);
        self
    }
}

impl From<CallPat> for Pat {
    fn from(b: CallPat) -> Pat {
        Pat::new(PatKind::Call {
            target: b.target,
            args: b.args,
            node_var: b.node_var,
        })
    }
}

// ── Builder: CallOtherPat ─────────────────────────────────────────────────────

/// Builder for `CallOther` node patterns.  Created by [`call_other`].
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
    pub fn user_op_id(mut self, id: u64) -> Self {
        self.user_op_id = Some(id);
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
        Pat::new(PatKind::CallOther {
            user_op_id: b.user_op_id,
            args: b.args,
            node_var: b.node_var,
        })
    }
}

// ── Builder: RetPat ───────────────────────────────────────────────────────────

/// Builder for `Return` node patterns.  Created by [`ret`].
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
    pub fn preceded_by(mut self, call: impl Into<Pat>) -> Self {
        self.preceded_by = Some(call.into());
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
        Pat::new(PatKind::Return {
            preceded_by: b.preceded_by,
            ret_vals: b.ret_vals,
            node_var: b.node_var,
        })
    }
}

// ── Builder: IfPat ────────────────────────────────────────────────────────────

/// Builder for `If` node patterns.  Created by [`if_node`].
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
pub fn any() -> Pat {
    Pat::new(PatKind::Any)
}

/// Matches any output and binds it to `v`.
///
/// If `v` is already bound the output must equal the stored binding.
/// Shorthand for `any().capture(v)`.
pub fn var(v: Var) -> Pat {
    Pat::new(PatKind::Capture(v))
}

/// Matches an `IntConst` node with value exactly `v`.
pub fn int_const(v: u64) -> Pat {
    Pat::new(PatKind::IntConst(v))
}

/// Matches a `BoolConst` node with value exactly `v`.
pub fn bool_const(v: bool) -> Pat {
    Pat::new(PatKind::BoolConst(v))
}
/// Matches any `IntConst` node and binds either the output (for a [`Var`]) or
/// the concrete constant value (for an [`IntVar`]).
///
/// Fails if the producing node is not an `IntConst` — use this instead of
/// `var(v)` when you want the pattern itself to enforce the node is a
/// compile-time constant.
pub fn any_int_const<C: IntoAnyIntConst>(v: C) -> Pat {
    v.into_any_int_const_pat()
}

/// Matches any `BoolConst` node and binds its output (for a [`Var`]) or its
/// value (for a [`BoolVar`]).
pub fn any_bool_const<C: IntoAnyBoolConst>(v: C) -> Pat {
    v.into_any_bool_const_pat()
}

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
    IntBinaryOpPat {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    }
}
/// Matches an unsigned addition node (`lhs + rhs`).  Commutative.
pub fn add(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Add, lhs, rhs)
}
/// Matches an unsigned subtraction node (`lhs - rhs`).  Not commutative.
pub fn sub(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Sub, lhs, rhs)
}
/// Matches an unsigned multiplication node.  Commutative.
pub fn mul(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Mul, lhs, rhs)
}
/// Matches an unsigned division node.  Not commutative.
pub fn div(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Div, lhs, rhs)
}
/// Matches a signed division node.  Not commutative.
pub fn sdiv(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Sdiv, lhs, rhs)
}
/// Matches an unsigned remainder node.  Not commutative.
pub fn rem(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Rem, lhs, rhs)
}
/// Matches a signed remainder node.  Not commutative.
pub fn srem(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Srem, lhs, rhs)
}
/// Matches a bitwise AND node.  Commutative.
pub fn and(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::And, lhs, rhs)
}
/// Matches a bitwise OR node.  Commutative.
pub fn or(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Or, lhs, rhs)
}
/// Matches a bitwise XOR node.  Commutative.
pub fn xor(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Xor, lhs, rhs)
}
/// Matches a logical left-shift node.  Not commutative.
pub fn shl(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::ShiftLeft, lhs, rhs)
}
/// Matches a logical right-shift node.  Not commutative.
pub fn shr(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::ShiftRight, lhs, rhs)
}
/// Matches an arithmetic (signed) right-shift node.  Not commutative.
pub fn sshr(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::SShiftRight, lhs, rhs)
}

// Integer unary ops

/// Matches an integer unary operation with the given `op`.
pub fn int_unary(op: IntUnaryOp, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntUnaryOp {
        op,
        operand: operand.into(),
    })
}
/// Matches an integer negation node (`-operand`).
pub fn neg(operand: impl Into<Pat>) -> Pat {
    int_unary(IntUnaryOp::Neg, operand)
}
/// Matches a bitwise complement node (`~operand`).
pub fn not(operand: impl Into<Pat>) -> Pat {
    int_unary(IntUnaryOp::Not, operand)
}

// Integer comparisons (→ Bool)

/// Matches an integer comparison node with the given `op`.
///
/// For commutative ops (`Equal`, `Carry`, `Scarry`), both operand orderings
/// are tried automatically.  Use `int_cmp_ordered` to disable this.
pub fn int_cmp(op: IntCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntCmpOp {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}
/// Matches an unsigned equality comparison (`lhs == rhs`).
pub fn int_eq(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::Equal, lhs, rhs)
}
/// Matches an unsigned less-than comparison (`lhs < rhs`).
pub fn int_lt(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::Less, lhs, rhs)
}
/// Matches an unsigned less-or-equal comparison (`lhs <= rhs`).
pub fn int_le(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::LessEqual, lhs, rhs)
}
/// Matches a signed less-than comparison.
pub fn int_slt(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::Sless, lhs, rhs)
}
/// Matches a signed less-or-equal comparison.
pub fn int_sle(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::SlessEqual, lhs, rhs)
}
/// Matches an unsigned addition carry-out check.
pub fn int_carry(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::Carry, lhs, rhs)
}
/// Matches a signed addition overflow check.
pub fn int_scarry(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::Scarry, lhs, rhs)
}
/// Matches a signed subtraction borrow check.
pub fn int_sborrow(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::Sborrow, lhs, rhs)
}

// Bool ops

/// Matches a boolean binary operation with the given `op`.
///
/// Commutative ops (`And`, `Or`, `Xor`) try both orderings automatically.
pub fn bool_binary(op: BoolBinaryOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    BoolBinaryOpPat {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    }
}
/// Matches a boolean AND node.  Commutative.
pub fn bool_and(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    bool_binary(BoolBinaryOp::And, lhs, rhs)
}
/// Matches a boolean OR node.  Commutative.
pub fn bool_or(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    bool_binary(BoolBinaryOp::Or, lhs, rhs)
}
/// Matches a boolean XOR node.  Commutative.
pub fn bool_xor(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    bool_binary(BoolBinaryOp::Xor, lhs, rhs)
}
/// Matches a boolean unary operation with the given `op`.
pub fn bool_unary(op: BoolUnaryOp, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::BoolUnaryOp {
        op,
        operand: operand.into(),
    })
}
/// Matches a boolean NOT node.
pub fn bool_not(operand: impl Into<Pat>) -> Pat {
    bool_unary(BoolUnaryOp::Neg, operand)
}

// Casts / coercions

/// Matches a `CastToBool` node (non-zero integer → `true`).
pub fn cast_to_bool(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::CastToBool {
        operand: operand.into(),
    })
}
/// Matches a `CastToInt` node (`bool` → `0` or `1`).
pub fn cast_to_int(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::CastToInt {
        operand: operand.into(),
    })
}
/// Matches a `Truncate` node (narrows an integer to fewer bits).
pub fn truncate(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Truncate {
        operand: operand.into(),
    })
}
/// Matches an `Extend` node with the given extension kind.
pub fn extend(op: ExtendOp, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Extend {
        op,
        operand: operand.into(),
    })
}
/// Matches a zero-extension node.
pub fn zero_extend(operand: impl Into<Pat>) -> Pat {
    extend(ExtendOp::ZeroExtend, operand)
}
/// Matches a sign-extension node.
pub fn sign_extend(operand: impl Into<Pat>) -> Pat {
    extend(ExtendOp::SignExtend, operand)
}
/// Matches a popcount (count-set-bits) node.
pub fn popcount(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Popcount {
        operand: operand.into(),
    })
}
/// Matches a leading-zero-count node.
pub fn lzcount(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Lzcount {
        operand: operand.into(),
    })
}
/// Matches a piece (concatenation) node: `(hi << bits(lo)) | lo`.
pub fn piece(hi: impl Into<Pat>, lo: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Piece {
        hi: hi.into(),
        lo: lo.into(),
    })
}
/// Matches an extract node.  Pass `None` for `lsb`/`len` to match any value.
pub fn extract(lsb: Option<u8>, len: Option<u8>, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Extract {
        lsb,
        len,
        operand: operand.into(),
    })
}
/// Matches an insert node.  Pass `None` for `lsb`/`len` to match any value.
pub fn insert(lsb: Option<u8>, len: Option<u8>, dest: impl Into<Pat>, src: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Insert {
        lsb,
        len,
        dest: dest.into(),
        src: src.into(),
    })
}

// Float ops

/// Matches a float binary operation with the given `op`.
///
/// Commutative ops (`Add`, `Mul`) will try both operand orderings automatically.
/// Call `.ordered()` on the result to disable this.
pub fn float_binary(
    op: FloatBinaryOp,
    lhs: impl Into<Pat>,
    rhs: impl Into<Pat>,
) -> FloatBinaryOpPat {
    FloatBinaryOpPat {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    }
}
/// Matches a float addition node.  Commutative.
pub fn float_add(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> FloatBinaryOpPat {
    float_binary(FloatBinaryOp::Add, lhs, rhs)
}
/// Matches a float subtraction node.  Not commutative.
pub fn float_sub(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> FloatBinaryOpPat {
    float_binary(FloatBinaryOp::Sub, lhs, rhs)
}
/// Matches a float multiplication node.  Commutative.
pub fn float_mul(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> FloatBinaryOpPat {
    float_binary(FloatBinaryOp::Mul, lhs, rhs)
}
/// Matches a float division node.  Not commutative.
pub fn float_div(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> FloatBinaryOpPat {
    float_binary(FloatBinaryOp::Div, lhs, rhs)
}

/// Matches a float unary operation with the given `op`.
pub fn float_unary(op: FloatUnaryOp, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatUnaryOp {
        op,
        operand: operand.into(),
    })
}
/// Matches a float negation node.
pub fn float_neg(operand: impl Into<Pat>) -> Pat {
    float_unary(FloatUnaryOp::Neg, operand)
}
/// Matches a float absolute-value node.
pub fn float_abs(operand: impl Into<Pat>) -> Pat {
    float_unary(FloatUnaryOp::Abs, operand)
}
/// Matches a float square-root node.
pub fn float_sqrt(operand: impl Into<Pat>) -> Pat {
    float_unary(FloatUnaryOp::Sqrt, operand)
}
/// Matches a float ceiling node.
pub fn float_ceil(operand: impl Into<Pat>) -> Pat {
    float_unary(FloatUnaryOp::Ceil, operand)
}
/// Matches a float floor node.
pub fn float_floor(operand: impl Into<Pat>) -> Pat {
    float_unary(FloatUnaryOp::Floor, operand)
}
/// Matches a float round node.
pub fn float_round(operand: impl Into<Pat>) -> Pat {
    float_unary(FloatUnaryOp::Round, operand)
}

/// Matches a float comparison node with the given `op`.
pub fn float_cmp(op: FloatCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatCmpOp {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
    })
}
/// Matches a float equality comparison.
pub fn float_eq(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    float_cmp(FloatCmpOp::Equal, lhs, rhs)
}
/// Matches a float not-equal comparison.
pub fn float_ne(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    float_cmp(FloatCmpOp::NotEqual, lhs, rhs)
}
/// Matches a float less-than comparison.
pub fn float_lt(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    float_cmp(FloatCmpOp::Less, lhs, rhs)
}
/// Matches a float less-or-equal comparison.
pub fn float_le(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    float_cmp(FloatCmpOp::LessEqual, lhs, rhs)
}

/// Matches a `FloatIsNan` node.
pub fn float_is_nan(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatIsNan {
        operand: operand.into(),
    })
}

/// Matches a `FloatConst` node with the exact bit pattern `bits`.
pub fn float_const(bits: u64) -> Pat {
    Pat::new(PatKind::FloatConst(bits))
}
/// Matches any `FloatConst` node and binds either the output (for a [`Var`])
/// or its IEEE 754 bit pattern (for a [`FloatVar`]).
pub fn any_float_const<C: IntoAnyFloatConst>(v: C) -> Pat {
    v.into_any_float_const_pat()
}

/// Matches an `IntToFloat` value-conversion node.
pub fn int_to_float(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntToFloat {
        operand: operand.into(),
    })
}
/// Matches a `FloatToInt` value-conversion node.
pub fn float_to_int(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatToInt {
        operand: operand.into(),
    })
}
/// Matches a `FloatToFloat` precision-conversion node.
pub fn float_to_float(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatToFloat {
        operand: operand.into(),
    })
}
/// Matches an `IntBitsToFloat` bitcast node.
pub fn int_bits_to_float(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntBitsToFloat {
        operand: operand.into(),
    })
}
/// Matches a `FloatBitsToInt` bitcast node.
pub fn float_bits_to_int(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatBitsToInt {
        operand: operand.into(),
    })
}
/// Matches a `CastToFloat` generic-cast node.
pub fn cast_to_float(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::CastToFloat {
        operand: operand.into(),
    })
}

// Memory

/// Starts building a `Load` pattern.  Chain `.addr()` / `.space()` to add
/// constraints.
pub fn load() -> LoadPat {
    LoadPat::new()
}
/// Starts building a `Store` pattern.  Chain `.addr()` / `.data()` / `.space()`
/// to add constraints.
pub fn store() -> StorePat {
    StorePat::new()
}
/// Starts building a `StackStore` pattern.  Chain `.offset()` / `.data()` /
/// `.space()` to add constraints.
pub fn stack_store() -> StackStorePat {
    StackStorePat::new()
}
/// Starts building a `StackStorePhi` pattern.  Chain `.offsets(…)` /
/// `.data()` / `.space()` to add constraints.
pub fn stack_store_phi() -> StackStorePhiPat {
    StackStorePhiPat::new()
}

// Phi nodes

/// Starts building a `ControlPhi` pattern.  Matches any phi node.
pub fn phi() -> PhiPat {
    PhiPat::new()
}
/// Starts building a `ControlPhi` pattern pinned to varnode `vn`.
pub fn phi_for(vn: rsleigh::Vn) -> PhiPat {
    PhiPat::new().for_vn(vn)
}

// Entry values

/// Matches any `InitialVar` node (function-entry value of any varnode).
pub fn initial_var() -> Pat {
    Pat::new(PatKind::InitialVar { vn: None })
}
/// Matches the `InitialVar` node for the specific varnode `vn`.
pub fn initial_var_for(vn: rsleigh::Vn) -> Pat {
    Pat::new(PatKind::InitialVar { vn: Some(vn) })
}

// Control nodes

/// Starts building a `Call` pattern.  Chain `.at()`, `.arg()`, `.target()` to
/// add constraints.
pub fn call() -> CallPat {
    CallPat::new()
}
/// Starts building a `CallOther` (user-defined op) pattern.  Chain
/// `.user_op_id()`, `.arg()`, `.capture()` to add constraints.
pub fn call_other() -> CallOtherPat {
    CallOtherPat::new()
}
/// Starts building a `Return` pattern.  Chain `.preceded_by()` / `.ret_val()`
/// to add constraints.
pub fn ret() -> RetPat {
    RetPat::new()
}
/// Starts building an `If` pattern.  Chain `.cond()`, `.true_branch()`,
/// `.false_branch()` to add constraints.
pub fn if_node() -> IfPat {
    IfPat::new()
}

// Region search

/// Matches any node reachable via a forward control-chain walk from the current
/// node that satisfies `p`.
///
/// Transparent to `ControlState`, `IfCase`, and `Call` nodes; stops at `If`
/// and `Return` terminators.
pub fn contains(p: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Contains(p.into()))
}

// ── Variant-agnostic op constructors ─────────────────────────────────────────

/// Matches **any** integer binary operation and binds the actual operator
/// variant to `op`.
///
/// Commutative ops (`Add`, `Mul`, `And`, `Or`, `Xor`) will try both operand
/// orderings automatically unless the returned `Pat` is wrapped via a custom
/// ordered pattern.  Because `int_binary_any` returns a `Pat` directly rather
/// than a builder, there is no `.ordered()` method; use `ordered: true` at
/// the `PatKind` level if you need to construct one manually, or build the
/// `PatKind::IntBinaryAny` directly.
pub fn int_binary_any(op: IntBinaryOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntBinaryAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}

/// Matches **any** integer unary operation and binds the actual operator
/// variant to `op`.
pub fn int_unary_any(op: IntUnaryOpVar, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntUnaryAny {
        op,
        operand: operand.into(),
    })
}

/// Matches **any** integer comparison and binds the actual operator variant
/// to `op`.
///
/// Commutative comparisons (`Equal`, `Carry`, `Scarry`) try both operand
/// orderings automatically.
pub fn int_cmp_any(op: IntCmpOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntCmpAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}

/// Matches **any** boolean binary operation and binds the actual operator
/// variant to `op`.
///
/// Commutative ops (`And`, `Or`, `Xor`) try both operand orderings
/// automatically.
pub fn bool_binary_any(op: BoolBinaryOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::BoolBinaryAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}

/// Matches **any** boolean unary operation and binds the actual operator
/// variant to `op`.
pub fn bool_unary_any(op: BoolUnaryOpVar, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::BoolUnaryAny {
        op,
        operand: operand.into(),
    })
}

/// Matches **any** float binary operation and binds the actual operator
/// variant to `op`.
///
/// Commutative ops (`Add`, `Mul`) try both operand orderings automatically.
pub fn float_binary_any(op: FloatBinaryOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatBinaryAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}

/// Matches **any** float unary operation and binds the actual operator
/// variant to `op`.
pub fn float_unary_any(op: FloatUnaryOpVar, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatUnaryAny {
        op,
        operand: operand.into(),
    })
}

/// Matches **any** float comparison and binds the actual operator variant
/// to `op`.
///
/// No float comparison operators are currently treated as commutative, so no
/// automatic operand-swap retry is attempted.
pub fn float_cmp_any(op: FloatCmpOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatCmpAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}
