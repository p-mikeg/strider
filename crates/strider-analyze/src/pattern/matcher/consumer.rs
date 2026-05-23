//! One-step forward consumer lookup used by [`NodePat`]'s `ConsumersSpec`.
//!
//! Only one helper remains: [`next_control_node`] resolves the single
//! consumer of a given `NodeOutputId` (or `None` if the output has zero or
//! multiple consumers). Direct-step backward walks are unnecessary — the
//! default [`Pattern::try_match`](crate::pattern::pat::traits::Pattern::try_match)
//! already does `graph.get_node_from_output(input_out)` when a sub-pattern
//! is matched against an input.

use strider_ir::node::{NodeId, NodeOutputId};

use super::Matcher;

/// Returns the single consumer of `out`, or `None` if there are zero or
/// multiple consumers. We refuse to pick arbitrarily when a control output
/// forks, so callers see a clean no-match rather than an unpredictable one.
pub(crate) fn next_control_node(matcher: &Matcher, out: NodeOutputId) -> Option<NodeId> {
    let mut uses = matcher.graph.output_uses(out);
    let first = uses.next()?;
    if uses.next().is_some() {
        return None;
    }
    Some(first.0)
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use strider_ir::FunctionBuilder;
    use strider_ir::node::{NodeKind, NodeOutputType};
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    use crate::pattern::matcher::Matcher;

    /// Entry → Call → Return, single region (no `ControlState` between
    /// Entry and Call nor between Call and Return).
    fn graph_call_return() -> strider_ir::Result<strider_ir::Graph> {
        let mut b = FunctionBuilder::empty()?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let tgt = b.build_int_const(0x1234u64, NodeOutputType::U64)?;
        b.build_call(tgt)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        b.build()
    }

    fn entry_ctrl_out(g: &strider_ir::Graph) -> NodeOutputId {
        g.node_outputs(g.entry().expect("test fixture must be built"))
            .iter()
            .copied()
            .next()
            .expect("Entry has a Control output")
    }

    #[test]
    fn next_control_node_returns_single_consumer() -> strider_ir::Result<()> {
        let g = graph_call_return()?;
        let m = Matcher::new(&g);
        // Entry.ctrl feeds the region's `ControlState` header directly —
        // it is the single consumer. The walk helper does not skip it;
        // the caller's pattern decides whether to match a ControlState.
        let got = next_control_node(&m, entry_ctrl_out(&g)).expect("one consumer");
        assert!(matches!(g.node_kind(got), NodeKind::ControlState));
        Ok(())
    }

    #[test]
    fn next_control_node_returns_none_when_no_consumer() -> strider_ir::Result<()> {
        let mut g = graph_call_return()?;
        let detached = g.create_node(
            NodeKind::Entry,
            [],
            [strider_ir::node::NodeOutputKind::Control],
        );
        let out = g
            .node_outputs(detached)
            .iter()
            .copied()
            .next()
            .expect("Entry has one output");
        let m = Matcher::new(&g);
        assert_eq!(next_control_node(&m, out), None);
        Ok(())
    }
}
