//! Which arm of a phi control can have taken to reach a point.

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{CtrlKey, Function, IRViewer, control_edge_dominator_tree};

use super::{Path, ReturnTargets};

impl<'f> ReturnTargets<'f> {
    /// Each arm of `phi` control can have taken to reach `path`, observed on
    /// the edge it enters by.
    ///
    /// An arm entering a `Region` both of whose inputs are one `If`'s edges is
    /// cut when an `If` on the same condition value, below the region, takes
    /// the other edge on the way to `path`. The region is not a loop header
    /// (the `If` dominates it), and nothing between the two `If`s is above that
    /// `If`, so both test the one evaluation of the condition that chose the
    /// region's latest arm.
    pub(super) fn arms(
        &mut self,
        path: Path,
        phi: NodeId,
    ) -> impl Iterator<Item = (Path, ValueId)> + 'f {
        let function = self.function;
        let region = function.producer(function.node_inputs(phi)[NodeKind::PHI_TOKEN_SLOT]);
        let feasible = path.and_then(|point| self.feasible_arm(point, region));
        function
            .phi_data_inputs(phi)
            .zip(function.node_inputs(region))
            .enumerate()
            .filter(move |&(arm, _)| feasible.is_none_or(|only| only == arm))
            .map(|(_, (value, edge))| (Some(CtrlKey::Edge(edge)), value))
    }

    /// The one input of `region` control can have arrived through when it
    /// passes `point`, `None` when that is not shown.
    pub(super) fn feasible_arm(&mut self, point: CtrlKey, region: NodeId) -> Option<usize> {
        if let Some(&arm) = self.feasible.get(&(point, region)) {
            return arm;
        }
        let arm = self.decide_arm(point, region);
        self.feasible.insert((point, region), arm);
        arm
    }

    pub(super) fn decide_arm(&mut self, point: CtrlKey, region: NodeId) -> Option<usize> {
        let function = self.function;
        let [a, b] = function.node_inputs_exact::<2>(region).ok()?;
        let branch = function.producer(a);
        if function.producer(b) != branch || !matches!(function.node_kind(branch), NodeKind::If) {
            return None;
        }
        let (cond, flipped) = condition(function, function.if_cond(branch));
        let guards = self
            .guards
            .entry(cond)
            .or_insert_with(|| guards(function, cond).collect());
        let doms = self
            .doms
            .get_or_insert_with(|| control_edge_dominator_tree(function));
        let below = |edge| {
            doms.dominates(CtrlKey::Node(region), CtrlKey::Edge(edge))
                && doms.dominates(CtrlKey::Edge(edge), point)
        };
        // Whether the condition value is true at `point`.
        let known = guards.iter().find_map(|&(guard, guard_flipped)| {
            let &[on_true, on_false] = function.node_outputs(guard) else {
                return None;
            };
            if below(on_true) {
                Some(!guard_flipped)
            } else if below(on_false) {
                Some(guard_flipped)
            } else {
                None
            }
        })?;
        let taken = function.node_outputs(branch)[usize::from(known == flipped)];
        [a, b].iter().position(|&input| input == taken)
    }
}
/// `value` with every `Xor(_, 1)` of a boolean peeled off, and whether an odd
/// number were.
fn condition(function: &Function, mut value: ValueId) -> (ValueId, bool) {
    let mut flipped = false;
    while let Some(inner) = crate::opt::if_cond_inversion::bool_not_operand(function, value) {
        value = inner;
        flipped = !flipped;
    }
    (value, flipped)
}

/// Each `If` testing `cond` or its negation, and whether it tests the negation.
fn guards(function: &Function, cond: ValueId) -> impl Iterator<Item = (NodeId, bool)> + '_ {
    let direct = function.value_uses(cond).map(|(user, _)| (user, false));
    let negations = function
        .value_uses(cond)
        .filter_map(move |(user, _)| {
            let out = function.first_value_output_of(user)?;
            (crate::opt::if_cond_inversion::bool_not_operand(function, out) == Some(cond))
                .then_some(out)
        })
        .flat_map(|out| function.value_uses(out).map(|(user, _)| (user, true)));
    direct
        .chain(negations)
        .filter(|&(user, _)| matches!(function.node_kind(user), NodeKind::If))
}
