use std::collections::HashSet;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};

use crate::pat::{Pat, PatKind};

mod bindings;
mod commutativity;
mod match_result;

#[cfg(test)]
mod tests;

pub use bindings::Bindings;
pub use match_result::Match;

use commutativity::{
    is_commutative_bool_op, is_commutative_float_op, is_commutative_int_cmp_op,
    is_commutative_int_op,
};

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
