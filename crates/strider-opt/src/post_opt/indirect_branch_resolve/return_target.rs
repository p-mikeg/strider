//! Whether a return instruction jumps back to the address the function was
//! entered with.

use rustc_hash::{FxHashMap, FxHashSet};
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{
    CtrlKey, DominatorTree, Function, IRViewer, IntBinaryOp, control_edge_dominator_tree,
};

use crate::mem_analysis::{
    MemExpr, MemKind, alignment_masked_operand, decompose, store_value_byte_size,
    wrap_to_addr_width,
};

/// Answers, for the address each Sleigh `RETURN` op of one function jumps to,
/// whether it is the one the function was entered with: the entry link
/// register, or on a stack-push ISA the entry-SP slot a `call` pushed it into.
///
/// Read off the optimised IR, so a return address saved to the stack and
/// restored is followed back to the store that saved it, and a stack pointer
/// saved in a realigned frame back to the store that saved it. `false` when
/// that cannot be shown, and the site is then an indirect branch.
///
/// The one claim this rests on beyond the IR: only a store this function makes
/// at a known offset from its entry SP, or from an alignment of it, can
/// overwrite a value it saved to the stack. A callee, a user-op, or a store
/// through any other pointer is taken to leave it alone; code that does
/// otherwise smashes its own stack.
pub struct ReturnTargets<'f> {
    function: &'f Function,
    assumptions: &'f crate::AssumptionOptions,
    /// Every claim a query has decided. Return sites mostly share their
    /// claims, and each is explored once.
    settled: FxHashMap<(Path, Claim), bool>,
    doms: Option<DominatorTree<CtrlKey>>,
    feasible: FxHashMap<(CtrlKey, NodeId), Option<usize>>,
    /// The one offset from the entry SP a value is claimed at, per path.
    claimed: FxHashMap<(Path, ValueId), i128>,
    guesses: FxHashMap<(Path, ValueId), Option<i128>>,
    stored: FxHashMap<(ValueId, StackAddr, i128), Option<ValueId>>,
    guards: FxHashMap<ValueId, Vec<(NodeId, bool)>>,
}

/// Where a claim is observed: `Some(point)` each time control passes `point`,
/// of the values current there, `None` everywhere.
type Path = Option<CtrlKey>;

/// One fact a return target's verification rests on.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Claim {
    /// The value is the address the function was entered with.
    Target(ValueId),
    /// The value is the entry SP plus `offset`.
    EntrySp { value: ValueId, offset: i128 },
    /// In memory state `mem`, the `size` bytes at `at` hold `holds`.
    Slot {
        mem: ValueId,
        at: StackAddr,
        size: i128,
        holds: Held,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Held {
    ReturnAddress,
    /// The entry SP plus this offset.
    EntrySp(i128),
}

/// `offset` bytes off the entry SP (`base: None`) or off an alignment-masked
/// SP value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct StackAddr {
    base: Option<ValueId>,
    offset: i128,
}

enum StoreEffect {
    Writes(ValueId),
    Clobbers,
    Passes,
}

impl<'f> ReturnTargets<'f> {
    #[must_use]
    pub fn new(function: &'f Function, assumptions: &'f crate::AssumptionOptions) -> Self {
        Self {
            function,
            assumptions,
            settled: FxHashMap::default(),
            doms: None,
            feasible: FxHashMap::default(),
            claimed: FxHashMap::default(),
            guesses: FxHashMap::default(),
            stored: FxHashMap::default(),
            guards: FxHashMap::default(),
        }
    }

