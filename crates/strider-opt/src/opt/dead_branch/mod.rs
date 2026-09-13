//! Folds a control branch with a constant selector: an `If` with a constant
//! `I1` condition, or a `Switch` with a constant dispatch address.  The live
//! control output is replaced with the branch's own control input so the live
//! successor is wired past it, then the branch node is killed unconditionally
//! (it is side-effecting, so the automatic dead-cone cull never reaches it).
//!
//! The fold is DECLINED where dropping the dead arm would strand memory: an
//! `Unreachable` consuming the dead control output, which only the `If` arm
//! tests, or a live side reaching no terminator, which both test.
//!
//! A dead subgraph can still escape to live data (a dead `Call`'s `mem_value`
//! flowing into a live `MemPhi`), leaving it transiently reachable backward
//! through data.  Validation runs only once the destructive pipeline converges,
//! so that transient shape is never observed.

use std::cell::RefCell;

use cranelift_entity::SecondaryMap;
use entity_utils::DenseEntitySet;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IRWalker};

use crate::error::Result;
use crate::peephole::{PeepholePass, PeepholeRewrite};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub struct DeadBranchElimination;

impl PeepholePass for DeadBranchElimination {
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::If | NodeKind::Switch(_))
    }

    fn try_rewrite(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        match edit.node_kind(root) {
            NodeKind::If => {
                // Inputs are [ctrl_in, condition].  A detached If is never handed
                // back here (no consumer re-enqueue, no reported `new_node`), so
                // `root` always still carries both.
                let [ctrl_value, cond_value] = edit
                    .graph_ref()
                    .node_inputs_exact::<2>(root)
                    .expect("If has 2 inputs per node signature");

                let Some(cond_val) = edit.function().bool_const_val(cond_value) else {
                    return Ok(PeepholeRewrite::NoChange);
                };

                let [ctrl_true, ctrl_false] = edit
                    .node_outputs_exact::<2>(root)
                    .expect("If has 2 outputs per node signature");
                let live_ctrl = if cond_val { ctrl_true } else { ctrl_false };
                let dead_ctrl = if cond_val { ctrl_false } else { ctrl_true };

                // An `Unreachable` on the dead arm anchors the memory of an
                // exit-free control cycle. Folding the branch orphans it and the
                // cycle loses its stores. `live_side_reaches_terminator` below
                // rejects that shape too, so this is the deliberately
                // conservative half: it also declines a dead arm that is a bare
                // `Unreachable`, where the fold would be sound. `if (const) ..
                // else abort();` still folds, its dead arm reaching the
                // `Unreachable` through the `Call`. Kept because the cost is one
                // unfolded branch and the failure mode is a lost store.
                let dead_consumers: Vec<NodeId> = edit
                    .graph_ref()
                    .value_uses(dead_ctrl)
                    .map(|(node, _)| node)
                    .collect();
                if dead_consumers
                    .iter()
                    .any(|&node| matches!(edit.node_kind(node), NodeKind::Unreachable))
                {
                    return Ok(PeepholeRewrite::NoChange);
                }

                if !live_side_reaches_terminator(edit, root, live_ctrl) {
                    return Ok(PeepholeRewrite::NoChange);
                }

                // The condition is the proof for taking this arm, so its
                // fingerprint must survive `kill_node` cascade-culling the
                // condition cone.  Absorb it into the surviving control source.
                edit.absorb_fingerprint(ctrl_value, cond_value);

                edit.replace_value(live_ctrl, ctrl_value)?;
                edit.kill_node(root);
                Ok(PeepholeRewrite::Changed { new_node: None })
            }
            NodeKind::Switch(_) => {
                let inputs = edit.graph_ref().node_inputs(root);
                let (ctrl_value, addr_value) = (inputs[0], inputs[1]);

                let Some(k) = edit.function().int_const_u128(addr_value) else {
                    return Ok(PeepholeRewrite::NoChange);
                };

                // Output i corresponds to cases[i].  Computing the index inside
                // this expression ends the immutable borrow of `edit.function()`
                // before the mutable calls below, without cloning the slice.
                let Some(i) = edit
                    .function()
                    .switch_targets(root)
                    .iter()
                    .position(|&t| u128::from(t) == k)
                else {
                    return Ok(PeepholeRewrite::NoChange); // exhaustive table => shouldn't happen
                };
                let live_ctrl = edit.node_outputs(root)[i];

                if !live_side_reaches_terminator(edit, root, live_ctrl) {
                    return Ok(PeepholeRewrite::NoChange);
                }

                // Same proof-completeness rationale as the `If` arm, with the
                // constant dispatch address in place of the condition.
                edit.absorb_fingerprint(ctrl_value, addr_value);
                edit.replace_value(live_ctrl, ctrl_value)?;
                edit.kill_node(root);
                Ok(PeepholeRewrite::Changed { new_node: None })
            }
            _ => Ok(PeepholeRewrite::NoChange),
        }
    }

    fn start_sweep(&self) {
        ESCAPES.with(|memo| *memo.borrow_mut() = None);
        NO_ESCAPE.with(|memo| *memo.borrow_mut() = DenseEntitySet::new());
        WITNESSES.with(|memo| *memo.borrow_mut() = Witnesses::default());
    }

    fn propagate_to_consumers(&self) -> bool {
        false
    }
}

