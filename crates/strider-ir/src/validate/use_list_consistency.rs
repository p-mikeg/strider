use entity_utils::set::DenseEntitySet;

use crate::graph::Graph;
use crate::node::UseId;
use crate::walk::NodeIdSet;

use super::ValidationError;

/// Use-list consistency.  For every node input, verify that the
/// output it references still lists that input as one of its consumers
/// (forward check).  For every output's use-list, verify that each listed
/// input still points back to that output (backward check).
///
/// Scoped to nodes reachable from the entry — opt passes
/// (`DeadBranchElimination`, `CfgDetach`) detach unreachable
/// subgraphs but leave the zombie nodes in the arena, and re-checking
/// their use-list integrity would surface noise rather than real bugs.
/// `check_local_typing` and `check_graph_invariants_phis` are scoped
/// the same way; this makes the three checks' coverage consistent.
///
/// Implementation: a single sweep over every reachable node's outputs
/// builds a `listed_uses` set of every `UseId` that currently
/// appears in some use-list, and simultaneously runs the backward
/// consistency check.  The forward check is then a per-input O(1)
/// membership test against that set — total cost O(E) where E is the
/// edge count, vs. the previous O(E·U) "for each edge, scan the
/// target's use-list".
pub(super) fn check_use_list_consistency(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    // Single sweep over use-lists, restricted to outputs whose source
    // node is reachable.  Sweeping all outputs would also visit zombie
    // nodes' outputs — those legitimately have empty use-lists, but
    // any consumer in their use-list would itself be a zombie consumer,
    // which we don't want to flag here.
    let mut listed_uses: DenseEntitySet<UseId> = DenseEntitySet::new();
    for value in graph.all_value_ids() {
        let (source, _) = graph.value_definition(value);
        if !reachable.contains(source) {
            continue;
        }
        let mut cur = graph.value_first_use_id(value);
        while let Some(iid) = cur {
            listed_uses.insert(iid);
            let referenced = graph.value_of_use(iid);
            if referenced != value {
                errs.push(ValidationError::UseListContainsStaleInput {
                    value,
                    listed_use: iid,
                });
            }
            cur = graph.next_use(iid);
        }
    }

    // Forward check: every reachable node's input must appear in some
    // use-list.  Catches the "input was created but never threaded into
    // the producer's use-list" failure mode (covered by the
    // `use_list_input_missing_from_use_list` test, which simulates a
    // corrupted graph via `corrupt_clear_first_use`).
    for node in graph.all_node_ids() {
        if !reachable.contains(node) {
            continue;
        }
        let input_count = graph.node_inputs(node).len();
        for idx in 0..input_count {
            // The index range is by construction valid (we just measured the
            // input count); a failure here would be an internal-consistency
            // bug rather than user input. Skip the offending index in the
            // unlikely event of one.
            let Ok(use_id) = graph.node_input_id_at(node, idx) else {
                continue;
            };
            if !listed_uses.contains(use_id) {
                let target = graph.value_of_use(use_id);
                errs.push(ValidationError::InputMissingFromUseList {
                    node,
                    input_idx: idx,
                    value: target,
                });
            }
        }
    }
}
