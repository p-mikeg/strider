//! Whether a constant branch's surviving arm still reaches a terminator once
//! the branch is folded, answered for every root of one sweep.

use cranelift_entity::SecondaryMap;
use entity_utils::DenseEntitySet;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IRWalker};

#[cfg(test)]
thread_local! {
    /// Whole-CFG walks neither memo could answer.  Unlike them it is not
    /// reset per sweep; a test zeroes it before measuring.
    pub(super) static FULL_WALKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };

    /// Nodes the exact walks popped, reset the same way.
    pub(super) static WALKED_NODES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Three one-sided tiers, each sound: the dead-arm-free escape set, the
/// negative memo, then the exact walk, which recorded witnesses cut short.
/// Valid for one sweep.
#[derive(Default)]
pub(super) struct EscapeOracle {
    /// [`escaping_nodes`], built by the first root that asks.
    escapes: Option<EscapeSet>,
    /// Nodes an exact walk found no terminator from.  Negative only: a verdict
    /// recorded while another root was excluded can differ from this root's
    /// exact answer in one direction only, declining a fold the walk would
    /// have allowed.
    no_escape: DenseEntitySet<NodeId>,
    /// The escape paths earlier exact walks found.
    witnesses: Witnesses,
}

struct EscapeSet {
    escapes: DenseEntitySet<NodeId>,
    /// Every control output a constant selector never takes, as the sweep began.
    dead_arms: DenseEntitySet<ValueId>,
}

/// A walk's route to an escape stops being one only by losing an edge, and a
/// fold removes exactly the dead arms of the branch it kills, so a route
/// proves an escape for `root` while no branch whose dead arm it takes is
/// dead or `root`.  It must also avoid `EscapeOracle::no_escape`, which the
/// walk prunes.
struct Witness {
    /// The constant-selector branches whose dead arms the route takes before
    /// joining `tail`.
    crossed: SmallVec<[NodeId; 2]>,
    /// The witness the route finishes along.
    tail: Option<u32>,
    /// Branches and links [`Witnesses::holds`] checks along the `tail` chain.
    cost: usize,
    /// Cleared when a node on the route joins `EscapeOracle::no_escape`, or is
    /// claimed by a later witness.
    intact: bool,
}

#[derive(Default)]
struct Witnesses {
    /// One past the index of the witness whose route a node is on; `0` for none.
    of: SecondaryMap<NodeId, u32>,
    routes: Vec<Witness>,
}

/// Bounds the check a witness costs; a longer one is not recorded.
const MAX_WITNESS_COST: usize = 16;

impl Witnesses {
    /// Whether `node`'s recorded route still escapes with `root` gone.
    fn holds(&self, edit: &crate::EditFunction<'_>, node: NodeId, root: NodeId) -> bool {
        let Some(mut at) = self.of[node].checked_sub(1) else {
            return false;
        };
        loop {
            let witness = &self.routes[at as usize];
            if !witness.intact
                || witness
                    .crossed
                    .iter()
                    .any(|&branch| branch == root || !edit.is_live(branch))
            {
                return false;
            }
            match witness.tail {
                Some(tail) => at = tail,
                None => return true,
            }
        }
    }

    fn forget(&mut self, node: NodeId) {
        if let Some(at) = self.of[node].checked_sub(1) {
            self.routes[at as usize].intact = false;
        }
    }

    /// Records the route `came_from` leads back from `end`, which is an escape
    /// itself or, with `tail`, the node where the route joins that witness.
    fn record(
        &mut self,
        came_from: &FxHashMap<NodeId, (NodeId, bool)>,
        end: NodeId,
        tail: Option<u32>,
    ) {
        let mut route: Vec<NodeId> = Vec::new();
        let mut crossed: SmallVec<[NodeId; 2]> = SmallVec::new();
        let mut at = end;
        while let Some(&(prev, dead_arm)) = came_from.get(&at) {
            if dead_arm {
                crossed.push(prev);
            }
            route.push(prev);
            at = prev;
        }
        if tail.is_none() {
            route.push(end);
        }
        let cost = crossed.len() + 1 + tail.map_or(0, |t| self.routes[t as usize].cost);
        if cost > MAX_WITNESS_COST {
            return;
        }
        let index = u32::try_from(self.routes.len()).expect("fewer witnesses than nodes");
        self.routes.push(Witness {
            crossed,
            tail,
            cost,
            intact: true,
        });
        for node in route {
            self.forget(node);
            self.of[node] = index + 1;
        }
    }
}

