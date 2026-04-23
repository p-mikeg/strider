use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeOutputId, NodeOutputType};
use ir::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};

use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, BoolVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    FloatVar, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar, NodeVar, Var,
};

mod builders;
mod ctor;

pub(crate) mod traits;
pub(crate) mod node_pat;
pub(crate) mod control_pat;
pub(crate) mod any;
pub(crate) mod guards;
pub(crate) mod contains;
pub use builders::{
    BoolBinaryOpPat, CallOtherPat, CallPat, FloatBinaryOpPat, FunctionArgPat, IfPat,
    IntBinaryOpPat, LoadPat, PhiPat, RetPat, StackStorePat, StackStorePhiPat, StorePat,
};
pub use ctor::*;

/// Predicate function type used in [`PatKind::WithPredicate`].
pub type PredicateFn =
    Arc<dyn Fn(&BuiltFunctionGraph, NodeOutputType, NodeOutputId) -> bool + Send + Sync>;

/// Predicate function type used in [`PatKind::WithMatchPredicate`].
///
/// Unlike [`PredicateFn`], this variant sees the full capture [`crate::matcher::Bindings`]
/// map, not just the single matched output — useful for guards that
/// reference multiple captures.
pub type MatchPredicateFn = Arc<
    dyn Fn(&BuiltFunctionGraph, NodeOutputType, &crate::matcher::Bindings) -> bool + Send + Sync,
>;

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
    pub(crate) fn new(kind: PatKind) -> Self {
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
        F: Fn(&BuiltFunctionGraph, NodeOutputType, NodeOutputId) -> bool + Send + Sync + 'static,
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
        F: Fn(&BuiltFunctionGraph, NodeOutputType, &crate::matcher::Bindings) -> bool
            + Send
            + Sync
            + 'static,
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
        F: Fn(&BuiltFunctionGraph, NodeOutputType, NodeOutputId) -> bool + Send + Sync + 'static,
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

    /// Matches `NodeKind::FunctionArg { source, index }`.  Any `None` field
    /// matches all values for that field.  `output_var` / `node_var` bind the
    /// value output / `NodeId` respectively.
    FunctionArg {
        source: Option<ir::node::FunctionArgSource>,
        index: Option<u32>,
        output_var: Option<Var>,
        node_var: Option<NodeVar>,
    },

    // ── Control-level nodes ───────────────────────────────────────────────────
    /// `Call`: inputs = [ctrl(0), mem(1), target(2), arg0(3), arg1(4)…];
    /// outputs = [ctrl(0), mem(1), retval0(2), retval1(3), …, other_clobbered(N), …]
    /// where `retval_i` corresponds to the calling convention's i-th return
    /// register.  `ret_outputs` matches patterns against the Call's output at
    /// slot `2 + idx`, so `.ret_output(0, var(v))` captures the value flowing
    /// out of (e.g.) `rax` on x86_64.  A ret reg that is callee-saved does not
    /// appear as a Call output, and the match fails for that slot.
    Call {
        target: Option<Pat>,
        args: Vec<(usize, Pat)>,
        ret_outputs: Vec<(usize, Pat)>,
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

