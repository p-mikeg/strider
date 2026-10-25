//! Nearest-clobber queries over the memory graph.
//!
//! [`layout`] answers one by climbing the memory dominator tree over an index
//! built once per graph state.

mod layout;

#[cfg(test)]
pub(crate) use layout::CLIMB_STEPS;
pub(crate) use layout::{Answers, ClobberProbe, MemLayout, Shape};

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

/// Repoints a `Load`'s memory input onto `clobber`'s memory output, skipping
/// the proven-disjoint defs in between.
///
/// Sound only for a `Load`: it is a pure consumer with no memory output, so
/// moving its single incoming memory edge is invisible to every other node.
/// Idempotent, and monotone across fixed-point iterations (verdicts only ever
/// move MayAlias to Disjoint).  Returns whether the edge moved.
pub(crate) fn narrow_load_to(
    edit: &mut crate::EditFunction<'_>,
    load: NodeId,
    clobber: NodeId,
) -> bool {
    let rewire = {
        let function = edit.function();
        // Checked in release too: a `Store` also has a memory input at this
        // slot, so falling through would rewire one.
        assert!(
            matches!(function.node_kind(load), NodeKind::Load(_)),
            "narrow_load_to on a non-Load"
        );
        let target_mem = function
            .memory_output_of(clobber)
            .expect("a clobber node has a memory output");
        let mem_use = function
            .node_input_id_at(load, NodeKind::ACCESS_MEM_SLOT)
            .expect("a Load has a memory input");
        let cur_mem = function.node_inputs(load)[NodeKind::ACCESS_MEM_SLOT];
        (cur_mem != target_mem).then_some((mem_use, target_mem))
    };
    if let Some((mem_use, target_mem)) = rewire {
        edit.update_input(mem_use, target_mem);
    }
    rewire.is_some()
}

#[cfg(test)]
mod tests;
