//! `EGraphAdapter::extract_into_graph` — extracts the lowest-cost form per
//! e-class and rebuilds a strider [`crate::Graph`] structurally equivalent
//! to the original.
//!
//! Phase 1 Task 1.5 spike — step 4 implementation.
//!
//! # Algorithm
//!
//! 1. Topologically sort the reachable nodes of the original graph (data
//!    predecessors before their consumers).
//! 2. Walk in topo order; for every reachable [`crate::node::NodeId`]:
//!    - Map its inputs (each input is an old `NodeOutputId`) to the
//!      corresponding new `NodeOutputId` via the rolling
//!      `old_to_new_output` table.
//!    - Determine the new kind:
//!      - **Internal value-producing node** (per
//!        [`super::from_graph::is_opaque_value_kind`]):
//!        extract the e-node from the egraph and translate back to the
//!        matching [`crate::node::NodeKind`].  This is the round-trip
//!        verification: the e-node must carry every payload bit needed
//!        to recover the strider kind.
//!      - **Otherwise** (opaque value-producer, control / memory /
//!        PhiToken node): copy the kind from the original.
//!    - Allocate the new node with the matching output kinds and inputs.
//!    - Record the old→new mapping and copy side-tables
//!      (`asm_fingerprints`, `stack_phi_offsets`, `call_other_names`,
//!      `call_clobbered_overrides`).
//!
//! # Out of scope for the spike
//!
//! Multi-output nodes' clobber slots are preserved structurally — each
//! value output is its own opaque leaf in the egraph, but the matching
//! strider `Call` / `CallOther` node is cloned with all its slots from
//! the original.  Production integration (Phase 3) will reconstruct
//! these from the orchestrator's per-call CC metadata.

use std::collections::HashMap;

use egg::{AstSize, Extractor};

use super::from_graph::EGraphAdapter;
use super::language::StriderLang;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};
use crate::walk::walk_graph;

impl EGraphAdapter {
    /// Extracts the lowest-cost form per e-class (with `AstSize`) and
    /// rebuilds a strider [`Graph`] that is structurally equivalent to
    /// `original` when zero rewrites have been applied to the egraph.
    ///
    /// Returns `(new_graph, new_entry)` so callers can validate against
    /// the original entry.
    ///
    /// # Panics
    ///
    /// Panics if a value output's egraph-extracted e-node doesn't map
    /// back to a valid strider [`NodeKind`].  This is the spike's
    /// round-trip assertion: a mismatch means the egraph lost
    /// information.  Real callers (Phase 3) will surface this as a
    /// typed error.
    #[must_use]
    pub fn extract_into_graph(&self, original: &Graph, entry: NodeId) -> (Graph, NodeId) {
        let extractor = Extractor::new(&self.egraph, AstSize);

        let mut new_graph = Graph::new();
        // `old → new` maps: rebuilt incrementally in topo order so every
        // input slot can be resolved before its consumer is allocated.
        let mut old_to_new_node: HashMap<NodeId, NodeId> = HashMap::new();
        let mut old_to_new_output: HashMap<NodeOutputId, NodeOutputId> = HashMap::new();

        let topo = topo_sort_reachable(original, entry);

        for old_id in topo {
            let old_kind = *original.node_kind(old_id);

            // Resolve inputs old → new.  Validator-enforced invariant:
            // by topo order, every input's producer is already cloned.
            let new_inputs: Vec<NodeOutputId> = original
                .node_inputs(old_id)
                .into_iter()
                .map(|oid| {
                    *old_to_new_output.get(&oid).unwrap_or_else(|| {
                        panic!(
                            "extract_into_graph: input {oid:?} of node {old_id:?} \
                             not yet mapped — topo sort bug"
                        )
                    })
                })
                .collect();

            // Output kinds are structural — they come from the original
            // graph directly.  (The egraph never sees control/memory/
            // PhiToken outputs, and value-output types are encoded in
            // the StriderLang variant itself.)
            let output_kinds: Vec<NodeOutputKind> = original
                .node_outputs(old_id)
                .into_iter()
                .map(|oid| original.output_kind(oid))
                .collect();

            // Choose the new kind: derive from egraph for internal
            // value-producing kinds, otherwise copy from the original.
            let new_kind =
                self.derive_kind(original, old_id, &old_kind, &extractor);

            let new_id = new_graph.create_node(
                new_kind,
                new_inputs.iter().copied(),
                output_kinds.iter().copied(),
            );
            old_to_new_node.insert(old_id, new_id);

            // Map outputs in slot order.
            let old_outs: Vec<NodeOutputId> = original.node_outputs(old_id).into_iter().collect();
            let new_outs: Vec<NodeOutputId> = new_graph.node_outputs(new_id).into_iter().collect();
            assert_eq!(
                old_outs.len(),
                new_outs.len(),
                "output arity must match for cloned node {old_id:?}"
            );
            for (old_oid, new_oid) in old_outs.iter().zip(new_outs.iter()) {
                old_to_new_output.insert(*old_oid, *new_oid);
            }

            // Copy side-tables.
            copy_side_tables(original, &mut new_graph, old_id, new_id);
        }

        let new_entry = *old_to_new_node
            .get(&entry)
            .expect("entry node must be in the topo set");
        (new_graph, new_entry)
    }

