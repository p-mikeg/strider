//! `impl<R> Pattern for PatGraph<R>` — recursive DAG walk with
//! commutative-operand retry.
//!
//! The matcher visits the pattern graph in pull order rooted at
//! `PatGraph::root`: for each pat node it kind-checks the corresponding
//! IR node, walks each incoming pattern edge against the IR's input at
//! the same `consumer_slot`, then binds the pat node's capture
//! (`Bindings::bind_capture` enforces capture-equality across multiple
//! occurrences) and finally runs any post-match hook.
//!
//! For arity-2 pat nodes whose kind is commutative (per
//! `NodeKind::is_commutative()`) the matcher tries the natural operand
//! order first; on failure it rolls back via `Bindings::mark` /
//! `Bindings::restore` and retries with the operand slots swapped.
//! This mirrors the proven semantics of
//! `strider-analyze::pattern::pat::node_pat::try_match_common`.

use std::mem::Discriminant;

use petgraph::stable_graph::NodeIndex;
use petgraph::visit::EdgeRef;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::capture::{Binding, Bindings};
use crate::matcher::{MatchCtx, Pattern};
use crate::pat_graph::PatGraph;

impl<R> Pattern for PatGraph<R> {
    fn root_kind_discriminant(&self) -> Option<Discriminant<NodeKind>> {
        let root = self.root?;
        self.inner.node_weight(root)?.kind.discriminant()
    }

    fn try_match(
        &self,
        ctx: &MatchCtx,
        root_out: NodeOutputId,
        bindings: &mut Bindings,
    ) -> bool {
        let Some(root) = self.root else {
            return false;
        };
        let root_node = ctx.function.node_for_output(root_out);
        try_match_at(self, root, ctx, root_node, Some(root_out), bindings)
    }

    fn try_match_node(
        &self,
        ctx: &MatchCtx,
        node: NodeId,
        bindings: &mut Bindings,
    ) -> bool {
        // Zero-output entry path: dispatch into `try_match_at` with no
        // `root_out`.  Used for `Return` (no outputs at all) and for
        // any other future zero-output kind a builder targets.
        let Some(root) = self.root else {
            return false;
        };
        try_match_at(self, root, ctx, node, None, bindings)
    }
}

/// Recursive worker.  `pat_node` is the current pattern graph index;
/// `ir_node` is the IR node it's being matched against; `root_out` is
/// the `NodeOutputId` (when the pat node sits at a value-producing
/// position) used to record the capture binding on success.
fn try_match_at<R>(
    pat: &PatGraph<R>,
    pat_node: NodeIndex,
    ctx: &MatchCtx,
    ir_node: NodeId,
    root_out: Option<NodeOutputId>,
    bindings: &mut Bindings,
) -> bool {
    // `pat_node` is always read from `pat.inner`'s own index space, so
    // a missing weight would mean the index was invalidated.  `PatGraph`
    // never calls `remove_node`, so this should be unreachable in
    // practice — treat as a non-match defensively rather than panicking.
    let Some(nd) = pat.inner.node_weight(pat_node) else {
        return false;
    };
    if !nd.kind.matches(ctx.function.node_kind(ir_node)) {
        return false;
    }

    // Collect incoming pattern edges (producers feeding this pat node).
    // Each carries the `consumer_slot` (which IR input position to walk
    // against) plus the producer pat node.  `producer_output_slot` is
    // currently informational — every NodeOutputId already identifies
    // its slot, so we don't need to gate on it here.
    let edges: Vec<(usize, NodeIndex)> = pat
        .inner
        .edges_directed(pat_node, petgraph::Incoming)
        .map(|e| (e.weight().consumer_slot, e.source()))
        .collect();

    // Determine commutativity.  Only meaningful for arity-2 nodes whose
    // kind is concrete enough to ask `NodeKind::is_commutative()` — for
    // a `KindSpec::Any` root we conservatively peek at the IR kind
    // (since the IR kind is the operative one once a match is being
    // attempted).
    let commutative = edges.len() == 2
        && ctx.function.node_kind(ir_node).is_commutative();

    let mark = bindings.mark();

    let attempt = |swap: bool, b: &mut Bindings| -> bool {
        for &(consumer_slot, producer_pat) in &edges {
            // For an arity-2 commutative retry, swap slots 0 and 1.
            // Other slots (if any — shouldn't occur for arity-2) are
            // passed through unchanged.
            let ir_slot = if swap {
                match consumer_slot {
                    0 => 1,
                    1 => 0,
                    other => other,
                }
            } else {
                consumer_slot
            };
            let Ok(input_id) = ctx.function.node_input_id_at(ir_node, ir_slot) else {
                return false;
            };
            let producer_out = ctx.function.input_output_id(input_id);
            let producer_ir = ctx.function.node_for_output(producer_out);
            if !try_match_at(pat, producer_pat, ctx, producer_ir, Some(producer_out), b) {
                return false;
            }
        }
        true
    };

    let inputs_ok = if attempt(false, bindings) {
        true
    } else if commutative {
        bindings.restore(mark);
        attempt(true, bindings)
    } else {
        false
    };
    if !inputs_ok {
        bindings.restore(mark);
        return false;
    }

    // Capture-binding: after children matched, so any shared captures
    // bound deeper have already been recorded — `bind_capture` rejects a
    // re-bind to a different output here, enforcing capture-equality.
    if let Some(cap_ref) = nd.capture {
        let binding = root_out.map_or(Binding::Node(ir_node), Binding::Output);
        if !bindings.bind_capture(cap_ref.capture(), binding) {
            bindings.restore(mark);
            return false;
        }
    }
    if let Some(pm) = &nd.post_match {
        // post_match closure currently has the placeholder shape
        // `Box<dyn Fn() -> bool>` (a stub).  A subsequent task widens
        // the signature to take `MatchCtx` + bindings.
        if !pm() {
            bindings.restore(mark);
            return false;
        }
    }
    true
}
