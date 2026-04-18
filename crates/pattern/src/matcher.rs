use std::collections::{HashMap, HashSet};

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};
use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use crate::pat::{Pat, PatKind};
use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, BoolVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    FloatVar, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar, NodeVar, Var,
};

// ── Commutativity helpers ─────────────────────────────────────────────────────

fn is_commutative_int_op(op: IntBinaryOp) -> bool {
    matches!(
        op,
        IntBinaryOp::Add | IntBinaryOp::Mul | IntBinaryOp::And | IntBinaryOp::Or | IntBinaryOp::Xor
    )
}

fn is_commutative_bool_op(op: BoolBinaryOp) -> bool {
    matches!(op, BoolBinaryOp::And | BoolBinaryOp::Or | BoolBinaryOp::Xor)
}

fn is_commutative_float_op(op: FloatBinaryOp) -> bool {
    matches!(op, FloatBinaryOp::Add | FloatBinaryOp::Mul)
}

fn is_commutative_int_cmp_op(op: IntCmpOp) -> bool {
    matches!(op, IntCmpOp::Equal | IntCmpOp::Carry | IntCmpOp::Scarry)
}

// ── Bindings ──────────────────────────────────────────────────────────────────

/// A set of capture-variable bindings accumulated during a single match attempt.
///
/// Bindings are append-only: once a variable is bound it cannot be rebound to a
/// different value.  A mismatch (trying to bind an already-bound variable to a
/// different value) makes the containing match fail.  The matcher snapshots and
/// restores `Bindings` to implement backtracking.
#[derive(Clone, Default)]
pub struct Bindings {
    vars: HashMap<Var, NodeOutputId>,
    node_vars: HashMap<NodeVar, NodeId>,
    /// Values captured by [`IntVar`] bindings (integer constant bit patterns).
    int_vals: HashMap<IntVar, u64>,
    /// Values captured by [`BoolVar`] bindings (boolean constant values).
    bool_vals: HashMap<BoolVar, bool>,
    /// Values captured by [`FloatVar`] bindings (float constant IEEE 754 bit patterns).
    float_bits: HashMap<FloatVar, u64>,
    /// Operator variants captured by [`IntBinaryOpVar`] bindings.
    int_binary_ops: HashMap<IntBinaryOpVar, IntBinaryOp>,
    /// Operator variants captured by [`IntUnaryOpVar`] bindings.
    int_unary_ops: HashMap<IntUnaryOpVar, IntUnaryOp>,
    /// Operator variants captured by [`IntCmpOpVar`] bindings.
    int_cmp_ops: HashMap<IntCmpOpVar, IntCmpOp>,
    /// Operator variants captured by [`BoolBinaryOpVar`] bindings.
    bool_binary_ops: HashMap<BoolBinaryOpVar, BoolBinaryOp>,
    /// Operator variants captured by [`BoolUnaryOpVar`] bindings.
    bool_unary_ops: HashMap<BoolUnaryOpVar, BoolUnaryOp>,
    /// Operator variants captured by [`FloatBinaryOpVar`] bindings.
    float_binary_ops: HashMap<FloatBinaryOpVar, FloatBinaryOp>,
    /// Operator variants captured by [`FloatUnaryOpVar`] bindings.
    float_unary_ops: HashMap<FloatUnaryOpVar, FloatUnaryOp>,
    /// Operator variants captured by [`FloatCmpOpVar`] bindings.
    float_cmp_ops: HashMap<FloatCmpOpVar, FloatCmpOp>,
}

impl Bindings {
    fn bind_var(&mut self, v: Var, out: NodeOutputId) -> bool {
        if let Some(&existing) = self.vars.get(&v) {
            existing == out
        } else {
            self.vars.insert(v, out);
            true
        }
    }

    fn bind_node_var(&mut self, nv: NodeVar, node: NodeId) -> bool {
        if let Some(&existing) = self.node_vars.get(&nv) {
            existing == node
        } else {
            self.node_vars.insert(nv, node);
            true
        }
    }

    /// Bind `iv` to the integer constant `val`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same value.  Returns `false` if `iv` was already bound to a
    /// **different** value (the match should fail).
    pub fn bind_int(&mut self, iv: IntVar, val: u64) -> bool {
        if let Some(&existing) = self.int_vals.get(&iv) {
            existing == val
        } else {
            self.int_vals.insert(iv, val);
            true
        }
    }

    /// Bind `bv` to the boolean constant `val`.
    ///
    /// Returns `true` if the binding succeeded (new or idempotent), `false` on
    /// conflict.
    pub fn bind_bool(&mut self, bv: BoolVar, val: bool) -> bool {
        if let Some(&existing) = self.bool_vals.get(&bv) {
            existing == val
        } else {
            self.bool_vals.insert(bv, val);
            true
        }
    }

    /// Bind `fv` to the float constant IEEE 754 bit pattern `bits`.
    ///
    /// Returns `true` if the binding succeeded (new or idempotent), `false` on
    /// conflict.
    pub fn bind_float(&mut self, fv: FloatVar, bits: u64) -> bool {
        if let Some(&existing) = self.float_bits.get(&fv) {
            existing == bits
        } else {
            self.float_bits.insert(fv, bits);
            true
        }
    }

    /// Returns the `NodeOutputId` bound to `v`, or `None` if unbound.
    pub fn get(&self, v: Var) -> Option<NodeOutputId> {
        self.vars.get(&v).copied()
    }

    /// Returns the `NodeId` bound to `nv`, or `None` if unbound.
    pub fn get_node(&self, nv: NodeVar) -> Option<NodeId> {
        self.node_vars.get(&nv).copied()
    }

    /// Returns the integer constant value bound to `iv`, or `None` if unbound.
    pub fn get_int(&self, iv: IntVar) -> Option<u64> {
        self.int_vals.get(&iv).copied()
    }

    /// Returns the boolean constant value bound to `bv`, or `None` if unbound.
    pub fn get_bool(&self, bv: BoolVar) -> Option<bool> {
        self.bool_vals.get(&bv).copied()
    }

    /// Returns the float constant IEEE 754 bit pattern bound to `fv`, or `None`
    /// if unbound.
    pub fn get_float_bits(&self, fv: FloatVar) -> Option<u64> {
        self.float_bits.get(&fv).copied()
    }