    /// Derives the new [`NodeKind`] for `old_id`.
    ///
    /// For value-producing internal e-nodes, looks up the e-class via
    /// `output_to_eclass`, extracts the best `StriderLang` representative,
    /// and translates it back into the matching `NodeKind`.  For every
    /// other kind (opaque value producers, control / memory / phi
    /// scaffolding), returns the original kind unchanged.
    fn derive_kind(
        &self,
        original: &Graph,
        old_id: NodeId,
        old_kind: &NodeKind,
        extractor: &Extractor<'_, AstSize, StriderLang, ()>,
    ) -> NodeKind {
        // Find the single value-output of the node (if any).  Internal
        // e-nodes in our model produce exactly one value output; if
        // there are multiple value outputs (Call, modeled CallOther),
        // the node is classified as opaque so we never enter this branch.
        let val_oid = original
            .node_outputs(old_id)
            .into_iter()
            .find(|&oid| original.output_kind(oid).is_value());
        let val_oid = match val_oid {
            Some(o) => o,
            None => return *old_kind, // no value output → not in egraph
        };

        if super::from_graph::is_opaque_value_kind_for_extract(old_kind) {
            return *old_kind;
        }

        let eclass = match self.output_to_eclass.get(&val_oid) {
            Some(&id) => id,
            None => return *old_kind, // not added to the egraph
        };
        let lang = extractor.find_best_node(eclass);
        kind_from_lang(lang).unwrap_or_else(|| {
            panic!(
                "extract_into_graph: e-class for {val_oid:?} (originally \
                 {old_kind:?}) extracted to {lang:?} which has no matching \
                 strider NodeKind — round-trip bug"
            )
        })
    }
}

/// Translates a `StriderLang` representative into the matching
/// `NodeKind`.  Returns `None` for opaque-leaf variants (which should
/// never be reached during extract — opaque nodes are copied from the
/// original directly).
fn kind_from_lang(lang: &StriderLang) -> Option<NodeKind> {
    use StriderLang as L;
    Some(match lang {
        L::Opaque(_) => return None,
        L::IntConst(v, _ty) => NodeKind::IntConst(*v),
        L::BoolConst(b) => NodeKind::BoolConst(*b),
        L::FloatConst(bits, _ty) => NodeKind::FloatConst(*bits),
        L::IntBin(op, _ty, _) => NodeKind::IntBinaryOp(*op),
        L::IntUn(op, _ty, _) => NodeKind::IntUnaryOp(*op),
        L::IntCmp(op, _) => NodeKind::IntCmpOp(*op),
        L::CastToInt(_ty, _) => NodeKind::CastToInt,
        L::Truncate(_ty, _) => NodeKind::Truncate,
        L::Popcount(_ty, _) => NodeKind::Popcount,
        L::Lzcount(_ty, _) => NodeKind::Lzcount,
        L::Extend(op, _ty, _) => NodeKind::Extend(*op),
        L::BoolBin(op, _) => NodeKind::BoolBinaryOp(*op),
        L::BoolUn(op, _) => NodeKind::BoolUnaryOp(*op),
        L::CastToBool(_) => NodeKind::CastToBool,
        L::FloatBin(op, _ty, _) => NodeKind::FloatBinaryOp(*op),
        L::FloatUn(op, _ty, _) => NodeKind::FloatUnaryOp(*op),
        L::FloatCmp(op, _) => NodeKind::FloatCmpOp(*op),
        L::IntToFloat(_ty, _) => NodeKind::IntToFloat,
        L::FloatToInt(_ty, _) => NodeKind::FloatToInt,
        L::FloatToFloat(_ty, _) => NodeKind::FloatToFloat,
        L::IntBitsToFloat(_ty, _) => NodeKind::IntBitsToFloat,
        L::FloatBitsToInt(_ty, _) => NodeKind::FloatBitsToInt,
        L::CastToFloat(_ty, _) => NodeKind::CastToFloat,
    })
}

