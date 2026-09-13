//! Braun redundant-phi elimination for `Phi` and `MemPhi` ("Simple and
//! Efficient Construction of SSA Form", section 3.2).
//!
//! Inputs are `[phi_token, val_0, val_1, ...]`. The phi graph has an edge from
//! each phi to every phi among its value inputs. A strongly connected component
//! whose value inputs from outside itself are exactly ONE value is that value:
//! every use of every phi in it is redirected there. A single trivial phi, one
//! whose inputs other than itself are one value, is the one-phi case. A
//! component with two or more outside values is a genuine merge, and the same
//! test runs again over the components of its phis that take no outside value.
//! Zero outside values (a fully self-referential cycle) is left alone.
//!
//! A phi takes part only when its `Region` and every input producer are live.
//! Every value input counts, dead predecessor edge or not.

use cranelift_entity::SecondaryMap;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::pipeline::{OptCtx, OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Phis the inner-component reruns may visit, per phi taking part, so the
/// reruns stay linear in the phi count.
const INNER_VISITS_PER_PHI: usize = 4;

#[derive(Clone, Copy)]
pub struct PhiCollapse;

impl Optimizer for PhiCollapse {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _opt: &mut OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let phis: Vec<NodeId> = edit
            .reverse_postorder_filter(|k| matches!(k, NodeKind::Phi | NodeKind::MemPhi))
            .filter(|&phi| {
                edit.node_inputs(phi)
                    .into_iter()
                    .all(|v| edit.is_live(edit.producer(v)))
            })
            .collect();
        let mut sccs = PhiSccs::default();
        let mut frames = vec![sccs.run(edit, &phis)];
        let mut inner_budget = phis.len().saturating_mul(INNER_VISITS_PER_PHI);
        let mut overall = OptimizationResult::NoChange;
        while let Some(frame) = frames.last_mut() {
            let Some(range) = frame.next_component() else {
                frames.pop();
                continue;
            };
            let scc = &frame.nodes[range];
            match sccs.outside_values(edit, scc) {
                Outside::One(value) => {
                    collapse(edit, scc, value)?;
                    overall = OptimizationResult::Changed;
                }
                Outside::Many(inner) if !inner.is_empty() && inner.len() <= inner_budget => {
                    inner_budget -= inner.len();
                    frames.push(sccs.run(edit, &inner));
                }
                Outside::Many(_) | Outside::None => {}
            }
        }
        Ok(overall)
    }
}

/// Redirects every use of the phis in `scc` to `value`, then kills them.
///
/// Killed here rather than by the end-of-iteration cull, so a dead phi stops
/// counting as a live consumer of its Region's phi-token and `RegionCollapse`
/// can detach that Region in the same iteration.
fn collapse(edit: &mut crate::EditFunction<'_>, scc: &[NodeId], value: ValueId) -> Result<()> {
    for &phi in scc {
        let [out] = edit.node_outputs_exact::<1>(phi)?;
        edit.replace_value(out, value)?;
    }
    for &phi in scc {
        edit.kill_node(phi);
    }
    Ok(())
}

enum Outside {
    None,
    One(ValueId),
    /// The component's phis that take no outside value.
    Many(Vec<NodeId>),
}

/// Components of one run, in emission order: each follows every component its
/// phis take a value from.
struct Components {
    nodes: Vec<NodeId>,
    ends: Vec<usize>,
    next: usize,
}

impl Components {
    fn next_component(&mut self) -> Option<std::ops::Range<usize>> {
        let end = *self.ends.get(self.next)?;
        let start = self.next.checked_sub(1).map_or(0, |i| self.ends[i]);
        self.next += 1;
        Some(start..end)
    }
}

#[derive(Clone, Copy, Default)]
struct NodeState {
    /// The run whose member set holds the node.
    run: u32,
    /// Tarjan DFS number, `0` before the visit.
    index: u32,
    low: u32,
    on_stack: bool,
    /// The component the node was last emitted in; ids are unique across runs
    /// and start at 1.
    component: u32,
}

/// Iterative Tarjan over the phi graph restricted to a member set, with state
/// shared across runs so a rerun over a subset allocates nothing per node.
#[derive(Default)]
struct PhiSccs {
    state: SecondaryMap<NodeId, NodeState>,
    runs: u32,
    components: u32,
}

impl PhiSccs {
    fn run(&mut self, edit: &crate::EditFunction<'_>, members: &[NodeId]) -> Components {
        self.runs += 1;
        let run = self.runs;
        for &phi in members {
            self.state[phi] = NodeState {
                run,
                ..NodeState::default()
            };
        }
        let mut out = Components {
            nodes: Vec::with_capacity(members.len()),
            ends: Vec::new(),
            next: 0,
        };
        let mut next_index = 1;
        let mut stack: Vec<NodeId> = Vec::new();
        // `(phi, next input slot)`; slot 0 is the phi-token.
        let mut dfs: Vec<(NodeId, usize)> = Vec::new();
        for &root in members {
            if self.state[root].index != 0 {
                continue;
            }
            self.visit(root, &mut next_index, &mut stack, &mut dfs);
            while let Some(&mut (node, ref mut slot)) = dfs.last_mut() {
                if let Some(input) = edit.nth_input(node, *slot) {
                    *slot += 1;
                    let succ = edit.producer(input);
                    if self.state[succ].run != run {
                        continue;
                    }
                    if self.state[succ].index == 0 {
                        self.visit(succ, &mut next_index, &mut stack, &mut dfs);
                    } else if self.state[succ].on_stack {
                        self.state[node].low = self.state[node].low.min(self.state[succ].index);
                    }
                    continue;
                }
                dfs.pop();
                let low = self.state[node].low;
                if let Some(&(parent, _)) = dfs.last() {
                    self.state[parent].low = self.state[parent].low.min(low);
                }
                if low == self.state[node].index {
                    self.components += 1;
                    loop {
                        let member = stack.pop().expect("the root is on the stack");
                        self.state[member].on_stack = false;
                        self.state[member].component = self.components;
                        out.nodes.push(member);
                        if member == node {
                            break;
                        }
                    }
                    out.ends.push(out.nodes.len());
                }
            }
        }
        out
    }

    fn visit(
        &mut self,
        node: NodeId,
        next_index: &mut u32,
        stack: &mut Vec<NodeId>,
        dfs: &mut Vec<(NodeId, usize)>,
    ) {
        let state = &mut self.state[node];
        state.index = *next_index;
        state.low = *next_index;
        state.on_stack = true;
        *next_index += 1;
        stack.push(node);
        dfs.push((node, 1));
    }

    /// Reads current inputs, so a component emitted earlier and collapsed
    /// since shows as its replacement value.
    fn outside_values(&self, edit: &crate::EditFunction<'_>, scc: &[NodeId]) -> Outside {
        let component = self.state[scc[0]].component;
        let mut unique: Option<ValueId> = None;
        let mut many = false;
        let mut inner = Vec::new();
        for &phi in scc {
            let mut takes_outside = false;
            for value in edit.phi_data_inputs(phi) {
                if self.state[edit.producer(value)].component == component {
                    continue;
                }
                takes_outside = true;
                match unique {
                    None => unique = Some(value),
                    Some(u) if u == value => {}
                    Some(_) => many = true,
                }
            }
            if !takes_outside {
                inner.push(phi);
            }
        }
        match unique {
            Some(_) if many => Outside::Many(inner),
            Some(value)
                if scc.iter().all(|&phi| {
                    edit.value_kind(edit.node_outputs(phi)[0]) == edit.value_kind(value)
                }) =>
            {
                Outside::One(value)
            }
            _ => Outside::None,
        }
    }
}
