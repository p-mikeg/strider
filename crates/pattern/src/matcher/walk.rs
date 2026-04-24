//! One-step-direct walks that skip transparent SSA-join plumbing nodes
//! (`ControlState`).  Semantic nodes (`Call` / `Return` / `If` / `Load` /
//! `Store` / everything else) terminate the walk.
//!
//! The helpers advance exactly one semantic step at a time: transparent SSA
//! plumbing is skipped, but any real node stops the walk so the caller can
//! decide what to do next.  `Call` is treated as a semantic node (the walk
//! stops there), not as transparent.

use ir::node::{NodeId, NodeKind, NodeOutputId};

use super::Matcher;

/// Returns `true` if `kind` is a transparent pass-through control node that
/// the skip-helpers walk through without stopping.  Only `ControlState` is
/// transparent — every other node kind terminates the walk.
fn is_transparent(kind: &NodeKind) -> bool {
    matches!(kind, NodeKind::ControlState)
}

/// Return `node`'s first output, or `None` if the node has no outputs.
///
/// Control-flow nodes conventionally produce `Control` as their output[0];
/// callers use this helper to advance the walk to the "next ctrl edge" out
/// of a node.  Defensive `None` return preserves the project's no-panic
/// discipline — callers propagate the `None` rather than unwrapping.
fn first_output(matcher: &Matcher, node: NodeId) -> Option<NodeOutputId> {
    matcher
        .fn_graph
        .graph
        .node_outputs(node)
        .into_iter()
        .next()
}

/// Bound on the number of transparent hops the skip-helpers will traverse
/// before giving up.  The IR control graph is a DAG in practice, so this is
/// defensive insurance against malformed inputs rather than a normal-case
/// limit.
const MAX_TRANSPARENT_HOPS: usize = 64;

/// Follow a control output forward through transparent consumers until
/// reaching a semantic node.  Returns that node's primary output
/// (its `output[0]`), or `None` if the chain dead-ends.
///
/// The walk dead-ends when:
/// * The current ctrl output has no consumers, or has more than one
///   (ambiguous continuation — we refuse to guess).
/// * The terminating semantic node has no outputs (e.g. `Return`).
/// * The hop counter exhausts (defensive cycle guard).
///
/// Typical use: starting from `If.output[0]` (the true-ctrl edge), advance
/// past any `ControlState` plumbing to the next real node's ctrl output.
pub(crate) fn skip_forward_transparent(
    matcher: &Matcher,
    mut out: NodeOutputId,
) -> Option<NodeOutputId> {
    for _ in 0..MAX_TRANSPARENT_HOPS {
        let consumers: Vec<_> = matcher.fn_graph.graph.output_uses(out).collect();
        // Expect exactly one consumer on a ctrl chain.  Zero consumers is a
        // dead end; multiple consumers means the chain forks and we refuse
        // to pick arbitrarily — callers should use a more specific pattern.
        if consumers.len() != 1 {
            return None;
        }
        let (consumer_node, _input_idx) = consumers[0];
        let kind = matcher.fn_graph.graph.node_kind(consumer_node);
        if !is_transparent(kind) {
            // Reached a semantic node: return its primary output.
            return first_output(matcher, consumer_node);
        }
        // Transparent: advance through this consumer's primary output.
        match first_output(matcher, consumer_node) {
            Some(next) => out = next,
            None => return None,
        }
    }
    None
}