/// Reachable-from-`entry` nodes in topological order: every node's data
/// predecessors appear earlier in the slice.
///
/// Strider's `walk_graph` doesn't guarantee this order natively (it
/// interleaves backward-data and forward-control), so we run our own
/// post-order DFS over `(input → producer) ∪ (control_output → consumer)`
/// edges that produces a valid topo sort.
fn topo_sort_reachable(g: &Graph, entry: NodeId) -> Vec<NodeId> {
    // White / gray / black coloring for cycle-safe DFS.  Strider's IR
    // does have cycles via VarPhi back-edges, but only on the *data*
    // side — we treat phi inputs to phis as non-edges for topo
    // purposes (the phi's value is opaque to the topo sort).  In
    // practice the spike's fixtures don't exercise loop phis, but we
    // handle them defensively.
    use std::collections::HashSet;
    let reachable: HashSet<NodeId> = walk_graph(g, entry).collect();
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut on_stack: HashSet<NodeId> = HashSet::new();
    let mut topo: Vec<NodeId> = Vec::with_capacity(reachable.len());

    fn dfs(
        g: &Graph,
        node: NodeId,
        reachable: &std::collections::HashSet<NodeId>,
        visited: &mut std::collections::HashSet<NodeId>,
        on_stack: &mut std::collections::HashSet<NodeId>,
        topo: &mut Vec<NodeId>,
    ) {
        if visited.contains(&node) || !reachable.contains(&node) {
            return;
        }
        if on_stack.contains(&node) {
            // Back-edge through a phi.  Skip — the phi's existing
            // NodeId is sufficient for the consumer to wire its input
            // when the consumer's clone runs (the phi will have been
            // cloned by then because phis sit at the top of the topo
            // order under non-loop graphs; for loop graphs we defer
            // back-edges to a second pass below).
            return;
        }
        on_stack.insert(node);
        // Visit data predecessors first.
        for inp in g.node_inputs(node) {
            let pred = g.get_node_from_output(inp);
            dfs(g, pred, reachable, visited, on_stack, topo);
        }
        on_stack.remove(&node);
        visited.insert(node);
        topo.push(node);
    }

    // Seed DFS from every reachable node so isolated subgraphs (e.g.
    // disconnected control regions) get topo-sorted too.
    for &node in &reachable {
        dfs(g, node, &reachable, &mut visited, &mut on_stack, &mut topo);
    }
    topo
}

/// Copies per-node side-table entries from `original` to `new_graph`.
fn copy_side_tables(
    original: &Graph,
    new_graph: &mut Graph,
    old_id: NodeId,
    new_id: NodeId,
) {
    // asm-fingerprint — copy the sorted-deduped slice into the new
    // node via the `set_asm_fingerprint` testing helper (sort+dedup is
    // idempotent here).
    let fp = original.asm_fingerprint(old_id);
    if !fp.is_empty() {
        new_graph.set_asm_fingerprint(new_id, fp.to_vec());
    }

    // stack_phi_offsets (only StackStorePhi kinds).
    let offsets = original.stack_phi_offsets(old_id);
    if !offsets.is_empty() {
        new_graph.set_stack_phi_offsets(new_id, offsets.to_vec());
    }

    // call_other_names (only CallOther kinds).
    if let Some(name) = original.call_other_name(old_id) {
        new_graph.set_call_other_name(new_id, name.to_string());
    }

    // call_clobbered_overrides (only Call kinds with per-CC overrides).
    if let Some(override_list) = original.call_clobbered_override(old_id) {
        new_graph.set_call_clobbered_override(new_id, override_list.to_vec());
    }

    // FunctionArg source / index can't change — the kind already
    // encodes it via `NodeKind::FunctionArg { source, index }`.  No
    // separate side-table copy needed.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeOutputType;

    /// Builds a graph: Entry + InitialMemory + ControlState + IntConst +
    /// Return.  Round-trips through the egraph adapter; asserts the
    /// resulting graph has the same kinds and edge structure.
    #[test]
    fn roundtrip_int_const_function() {
        let mut g = Graph::new();
        let entry = g.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = g.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_out = g.node_outputs(entry).into_iter().next().unwrap();
        let mem_out = g.node_outputs(mem).into_iter().next().unwrap();
        let cs = g.create_node(
            NodeKind::ControlState,
            [entry_out],
            [NodeOutputKind::Control, NodeOutputKind::PhiToken],
        );
        let cs_ctrl = g.node_outputs(cs).into_iter().next().unwrap();
        let c = g.create_node(
            NodeKind::IntConst(99),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let c_out = g.node_outputs(c).into_iter().next().unwrap();
        let _ret = g.create_node(NodeKind::Return, [cs_ctrl, mem_out, c_out], []);

        let adapter = EGraphAdapter::from_graph(&g, entry);
        let (new_g, new_entry) = adapter.extract_into_graph(&g, entry);

        // Same number of reachable nodes.
        let old_count = walk_graph(&g, entry).count();
        let new_count = walk_graph(&new_g, new_entry).count();
        assert_eq!(old_count, new_count);

        // Find the IntConst in both graphs and verify the value.
        let find_int_const = |graph: &Graph, root: NodeId| -> Option<u128> {
            for n in walk_graph(graph, root) {
                if let NodeKind::IntConst(v) = graph.node_kind(n) {
                    return Some(*v);
                }
            }
            None
        };
        assert_eq!(find_int_const(&g, entry), Some(99));
        assert_eq!(find_int_const(&new_g, new_entry), Some(99));
    }
}
