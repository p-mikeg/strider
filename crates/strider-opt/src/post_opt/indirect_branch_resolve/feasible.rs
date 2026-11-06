use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{CtrlKey, DominatorTree, Function, IRViewer, control_edge_dominator_tree};

/// Which input of a `Region` of one `If`'s two edges control can have
/// arrived through on its way to a point: the one an `If` on the same
/// condition below the region agrees with.
pub(crate) struct FeasibleArms<'f> {
    function: &'f Function,
    doms: Option<DominatorTree<CtrlKey>>,
    memo: FxHashMap<(CtrlKey, NodeId), Option<usize>>,
    guards: FxHashMap<ValueId, Vec<(NodeId, bool)>>,
}

impl<'f> FeasibleArms<'f> {
    pub(crate) fn new(function: &'f Function) -> Self {
        Self {
            function,
            doms: None,
            memo: FxHashMap::default(),
            guards: FxHashMap::default(),
        }
    }

    /// The one input of `region` control can have arrived through when it
    /// passes `point`, `None` when that is not shown.
    pub(crate) fn only_arm(&mut self, point: CtrlKey, region: NodeId) -> Option<usize> {
        if let Some(&arm) = self.memo.get(&(point, region)) {
            return arm;
        }
        let arm = self.decide(point, region);
        self.memo.insert((point, region), arm);
        arm
    }

    fn decide(&mut self, point: CtrlKey, region: NodeId) -> Option<usize> {
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