impl EscapeOracle {
    /// Does the surviving successor still reach a terminator once `root` is
    /// gone?
    ///
    /// A loop whose only exit is the dead arm becomes an exit-free cycle, which
    /// [`strider_ir::validate`] rejects as `NoTerminatorReachable` and whose
    /// body compaction then drops. An `Unreachable` directly on the dead arm is
    /// the narrow case of this the `If` fold's guard catches before the fold's
    /// other bookkeeping; here the exit can sit any number of `Region`s away.
    ///
    /// Mirrors the validator: a dangling control output counts as an escape,
    /// so a half-wired CFG mid-pipeline is not read as a stranded one.
    ///
    /// [`escaping_nodes`] answers `true` for most roots without a walk, and
    /// `no_escape` carries the previous walks' `false` verdicts forward; only a
    /// live side neither can vouch for costs the whole-CFG traversal, which
    /// `witnesses` cuts short where an earlier walk's route still stands.
    pub(super) fn live_side_escapes(
        &mut self,
        edit: &crate::EditFunction<'_>,
        root: NodeId,
        live_ctrl: ValueId,
    ) -> bool {
        let stack: Vec<NodeId> = edit.value_uses(live_ctrl).map(|(node, _)| node).collect();
        // The same rule the walk below applies to every other control output:
        // the surviving arm having no consumer at all is a dangling edge, not a
        // stranded one.
        if stack.is_empty() {
            return true;
        }
        let escapes = self.escapes.get_or_insert_with(|| escaping_nodes(edit));
        if stack.iter().any(|&node| escapes.escapes.contains(node)) {
            return true;
        }
        if stack.iter().all(|&node| self.no_escape.contains(node)) {
            return false;
        }
        #[cfg(test)]
        FULL_WALKS.with(|c| c.set(c.get() + 1));
        let mut seen: DenseEntitySet<NodeId> = DenseEntitySet::new();
        let reaches = exact_walk(
            edit,
            root,
            stack,
            &mut seen,
            &self.no_escape,
            &escapes.dead_arms,
            &mut self.witnesses,
        );
        if !reaches {
            // The stack drained, so every node walked has the same verdict.
            for node in &seen {
                self.no_escape.insert(node);
                self.witnesses.forget(node);
            }
        }
        reaches
    }
}

/// The whole-CFG traversal [`EscapeOracle::live_side_escapes`] falls back
/// to, filling `seen` with everything it reached.
fn exact_walk(
    edit: &crate::EditFunction<'_>,
    root: NodeId,
    seeds: Vec<NodeId>,
    seen: &mut DenseEntitySet<NodeId>,
    no_escape: &DenseEntitySet<NodeId>,
    dead_arms: &DenseEntitySet<ValueId>,
    witnesses: &mut Witnesses,
) -> bool {
    // Each entry carries the node that pushed it and whether the edge taken is
    // a dead arm; a seed has none.
    let mut stack: Vec<(NodeId, Option<(NodeId, bool)>)> =
        seeds.into_iter().map(|node| (node, None)).collect();
    let mut came_from: FxHashMap<NodeId, (NodeId, bool)> = FxHashMap::default();
    while let Some((node, edge)) = stack.pop() {
        #[cfg(test)]
        WALKED_NODES.with(|c| c.set(c.get() + 1));
        // `root` is about to go, and its live successors are already seeded.
        if node == root || !seen.insert(node) {
            continue;
        }
        // Already proven terminator-free, so its cone adds nothing.
        if no_escape.contains(node) {
            continue;
        }
        if let Some(edge) = edge {
            came_from.insert(node, edge);
        }
        if witnesses.holds(edit, node, root) {
            let tail = witnesses.of[node] - 1;
            witnesses.record(&came_from, node, Some(tail));
            return true;
        }
        if edit.node_kind(node).is_terminator() {
            witnesses.record(&came_from, node, None);
            return true;
        }
        for &out in edit.node_outputs(node) {
            if !edit.value_kind(out).is_control() {
                continue;
            }
            let mut consumed = false;
            for (succ, _) in edit.value_uses(out) {
                consumed = true;
                stack.push((succ, Some((node, dead_arms.contains(out)))));
            }
            if !consumed {
                witnesses.record(&came_from, node, None);
                return true;
            }
        }
    }
    false
}