    /// Whether `target`, the address `ret` jumps to, is the address the
    /// function was entered with.
    ///
    /// A claim holds unless one it rests on fails outright, so a cycle of
    /// claims (a loop rewriting a slot with its own value, a phi of itself)
    /// brings in no value of its own. Linear in the claims reached.
    pub fn returns_to_caller(&mut self, ret: NodeId, target: ValueId) -> bool {
        let root = (Some(CtrlKey::Node(ret)), Claim::Target(target));
        let mut index: FxHashMap<(Path, Claim), usize> = FxHashMap::default();
        index.insert(root, 0);
        let mut claims = vec![root];
        // Per claim, the claims resting on it.
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new()];
        let mut failed: Vec<usize> = Vec::new();
        let mut premises = Vec::new();
        let mut next = 0;
        while let Some(&(path, claim)) = claims.get(next) {
            premises.clear();
            if !self.premises(path, claim, &mut premises) {
                failed.push(next);
            }
            for &premise in &premises {
                match self.settled.get(&premise) {
                    Some(true) => {}
                    Some(false) => failed.push(next),
                    None => {
                        let at = *index.entry(premise).or_insert_with(|| {
                            claims.push(premise);
                            dependents.push(Vec::new());
                            claims.len() - 1
                        });
                        dependents[at].push(next);
                    }
                }
            }
            next += 1;
        }
        let mut holds = vec![true; claims.len()];
        while let Some(at) = failed.pop() {
            if std::mem::replace(&mut holds[at], false) {
                failed.extend_from_slice(&dependents[at]);
            }
        }
        self.settled.extend(claims.into_iter().zip(holds));
        self.settled[&root]
    }

    /// Pushes what `claim` rests on onto `out`; `false` when it fails outright.
    fn premises(&mut self, path: Path, claim: Claim, out: &mut Vec<(Path, Claim)>) -> bool {
        let function = self.function;
        match claim {
            Claim::Target(target) => {
                let value = super::strip_isa_mode_mask(function, target);
                let producer = function.producer(value);
                match *function.node_kind(producer) {
                    NodeKind::InitialVar(id) => {
                        function.default_cc().link_register_vn == Some(function.initial_vn(id))
                    }
                    NodeKind::Phi => {
                        let arms = self.arms(path, producer);
                        out.extend(arms.map(|(path, arm)| (path, Claim::Target(arm))));
                        true
                    }
                    NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => {
                        self.push_slot(path, producer, value, Held::ReturnAddress, out)
                    }
                    _ => false,
                }
            }
            Claim::EntrySp { value, offset } => {
                if *self.claimed.entry((path, value)).or_insert(offset) != offset {
                    return false;
                }
                if let Some(at) = self.spine(value) {
                    return at.base.is_none() && at.offset == offset;
                }
                let producer = function.producer(value);
                match *function.node_kind(producer) {
                    NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                        let Some((operand, addend)) = const_addend(function, producer) else {
                            return false;
                        };
                        let offset = wrap_to_addr_width(function, value, offset - addend);
                        out.push((
                            path,
                            Claim::EntrySp {
                                value: operand,
                                offset,
                            },
                        ));
                        true
                    }
                    NodeKind::Phi => {
                        let arms = self.arms(path, producer);
                        out.extend(
                            arms.map(|(path, value)| (path, Claim::EntrySp { value, offset })),
                        );
                        true
                    }
                    NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => {
                        self.push_slot(path, producer, value, Held::EntrySp(offset), out)
                    }
                    _ => false,
                }
            }
            Claim::Slot {
                mem,
                at,
                size,
                holds,
            } => {
                let node = function.producer(mem);
                let below = |mem| {
                    (
                        None,
                        Claim::Slot {
                            mem,
                            at,
                            size,
                            holds,
                        },
                    )
                };
                match *function.node_kind(node) {
                    // Untouched since entry: a stack-push ISA's return slot.
                    NodeKind::InitialMemory => {
                        holds == Held::ReturnAddress
                            && at.base.is_none()
                            && at.offset == 0
                            && function.default_cc().link_register_vn.is_none()
                    }
                    NodeKind::MemPhi => {
                        out.extend(function.phi_data_inputs(node).map(below));
                        true
                    }
                    NodeKind::Store(_) => match self.store_effect(node, at, size) {
                        StoreEffect::Writes(data) => {
                            out.push((
                                None,
                                match holds {
                                    Held::ReturnAddress => Claim::Target(data),
                                    Held::EntrySp(offset) => Claim::EntrySp {
                                        value: data,
                                        offset,
                                    },
                                },
                            ));
                            true
                        }
                        StoreEffect::Clobbers => false,
                        StoreEffect::Passes => {
                            out.extend(function.memory_input_of(node).map(below));
                            true
                        }
                    },
                    NodeKind::Call { .. } | NodeKind::CallOther { .. } => {
                        out.extend(function.memory_input_of(node).map(below));
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    /// Each arm of `phi` control can have taken to reach `path`, observed on
    /// the edge it enters by.
    ///
    /// An arm entering a `Region` both of whose inputs are one `If`'s edges is
    /// cut when an `If` on the same condition value, below the region, takes
    /// the other edge on the way to `path`. The region is not a loop header
    /// (the `If` dominates it), and nothing between the two `If`s is above that
    /// `If`, so both test the one evaluation of the condition that chose the
    /// region's latest arm.
    fn arms(&mut self, path: Path, phi: NodeId) -> impl Iterator<Item = (Path, ValueId)> + 'f {
        let function = self.function;
        let region = function.producer(function.node_inputs(phi)[0]);
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
    fn feasible_arm(&mut self, point: CtrlKey, region: NodeId) -> Option<usize> {
        if let Some(&arm) = self.feasible.get(&(point, region)) {
            return arm;
        }
        let arm = self.decide_arm(point, region);
        self.feasible.insert((point, region), arm);
        arm
    }

    fn decide_arm(&mut self, point: CtrlKey, region: NodeId) -> Option<usize> {
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

    /// Pushes the slot a `Load` reads, holding `holds`, for the loaded
    /// `value`; `false` when its address is no known stack address.
    fn push_slot(
        &mut self,
        path: Path,
        load: NodeId,
        value: ValueId,
        holds: Held,
        out: &mut Vec<(Path, Claim)>,
    ) -> bool {
        let function = self.function;
        let (Some(at), Ok(ty), Some(mem)) = (
            self.locate(path, function.load_addr(load), out),
            function.value_type(value),
            function.memory_input_of(load),
        ) else {
            return false;
        };
        out.push((
            None,
            Claim::Slot {
                mem,
                at,
                size: ty.byte_size() as i128,
                holds,
            },
        ));
        true
    }

    /// `addr` as a stack address. One the address spine does not name is
    /// guessed as an entry-SP offset, and the claim that it is one pushed.
    fn locate(
        &mut self,
        path: Path,
        addr: ValueId,
        out: &mut Vec<(Path, Claim)>,
    ) -> Option<StackAddr> {
        if let Some(at) = self.spine(addr) {
            return Some(at);
        }
        let offset = self.guess(path, addr)?;
        out.push((
            path,
            Claim::EntrySp {
                value: addr,
                offset,
            },
        ));
        Some(StackAddr { base: None, offset })
    }

    /// The stack address the spine of `addr` names.
    fn spine(&self, addr: ValueId) -> Option<StackAddr> {
        let function = self.function;
        let MemExpr {
            base,
            offset,
            kind: MemKind::Stack,
        } = decompose(function, addr, &self.assumptions.noalias_allocators)?
        else {
            return None;
        };
        let at_entry = matches!(
            *function.node_kind(function.producer(base)),
            NodeKind::InitialVar(id) if function.initial_vn(id) == function.default_cc().stack_vn
        );
        Some(StackAddr {
            base: (!at_entry).then_some(base),
            offset,
        })
    }

    /// An entry-SP offset `value` may hold along `path`, read off one chain of
    /// additions, phi arms and the stack slots address spines name. A guess
    /// only: a claim proves it.
    fn guess(&mut self, path: Path, value: ValueId) -> Option<i128> {
        let function = self.function;
        let mut trail: Vec<((Path, ValueId), i128)> = Vec::new();
        let mut on_trail = FxHashSet::default();
        let (mut path, mut cur, mut accrued) = (path, value, 0i128);
        let found = loop {
            if let Some(&known) = self.guesses.get(&(path, cur)) {
                break known.map(|offset| offset + accrued);
            }
            if !on_trail.insert((path, cur)) {
                break None;
            }
            trail.push(((path, cur), accrued));
            if let Some(at) = self.spine(cur) {
                break at.base.is_none().then_some(at.offset + accrued);
            }
            let producer = function.producer(cur);
            match *function.node_kind(producer) {
                NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                    let Some((operand, addend)) = const_addend(function, producer) else {
                        break None;
                    };
                    accrued += addend;
                    cur = operand;
                }
                NodeKind::Phi => {
                    let Some(arm) = self
                        .arms(path, producer)
                        .find(|arm| !on_trail.contains(arm))
                    else {
                        break None;
                    };
                    (path, cur) = arm;
                }
                NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => {
                    let (Some(at), Ok(ty), Some(mem)) = (
                        self.spine(function.load_addr(producer)),
                        function.value_type(cur),
                        function.memory_input_of(producer),
                    ) else {
                        break None;
                    };
                    let Some(data) = self.stored_value(mem, at, ty.byte_size() as i128) else {
                        break None;
                    };
                    path = None;
                    cur = data;
                }
                _ => break None,
            }
        };
        let found = found.map(|offset| wrap_to_addr_width(function, value, offset));
        for (key, accrued) in trail {
            let offset = found.map(|offset| wrap_to_addr_width(function, key.1, offset - accrued));
            self.guesses.insert(key, offset);
        }
        found
    }

    /// The value a store left in the `size` bytes at `at` in memory state
    /// `mem`: the first store to write them found searching back from `mem`.
    /// A guess only, cached for every state the search visits.
    fn stored_value(&mut self, mem: ValueId, at: StackAddr, size: i128) -> Option<ValueId> {
        let function = self.function;
        let mut seen = Vec::new();
        let mut on_stack = FxHashSet::default();
        let mut stack = vec![mem];
        let mut found = None;
        while let Some(cur) = stack.pop() {
            if let Some(&known) = self.stored.get(&(cur, at, size)) {
                found = known;
                if found.is_some() {
                    break;
                }
                continue;
            }
            if !on_stack.insert(cur) {
                continue;
            }
            seen.push(cur);
            let node = function.producer(cur);
            match *function.node_kind(node) {
                NodeKind::Store(_) => match self.store_effect(node, at, size) {
                    StoreEffect::Writes(data) => {
                        found = Some(data);
                        break;
                    }
                    StoreEffect::Clobbers => {}
                    StoreEffect::Passes => stack.extend(function.memory_input_of(node)),
                },
                NodeKind::Call { .. } | NodeKind::CallOther { .. } => {
                    stack.extend(function.memory_input_of(node));
                }
                NodeKind::MemPhi => {
                    let arms: Vec<ValueId> = function.phi_data_inputs(node).collect();
                    stack.extend(arms.into_iter().rev());
                }
                _ => {}
            }
        }
        for cur in seen {
            self.stored.insert((cur, at, size), found);
        }
        found
    }

    /// What `store` does to the `size` bytes at `at`.
    fn store_effect(&self, store: NodeId, at: StackAddr, size: i128) -> StoreEffect {
        let function = self.function;
        let NodeKind::Store(space) = *function.node_kind(store) else {
            unreachable!("store_effect on a non-Store");
        };
        let written = (space == rsleigh::VnSpace::RAM)
            .then(|| self.spine(function.store_addr(store)))
            .flatten();
        let Some(written) = written else {
            return StoreEffect::Passes;
        };
        let data = function.store_data(store);
        let len = store_value_byte_size(function, data);
        if written.base == at.base {
            return if written.offset == at.offset && len == size {
                StoreEffect::Writes(data)
            } else if written.offset < at.offset + size && at.offset < written.offset + len {
                StoreEffect::Clobbers
            } else {
                StoreEffect::Passes
            };
        }
        // Offsets off two different bases compare through the range of entry-SP
        // offsets each base can stand at.
        match (self.entry_span(written), self.entry_span(at)) {
            (Some((w_lo, w_hi)), Some((a_lo, a_hi)))
                if w_lo >= a_hi + size || a_lo >= w_hi + len =>
            {
                StoreEffect::Passes
            }
            _ => StoreEffect::Clobbers,
        }
    }

    /// The lowest and highest entry-SP offset `at` can stand at.
    fn entry_span(&self, at: StackAddr) -> Option<(i128, i128)> {
        let function = self.function;
        let (mut lo, mut hi, mut base) = (at.offset, at.offset, at.base);
        while let Some(aligned) = base {
            let and = function.producer(aligned);
            let operand = alignment_masked_operand(function, and)?;
            let [l, r] = function.node_inputs_exact::<2>(and).ok()?;
            let mask = function.int_const_u128(if operand == l { r } else { l })?;
            let below = self.spine(operand)?;
            lo += below.offset - ((1i128 << mask.trailing_zeros()) - 1);
            hi += below.offset;
            base = below.base;
        }
        Some((lo, hi))
    }
}

/// `value` with every `Xor(_, 1)` of a boolean peeled off, and whether an odd
/// number were.
fn condition(function: &Function, mut value: ValueId) -> (ValueId, bool) {
    let mut flipped = false;
    while let Some(inner) = negated(function, value) {
        value = inner;
        flipped = !flipped;
    }
    (value, flipped)
}

/// `x` for a boolean `Xor(x, 1)`.
fn negated(function: &Function, value: ValueId) -> Option<ValueId> {
    if !function.value_kind(value).is_bool() {
        return None;
    }
    let producer = function.producer(value);
    if !matches!(
        function.node_kind(producer),
        NodeKind::IntBinaryOp(IntBinaryOp::Xor)
    ) {
        return None;
    }
    let [l, r] = function.node_inputs_exact::<2>(producer).ok()?;
    match (function.bool_const_val(l), function.bool_const_val(r)) {
        (_, Some(true)) => Some(l),
        (Some(true), _) => Some(r),
        _ => None,
    }
}

/// Each `If` testing `cond` or its negation, and whether it tests the negation.
fn guards(function: &Function, cond: ValueId) -> impl Iterator<Item = (NodeId, bool)> + '_ {
    let direct = function.value_uses(cond).map(|(user, _)| (user, false));
    let negations = function
        .value_uses(cond)
        .filter_map(move |(user, _)| {
            let out = function.first_value_output_of(user)?;
            (negated(function, out) == Some(cond)).then_some(out)
        })
        .flat_map(|out| function.value_uses(out).map(|(user, _)| (user, true)));
    direct
        .chain(negations)
        .filter(|&(user, _)| matches!(function.node_kind(user), NodeKind::If))
}

/// `(x, c)` for `Add(x, c)` with `c` a constant.
fn const_addend(function: &Function, add: NodeId) -> Option<(ValueId, i128)> {
    let [l, r] = function.node_inputs_exact::<2>(add).ok()?;
    match (function.int_const_i128(r), function.int_const_i128(l)) {
        (Some(c), _) => Some((l, c)),
        (None, Some(c)) => Some((r, c)),
        _ => None,
    }
}