    /// Bind `v` to the integer binary operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_int_binary_op(&mut self, v: IntBinaryOpVar, op: IntBinaryOp) -> bool {
        if let Some(&existing) = self.int_binary_ops.get(&v) {
            existing == op
        } else {
            self.int_binary_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`IntBinaryOp`] bound to `v`, or `None` if unbound.
    pub fn get_int_binary_op(&self, v: IntBinaryOpVar) -> Option<IntBinaryOp> {
        self.int_binary_ops.get(&v).copied()
    }

    /// Bind `v` to the integer unary operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_int_unary_op(&mut self, v: IntUnaryOpVar, op: IntUnaryOp) -> bool {
        if let Some(&existing) = self.int_unary_ops.get(&v) {
            existing == op
        } else {
            self.int_unary_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`IntUnaryOp`] bound to `v`, or `None` if unbound.
    pub fn get_int_unary_op(&self, v: IntUnaryOpVar) -> Option<IntUnaryOp> {
        self.int_unary_ops.get(&v).copied()
    }

    /// Bind `v` to the integer comparison operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_int_cmp_op(&mut self, v: IntCmpOpVar, op: IntCmpOp) -> bool {
        if let Some(&existing) = self.int_cmp_ops.get(&v) {
            existing == op
        } else {
            self.int_cmp_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`IntCmpOp`] bound to `v`, or `None` if unbound.
    pub fn get_int_cmp_op(&self, v: IntCmpOpVar) -> Option<IntCmpOp> {
        self.int_cmp_ops.get(&v).copied()
    }

    /// Bind `v` to the boolean binary operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_bool_binary_op(&mut self, v: BoolBinaryOpVar, op: BoolBinaryOp) -> bool {
        if let Some(&existing) = self.bool_binary_ops.get(&v) {
            existing == op
        } else {
            self.bool_binary_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`BoolBinaryOp`] bound to `v`, or `None` if unbound.
    pub fn get_bool_binary_op(&self, v: BoolBinaryOpVar) -> Option<BoolBinaryOp> {
        self.bool_binary_ops.get(&v).copied()
    }

    /// Bind `v` to the boolean unary operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_bool_unary_op(&mut self, v: BoolUnaryOpVar, op: BoolUnaryOp) -> bool {
        if let Some(&existing) = self.bool_unary_ops.get(&v) {
            existing == op
        } else {
            self.bool_unary_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`BoolUnaryOp`] bound to `v`, or `None` if unbound.
    pub fn get_bool_unary_op(&self, v: BoolUnaryOpVar) -> Option<BoolUnaryOp> {
        self.bool_unary_ops.get(&v).copied()
    }

    /// Bind `v` to the float binary operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_float_binary_op(&mut self, v: FloatBinaryOpVar, op: FloatBinaryOp) -> bool {
        if let Some(&existing) = self.float_binary_ops.get(&v) {
            existing == op
        } else {
            self.float_binary_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`FloatBinaryOp`] bound to `v`, or `None` if unbound.
    pub fn get_float_binary_op(&self, v: FloatBinaryOpVar) -> Option<FloatBinaryOp> {
        self.float_binary_ops.get(&v).copied()
    }

    /// Bind `v` to the float unary operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_float_unary_op(&mut self, v: FloatUnaryOpVar, op: FloatUnaryOp) -> bool {
        if let Some(&existing) = self.float_unary_ops.get(&v) {
            existing == op
        } else {
            self.float_unary_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`FloatUnaryOp`] bound to `v`, or `None` if unbound.
    pub fn get_float_unary_op(&self, v: FloatUnaryOpVar) -> Option<FloatUnaryOp> {
        self.float_unary_ops.get(&v).copied()
    }

    /// Bind `v` to the float comparison operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_float_cmp_op(&mut self, v: FloatCmpOpVar, op: FloatCmpOp) -> bool {
        if let Some(&existing) = self.float_cmp_ops.get(&v) {
            existing == op
        } else {
            self.float_cmp_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`FloatCmpOp`] bound to `v`, or `None` if unbound.
    pub fn get_float_cmp_op(&self, v: FloatCmpOpVar) -> Option<FloatCmpOp> {
        self.float_cmp_ops.get(&v).copied()
    }
}

// ── Match ─────────────────────────────────────────────────────────────────────

/// The result of a successful pattern match against a single root node.
///
/// Provides access to the captured variable bindings and convenience helpers
/// for reading constant values.
pub struct Match {
    /// The root node where the top-level pattern matched.
    pub root: NodeId,
    bindings: Bindings,
}

impl Match {
    /// Returns the `NodeOutputId` bound to the data-capture variable `v`,
    /// or `None` if `v` was not captured in this match.
    pub fn get(&self, v: Var) -> Option<NodeOutputId> {
        self.bindings.get(v)
    }

    /// Returns the `NodeId` bound to the control-capture variable `nv`,
    /// or `None` if `nv` was not captured in this match.
    pub fn get_node(&self, nv: NodeVar) -> Option<NodeId> {
        self.bindings.get_node(nv)
    }

    /// Returns the integer constant value bound to the [`IntVar`] `iv`, or
    /// `None` if `iv` was not captured in this match.
    pub fn get_int(&self, iv: IntVar) -> Option<u64> {
        self.bindings.get_int(iv)
    }

    /// Returns the boolean constant value bound to the [`BoolVar`] `bv`, or
    /// `None` if `bv` was not captured in this match.
    pub fn get_bool(&self, bv: BoolVar) -> Option<bool> {
        self.bindings.get_bool(bv)
    }

    /// Returns the float constant IEEE 754 bit pattern bound to the [`FloatVar`]
    /// `fv`, or `None` if `fv` was not captured in this match.
    ///
    /// Named `get_float` (not `get_float_bits`) to avoid colliding with the
    /// graph-lookup helper [`Match::get_float_bits`] which takes a [`Var`] and a
    /// graph reference.
    pub fn get_float(&self, fv: FloatVar) -> Option<u64> {
        self.bindings.get_float_bits(fv)
    }

