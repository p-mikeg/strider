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
//!
//! Braun's argument needs each arm's definition to dominate the phi, which
//! fails on a `Region` unreachable from entry: `phi(token, Add(phi, 2))` is
//! then a one-member component whose single outside value is the `Add`, and
//! collapsing onto it closes a non-phi data cycle.  Such a phi is skipped.
//!
//! The same argument folds `And(phi, C)` to `v` when every arm, masked by `C`,
//! is `v`, an arm that reaches the `And` or the phi itself agreeing with any
//! `v`: a loop that saves `(hi & ~C) | (phi & C)` on its back edge keeps the
//! masked bits it entered with. Each arm is peeled through a bounded chain of
//! `And` / `Or` / `Xor` with a constant or known-zero operand, never through a
//! phi, and a run spends at most [`PEEL_VISITS_PER_ARM`] steps per arm of
//! each phi, so a phi under many masks stays linear; a mask past its phi's
//! budget is left alone.

use cranelift_entity::SecondaryMap;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{ExtendOp, IRBuilderExt, IRViewer, IntBinaryOp};

use crate::const_eval::var_and_const;
use crate::error::Result;
use crate::pipeline::{OptCtx, OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Phis the inner-component reruns may visit, per phi taking part, so the
/// reruns stay linear in the phi count.
const INNER_VISITS_PER_PHI: usize = 4;

/// Nodes one arm of a masked phi is peeled through.
const PEEL_STEPS: usize = 8;

/// Peel steps the masked-phi fold may take per arm of a phi in one run, so a
/// phi with many masked uses stays linear in its arm count.
const PEEL_VISITS_PER_ARM: usize = 2 * PEEL_STEPS;

#[derive(Clone, Copy)]
pub struct PhiCollapse;

impl Optimizer for PhiCollapse {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _opt: &OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let reachable = strider_ir::walk::cfg_reachable(edit.function().graph(), edit.entry());
        let phis: Vec<NodeId> = edit
            .reverse_postorder_filter(|k| matches!(k, NodeKind::Phi | NodeKind::MemPhi))
            .filter(|&phi| {
                edit.nth_input(phi, NodeKind::PHI_TOKEN_SLOT)
                    .is_some_and(|token| reachable.contains(edit.producer(token)))
                    && edit
                        .node_inputs(phi)
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
        let mut walked = Vec::new();
        for &phi in &phis {
            if !edit.is_live(phi) || !matches!(edit.node_kind(phi), NodeKind::Phi) {
                continue;
            }
            let mut budget = (edit.node_inputs(phi).len() - 1).saturating_mul(PEEL_VISITS_PER_ARM);
            let phi_out = edit.node_outputs(phi)[0];
            let masks: Vec<(ValueId, u128, ValueType)> = edit
                .value_uses(phi_out)
                .filter_map(|(user, _)| masked_use(edit, user, phi_out))
                .collect();
            for (and_out, mask, ty) in masks {
                if budget == 0 {
                    break;
                }
                if !edit.is_live(edit.producer(and_out)) {
                    continue;
                }
                walked.clear();
                let Some(value) =
                    masked_phi_value(edit, phi, and_out, mask, ty, &mut walked, &mut budget)?
                else {
                    continue;
                };
                let into = edit.producer(value);
                for &n in &walked {
                    edit.absorb_node_fingerprint(into, n);
                }
                edit.absorb_node_fingerprint(into, phi);
                if edit.replace_value(and_out, value) {
                    overall = OptimizationResult::Changed;
                }
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
        edit.replace_value(out, value);
    }
    for &phi in scc {
        edit.kill_node(phi);
    }
    Ok(())
}

/// `user`'s output and mask when it is `And(phi_out, C)`.
fn masked_use(
    edit: &crate::EditFunction<'_>,
    user: NodeId,
    phi_out: ValueId,
) -> Option<(ValueId, u128, ValueType)> {
    if !matches!(
        edit.node_kind(user),
        NodeKind::IntBinaryOp(IntBinaryOp::And)
    ) {
        return None;
    }
    let (out, ty) = edit.single_value_output(user).ok()?;
    ty.int_mask_u128()?;
    let [a, b] = edit.node_inputs_exact::<2>(user).ok()?;
    let other = if a == phi_out { b } else { a };
    Some((out, edit.int_const_u128(other)?, ty))
}

/// `And(arm, C)` after peeling.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Masked {
    /// The arm reads the `And` being folded, or its phi.
    Itself,
    Const(u128),
    /// An existing value equal to `And(arm, C)`.
    Value(ValueId),
    /// `And(x, C)`.
    Of(ValueId),
}

/// The value every arm of `phi` masked by `mask` agrees on, built if needed;
/// `None` when two arms disagree, every arm is [`Masked::Itself`], or the
/// peel `budget` runs out.
///
/// Each arm other than an `Itself` one is available on its predecessor edge,
/// and so is anything it is peeled to. An `Itself` arm's edge is dominated by
/// the phi's `Region`, so every first entry into the `Region` comes through an
/// agreeing arm, and the agreed value dominates the `Region`.
fn masked_phi_value(
    edit: &mut crate::EditFunction<'_>,
    phi: NodeId,
    and_out: ValueId,
    mask: u128,
    ty: ValueType,
    walked: &mut Vec<NodeId>,
    budget: &mut usize,
) -> Result<Option<ValueId>> {
    let view = &*edit;
    let selves = [view.node_outputs(phi)[0], and_out];
    let mut agreed = None;
    let mut witness = None;
    for arm in view.phi_data_inputs(phi) {
        let Some((masked, seen)) = peel(view, arm, mask, selves, walked, budget) else {
            return Ok(None);
        };
        if masked == Masked::Itself {
            continue;
        }
        if agreed.is_some_and(|a| a != masked) {
            return Ok(None);
        }
        agreed = Some(masked);
        witness = witness.or(seen);
    }
    Ok(match agreed {
        Some(Masked::Const(k)) => Some(edit.build_int_const(k, ty)?),
        Some(Masked::Value(v)) => Some(v),
        Some(Masked::Of(x)) => match witness {
            Some(v) => Some(v),
            None => Some(edit.build_const_binop(mask, x, IntBinaryOp::And, ty)?),
        },
        // Every arm is `Itself`.
        _ => None,
    })
}

/// `And(arm, mask)` through at most [`PEEL_STEPS`] identities, and for a
/// [`Masked::Of`] an existing `And(x, mask)` when the last step peeled one;
/// `None` once `budget` is spent.
fn peel(
    edit: &crate::EditFunction<'_>,
    mut x: ValueId,
    mask: u128,
    selves: [ValueId; 2],
    walked: &mut Vec<NodeId>,
    budget: &mut usize,
) -> Option<(Masked, Option<ValueId>)> {
    let mut witness = None;
    for _ in 0..PEEL_STEPS {
        *budget = budget.checked_sub(1)?;
        if selves.contains(&x) {
            return Some((Masked::Itself, None));
        }
        if let Some(k) = edit.int_const_u128(x) {
            return Some((Masked::Const(k & mask), None));
        }
        let node = edit.producer(x);
        let NodeKind::IntBinaryOp(op) = *edit.node_kind(node) else {
            break;
        };
        let Ok([a, b]) = edit.node_inputs_exact::<2>(node) else {
            break;
        };
        let (next, exact) = match op {
            IntBinaryOp::And => {
                let Some((y, k)) = var_and_const(edit.function(), node) else {
                    break;
                };
                if k & mask == mask {
                    (y, k == mask)
                } else if k & !mask == 0 {
                    return Some((Masked::Value(x), None));
                } else {
                    break;
                }
            }
            IntBinaryOp::Or | IntBinaryOp::Xor => {
                if known_zero(edit, a) & mask == mask {
                    (b, false)
                } else if known_zero(edit, b) & mask == mask {
                    (a, false)
                } else {
                    break;
                }
            }
            _ => break,
        };
        witness = exact.then_some(x);
        walked.push(node);
        x = next;
    }
    Some((Masked::Of(x), witness))
}

/// Bits of `v` its producer alone proves zero: a one-hop subset of
/// `known_bits`' `node_known_bits`, so an over-width `ShiftLeft` proves
/// nothing here where that proves every bit.
fn known_zero(edit: &crate::EditFunction<'_>, v: ValueId) -> u128 {
    if let Some(k) = edit.int_const_u128(v) {
        return !k;
    }
    let node = edit.producer(v);
    match *edit.node_kind(node) {
        NodeKind::IntBinaryOp(IntBinaryOp::And) => {
            var_and_const(edit.function(), node).map_or(0, |(_, k)| !k)
        }
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => {
            let width = edit.value_type_opt(v).map_or(0, ValueType::bit_width);
            edit.nth_input(node, 1)
                .and_then(|count| edit.int_const_u128(count))
                .filter(|&k| k < width as u128)
                .map_or(0, |k| (1u128 << k) - 1)
        }
        NodeKind::Extend(ExtendOp::ZeroExtend) => edit
            .nth_input(node, 0)
            .and_then(|input| edit.value_type_opt(input))
            .and_then(ValueType::int_mask_u128)
            .map_or(0, |m| !m),
        _ => 0,
    }
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
