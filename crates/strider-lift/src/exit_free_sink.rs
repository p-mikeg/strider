//! Sinks for control cycles that never leave (`while (1)`, a spin loop).
//! Such a cycle reaches no terminator, so `Function::compact` would drop its
//! body and `validate` rejects it.

use anyhow::{Result, anyhow};
use strider_ir::graph::Graph;
use strider_ir::node::{NodeId, NodeKind, UseId, ValueId, ValueKind};
use strider_ir::walk::{NodeIdSet, cfg_outputs, stranded_nodes};
use strider_ir::{IRBuilder, IRBuilderExt, IRViewer};

use super::FunctionLifter;

#[cfg(test)]
thread_local! {
    /// Node visits by sink seating since a test last cleared it.
    pub(crate) static SINK_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The cycle-closing control edge a sink is seated on.
struct BackEdge {
    /// The consumer's use of `control`, repointed at the sink branch's live arm.
    use_id: UseId,
    control: ValueId,
    /// Memory live on this edge, which the sink anchors.
    memory: ValueId,
    /// The branch closing the cycle, which the sink is fingerprinted to.
    addr: u64,
}

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Seats `If(true) { back edge } { Unreachable(mem) }` on one edge of every
    /// exit-free control cycle; the `If` supplies the spare control output.
    pub(crate) fn seat_exit_free_sinks(&mut self) -> Result<()> {
        let function = self.builder.function();
        let mut stranded = stranded_nodes(function.graph(), function.entry());
        // The only whole-function scan; escaped nodes leave the set as seated.
        #[cfg(test)]
        SINK_VISITS.with(|v| v.set(v.get() + function.graph().all_node_ids().count()));
        let seeds: Vec<NodeId> = stranded.iter().collect();
        for seed in seeds {
            if !stranded.contains(seed) {
                continue;
            }
            let cycle = walk_to_cycle(self.builder.function().graph(), &stranded, seed);
            let edge = self.exit_free_back_edge(&cycle)?;
            self.with_lift_addr(Some(edge.addr), |s| s.seat_sink(&edge))?;
            drop_escaped(self.builder.function().graph(), &mut stranded, &cycle);
        }
        Ok(())
    }

    fn seat_sink(&mut self, edge: &BackEdge) -> Result<()> {
        let cond = self.builder.build_boolean_const(true);
        let branch = self.builder.create_node(
            NodeKind::If,
            [edge.control, cond],
            [ValueKind::Control, ValueKind::Control],
        );
        let [taken, never] = self.builder.function().node_outputs_exact(branch)?;
        self.builder
            .function_mut()
            .graph_mut()
            .update_input(edge.use_id, taken);
        self.builder
            .create_node(NodeKind::Unreachable, [never, edge.memory], []);
        Ok(())
    }

    /// The control edge closing `cycle`.
    fn exit_free_back_edge(&self, cycle: &NodeIdSet) -> Result<BackEdge> {
        let graph = self.builder.function().graph();
        // A cycle reachable from `Entry` is entered through a `Region`.
        let seat = cycle
            .iter()
            .find(|&n| matches!(graph.node_kind(n), NodeKind::Region))
            .ok_or_else(|| anyhow!("exit-free control cycle has no Region to seat a sink on"))?;
        let (index, control) = graph
            .node_inputs(seat)
            .into_iter()
            .enumerate()
            .find(|&(_, v)| cycle.contains(graph.value_definition(v).0))
            .ok_or_else(|| anyhow!("cycle seat {seat:?} has no control input from its cycle"))?;
        Ok(BackEdge {
            use_id: graph.node_input_id_at(seat, index)?,
            control,
            memory: region_memory_input(graph, seat, index)?,
            addr: self.back_branch_addr(graph.value_definition(control).0, seat),
        })
    }

    /// The machine address of the branch producing `control`, falling back to
    /// the producer's fingerprint, then to `seat`'s last address.
    fn back_branch_addr(&self, producer: NodeId, seat: NodeId) -> u64 {
        self.region_last_addrs
            .get(&producer)
            .copied()
            .or_else(|| {
                self.builder
                    .function()
                    .side_tables()
                    .asm_fingerprint(producer)
                    .into_iter()
                    .max()
            })
            .unwrap_or_else(|| self.region_last_addrs[&seat])
    }
}

/// The memory value on `region`'s `index`-th control predecessor (`MemPhi`
/// input `index + 1`, after its `PhiToken`).
fn region_memory_input(graph: &Graph, region: NodeId, index: usize) -> Result<ValueId> {
    let phi_token = graph
        .node_outputs(region)
        .get(NodeKind::REGION_PHI_OUT_SLOT)
        .copied()
        .ok_or_else(|| anyhow!("region {region:?} has no PhiToken output"))?;
    let mem_phi = graph
        .value_uses(phi_token)
        .map(|(node, _)| node)
        .find(|&n| matches!(graph.node_kind(n), NodeKind::MemPhi))
        .ok_or_else(|| anyhow!("region {region:?} has no MemPhi"))?;
    graph
        .nth_input(mem_phi, index + 1)
        .ok_or_else(|| anyhow!("MemPhi of {region:?} has no operand for predecessor {index}"))
}

/// Walks control forward from `start` through stranded nodes until one
/// repeats; returns that cycle, or an empty set if the walk dead-ends.
fn walk_to_cycle(graph: &Graph, stranded: &NodeIdSet, start: NodeId) -> NodeIdSet {
    let mut path: Vec<NodeId> = Vec::new();
    let mut on_path = NodeIdSet::new();
    let mut node = start;
    while on_path.insert(node) {
        path.push(node);
        #[cfg(test)]
        SINK_VISITS.with(|v| v.set(v.get() + 1));
        let next = cfg_outputs(graph, node)
            .flat_map(|v| graph.value_uses(v))
            .map(|(succ, _)| succ)
            .find(|&succ| stranded.contains(succ));
        match next {
            Some(succ) => node = succ,
            // Unreachable; an empty cycle makes the caller fail.
            None => return NodeIdSet::new(),
        }
    }
    let closed = path.iter().position(|&n| n == node).unwrap_or(path.len());
    path[closed..].iter().copied().collect()
}

/// Drops every stranded node reaching `cycle`, which its sink terminates.
/// Each node leaves once, so seating is linear overall.
fn drop_escaped(graph: &Graph, stranded: &mut NodeIdSet, cycle: &NodeIdSet) {
    let mut work: Vec<NodeId> = cycle.iter().collect();
    for &node in &work {
        stranded.remove(node);
    }
    while let Some(node) = work.pop() {
        #[cfg(test)]
        SINK_VISITS.with(|v| v.set(v.get() + 1));
        for value in graph.node_inputs(node) {
            if !graph.value_kind(value).is_control() {
                continue;
            }
            let pred = graph.value_definition(value).0;
            if stranded.contains(pred) {
                stranded.remove(pred);
                work.push(pred);
            }
        }
    }
}