    /// Returns the [`IntBinaryOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_int_binary_op(&self, v: IntBinaryOpVar) -> Option<IntBinaryOp> {
        self.bindings.get_int_binary_op(v)
    }

    /// Returns the [`IntUnaryOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_int_unary_op(&self, v: IntUnaryOpVar) -> Option<IntUnaryOp> {
        self.bindings.get_int_unary_op(v)
    }

    /// Returns the [`IntCmpOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_int_cmp_op(&self, v: IntCmpOpVar) -> Option<IntCmpOp> {
        self.bindings.get_int_cmp_op(v)
    }

    /// Returns the [`BoolBinaryOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_bool_binary_op(&self, v: BoolBinaryOpVar) -> Option<BoolBinaryOp> {
        self.bindings.get_bool_binary_op(v)
    }

    /// Returns the [`BoolUnaryOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_bool_unary_op(&self, v: BoolUnaryOpVar) -> Option<BoolUnaryOp> {
        self.bindings.get_bool_unary_op(v)
    }

    /// Returns the [`FloatBinaryOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_float_binary_op(&self, v: FloatBinaryOpVar) -> Option<FloatBinaryOp> {
        self.bindings.get_float_binary_op(v)
    }

    /// Returns the [`FloatUnaryOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_float_unary_op(&self, v: FloatUnaryOpVar) -> Option<FloatUnaryOp> {
        self.bindings.get_float_unary_op(v)
    }

    /// Returns the [`FloatCmpOp`] variant bound to `v`, or `None` if unbound.
    pub fn get_float_cmp_op(&self, v: FloatCmpOpVar) -> Option<FloatCmpOp> {
        self.bindings.get_float_cmp_op(v)
    }

    /// Returns an owned copy of the full [`Bindings`] captured by this match.
    ///
    /// Useful when a caller needs to keep the bindings alive past the match —
    /// e.g. the rewrite-rule engine drops the [`Matcher`] borrow before
    /// constructing fresh graph nodes, so it needs an owned snapshot of the
    /// captures to consult while mutating the graph.
    pub fn bindings_clone(&self) -> Bindings {
        self.bindings.clone()
    }

    /// If the output bound to `v` was produced by an `IntConst` node, returns
    /// the stored constant value.  Returns `None` for unbound vars or non-const
    /// outputs.
    pub fn get_int_const(&self, v: Var, graph: &BuiltFunctionGraph) -> Option<u64> {
        let out = self.bindings.get(v)?;
        let node = graph.graph.get_node_from_output(out);
        match graph.graph.node_kind(node) {
            NodeKind::IntConst(val) => Some(*val),
            _ => None,
        }
    }

    /// If the output bound to `v` was produced by a `BoolConst` node, returns
    /// the constant value.  Returns `None` for unbound vars or non-bool-const
    /// outputs.
    pub fn get_bool_const(&self, v: Var, graph: &BuiltFunctionGraph) -> Option<bool> {
        let out = self.bindings.get(v)?;
        let node = graph.graph.get_node_from_output(out);
        match graph.graph.node_kind(node) {
            NodeKind::BoolConst(val) => Some(*val),
            _ => None,
        }
    }

    /// If the output bound to `v` was produced by a `FloatConst` node, returns
    /// the raw IEEE 754 bit pattern stored as `u64`.  Returns `None` for
    /// unbound vars or non-float-const outputs.
    pub fn get_float_bits(&self, v: Var, graph: &BuiltFunctionGraph) -> Option<u64> {
        let out = self.bindings.get(v)?;
        let node = graph.graph.get_node_from_output(out);
        match graph.graph.node_kind(node) {
            NodeKind::FloatConst(bits) => Some(*bits),
            _ => None,
        }
    }
}

// ── Matcher ───────────────────────────────────────────────────────────────────

/// Precomputed per-kind `NodeId` lists used by [`Matcher::find_all`] to skip
/// the full-graph scan when the pattern root is `Call`, `CallOther`, `Return`,
/// or `If`.
///
/// Built lazily on the first `find_all` call; `match_at` never needs it.
struct NodeIndex {
    call_nodes: Vec<NodeId>,
    call_other_nodes: Vec<NodeId>,
    return_nodes: Vec<NodeId>,
    if_nodes: Vec<NodeId>,
    all_nodes: Vec<NodeId>,
}

/// Executes pattern queries against a [`BuiltFunctionGraph`].
///
/// Construction is O(1): the per-kind node indices used by
/// [`Matcher::find_all`] are populated lazily on first use.  Consumers that
/// only call [`Matcher::match_at`] (e.g. `rewrite_rule`) never pay the
/// indexing cost.
pub struct Matcher<'g> {
    fn_graph: &'g BuiltFunctionGraph,
    index: std::cell::OnceCell<NodeIndex>,
}

impl<'g> Matcher<'g> {
    /// Creates a new `Matcher`.  O(1); index construction is deferred until
    /// the first [`Matcher::find_all`] call.
    pub fn new(fn_graph: &'g BuiltFunctionGraph) -> Self {
        Self {
            fn_graph,
            index: std::cell::OnceCell::new(),
        }
    }

    /// Returns the lazily-built node index, constructing it on first access.
    fn index(&self) -> &NodeIndex {
        self.index.get_or_init(|| {
            let mut call_nodes = Vec::new();
            let mut call_other_nodes = Vec::new();
            let mut return_nodes = Vec::new();
            let mut if_nodes = Vec::new();
            let mut all_nodes = Vec::new();

            for node in self.fn_graph.preorder() {
                all_nodes.push(node);
                match self.fn_graph.graph.node_kind(node) {
                    NodeKind::Call => call_nodes.push(node),
                    NodeKind::CallOther { .. } => call_other_nodes.push(node),
                    NodeKind::Return => return_nodes.push(node),
                    NodeKind::If => if_nodes.push(node),
                    _ => {}
                }
            }

            NodeIndex {
                call_nodes,
                call_other_nodes,
                return_nodes,
                if_nodes,
                all_nodes,
            }
        })
    }