/// Follow a control input backward through transparent producers until
/// reaching a semantic producer.  Returns the output produced by that
/// semantic node (which is `input_out` itself when the immediate producer
/// is already semantic).
///
/// If the producer chain exhausts (missing ctrl input on a transparent
/// producer, or the hop counter runs out) the function returns the last
/// `input_out` it had, which is the safest defensive behaviour — callers
/// then apply their own matcher against that output and will simply fail
/// to match if the chain is malformed.
///
/// Typical use: starting from `Return.input[0]` (the incoming ctrl edge),
/// walk back past any `ControlState` plumbing to the preceding semantic
/// node's ctrl output.
pub(crate) fn skip_backward_transparent(
    matcher: &Matcher,
    mut input_out: NodeOutputId,
) -> NodeOutputId {
    for _ in 0..MAX_TRANSPARENT_HOPS {
        let producer = matcher.fn_graph.graph.get_node_from_output(input_out);
        let kind = matcher.fn_graph.graph.node_kind(producer);
        if !is_transparent(kind) {
            return input_out;
        }
        // Transparent: the producer's own ctrl input lives at input[0] for
        // `ControlState` (variadic ctrl inputs).  If the producer has no
        // inputs (shouldn't happen for a well-formed transparent node), fall
        // back to the current output.
        let inputs = matcher.fn_graph.graph.node_inputs(producer);
        match inputs.get(0) {
            Some(&prev) => input_out = prev,
            None => return input_out,
        }
    }
    input_out
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use ir::FunctionBuilder;

    use crate::matcher::Matcher;

    /// Returns the control output of `g.entry`, i.e. the `Entry` node's
    /// output[0].
    fn entry_ctrl_out(g: &ir::BuiltFunctionGraph) -> NodeOutputId {
        g.graph
            .node_outputs(g.entry)
            .into_iter()
            .next()
            .expect("Entry node always has at least one Control output")
    }

    /// Finds the first `Call` node in `g` (there must be at least one for
    /// the tests that call this helper).
    fn find_first_call(g: &ir::BuiltFunctionGraph) -> NodeId {
        g.preorder()
            .find(|&n| matches!(g.graph.node_kind(n), NodeKind::Call))
            .expect("test graph must contain a Call node")
    }

    /// Finds the `Return` node (there's exactly one in the test graphs).
    fn find_return(g: &ir::BuiltFunctionGraph) -> NodeId {
        g.preorder()
            .find(|&n| matches!(g.graph.node_kind(n), NodeKind::Return))
            .expect("test graph must contain a Return node")
    }

    /// Entry → ControlState → Call → Return.  One hop of transparency
    /// between Entry's ctrl output and the Call.
    fn graph_entry_call_return() -> ir::Result<ir::BuiltFunctionGraph> {
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        let tgt = b.build_uint64_const(0x1234);
        b.build_call(tgt)?;
        b.build_return(None, &[])?;
        b.build()
    }

    /// Entry → ControlState(a) → ControlState(b) → Call → Return.
    /// Two fallthrough-linked regions produce two chained `ControlState`
    /// nodes between the Entry's ctrl output and the `Call`.
    fn graph_two_regions_call_return() -> ir::Result<ir::BuiltFunctionGraph> {
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let a = b.create_region()?;
        let bb = b.create_region()?;
        b.set_entry_region(a)?;
        b.set_region(a);
        b.build_branch(bb)?;
        b.set_region(bb);
        let tgt = b.build_uint64_const(0x5678);
        b.build_call(tgt)?;
        b.build_return(None, &[])?;
        b.build()
    }

    /// Entry → ControlState(a) → Call → ControlState(b) → Return.
    /// Splits the function into "pre-call region a" and "post-call region b"
    /// so a `ControlState` sits between the `Call`'s ctrl output and the
    /// `Return`.
    fn graph_call_then_region_return() -> ir::Result<ir::BuiltFunctionGraph> {
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let a = b.create_region()?;
        let bb = b.create_region()?;
        b.set_entry_region(a)?;
        b.set_region(a);
        let tgt = b.build_uint64_const(0x9999);
        b.build_call(tgt)?;
        b.build_branch(bb)?;
        b.set_region(bb);
        b.build_return(None, &[])?;
        b.build()
    }

    /// Entry → ControlState → Call1 → Call2 → Return.  Two calls in a row,
    /// so `skip_forward_transparent(entry_ctrl)` must stop at Call1 without
    /// walking through it.
    fn graph_two_calls_return() -> ir::Result<ir::BuiltFunctionGraph> {
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        let t1 = b.build_uint64_const(0x1111);
        let t2 = b.build_uint64_const(0x2222);
        b.build_call(t1)?;
        b.build_call(t2)?;
        b.build_return(None, &[])?;
        b.build()
    }

    #[test]
    fn skip_forward_transparent_stops_at_first_semantic_node() -> ir::Result<()> {
        let g = graph_entry_call_return()?;
        let m = Matcher::new(&g);

        let entry_ctrl = entry_ctrl_out(&g);
        let call = find_first_call(&g);
        let expected = g.graph.node_outputs(call).into_iter().next().unwrap();

        let got = skip_forward_transparent(&m, entry_ctrl);
        assert_eq!(
            got,
            Some(expected),
            "forward walk should land on Call.output[0], skipping the single ControlState"
        );

        // Sanity: the Call must actually be reached, not some earlier node.
        let got_node = g.graph.get_node_from_output(got.unwrap());
        assert!(matches!(g.graph.node_kind(got_node), NodeKind::Call));
        Ok(())
    }

    #[test]
    fn skip_forward_transparent_skips_multiple_transparent() -> ir::Result<()> {
        let g = graph_two_regions_call_return()?;
        let m = Matcher::new(&g);

        let entry_ctrl = entry_ctrl_out(&g);
        let call = find_first_call(&g);
        let expected = g.graph.node_outputs(call).into_iter().next().unwrap();

        let got = skip_forward_transparent(&m, entry_ctrl);
        assert_eq!(
            got,
            Some(expected),
            "forward walk should hop through BOTH ControlStates and land on Call.output[0]"
        );
        Ok(())
    }

    #[test]
    fn skip_forward_transparent_dead_end_returns_none() -> ir::Result<()> {
        // Build any small graph so we have a valid `BuiltFunctionGraph`,
        // then splice in a fresh detached `Entry` whose control output has
        // no consumers.  `Entry` is non-cacheable, so `create_node` always
        // produces a brand-new node.
        let mut g = graph_entry_call_return()?;
        let detached = g.graph.create_node(
            NodeKind::Entry,
            [],
            [ir::node::NodeOutputKind::Control],
        );
        let detached_ctrl = g
            .graph
            .node_outputs(detached)
            .into_iter()
            .next()
            .expect("freshly-built Entry has one Control output");

        let m = Matcher::new(&g);
        let got = skip_forward_transparent(&m, detached_ctrl);
        assert_eq!(
            got, None,
            "ctrl output with no consumers must return None (dead end)"
        );
        Ok(())
    }

    #[test]
    fn skip_backward_transparent_stops_at_first_semantic_producer() -> ir::Result<()> {
        let g = graph_call_then_region_return()?;
        let m = Matcher::new(&g);

        // Return.input[0] is the ctrl edge.  In this graph it comes from a
        // ControlState (post-call region), which must be walked through to
        // reach the Call.
        let ret = find_return(&g);
        let ret_ctrl_in = g
            .graph
            .node_inputs(ret)
            .get(0)
            .copied()
            .expect("Return has a ctrl input");
        // Sanity: confirm we really do have a ControlState producer here;
        // otherwise this test degenerates into the direct-producer case.
        let immediate = g.graph.get_node_from_output(ret_ctrl_in);
        assert!(
            matches!(g.graph.node_kind(immediate), NodeKind::ControlState),
            "test expects a ControlState between Call and Return"
        );

        let call = find_first_call(&g);
        let call_ctrl_out = g.graph.node_outputs(call).into_iter().next().unwrap();

        let got = skip_backward_transparent(&m, ret_ctrl_in);
        assert_eq!(
            got, call_ctrl_out,
            "backward walk must skip the ControlState and land on Call.output[0]"
        );
        Ok(())
    }

    #[test]
    fn skip_backward_transparent_direct_producer() -> ir::Result<()> {
        // In a single-region graph, Return.input[0] is produced directly by
        // the Call — no ControlState in between.  The helper must return the
        // input unchanged.
        let g = graph_entry_call_return()?;
        let m = Matcher::new(&g);

        let ret = find_return(&g);
        let ret_ctrl_in = g
            .graph
            .node_inputs(ret)
            .get(0)
            .copied()
            .expect("Return has a ctrl input");
        // Sanity: the immediate producer must be the Call.
        let producer = g.graph.get_node_from_output(ret_ctrl_in);
        assert!(
            matches!(g.graph.node_kind(producer), NodeKind::Call),
            "test setup requires Return.input[0] to come directly from the Call"
        );

        let got = skip_backward_transparent(&m, ret_ctrl_in);
        assert_eq!(
            got, ret_ctrl_in,
            "no transparent producer — the helper must return the input unchanged"
        );
        Ok(())
    }

    #[test]
    fn skip_forward_transparent_treats_call_as_semantic() -> ir::Result<()> {
        // Entry → CS → Call1 → Call2 → Return.  A forward walk from
        // Entry.ctrl must stop at Call1, NOT hop through it to Call2.
        let g = graph_two_calls_return()?;
        let m = Matcher::new(&g);

        let entry_ctrl = entry_ctrl_out(&g);
        // Collect both Calls in preorder; the first-reached (smallest
        // address consumer) must be Call1.  We verify by checking that the
        // returned output belongs to a `Call` that itself has a `Call`
        // consumer, which is Call2.
        let got = skip_forward_transparent(&m, entry_ctrl).expect("forward walk reaches Call1");
        let got_node = g.graph.get_node_from_output(got);
        assert!(
            matches!(g.graph.node_kind(got_node), NodeKind::Call),
            "expected to land on a Call node"
        );

        // Verify Call1-not-Call2: the landed-on node's ctrl output must
        // have a consumer that is itself a `Call` (i.e. Call2).
        let downstream: Vec<_> = g.graph.output_uses(got).collect();
        assert_eq!(
            downstream.len(),
            1,
            "Call1 ctrl output feeds exactly one consumer"
        );
        let downstream_node = downstream[0].0;
        assert!(
            matches!(g.graph.node_kind(downstream_node), NodeKind::Call),
            "landed node must be Call1 — its ctrl consumer is Call2, confirming Call is NOT transparent"
        );
        Ok(())
    }
}