#[cfg(test)]
thread_local! {
    /// Whole-CFG walks neither memo below could answer.  Unlike them it is not
    /// reset per sweep; a test zeroes it before measuring.
    pub(super) static FULL_WALKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };

    /// Nodes the exact walks popped, reset the same way.
    pub(super) static WALKED_NODES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// [`escaping_nodes`] for the sweep in progress, shared by every root.
    /// Not a field on the pass: `DeadBranchElimination` is a unit struct other
    /// crates name as a value.
    static ESCAPES: RefCell<Option<EscapeSet>> = const { RefCell::new(None) };

    /// Nodes an exact walk found no terminator from, shared by every root of
    /// the sweep.  Negative only: a verdict recorded while another root was
    /// excluded can differ from this root's exact answer in one direction
    /// only, declining a fold the walk would have allowed.
    static NO_ESCAPE: RefCell<DenseEntitySet<NodeId>> = RefCell::new(DenseEntitySet::new());

    /// The escape paths earlier exact walks found, shared by every root of the
    /// sweep.
    static WITNESSES: RefCell<Witnesses> = RefCell::new(Witnesses::default());
}

struct EscapeSet {
    escapes: DenseEntitySet<NodeId>,
    /// Every control output a constant selector never takes, as the sweep began.
    dead_arms: DenseEntitySet<ValueId>,
}

/// A walk's route to an escape stops being one only by losing an edge, and a
/// fold removes exactly the dead arms of the branch it kills, so a route
/// proves an escape for `root` while no branch whose dead arm it takes is
/// dead or `root`.  It must also avoid [`NO_ESCAPE`], which the walk prunes.
struct Witness {
    /// The constant-selector branches whose dead arms the route takes before
    /// joining `tail`.
    crossed: SmallVec<[NodeId; 2]>,
    /// The witness the route finishes along.
    tail: Option<u32>,
    /// Branches and links [`Witnesses::holds`] checks along the `tail` chain.
    cost: usize,
    /// Cleared when a node on the route joins [`NO_ESCAPE`], or is claimed by
    /// a later witness.
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

/// Does the surviving successor still reach a terminator once `root` is gone?
///
/// A loop whose only exit is the dead arm becomes an exit-free cycle, which
/// [`strider_ir::validate`] rejects as `NoTerminatorReachable` and whose body
/// compaction then drops. An `Unreachable` directly on the dead arm is the
/// narrow case of this the `If` arm's guard above catches before the fold's
/// other bookkeeping; here the exit can sit any number of `Region`s away.
///
/// Mirrors the validator: a dangling control output counts as an escape, so a
/// half-wired CFG mid-pipeline is not read as a stranded one.
///
/// [`escaping_nodes`] answers `true` for most roots without a walk, and
/// [`NO_ESCAPE`] carries the previous walks' `false` verdicts forward; only a
/// live side neither can vouch for costs the whole-CFG traversal, which
/// [`WITNESSES`] cuts short where an earlier walk's route still stands.
fn live_side_reaches_terminator(
    edit: &crate::EditFunction<'_>,
    root: NodeId,
    live_ctrl: ValueId,
) -> bool {
    let mut seen: DenseEntitySet<NodeId> = DenseEntitySet::new();
    let stack: Vec<NodeId> = edit.value_uses(live_ctrl).map(|(node, _)| node).collect();
    // The same rule the walk below applies to every other control output: the
    // surviving arm having no consumer at all is a dangling edge, not a
    // stranded one.
    if stack.is_empty() {
        return true;
    }
    if ESCAPES.with(|memo| {
        let mut memo = memo.borrow_mut();
        let set = memo.get_or_insert_with(|| escaping_nodes(edit));
        stack.iter().any(|&node| set.escapes.contains(node))
    }) {
        return true;
    }
    if NO_ESCAPE.with(|memo| {
        let memo = memo.borrow();
        stack.iter().all(|&node| memo.contains(node))
    }) {
        return false;
    }
    #[cfg(test)]
    FULL_WALKS.with(|c| c.set(c.get() + 1));
    let reaches = ESCAPES.with(|escapes| {
        NO_ESCAPE.with(|no_escape| {
            WITNESSES.with(|witnesses| {
                let escapes = escapes.borrow();
                let dead_arms = &escapes.as_ref().expect("filled above").dead_arms;
                exact_walk(
                    edit,
                    root,
                    stack,
                    &mut seen,
                    &no_escape.borrow(),
                    dead_arms,
                    &mut witnesses.borrow_mut(),
                )
            })
        })
    });
    if !reaches {
        // The stack drained, so every node walked has the same verdict.
        NO_ESCAPE.with(|memo| {
            WITNESSES.with(|witnesses| {
                let (mut memo, mut witnesses) = (memo.borrow_mut(), witnesses.borrow_mut());
                for node in &seen {
                    memo.insert(node);
                    witnesses.forget(node);
                }
            });
        });
    }
    reaches
}

/// The whole-CFG traversal [`live_side_reaches_terminator`] falls back to,
/// filling `seen` with everything it reached.
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
/// A sound one-sided answer for [`live_side_reaches_terminator`]: a route that
/// avoids every dead arm avoids the branch being folded too, so membership
/// proves the live side escapes, while absence proves nothing and falls back
/// to the exact walk.
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
