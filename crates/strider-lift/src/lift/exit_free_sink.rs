//! Sinks for control cycles that never leave: a `while (1)` body, a spin
//! loop, a `panic` helper ending in a self-jump. Common, not a corner: the
//! x86-family and aarch64 CRT entries seat one, x86's sla lifting `hlt` to a
//! self-loop.
//!
//! Such a cycle reaches no `Return` / `IndirectBranch` / `Unreachable`, so it
//! roots nothing: `Function::retain_reachable` walks data inputs backward from
//! terminators, and without one the cycle's whole body, stores included, is
//! dropped. `validate` rejects the shape outright.

use anyhow::{Result, anyhow};
use strider_ir::graph::Graph;
use strider_ir::node::{NodeId, NodeKind, UseId, ValueId, ValueKind};
use strider_ir::walk::{NodeIdSet, cfg_outputs, stranded_nodes};
use strider_ir::{IRBuilder, IRBuilderExt, IRViewer};

use super::FunctionLifter;

/// The cycle-closing control edge a sink is seated on.
struct BackEdge {
    /// The consumer's use of `control`, repointed at the sink branch's live arm.
    use_id: UseId,
    control: ValueId,
    /// Memory live on this edge, which the sink anchors.
    memory: ValueId,
    /// The branch closing the cycle, which the sink is fingerprinted to.
    addr: Option<u64>,
}

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Seats `If(true) { back edge } { Unreachable(mem) }` on one edge of every
    /// exit-free control cycle, and returns the node visits that took.
    ///
    /// The `If` is what supplies the extra control output: every control output
    /// inside the cycle already has its one permitted consumer, and `Region`'s
    /// signature has no spare.
    pub(crate) fn seat_exit_free_sinks(&mut self) -> Result<usize> {
        let function = self.builder.function();
        let mut stranded = stranded_nodes(function.graph(), function.entry());
        // The one whole-function scan. Seating a sink makes exactly the nodes
        // reaching that cycle escape, and they leave the set incrementally.
        let mut visits = function.graph().all_node_ids().count();
        let seeds: Vec<NodeId> = stranded.iter().collect();
        for seed in seeds {
            if !stranded.contains(seed) {
                continue;
            }
            let cycle = walk_to_cycle(
                self.builder.function().graph(),
                &stranded,
                seed,
                &mut visits,
            );
            let edge = self.exit_free_back_edge(&cycle)?;
            self.with_lift_addr(edge.addr, |s| s.seat_sink(&edge))?;
            drop_escaped(
                self.builder.function().graph(),
                &mut stranded,
                &cycle,
                &mut visits,
            );
        }
        Ok(visits)
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
        // Only a `Region` takes more than one control input, so a cycle
        // reachable from `Entry` is entered through one.
        let seat = cycle
            .iter()
            .find(|&n| matches!(graph.node_kind(n), NodeKind::Region))
            .ok_or_else(|| anyhow!("exit-free control cycle has no Region to seat a sink on"))?;
        let index = graph
            .node_inputs(seat)
            .into_iter()
            .position(|v| cycle.contains(graph.value_definition(v).0))
            .ok_or_else(|| anyhow!("cycle seat {seat:?} has no control input from its cycle"))?;
        let control = graph
            .nth_input(seat, index)
            .ok_or_else(|| anyhow!("cycle seat {seat:?} lost input {index}"))?;
        Ok(BackEdge {
            use_id: graph.node_input_id_at(seat, index)?,
            control,
            memory: region_memory_input(graph, seat, index)?,
            addr: self.back_branch_addr(graph.value_definition(control).0, seat),
        })
    }

    /// The machine address of the branch producing `control`: the region it
    /// leaves when that producer is a `Region`, otherwise the producer's own
    /// asm fingerprint.
    fn back_branch_addr(&self, producer: NodeId, seat: NodeId) -> Option<u64> {
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
            .or_else(|| self.region_last_addrs.get(&seat).copied())
            .or_else(|| self.entry_machine_addr())
    }
}

/// The memory value paired with `region`'s `index`-th control predecessor:
/// `MemPhi`'s inputs are its `PhiToken` then one memory value per predecessor,
/// in the order `link_region` appends them.
fn region_memory_input(graph: &Graph, region: NodeId, index: usize) -> Result<ValueId> {
    let phi_token = graph
        .node_outputs(region)
        .get(1)
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

/// Walks control forward from `start` until a node repeats, and returns that
/// cycle. Every successor of a stranded node is stranded (a successor that
/// reached a terminator would carry its predecessor with it), so the walk
/// stays inside the set and, being finite, must close.
fn walk_to_cycle(
    graph: &Graph,
    stranded: &NodeIdSet,
    start: NodeId,
    visits: &mut usize,
) -> NodeIdSet {
    let mut path: Vec<NodeId> = Vec::new();
    let mut on_path = NodeIdSet::new();
    let mut node = start;
    while on_path.insert(node) {
        path.push(node);
        *visits += 1;
        let next = cfg_outputs(graph, node)
            .flat_map(|v| graph.value_uses(v))
            .map(|(succ, _)| succ)
            .find(|&succ| stranded.contains(succ));
        match next {
            Some(succ) => node = succ,
            // Unreachable for a stranded node, which always has a stranded
            // successor; the empty result makes the caller's `Region` search
            // fail loudly instead of looping.
            None => return NodeIdSet::new(),
        }
    }
    let mut cycle = NodeIdSet::new();
    let closed = path.iter().position(|&n| n == node).unwrap_or(path.len());
    for &n in &path[closed..] {
        cycle.insert(n);
    }
    cycle
}

/// Drops every stranded node reaching `cycle`: the sink seated there is now
/// their terminator. Each node leaves the set once, so the whole seating
/// loop stays linear in the function.
fn drop_escaped(graph: &Graph, stranded: &mut NodeIdSet, cycle: &NodeIdSet, visits: &mut usize) {
    let mut work: Vec<NodeId> = cycle.iter().collect();
    for &node in &work {
        stranded.remove(node);
    }
    while let Some(node) = work.pop() {
        *visits += 1;
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
