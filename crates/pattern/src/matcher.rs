use std::collections::{HashMap, HashSet};

use ir::{BoolBinaryOp, FloatBinaryOp, IntBinaryOp};
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};
use ir::BuiltFunctionGraph;

use crate::pat::{Pat, PatKind};
use crate::var::{NodeVar, Var};

// ── Commutativity helpers ─────────────────────────────────────────────────────

fn is_commutative_int_op(op: IntBinaryOp) -> bool {
    matches!(op, IntBinaryOp::Add | IntBinaryOp::Mul
               | IntBinaryOp::And | IntBinaryOp::Or | IntBinaryOp::Xor)
}

fn is_commutative_bool_op(op: BoolBinaryOp) -> bool {
    matches!(op, BoolBinaryOp::And | BoolBinaryOp::Or | BoolBinaryOp::Xor)
}

fn is_commutative_float_op(op: FloatBinaryOp) -> bool {
    matches!(op, FloatBinaryOp::Add | FloatBinaryOp::Mul)
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
    vars:      HashMap<Var,     NodeOutputId>,
    node_vars: HashMap<NodeVar, NodeId>,
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

    /// Returns the `NodeOutputId` bound to `v`, or `None` if unbound.
    pub fn get(&self, v: Var) -> Option<NodeOutputId> {
        self.vars.get(&v).copied()
    }

    /// Returns the `NodeId` bound to `nv`, or `None` if unbound.
    pub fn get_node(&self, nv: NodeVar) -> Option<NodeId> {
        self.node_vars.get(&nv).copied()
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

    /// If the output bound to `v` was produced by an `IntConst` node, returns
    /// the stored constant value.  Returns `None` for unbound vars or non-const
    /// outputs.
    pub fn get_int_const(&self, v: Var, graph: &BuiltFunctionGraph) -> Option<u64> {
        let out  = self.bindings.get(v)?;
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
        let out  = self.bindings.get(v)?;
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
        let out  = self.bindings.get(v)?;
        let node = graph.graph.get_node_from_output(out);
        match graph.graph.node_kind(node) {
            NodeKind::FloatConst(bits) => Some(*bits),
            _ => None,
        }
    }
}

// ── Matcher ───────────────────────────────────────────────────────────────────

/// Executes pattern queries against a [`BuiltFunctionGraph`].
///
/// `Matcher::new` pre-indexes all `Call`, `Return`, and `If` nodes in the
/// graph so that control-level queries can skip the full node list.
pub struct Matcher<'g> {
    fn_graph:     &'g BuiltFunctionGraph,
    call_nodes:   Vec<NodeId>,
    return_nodes: Vec<NodeId>,
    if_nodes:     Vec<NodeId>,
    all_nodes:    Vec<NodeId>,
}

impl<'g> Matcher<'g> {
    /// Creates a new `Matcher` and pre-indexes the graph.
    ///
    /// This does a single preorder traversal over all nodes; subsequent
    /// `find_all` calls pay only the cost of the pattern match itself.
    pub fn new(fn_graph: &'g BuiltFunctionGraph) -> Self {
        let mut call_nodes   = Vec::new();
        let mut return_nodes = Vec::new();
        let mut if_nodes     = Vec::new();
        let mut all_nodes    = Vec::new();

        for node in fn_graph.preorder() {
            all_nodes.push(node);
            match fn_graph.graph.node_kind(node) {
                NodeKind::Call    => call_nodes.push(node),
                NodeKind::Return  => return_nodes.push(node),
                NodeKind::If      => if_nodes.push(node),
                _ => {}
            }
        }

        Self { fn_graph, call_nodes, return_nodes, if_nodes, all_nodes }
    }

    /// Finds all nodes in the graph where `pat` matches and returns a [`Match`]
    /// for each.
    ///
    /// The search is exhaustive: every node is tried as a potential root.
    /// Top-level `Call`, `Return`, and `If` patterns use the pre-indexed node
    /// lists and skip the others.
    pub fn find_all(&self, pat: &Pat) -> Vec<Match> {
        let candidates: &[NodeId] = match pat.inner() {
            PatKind::Call    { .. } => &self.call_nodes,
            PatKind::Return  { .. } => &self.return_nodes,
            PatKind::If      { .. } => &self.if_nodes,
            _                       => &self.all_nodes,
        };

        candidates.iter().filter_map(|&node| {
            let mut bindings = Bindings::default();
            if self.match_node_id(node, pat, &mut bindings) {
                Some(Match { root: node, bindings })
            } else {
                None
            }
        }).collect()
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
                if !matches!(kind, NodeKind::IntConst(_)) { return false; }
                bindings.bind_var(*v, output)
            }

