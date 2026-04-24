//! One-step-direct control-chain lookups used by control patterns.
//!
//! The helpers perform a single step in the control chain — no skipping,
//! no walking. Transparent SSA-join plumbing nodes like `ControlState` are
//! returned directly; the caller's pattern decides how to match them.
//!
//! * [`next_control_node`] — single consumer of a control output (forward).
//! * [`prev_control_node`] — producer of a control input (backward).

use ir::node::{NodeId, NodeOutputId};

use super::Matcher;

/// Returns the single consumer of `out`, or `None` if there are zero or
/// multiple consumers. We refuse to pick arbitrarily when a control output
/// forks, so callers see a clean no-match rather than an unpredictable one.
pub(crate) fn next_control_node(matcher: &Matcher, out: NodeOutputId) -> Option<NodeId> {
    let consumers: Vec<_> = matcher.fn_graph.graph.output_uses(out).collect();
    if consumers.len() != 1 {
        return None;
    }
    Some(consumers[0].0)
}

/// Returns the node that produces `input_out`. This is the direct
/// backward step — whatever produces the ctrl edge, without walking further.
pub(crate) fn prev_control_node(matcher: &Matcher, input_out: NodeOutputId) -> NodeId {
    matcher.fn_graph.graph.get_node_from_output(input_out)
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use ir::FunctionBuilder;
    use ir::node::NodeKind;

    use crate::matcher::Matcher;

    /// Entry → Call → Return, single region (no `ControlState` between
    /// Entry and Call nor between Call and Return).
    fn graph_call_return() -> ir::Result<ir::BuiltFunctionGraph> {
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        let tgt = b.build_uint64_const(0x1234);
        b.build_call(tgt)?;
        b.build_return(None, &[])?;
        b.build()
    }

    fn entry_ctrl_out(g: &ir::BuiltFunctionGraph) -> NodeOutputId {
        g.graph
            .node_outputs(g.entry)
            .into_iter()
            .next()
            .expect("Entry has a Control output")
    }

    fn find_first_call(g: &ir::BuiltFunctionGraph) -> NodeId {
        g.preorder()
            .find(|&n| matches!(g.graph.node_kind(n), NodeKind::Call))
            .expect("test graph contains a Call")
    }

    fn find_return(g: &ir::BuiltFunctionGraph) -> NodeId {
        g.preorder()
            .find(|&n| matches!(g.graph.node_kind(n), NodeKind::Return))
            .expect("test graph contains a Return")
    }

    #[test]
    fn next_control_node_returns_single_consumer() -> ir::Result<()> {
        let g = graph_call_return()?;
        let m = Matcher::new(&g);
        // Entry.ctrl feeds the region's `ControlState` header directly —
        // it is the single consumer. The walk helper does not skip it;
        // the caller's pattern decides whether to match a ControlState.
        let got = next_control_node(&m, entry_ctrl_out(&g)).expect("one consumer");
        assert!(matches!(g.graph.node_kind(got), NodeKind::ControlState));
        Ok(())
    }

    #[test]
    fn next_control_node_returns_none_when_no_consumer() -> ir::Result<()> {
        // Build a graph then inject a detached Entry with no consumer.
        let mut g = graph_call_return()?;
        let detached = g.graph.create_node(
            NodeKind::Entry,
            [],
            [ir::node::NodeOutputKind::Control],
        );
        let out = g
            .graph
            .node_outputs(detached)
            .into_iter()
            .next()
            .expect("Entry has one output");
        let m = Matcher::new(&g);
        assert_eq!(next_control_node(&m, out), None);
        Ok(())
    }

    #[test]
    fn prev_control_node_returns_direct_producer() -> ir::Result<()> {
        // graph_call_return has Call directly feeding Return (no ControlState
        // in single-region graphs).
        let g = graph_call_return()?;
        let m = Matcher::new(&g);
        let ret = find_return(&g);
        let ret_ctrl_in = g
            .graph
            .node_inputs(ret)
            .get(0)
            .copied()
            .expect("Return has a ctrl input");
        let producer = prev_control_node(&m, ret_ctrl_in);
        assert_eq!(producer, find_first_call(&g));
        Ok(())
    }
}