    /// Finds all nodes in the graph where `pat` matches and returns a [`Match`]
    /// for each.
    ///
    /// The search is exhaustive: every node is tried as a potential root.
    /// Top-level `Call`, `Return`, and `If` patterns use the pre-indexed node
    /// lists (built lazily on first call) and skip the others.
    pub fn find_all(&self, pat: &Pat) -> Vec<Match> {
        let idx = self.index();
        let candidates: &[NodeId] = match pat.inner() {
            PatKind::Call { .. } => &idx.call_nodes,
            PatKind::CallOther { .. } => &idx.call_other_nodes,
            PatKind::Return { .. } => &idx.return_nodes,
            PatKind::If { .. } => &idx.if_nodes,
            _ => &idx.all_nodes,
        };

        candidates
            .iter()
            .filter_map(|&node| {
                let mut bindings = Bindings::default();
                if self.match_node_id(node, pat, &mut bindings) {
                    Some(Match {
                        root: node,
                        bindings,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Try to match `pat` against the subgraph rooted at `node`.  Returns the
    /// successful [`Match`] (with bindings) if the match succeeds, `None`
    /// otherwise.
    ///
    /// Unlike [`find_all`] which iterates every candidate root, this checks a
    /// single root.  Used by [`crate::build::rewrite_rule`] and other callers
    /// that already know the candidate.
    pub fn match_at(&self, node: NodeId, pat: &Pat) -> Option<Match> {
        let mut bindings = Bindings::default();
        if self.match_node_id(node, pat, &mut bindings) {
            Some(Match { root: node, bindings })
        } else {
            None
        }
    }

    // ── match_unary_op / match_binary_op helpers ──────────────────────────────

    /// Check that `node` satisfies `kind_ok`, fetch its single input, and
    /// recurse on `operand`.  Returns `false` (with bindings unchanged) if the
    /// kind check or input-count check fails; otherwise propagates the result of
    /// `match_output`.
    fn match_unary_op<F>(
        &self,
        node: NodeId,
        operand: &Pat,
        bindings: &mut Bindings,
        kind_ok: F,
    ) -> bool
    where
        F: FnOnce(&NodeKind) -> bool,
    {
        if !kind_ok(self.fn_graph.graph.node_kind(node)) {
            return false;
        }
        let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else {
            return false;
        };
        let snap = bindings.clone();
        if self.match_output(inp, operand, bindings) {
            true
        } else {
            *bindings = snap;
            false
        }
    }

    /// Check that `node` satisfies `kind_ok`, fetch its two inputs, and try
    /// matching `lhs`/`rhs` in order.  Backtracks on failure.
    fn match_binary_op<F>(
        &self,
        node: NodeId,
        lhs: &Pat,
        rhs: &Pat,
        bindings: &mut Bindings,
        kind_ok: F,
    ) -> bool
    where
        F: FnOnce(&NodeKind) -> bool,
    {
        if !kind_ok(self.fn_graph.graph.node_kind(node)) {
            return false;
        }
        let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else {
            return false;
        };
        let snap = bindings.clone();
        if self.match_output(l, lhs, bindings) && self.match_output(r, rhs, bindings) {
            true
        } else {
            *bindings = snap;
            false
        }
    }

    // ── match_output ──────────────────────────────────────────────────────────

    /// Match a `NodeOutputId` (data edge) against a pattern.
    ///
    /// Returns `true` and updates `bindings` on success.  On failure returns
    /// `false`; the caller is responsible for restoring `bindings` if needed.
    fn match_output(&self, output: NodeOutputId, pat: &Pat, bindings: &mut Bindings) -> bool {
        let node = self.fn_graph.graph.get_node_from_output(output);
        let kind = self.fn_graph.graph.node_kind(node);

        match pat.inner() {
            PatKind::Any => true,

            PatKind::Capture(v) => bindings.bind_var(*v, output),

            PatKind::IntConst(c) => matches!(kind, NodeKind::IntConst(v) if *v == *c),

            PatKind::BoolConst(c) => matches!(kind, NodeKind::BoolConst(v) if *v == *c),

            PatKind::AnyIntConst(v) => {
                if !matches!(kind, NodeKind::IntConst(_)) {
                    return false;
                }
                bindings.bind_var(*v, output)
            }

            PatKind::AnyBoolConst(v) => {
                if !matches!(kind, NodeKind::BoolConst(_)) {
                    return false;
                }
                bindings.bind_var(*v, output)
            }

            PatKind::AnyIntConstTyped(iv) => {
                let NodeKind::IntConst(val) = kind else {
                    return false;
                };
                bindings.bind_int(*iv, *val)
            }

            PatKind::AnyBoolConstTyped(bv) => {
                let NodeKind::BoolConst(val) = kind else {
                    return false;
                };
                bindings.bind_bool(*bv, *val)
            }

            PatKind::AnyFloatConstTyped(fv) => {
                let NodeKind::FloatConst(bits) = kind else {
                    return false;
                };
                bindings.bind_float(*fv, *bits)
            }

            PatKind::IntBinaryOp {
                op,
                lhs,
                rhs,
                ordered,
            } => {
                let NodeKind::IntBinaryOp(actual) = kind else {
                    return false;
                };
                if actual != op {
                    return false;
                }
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(l, lhs, bindings) && self.match_output(r, rhs, bindings) {
                    return true;
                }
                if !ordered && is_commutative_int_op(*op) {
                    *bindings = snap.clone();
                    if self.match_output(r, lhs, bindings) && self.match_output(l, rhs, bindings) {
                        return true;
                    }
                }
                *bindings = snap;
                false
            }

            PatKind::IntUnaryOp { op, operand } => {
                let NodeKind::IntUnaryOp(actual) = kind else {
                    return false;
                };
                if actual != op {
                    return false;
                }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) {
                    true
                } else {
                    *bindings = snap;
                    false
                }
            }

            PatKind::IntCmpOp { op, lhs, rhs, ordered } => {
                let NodeKind::IntCmpOp(actual) = kind else {
                    return false;
                };
                if actual != op {
                    return false;
                }
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(l, lhs, bindings) && self.match_output(r, rhs, bindings) {
                    return true;
                }
                if !ordered && is_commutative_int_cmp_op(*op) {
                    *bindings = snap.clone();
                    if self.match_output(r, lhs, bindings) && self.match_output(l, rhs, bindings) {
                        return true;
                    }
                }
                *bindings = snap;
                false
            }

            PatKind::BoolBinaryOp {
                op,
                lhs,
                rhs,
                ordered,
            } => {
                let NodeKind::BoolBinaryOp(actual) = kind else {
                    return false;
                };
                if actual != op {
                    return false;
                }
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(l, lhs, bindings) && self.match_output(r, rhs, bindings) {
                    return true;
                }
                if !ordered && is_commutative_bool_op(*op) {
                    *bindings = snap.clone();
                    if self.match_output(r, lhs, bindings) && self.match_output(l, rhs, bindings) {
                        return true;
                    }
                }
                *bindings = snap;
                false
            }

            PatKind::BoolUnaryOp { op, operand } => {
                let NodeKind::BoolUnaryOp(actual) = kind else {
                    return false;
                };
                if actual != op {
                    return false;
                }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) {
                    true
                } else {
                    *bindings = snap;
                    false
                }
            }

            PatKind::IntBinaryAny {
                op: op_var,
                lhs,
                rhs,
                ordered,
            } => {
                let NodeKind::IntBinaryOp(actual_op) = kind else {
                    return false;
                };
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(l, lhs, bindings)
                    && self.match_output(r, rhs, bindings)
                    && bindings.bind_int_binary_op(*op_var, *actual_op)
                {
                    return true;
                }
                if !ordered && is_commutative_int_op(*actual_op) {
                    *bindings = snap.clone();
                    if self.match_output(r, lhs, bindings)
                        && self.match_output(l, rhs, bindings)
                        && bindings.bind_int_binary_op(*op_var, *actual_op)
                    {
                        return true;
                    }
                }
                *bindings = snap;
                false
            }

            PatKind::IntUnaryAny { op: op_var, operand } => {
                let NodeKind::IntUnaryOp(actual_op) = kind else {
                    return false;
                };
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings)
                    && bindings.bind_int_unary_op(*op_var, *actual_op)
                {
                    return true;
                }
                *bindings = snap;
                false
            }

            PatKind::IntCmpAny {
                op: op_var,
                lhs,
                rhs,
                ordered,
            } => {
                let NodeKind::IntCmpOp(actual_op) = kind else {
                    return false;
                };
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(l, lhs, bindings)
                    && self.match_output(r, rhs, bindings)
                    && bindings.bind_int_cmp_op(*op_var, *actual_op)
                {
                    return true;
                }
                if !ordered && is_commutative_int_cmp_op(*actual_op) {
                    *bindings = snap.clone();
                    if self.match_output(r, lhs, bindings)
                        && self.match_output(l, rhs, bindings)
                        && bindings.bind_int_cmp_op(*op_var, *actual_op)
                    {
                        return true;
                    }
                }
                *bindings = snap;
                false
            }

            PatKind::BoolBinaryAny {
                op: op_var,
                lhs,
                rhs,
                ordered,
            } => {
                let NodeKind::BoolBinaryOp(actual_op) = kind else {
                    return false;
                };
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(l, lhs, bindings)
                    && self.match_output(r, rhs, bindings)
                    && bindings.bind_bool_binary_op(*op_var, *actual_op)
                {
                    return true;
                }
                if !ordered && is_commutative_bool_op(*actual_op) {
                    *bindings = snap.clone();
                    if self.match_output(r, lhs, bindings)
                        && self.match_output(l, rhs, bindings)
                        && bindings.bind_bool_binary_op(*op_var, *actual_op)
                    {
                        return true;
                    }
                }
                *bindings = snap;
                false
            }

            PatKind::BoolUnaryAny { op: op_var, operand } => {
                let NodeKind::BoolUnaryOp(actual_op) = kind else {
                    return false;
                };
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings)
                    && bindings.bind_bool_unary_op(*op_var, *actual_op)
                {
                    return true;
                }
                *bindings = snap;
                false
            }

            PatKind::FloatBinaryAny {
                op: op_var,
                lhs,
                rhs,
                ordered,
            } => {
                let NodeKind::FloatBinaryOp(actual_op) = kind else {
                    return false;
                };
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(l, lhs, bindings)
                    && self.match_output(r, rhs, bindings)
                    && bindings.bind_float_binary_op(*op_var, *actual_op)
                {
                    return true;
                }
                if !ordered && is_commutative_float_op(*actual_op) {
                    *bindings = snap.clone();
                    if self.match_output(r, lhs, bindings)
                        && self.match_output(l, rhs, bindings)
                        && bindings.bind_float_binary_op(*op_var, *actual_op)
                    {
                        return true;
                    }
                }
                *bindings = snap;
                false
            }

            PatKind::FloatUnaryAny { op: op_var, operand } => {
                let NodeKind::FloatUnaryOp(actual_op) = kind else {
                    return false;
                };
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings)
                    && bindings.bind_float_unary_op(*op_var, *actual_op)
                {
                    return true;
                }
                *bindings = snap;
                false
            }

            PatKind::FloatCmpAny {
                op: op_var,
                lhs,
                rhs,
                ordered: _,
            } => {
                // No float comparison operators are commutative in the existing
                // helpers, so the `ordered` flag has no effect here — the swap
                // path is never taken.  The field is retained for API symmetry
                // with the other binary-any variants.
                let NodeKind::FloatCmpOp(actual_op) = kind else {
                    return false;
                };
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(l, lhs, bindings)
                    && self.match_output(r, rhs, bindings)
                    && bindings.bind_float_cmp_op(*op_var, *actual_op)
                {
                    return true;
                }
                *bindings = snap;
                false
            }

            PatKind::CastToBool { operand } =>
                self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::CastToBool)),

            PatKind::CastToInt { operand } =>
                self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::CastToInt)),

            PatKind::Truncate { operand } =>
                self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::Truncate)),

            PatKind::Popcount { operand } =>
                self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::Popcount)),

            PatKind::Lzcount { operand } =>
                self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::Lzcount)),

            PatKind::Extend { op, operand } =>
                self.match_unary_op(node, operand, bindings,
                    |k| matches!(k, NodeKind::Extend(actual) if actual == op)),

            PatKind::Load {
                space,
                addr,
                output_var,
                node_var,
            } => {
                let NodeKind::Load(actual_space) = kind else {
                    return false;
                };
                if let Some(s) = space
                    && actual_space != s
                {
                    return false;
                }
                let snap = bindings.clone();
                let inputs = self.fn_graph.graph.node_inputs(node);
                if let Some(addr_pat) = addr {
                    let Some(&addr_out) = inputs.get(1) else {
                        *bindings = snap;
                        return false;
                    };
                    if !self.match_output(addr_out, addr_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }
                if let Some(v) = output_var
                    && !bindings.bind_var(*v, output)
                {
                    *bindings = snap;
                    return false;
                }
                if let Some(nv) = node_var
                    && !bindings.bind_node_var(*nv, node)
                {
                    *bindings = snap;
                    return false;
                }
                true
            }

            PatKind::Store {
                space,
                addr,
                data,
                output_var,
                node_var,
            } => {
                let NodeKind::Store(actual_space) = kind else {
                    return false;
                };
                if let Some(s) = space
                    && actual_space != s
                {
                    return false;
                }
                let inputs = self.fn_graph.graph.node_inputs(node);
                let snap = bindings.clone();
                if let Some(addr_pat) = addr {
                    let Some(&addr_out) = inputs.get(1) else {
                        *bindings = snap;
                        return false;
                    };
                    if !self.match_output(addr_out, addr_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }
                if let Some(data_pat) = data {
                    let Some(&data_out) = inputs.get(2) else {
                        *bindings = snap;
                        return false;
                    };
                    if !self.match_output(data_out, data_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }
                if let Some(v) = output_var
                    && !bindings.bind_var(*v, output)
                {
                    *bindings = snap;
                    return false;
                }
                if let Some(nv) = node_var
                    && !bindings.bind_node_var(*nv, node)
                {
                    *bindings = snap;
                    return false;
                }
                true
            }

            PatKind::StackStore {
                space,
                offset,
                data,
                output_var,
                node_var,
            } => {
                let NodeKind::StackStore {
                    space: actual_space,
                    offset: actual_offset,
                } = *kind
                else {
                    return false;
                };
                if let Some(s) = space
                    && actual_space != *s
                {
                    return false;
                }
                if let Some(o) = offset
                    && actual_offset != *o
                {
                    return false;
                }
                let inputs = self.fn_graph.graph.node_inputs(node);
                let snap = bindings.clone();
                if let Some(data_pat) = data {
                    // StackStore inputs = [memory(0), base(1), data(2)].
                    let Some(&data_out) = inputs.get(2) else {
                        *bindings = snap;
                        return false;
                    };
                    if !self.match_output(data_out, data_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }
                if let Some(v) = output_var
                    && !bindings.bind_var(*v, output)
                {
                    *bindings = snap;
                    return false;
                }
                if let Some(nv) = node_var
                    && !bindings.bind_node_var(*nv, node)
                {
                    *bindings = snap;
                    return false;
                }
                true
            }

            PatKind::StackStorePhi {
                space,
                offsets,
                data,
                output_var,
                node_var,
            } => {
                let NodeKind::StackStorePhi {
                    space: actual_space,
                } = *kind
                else {
                    return false;
                };
                if let Some(s) = space
                    && actual_space != *s
                {
                    return false;
                }
                if let Some(expected) = offsets {
                    let mut actual: Vec<i64> = self.fn_graph.graph.stack_phi_offsets(node).to_vec();
                    actual.sort();
                    if &actual != expected {
                        return false;
                    }
                }
                let inputs = self.fn_graph.graph.node_inputs(node);
                let snap = bindings.clone();
                if let Some(data_pat) = data {
                    // StackStorePhi inputs = [phi_token(0), memory(1), data(2)].
                    let Some(&data_out) = inputs.get(2) else {
                        *bindings = snap;
                        return false;
                    };
                    if !self.match_output(data_out, data_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }
                if let Some(v) = output_var
                    && !bindings.bind_var(*v, output)
                {
                    *bindings = snap;
                    return false;
                }
                if let Some(nv) = node_var
                    && !bindings.bind_node_var(*nv, node)
                {
                    *bindings = snap;
                    return false;
                }
                true
            }

            PatKind::Phi {
                vn,
                inputs: slot_pats,
                output_var,
                node_var,
            } => {
                let NodeKind::ControlPhi(actual_vn) = kind else {
                    return false;
                };
                if let Some(v) = vn
                    && actual_vn != v
                {
                    return false;
                }
                let inputs = self.fn_graph.graph.node_inputs(node);
                let snap = bindings.clone();
                for (idx, slot_pat) in slot_pats {
                    let Some(&slot_out) = inputs.get(*idx) else {
                        *bindings = snap;
                        return false;
                    };
                    if !self.match_output(slot_out, slot_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }
                if let Some(v) = output_var
                    && !bindings.bind_var(*v, output)
                {
                    *bindings = snap;
                    return false;
                }
                if let Some(nv) = node_var
                    && !bindings.bind_node_var(*nv, node)
                {
                    *bindings = snap;
                    return false;
                }
                true
            }

            PatKind::InitialVar { vn } => {
                let NodeKind::InitialVar(actual_vn) = kind else {
                    return false;
                };
                if let Some(v) = vn
                    && actual_vn != v
                {
                    return false;
                }
                true
            }

            PatKind::WithCapture { inner, var } => {
                let snap = bindings.clone();
                if !self.match_output(output, inner, bindings) {
                    return false;
                }
                if bindings.bind_var(*var, output) {
                    true
                } else {
                    *bindings = snap;
                    false
                }
            }

            PatKind::WithPredicate { inner, func } => {
                let snap = bindings.clone();
                if !self.match_output(output, inner, bindings) {
                    return false;
                }
                let Some(out_ty) = self.fn_graph.graph.output_kind(output).as_value() else {
                    *bindings = snap;
                    return false;
                };
                if func(self.fn_graph, out_ty, output) {
                    true
                } else {
                    *bindings = snap;
                    false
                }
            }

            PatKind::WithMatchPredicate { inner, func } => {
                let snap = bindings.clone();
                if !self.match_output(output, inner, bindings) {
                    return false;
                }
                let Some(out_ty) = self.fn_graph.graph.output_kind(output).as_value() else {
                    *bindings = snap;
                    return false;
                };
                if func(self.fn_graph, out_ty, bindings) {
                    true
                } else {
                    *bindings = snap;
                    false
                }
            }

            PatKind::FloatConst(c) => matches!(kind, NodeKind::FloatConst(v) if *v == *c),

            PatKind::AnyFloatConst(v) => {
                if !matches!(kind, NodeKind::FloatConst(_)) {
                    return false;
                }
                bindings.bind_var(*v, output)
            }

            PatKind::FloatBinaryOp {
                op,
                lhs,
                rhs,
                ordered,
            } => {
                let NodeKind::FloatBinaryOp(actual) = kind else {
                    return false;
                };
                if actual != op {
                    return false;
                }
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(l, lhs, bindings) && self.match_output(r, rhs, bindings) {
                    return true;
                }
                if !ordered && is_commutative_float_op(*op) {
                    *bindings = snap.clone();
                    if self.match_output(r, lhs, bindings) && self.match_output(l, rhs, bindings) {
                        return true;
                    }
                }
                *bindings = snap;
                false
            }

            PatKind::FloatUnaryOp { op, operand } => {
                let NodeKind::FloatUnaryOp(actual) = kind else {
                    return false;
                };
                if actual != op {
                    return false;
                }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else {
                    return false;
                };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) {
                    true
                } else {
                    *bindings = snap;
                    false
                }
            }

            PatKind::FloatCmpOp { op, lhs, rhs } =>
                self.match_binary_op(node, lhs, rhs, bindings,
                    |k| matches!(k, NodeKind::FloatCmpOp(actual) if actual == op)),

            PatKind::IntToFloat { operand } =>
                self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::IntToFloat)),

            PatKind::FloatToInt { operand } =>
                self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::FloatToInt)),

            PatKind::FloatToFloat { operand } =>
                self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::FloatToFloat)),

            PatKind::IntBitsToFloat { operand } =>
                self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::IntBitsToFloat)),

            PatKind::FloatBitsToInt { operand } =>
                self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::FloatBitsToInt)),

            PatKind::CastToFloat { operand } =>
                self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::CastToFloat)),

            // Control-level patterns in a data context → no match.
            PatKind::Call { .. }
            | PatKind::CallOther { .. }
            | PatKind::Return { .. }
            | PatKind::If { .. }
            | PatKind::Contains(_) => false,
        }
    }

    // ── match_node_id ─────────────────────────────────────────────────────────

    /// Match a `NodeId` (control-level node) against a pattern.
    fn match_node_id(&self, node: NodeId, pat: &Pat, bindings: &mut Bindings) -> bool {
        let kind = self.fn_graph.graph.node_kind(node);
        let inputs: Vec<NodeOutputId> = self.fn_graph.graph.node_inputs(node).into_iter().collect();

        match pat.inner() {
            PatKind::Call {
                target,
                args,
                node_var,
            } => {
                if !matches!(kind, NodeKind::Call) {
                    return false;
                }
                let snap = bindings.clone();

                if let Some(tgt_pat) = target {
                    let Some(&tgt_out) = inputs.get(2) else {
                        *bindings = snap;
                        return false;
                    };
                    if !self.match_output(tgt_out, tgt_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }

                for (idx, arg_pat) in args {
                    let Some(&arg_out) = inputs.get(3 + idx) else {
                        *bindings = snap;
                        return false;
                    };
                    if !self.match_output(arg_out, arg_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }

                if let Some(nv) = node_var
                    && !bindings.bind_node_var(*nv, node)
                {
                    *bindings = snap;
                    return false;
                }
                true
            }

            PatKind::CallOther {
                user_op_id,
                args,
                node_var,
            } => {
                let NodeKind::CallOther {
                    user_op_id: actual_id,
                } = kind
                else {
                    return false;
                };
                if let Some(id) = user_op_id
                    && actual_id != id
                {
                    return false;
                }
                let snap = bindings.clone();

                for (idx, arg_pat) in args {
                    // CallOther inputs: [ctrl(0), mem(1), arg0(2), arg1(3), …]
                    let Some(&arg_out) = inputs.get(2 + idx) else {
                        *bindings = snap;
                        return false;
                    };
                    if !self.match_output(arg_out, arg_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }

                if let Some(nv) = node_var
                    && !bindings.bind_node_var(*nv, node)
                {
                    *bindings = snap;
                    return false;
                }
                true
            }

            PatKind::Return {
                preceded_by,
                ret_vals,
                node_var,
            } => {
                if !matches!(kind, NodeKind::Return) {
                    return false;
                }
                let snap = bindings.clone();

                if let Some(call_pat) = preceded_by {
                    let Some(&ctrl_in) = inputs.first() else {
                        *bindings = snap;
                        return false;
                    };
                    if !self.preceded_by_search(ctrl_in, call_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }

                // Return inputs: [ctrl(0), retval0(1), retval1(2), …]
                // There is no memory edge on Return — only ctrl then the return values.
                for (idx, rv_pat) in ret_vals {
                    let Some(&rv_out) = inputs.get(1 + idx) else {
                        *bindings = snap;
                        return false;
                    };
                    if !self.match_output(rv_out, rv_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }

                if let Some(nv) = node_var
                    && !bindings.bind_node_var(*nv, node)
                {
                    *bindings = snap;
                    return false;
                }
                true
            }

            PatKind::If {
                cond,
                true_branch,
                false_branch,
                node_var,
            } => {
                if !matches!(kind, NodeKind::If) {
                    return false;
                }
                let snap = bindings.clone();

                if let Some(cond_pat) = cond {
                    let Some(&cond_out) = inputs.get(1) else {
                        *bindings = snap;
                        return false;
                    };
                    if !self.match_output(cond_out, cond_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }

                let outputs = self.fn_graph.graph.node_outputs(node);

                if let Some(tb_pat) = true_branch {
                    let Some(&true_ctrl) = outputs.get(0) else {
                        *bindings = snap;
                        return false;
                    };
                    let mut visited = HashSet::new();
                    if !self.match_contains(true_ctrl, tb_pat, bindings, &mut visited) {
                        *bindings = snap;
                        return false;
                    }
                }

                if let Some(fb_pat) = false_branch {
                    let Some(&false_ctrl) = outputs.get(1) else {
                        *bindings = snap;
                        return false;
                    };
                    let mut visited = HashSet::new();
                    if !self.match_contains(false_ctrl, fb_pat, bindings, &mut visited) {
                        *bindings = snap;
                        return false;
                    }
                }

                if let Some(nv) = node_var
                    && !bindings.bind_node_var(*nv, node)
                {
                    *bindings = snap;
                    return false;
                }
                true
            }

            // For all other patterns try every output of the node.
            //
            // We must try *all* output kinds, not just value outputs, so that
            // nodes like `Store` (whose only output is a Memory edge) or
            // `ControlPhi` (whose output is a ControlPhi edge) can
            // be matched from top-level `find_all` queries.
            //
            // `match_output` does its own kind-check, so trying a wrong output
            // (e.g. a Control edge against an IntBinaryOp pattern) just
            // returns `false` cleanly.
            _ => {
                for out in self.fn_graph.graph.node_outputs(node).into_iter() {
                    let snap = bindings.clone();
                    if self.match_output(out, pat, bindings) {
                        return true;
                    }
                    *bindings = snap;
                }
                false
            }
        }
    }

    // ── match_contains ────────────────────────────────────────────────────────

    /// Forward walk along a ctrl chain.  Tries `inner_pat` against each node
    /// encountered until a match is found or the chain ends.
    fn match_contains(
        &self,
        ctrl_output: NodeOutputId,
        inner_pat: &Pat,
        bindings: &mut Bindings,
        visited: &mut HashSet<NodeId>,
    ) -> bool {
        // Peel a `Contains` shell — this function *is* the forward search, so
        // the wrapper adds no extra semantics here.
        let inner_pat = match inner_pat.inner() {
            PatKind::Contains(p) => p,
            _ => inner_pat,
        };

        let consumers: Vec<(NodeId, u32)> = self.fn_graph.graph.output_uses(ctrl_output).collect();

        for (consumer, _) in consumers {
            if !visited.insert(consumer) {
                continue;
            }

            // Try to match here.
            let snap = bindings.clone();
            if self.match_node_id(consumer, inner_pat, bindings) {
                return true;
            }
            *bindings = snap;

            // Continue forward through transparent nodes.
            match self.fn_graph.graph.node_kind(consumer) {
                NodeKind::ControlState | NodeKind::IfCase(_) => {
                    if let Some(next_ctrl) = self.first_ctrl_output(consumer)
                        && self.match_contains(next_ctrl, inner_pat, bindings, visited)
                    {
                        return true;
                    }
                }
                NodeKind::Call => {
                    // Continue past the call.
                    if let Some(next_ctrl) = self.first_ctrl_output(consumer)
                        && self.match_contains(next_ctrl, inner_pat, bindings, visited)
                    {
                        return true;
                    }
                }
                // If / Return are terminating — don't cross them.
                _ => {}
            }
        }
        false
    }

    // ── preceded_by_search ────────────────────────────────────────────────────

    /// Backward walk from a ctrl input to find the preceding Call node.
    fn preceded_by_search(
        &self,
        ctrl_output: NodeOutputId,
        call_pat: &Pat,
        bindings: &mut Bindings,
    ) -> bool {
        let producing = self.fn_graph.graph.get_node_from_output(ctrl_output);

        match self.fn_graph.graph.node_kind(producing) {
            NodeKind::Call => {
                let snap = bindings.clone();
                if self.match_node_id(producing, call_pat, bindings) {
                    true
                } else {
                    // This call did not match — keep walking backwards through
                    // its own ctrl input so earlier calls in a sequence can
                    // still be found.
                    *bindings = snap;
                    let call_inputs: Vec<NodeOutputId> = self
                        .fn_graph
                        .graph
                        .node_inputs(producing)
                        .into_iter()
                        .collect();
                    if let Some(&prev_ctrl) = call_inputs.first() {
                        self.preceded_by_search(prev_ctrl, call_pat, bindings)
                    } else {
                        false
                    }
                }
            }
            NodeKind::ControlState => {
                // Try each predecessor ctrl edge.
                let preds: Vec<NodeOutputId> = self
                    .fn_graph
                    .graph
                    .node_inputs(producing)
                    .into_iter()
                    .collect();
                for pred_ctrl in preds {
                    let snap = bindings.clone();
                    if self.preceded_by_search(pred_ctrl, call_pat, bindings) {
                        return true;
                    }
                    *bindings = snap;
                }
                false
            }
            _ => false,
        }
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    fn first_ctrl_output(&self, node: NodeId) -> Option<NodeOutputId> {
        self.fn_graph
            .graph
            .node_outputs(node)
            .into_iter()
            .find(|&o| self.fn_graph.graph.output_kind(o) == NodeOutputKind::Control)
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::var::{BoolVar, FloatVar, IntVar};

    // ── IntVar ────────────────────────────────────────────────────────────────

    #[test]
    fn int_var_bind_and_get() {
        let mut b = Bindings::default();
        let iv = IntVar::new();

        // Unbound → None.
        assert_eq!(b.get_int(iv), None);

        // First bind succeeds.
        assert!(b.bind_int(iv, 42));
        assert_eq!(b.get_int(iv), Some(42));
    }

    #[test]
    fn int_var_idempotent_rebind() {
        let mut b = Bindings::default();
        let iv = IntVar::new();
        assert!(b.bind_int(iv, 42));
        // Rebinding to the same value is OK.
        assert!(b.bind_int(iv, 42));
        assert_eq!(b.get_int(iv), Some(42));
    }

    #[test]
    fn int_var_conflict_fails() {
        let mut b = Bindings::default();
        let iv = IntVar::new();
        assert!(b.bind_int(iv, 42));
        // Rebinding to a different value fails.
        assert!(!b.bind_int(iv, 43));
        // The original binding is preserved after a conflict.
        assert_eq!(b.get_int(iv), Some(42));
    }

    // ── BoolVar ───────────────────────────────────────────────────────────────

    #[test]
    fn bool_var_bind_and_get() {
        let mut b = Bindings::default();
        let bv = BoolVar::new();

        assert_eq!(b.get_bool(bv), None);

        assert!(b.bind_bool(bv, true));
        assert_eq!(b.get_bool(bv), Some(true));
    }

    #[test]
    fn bool_var_idempotent_rebind() {
        let mut b = Bindings::default();
        let bv = BoolVar::new();
        assert!(b.bind_bool(bv, false));
        assert!(b.bind_bool(bv, false));
        assert_eq!(b.get_bool(bv), Some(false));
    }

    #[test]
    fn bool_var_conflict_fails() {
        let mut b = Bindings::default();
        let bv = BoolVar::new();
        assert!(b.bind_bool(bv, true));
        assert!(!b.bind_bool(bv, false));
        assert_eq!(b.get_bool(bv), Some(true));
    }

    // ── FloatVar ──────────────────────────────────────────────────────────────

    #[test]
    fn float_var_bind_and_get() {
        let mut b = Bindings::default();
        let fv = FloatVar::new();

        assert_eq!(b.get_float_bits(fv), None);

        // Use the IEEE 754 bit pattern for 1.0f64.
        let bits = 1.0f64.to_bits();
        assert!(b.bind_float(fv, bits));
        assert_eq!(b.get_float_bits(fv), Some(bits));
    }

    #[test]
    fn float_var_idempotent_rebind() {
        let mut b = Bindings::default();
        let fv = FloatVar::new();
        let bits = 2.0f64.to_bits();
        assert!(b.bind_float(fv, bits));
        assert!(b.bind_float(fv, bits));
        assert_eq!(b.get_float_bits(fv), Some(bits));
    }

    #[test]
    fn float_var_conflict_fails() {
        let mut b = Bindings::default();
        let fv = FloatVar::new();
        let bits_a = 1.0f64.to_bits();
        let bits_b = 2.0f64.to_bits();
        assert!(b.bind_float(fv, bits_a));
        assert!(!b.bind_float(fv, bits_b));
        assert_eq!(b.get_float_bits(fv), Some(bits_a));
    }

    // ── IDs are globally unique across types ──────────────────────────────────

    #[test]
    fn capture_ids_are_globally_unique() {
        // Each call to ::new() increments the shared counter.  The only
        // guarantee we need is that two successive calls produce different ids.
        let iv = IntVar::new();
        let bv = BoolVar::new();
        let fv = FloatVar::new();
        let v = Var::new();
        let nv = NodeVar::new();
        // All five must be distinct (compared as their raw u32 inner values by
        // verifying each pair is not identical when cast to the same type as u32).
        // We expose no public field, so we use Debug output as a proxy.
        let ids: Vec<String> = vec![
            format!("{iv:?}"),
            format!("{bv:?}"),
            format!("{fv:?}"),
            format!("{v:?}"),
            format!("{nv:?}"),
        ];
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "all capture IDs must be globally unique");
    }
}