            PatKind::IntBinaryOp { op, lhs, rhs, ordered } => {
                let NodeKind::IntBinaryOp(actual) = kind else { return false; };
                if actual != op { return false; }
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else { return false; };
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
                let NodeKind::IntUnaryOp(actual) = kind else { return false; };
                if actual != op { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::IntCmpOp { op, lhs, rhs } => {
                let NodeKind::IntCmpOp(actual) = kind else { return false; };
                if actual != op { return false; }
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(l, lhs, bindings) && self.match_output(r, rhs, bindings) {
                    true
                } else {
                    *bindings = snap;
                    false
                }
            }

            PatKind::BoolBinaryOp { op, lhs, rhs, ordered } => {
                let NodeKind::BoolBinaryOp(actual) = kind else { return false; };
                if actual != op { return false; }
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else { return false; };
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
                let NodeKind::BoolUnaryOp(actual) = kind else { return false; };
                if actual != op { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::CastToBool { operand } => {
                if !matches!(kind, NodeKind::CastToBool) { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::CastToInt { operand } => {
                if !matches!(kind, NodeKind::CastToInt) { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::Truncate { operand } => {
                if !matches!(kind, NodeKind::Truncate) { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::Popcount { operand } => {
                if !matches!(kind, NodeKind::Popcount) { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::Lzcount { operand } => {
                if !matches!(kind, NodeKind::Lzcount) { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::Piece { hi, lo } => {
                if !matches!(kind, NodeKind::Piece) { return false; }
                let Ok([h, l]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(h, hi, bindings) && self.match_output(l, lo, bindings) {
                    true
                } else {
                    *bindings = snap;
                    false
                }
            }

            PatKind::Extract { lsb: pat_lsb, len: pat_len, operand } => {
                let NodeKind::Extract { lsb, len } = kind else { return false; };
                if pat_lsb.is_some_and(|pl| pl != *lsb) { return false; }
                if pat_len.is_some_and(|pl| pl != *len)  { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::Insert { lsb: pat_lsb, len: pat_len, dest, src } => {
                let NodeKind::Insert { lsb, len } = kind else { return false; };
                if pat_lsb.is_some_and(|pl| pl != *lsb) { return false; }
                if pat_len.is_some_and(|pl| pl != *len)  { return false; }
                let Ok([d, s]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(d, dest, bindings) && self.match_output(s, src, bindings) {
                    true
                } else {
                    *bindings = snap;
                    false
                }
            }

            PatKind::Extend { op, operand } => {
                let NodeKind::Extend(actual) = kind else { return false; };
                if actual != op { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::Load { space, addr, output_var, node_var } => {
                let NodeKind::Load(actual_space) = kind else { return false; };
                if let Some(s) = space {
                    if actual_space != s { return false; }
                }
                let snap = bindings.clone();
                let inputs = self.fn_graph.graph.node_inputs(node);
                if let Some(addr_pat) = addr {
                    let Some(&addr_out) = inputs.get(1) else { *bindings = snap; return false; };
                    if !self.match_output(addr_out, addr_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }
                if let Some(v) = output_var {
                    if !bindings.bind_var(*v, output) { *bindings = snap; return false; }
                }
                if let Some(nv) = node_var {
                    if !bindings.bind_node_var(*nv, node) { *bindings = snap; return false; }
                }
                true
            }

            PatKind::Store { space, addr, data, output_var, node_var } => {
                let NodeKind::Store(actual_space) = kind else { return false; };
                if let Some(s) = space {
                    if actual_space != s { return false; }
                }
                let inputs = self.fn_graph.graph.node_inputs(node);
                let snap = bindings.clone();
                if let Some(addr_pat) = addr {
                    let Some(&addr_out) = inputs.get(1) else { *bindings = snap; return false; };
                    if !self.match_output(addr_out, addr_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }
                if let Some(data_pat) = data {
                    let Some(&data_out) = inputs.get(2) else { *bindings = snap; return false; };
                    if !self.match_output(data_out, data_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }
                if let Some(v) = output_var {
                    if !bindings.bind_var(*v, output) { *bindings = snap; return false; }
                }
                if let Some(nv) = node_var {
                    if !bindings.bind_node_var(*nv, node) { *bindings = snap; return false; }
                }
                true
            }

            PatKind::StackStore { space, offset, data, output_var, node_var } => {
                let NodeKind::StackStore { space: actual_space, offset: actual_offset } = *kind
                    else { return false; };
                if let Some(s) = space {
                    if actual_space != *s { return false; }
                }
                if let Some(o) = offset {
                    if actual_offset != *o { return false; }
                }
                let inputs = self.fn_graph.graph.node_inputs(node);
                let snap = bindings.clone();
                if let Some(data_pat) = data {
                    // StackStore inputs = [memory(0), base(1), data(2)].
                    let Some(&data_out) = inputs.get(2) else { *bindings = snap; return false; };
                    if !self.match_output(data_out, data_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }
                if let Some(v) = output_var {
                    if !bindings.bind_var(*v, output) { *bindings = snap; return false; }
                }
                if let Some(nv) = node_var {
                    if !bindings.bind_node_var(*nv, node) { *bindings = snap; return false; }
                }
                true
            }

            PatKind::StackStorePhi { space, offsets, data, output_var, node_var } => {
                let NodeKind::StackStorePhi { space: actual_space } = *kind
                    else { return false; };
                if let Some(s) = space {
                    if actual_space != *s { return false; }
                }
                if let Some(expected) = offsets {
                    let mut actual: Vec<i64> = self.fn_graph.graph.stack_phi_offsets(node).to_vec();
                    actual.sort();
                    if &actual != expected { return false; }
                }
                let inputs = self.fn_graph.graph.node_inputs(node);
                let snap = bindings.clone();
                if let Some(data_pat) = data {
                    // StackStorePhi inputs = [phi_token(0), memory(1), data(2)].
                    let Some(&data_out) = inputs.get(2) else { *bindings = snap; return false; };
                    if !self.match_output(data_out, data_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }
                if let Some(v) = output_var {
                    if !bindings.bind_var(*v, output) { *bindings = snap; return false; }
                }
                if let Some(nv) = node_var {
                    if !bindings.bind_node_var(*nv, node) { *bindings = snap; return false; }
                }
                true
            }

            PatKind::Phi { vn, inputs: slot_pats, output_var, node_var } => {
                let NodeKind::ControlPhi(actual_vn) = kind else { return false; };
                if let Some(v) = vn {
                    if actual_vn != v { return false; }
                }
                let inputs = self.fn_graph.graph.node_inputs(node);
                let snap = bindings.clone();
                for (idx, slot_pat) in slot_pats {
                    let Some(&slot_out) = inputs.get(*idx) else { *bindings = snap; return false; };
                    if !self.match_output(slot_out, slot_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }
                if let Some(v) = output_var {
                    if !bindings.bind_var(*v, output) { *bindings = snap; return false; }
                }
                if let Some(nv) = node_var {
                    if !bindings.bind_node_var(*nv, node) { *bindings = snap; return false; }
                }
                true
            }

            PatKind::InitialVar { vn } => {
                let NodeKind::InitialVar(actual_vn) = kind else { return false; };
                if let Some(v) = vn {
                    if actual_vn != v { return false; }
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
                if func(self.fn_graph, output) {
                    true
                } else {
                    *bindings = snap;
                    false
                }
            }

            PatKind::FloatConst(c) => matches!(kind, NodeKind::FloatConst(v) if *v == *c),

            PatKind::AnyFloatConst(v) => {
                if !matches!(kind, NodeKind::FloatConst(_)) { return false; }
                bindings.bind_var(*v, output)
            }

            PatKind::FloatBinaryOp { op, lhs, rhs, ordered } => {
                let NodeKind::FloatBinaryOp(actual) = kind else { return false; };
                if actual != op { return false; }
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else { return false; };
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
                let NodeKind::FloatUnaryOp(actual) = kind else { return false; };
                if actual != op { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::FloatCmpOp { op, lhs, rhs } => {
                let NodeKind::FloatCmpOp(actual) = kind else { return false; };
                if actual != op { return false; }
                let Ok([l, r]) = self.fn_graph.graph.node_inputs_exact::<2>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(l, lhs, bindings) && self.match_output(r, rhs, bindings) {
                    true
                } else {
                    *bindings = snap;
                    false
                }
            }

            PatKind::FloatIsNan { operand } => {
                if !matches!(kind, NodeKind::FloatIsNan) { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::IntToFloat { operand } => {
                if !matches!(kind, NodeKind::IntToFloat) { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::FloatToInt { operand } => {
                if !matches!(kind, NodeKind::FloatToInt) { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::FloatToFloat { operand } => {
                if !matches!(kind, NodeKind::FloatToFloat) { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::IntBitsToFloat { operand } => {
                if !matches!(kind, NodeKind::IntBitsToFloat) { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::FloatBitsToInt { operand } => {
                if !matches!(kind, NodeKind::FloatBitsToInt) { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            PatKind::CastToFloat { operand } => {
                if !matches!(kind, NodeKind::CastToFloat) { return false; }
                let Ok([inp]) = self.fn_graph.graph.node_inputs_exact::<1>(node) else { return false; };
                let snap = bindings.clone();
                if self.match_output(inp, operand, bindings) { true } else { *bindings = snap; false }
            }

            // Control-level patterns in a data context → no match.
            PatKind::Call { .. } | PatKind::Return { .. } | PatKind::If { .. }
            | PatKind::Contains(_) => false,
        }
    }

    // ── match_node_id ─────────────────────────────────────────────────────────

    /// Match a `NodeId` (control-level node) against a pattern.
    fn match_node_id(&self, node: NodeId, pat: &Pat, bindings: &mut Bindings) -> bool {
        let kind = self.fn_graph.graph.node_kind(node);
        let inputs: Vec<NodeOutputId> = self.fn_graph.graph.node_inputs(node).into_iter().collect();

        match pat.inner() {
            PatKind::Call { target, args, node_var } => {
                if !matches!(kind, NodeKind::Call) { return false; }
                let snap = bindings.clone();

                if let Some(tgt_pat) = target {
                    let Some(&tgt_out) = inputs.get(2) else { *bindings = snap; return false; };
                    if !self.match_output(tgt_out, tgt_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }

                for (idx, arg_pat) in args {
                    let Some(&arg_out) = inputs.get(3 + idx) else { *bindings = snap; return false; };
                    if !self.match_output(arg_out, arg_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }

                if let Some(nv) = node_var {
                    if !bindings.bind_node_var(*nv, node) { *bindings = snap; return false; }
                }
                true
            }

            PatKind::Return { preceded_by, ret_vals, node_var } => {
                if !matches!(kind, NodeKind::Return) { return false; }
                let snap = bindings.clone();

                if let Some(call_pat) = preceded_by {
                    let Some(&ctrl_in) = inputs.first() else { *bindings = snap; return false; };
                    if !self.preceded_by_search(ctrl_in, call_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }

                // Return inputs: [ctrl(0), retval0(1), retval1(2), …]
                // There is no memory edge on Return — only ctrl then the return values.
                for (idx, rv_pat) in ret_vals {
                    let Some(&rv_out) = inputs.get(1 + idx) else { *bindings = snap; return false; };
                    if !self.match_output(rv_out, rv_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }

                if let Some(nv) = node_var {
                    if !bindings.bind_node_var(*nv, node) { *bindings = snap; return false; }
                }
                true
            }

            PatKind::If { cond, true_branch, false_branch, node_var } => {
                if !matches!(kind, NodeKind::If) { return false; }
                let snap = bindings.clone();

                if let Some(cond_pat) = cond {
                    let Some(&cond_out) = inputs.get(1) else { *bindings = snap; return false; };
                    if !self.match_output(cond_out, cond_pat, bindings) {
                        *bindings = snap;
                        return false;
                    }
                }

                let outputs = self.fn_graph.graph.node_outputs(node);

                if let Some(tb_pat) = true_branch {
                    let Some(&true_ctrl) = outputs.get(0) else { *bindings = snap; return false; };
                    let mut visited = HashSet::new();
                    if !self.match_contains(true_ctrl, tb_pat, bindings, &mut visited) {
                        *bindings = snap;
                        return false;
                    }
                }

                if let Some(fb_pat) = false_branch {
                    let Some(&false_ctrl) = outputs.get(1) else { *bindings = snap; return false; };
                    let mut visited = HashSet::new();
                    if !self.match_contains(false_ctrl, fb_pat, bindings, &mut visited) {
                        *bindings = snap;
                        return false;
                    }
                }

                if let Some(nv) = node_var {
                    if !bindings.bind_node_var(*nv, node) { *bindings = snap; return false; }
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
            if !visited.insert(consumer) { continue; }

            // Try to match here.
            let snap = bindings.clone();
            if self.match_node_id(consumer, inner_pat, bindings) {
                return true;
            }
            *bindings = snap;

            // Continue forward through transparent nodes.
            match self.fn_graph.graph.node_kind(consumer) {
                NodeKind::ControlState | NodeKind::IfCase(_) => {
                    if let Some(next_ctrl) = self.first_ctrl_output(consumer) {
                        if self.match_contains(next_ctrl, inner_pat, bindings, visited) {
                            return true;
                        }
                    }
                }
                NodeKind::Call => {
                    // Continue past the call.
                    if let Some(next_ctrl) = self.first_ctrl_output(consumer) {
                        if self.match_contains(next_ctrl, inner_pat, bindings, visited) {
                            return true;
                        }
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
                    let call_inputs: Vec<NodeOutputId> =
                        self.fn_graph.graph.node_inputs(producing).into_iter().collect();
                    if let Some(&prev_ctrl) = call_inputs.first() {
                        self.preceded_by_search(prev_ctrl, call_pat, bindings)
                    } else {
                        false
                    }
                }
            }
            NodeKind::ControlState => {
                // Try each predecessor ctrl edge.
                let preds: Vec<NodeOutputId> =
                    self.fn_graph.graph.node_inputs(producing).into_iter().collect();
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
        self.fn_graph.graph.node_outputs(node)
            .into_iter()
            .find(|&o| self.fn_graph.graph.output_kind(o) == NodeOutputKind::Control)
    }
}