/// Nodes from which a terminator or a dangling control edge is reachable
/// without taking the dead arm of a constant-selector branch.
///
/// A sound one-sided answer for [`EscapeOracle::live_side_escapes`]: a
/// route that avoids every dead arm avoids the branch being folded too, so
/// membership proves the live side escapes, while absence proves nothing and
/// falls back to the exact walk.
///
/// Valid for the whole sweep.  A fold rewires the live arm onto the branch's
/// own control input and orphans the dead cone, neither of which is on any
/// route this set was built from.
fn escaping_nodes(edit: &crate::EditFunction<'_>) -> EscapeSet {
    let mut dead_arms: DenseEntitySet<ValueId> = DenseEntitySet::new();
    let nodes: Vec<NodeId> = edit.walk().collect();
    for &node in &nodes {
        for arm in dead_arm_values(edit, node) {
            dead_arms.insert(arm);
        }
    }

    let live_ctrl_outputs = |node: NodeId| {
        edit.node_outputs(node)
            .iter()
            .copied()
            .filter(|&out| edit.value_kind(out).is_control() && !dead_arms.contains(out))
    };

    let mut escapes: DenseEntitySet<NodeId> = DenseEntitySet::new();
    let mut stack: Vec<NodeId> = Vec::new();
    for &node in &nodes {
        let escapes_here = edit.node_kind(node).is_terminator()
            || live_ctrl_outputs(node).any(|out| edit.value_uses(out).next().is_none());
        if escapes_here && escapes.insert(node) {
            stack.push(node);
        }
    }

    strider_ir::walk::close_over_control_preds(
        edit.function().graph(),
        &mut escapes,
        stack,
        |v| dead_arms.contains(v),
        None,
    );
    EscapeSet { escapes, dead_arms }
}

/// The control outputs a constant-selector branch never takes; empty for every
/// other node.
fn dead_arm_values(edit: &crate::EditFunction<'_>, node: NodeId) -> Vec<ValueId> {
    let outputs = edit.node_outputs(node);
    match edit.node_kind(node) {
        NodeKind::If => {
            let [_, cond_value] = edit
                .node_inputs_exact::<2>(node)
                .expect("If has 2 inputs per node signature");
            let [ctrl_true, ctrl_false] = edit
                .node_outputs_exact::<2>(node)
                .expect("If has 2 outputs per node signature");
            let Some(cond) = edit.bool_const_val(cond_value) else {
                return Vec::new();
            };
            vec![if cond { ctrl_false } else { ctrl_true }]
        }
        NodeKind::Switch(_) => {
            let addr_value = edit.node_inputs(node)[1];
            let Some(k) = edit.int_const_u128(addr_value) else {
                return Vec::new();
            };
            let Some(live) = edit
                .function()
                .switch_targets(node)
                .iter()
                .position(|&t| u128::from(t) == k)
            else {
                return Vec::new();
            };
            outputs
                .iter()
                .enumerate()
                .filter(|&(i, _)| i != live)
                .map(|(_, &out)| out)
                .collect()
        }
        _ => Vec::new(),
    }
}
